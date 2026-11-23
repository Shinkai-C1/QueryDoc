//! 解析执行器（任务 3.2）：PDF → 渲染 → doc_ori → deskew → det → textline_ori →
//! rec（单字框）→ 金额校验 → 段落聚类 → text_flow/flow_map → 0–1000 归一化 → ocr.json + .md。
//! 全部坐标最终逆映射回原始渲染页坐标系。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::{Rgb, RgbImage};
use imageproc::geometric_transformations::{warp_into, Interpolation, Projection};
use pdfium_render::prelude::*;
use regex::Regex;

use crate::error::AppError;
use crate::ocr::geo::{Homography, Pt, Quad};
use crate::ocr::{cluster, deskew, detect, models::OrtEngine, orient, recognize, schema};

/// 渲染 DPI（Spike 1.8 结论：150/200 精度相当，取 200 留密集字符余量）。
pub const DEFAULT_DPI: u32 = 200;

/// deskew 触发阈值（度）。
const DESKEW_TRIGGER_DEG: f32 = 0.5;

/// 行级 rec 置信度下限：低于则丢弃该行（印章/残影噪声；实测真实行 ≥0.97、噪声 ≤0.72）。
const MIN_LINE_SCORE: f32 = 0.75;

pub struct ParseFileOutput {
    pub json_path: PathBuf,
    /// .md 路径（parse.rs 目前只回写 json_path；md 供 Phase 6 导出等复用）
    #[allow(dead_code)]
    pub md_path: PathBuf,
    pub page_count: u32,
}

/// 可疑金额模式（不改正文，仅标注）：
/// - `\d\.\d{3}\.\d`：小数点被当千分位（1.234.567）
/// - `\d,\d{1,2}([^\d,]|$)`：千分位分组不足 3 位（1,23 / 12,34 / 1,234,56）
/// - `\d,\d{4,}`：分组超 3 位（1,2345）
pub fn amount_suspect(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"\d\.\d{3}\.\d|\d,\d{1,2}([^\d,]|$)|\d,\d{4,}").unwrap());
    re.is_match(text)
}

/// 裁剪行区域（带 padding），返回行图和 crop→page 的坐标映射。
fn crop_line(img: &RgbImage, quad: &Quad, pad: f32) -> Option<(RgbImage, Homography)> {
    let q = quad.expand(pad, pad);
    let w = q.width().ceil().max(1.0) as u32;
    let h = q.height().ceil().max(1.0) as u32;
    let dst = [
        (0.0f32, 0.0f32),
        (w as f32 - 1.0, 0.0),
        (w as f32 - 1.0, h as f32 - 1.0),
        (0.0, h as f32 - 1.0),
    ];
    // warp_into 内部对传入 projection 取逆：传入 page→crop，采样即 crop→page
    let proj = Projection::from_control_points(q.pts, dst)?;
    let mut out = RgbImage::new(w, h);
    warp_into(
        img,
        &proj,
        Interpolation::Bilinear,
        Rgb([255, 255, 255]),
        &mut out,
    );
    let crop2page = Homography::from_control_points(dst, q.pts)?;
    Some((out, crop2page))
}

/// 行中间产物（像素坐标，已逆映射回原始页面）。
struct LineWork {
    text: String,
    bbox: [f32; 4],
    chars: Vec<(String, [f32; 4])>,
    amount_suspect: bool,
    /// 行级 rec 平均置信度（写入 ocr.json 供 Phase 4 confidence 计算）
    score: f32,
}

/// 坐标链：crop_rec → （行翻转还原） → homography → deskew 逆 → doc_ori 逆 → 原始页面。
#[allow(clippy::too_many_arguments)]
fn map_char_bbox(
    x0: f32,
    x1: f32,
    crop_w: u32,
    crop_h: u32,
    flipped: bool,
    crop2page: &Homography,
    rot: Option<&deskew::Rot>,
    deg: u32,
    orig_w: u32,
    orig_h: u32,
) -> [f32; 4] {
    let mut corners = [
        (x0, 0.0f32),
        (x1, 0.0),
        (x1, crop_h as f32 - 1.0),
        (x0, crop_h as f32 - 1.0),
    ];
    if flipped {
        let (cw, ch) = (crop_w as f32 - 1.0, crop_h as f32 - 1.0);
        corners = corners.map(|(x, y)| (cw - x, ch - y));
    }
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for c in corners {
        let mut p: Pt = crop2page.apply(c);
        if let Some(r) = rot {
            p = r.inv(p);
        }
        p = orient::unrotate_point(p, deg, orig_w, orig_h);
        b[0] = b[0].min(p.0);
        b[1] = b[1].min(p.1);
        b[2] = b[2].max(p.0);
        b[3] = b[3].max(p.1);
    }
    b
}

/// 单页处理。
fn process_page(
    engine: &mut OrtEngine,
    img0: &RgbImage,
    page_idx: usize,
) -> Result<schema::PageJson, AppError> {
    let (w0, h0) = img0.dimensions();

    // 1. doc_ori：非 0° 则精确旋转扶正
    let deg = orient::classify_doc_ori(&mut engine.doc_ori, img0)?;
    let img1 = if deg == 0 {
        img0.clone()
    } else {
        orient::rotate_exact(img0, deg)
    };

    // 2. det（在扶正图上）
    let (input, _rw, _rh) = detect::preprocess(&img1);
    let (prob, mw, mh) = detect::infer(&mut engine.det, input)?;
    let quads = detect::postprocess(
        &prob,
        mw,
        mh,
        img1.width(),
        img1.height(),
        &detect::DbParams::default(),
    );

    // 3. deskew：|angle| > 阈值才旋转；det 框随之前向映射
    let angle = deskew::estimate_angle(&quads.iter().map(|(q, _)| *q).collect::<Vec<_>>());
    let (img2, rot, applied) = if angle.abs() > DESKEW_TRIGGER_DEG {
        let r = deskew::Rot::new(img1.width(), img1.height(), -angle);
        (deskew::rotate_image(&img1, &r), Some(r), angle)
    } else {
        (img1, None, 0.0)
    };

    // 4. 逐行：框前向映射 → 裁剪 → 行方向分类 → rec → 坐标逆映射
    let mut lines: Vec<LineWork> = Vec::new();
    for (quad1, _det_score) in &quads {
        let quad2 = match &rot {
            Some(r) => quad1.map_pts(|p| r.fwd(p)),
            None => *quad1,
        };
        // 非水平行（竖排/弧形文字，如印章）超出本管线范围，直接跳过：
        // deskew 后水平行底边角度应 ≈0°
        let bl = quad2.pts[3];
        let br = quad2.pts[2];
        let line_deg = (br.1 - bl.1).atan2(br.0 - bl.0).to_degrees();
        if line_deg.abs() > 45.0 {
            continue;
        }
        let pad = (quad2.height() * 0.12).max(2.0);
        let Some((crop, crop2page)) = crop_line(&img2, &quad2, pad) else {
            continue;
        };
        // 行方向：仅在高置信时翻转（低置信的 180° 多为印章/噪声歧义裁剪）
        let (upside_down, ori_conf) = orient::is_line_upside_down(&mut engine.textline_ori, &crop)?;
        let flipped = upside_down && ori_conf >= 0.75;
        let crop_rec = if flipped {
            orient::rotate180(&crop)
        } else {
            crop.clone()
        };
        let (crop_w, crop_h) = crop_rec.dimensions();

        let (rec_in, resized_w) = recognize::preprocess(&crop_rec);
        let (logits, t_steps, n_cls) = recognize::infer(&mut engine.rec, rec_in)?;
        let rr = recognize::decode(&logits, t_steps, n_cls, &engine.dict, resized_w);
        if rr.text.trim().is_empty() {
            continue; // 空行剔除（印章/噪声区域）
        }
        if rr.score < MIN_LINE_SCORE {
            // 低置信行剔除：实测真实行 ≥0.97，印章/残影噪声 ≤0.72（阈值可随样本调）
            continue;
        }

        let sx = crop_w as f32 / resized_w as f32;
        let chars: Vec<(String, [f32; 4])> = rr
            .chars
            .iter()
            .map(|c| {
                let b = map_char_bbox(
                    c.x0 * sx,
                    c.x1 * sx,
                    crop_w,
                    crop_h,
                    flipped,
                    &crop2page,
                    rot.as_ref(),
                    deg,
                    w0,
                    h0,
                );
                (c.ch.clone(), b)
            })
            .collect();
        // 行框：quad2 四角逆映射回原始页面
        let mut lb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &p in &quad2.pts {
            let mut q = p;
            if let Some(r) = rot.as_ref() {
                q = r.inv(q);
            }
            q = orient::unrotate_point(q, deg, w0, h0);
            lb[0] = lb[0].min(q.0);
            lb[1] = lb[1].min(q.1);
            lb[2] = lb[2].max(q.0);
            lb[3] = lb[3].max(q.1);
        }
        lines.push(LineWork {
            amount_suspect: amount_suspect(&rr.text),
            score: rr.score,
            text: rr.text,
            bbox: lb,
            chars,
        });
    }

    // 5. 段落聚类（空行已在上一步剔除）
    let clines: Vec<cluster::ClusterLine> = lines
        .iter()
        .map(|l| cluster::ClusterLine {
            bbox: l.bbox,
            text: l.text.clone(),
        })
        .collect();
    let blocks_idx = cluster::cluster(&clines);

    // 6. 组装 BlockJson（text_flow/flow_map + 归一化）
    let mut blocks = Vec::with_capacity(blocks_idx.len());
    for members in blocks_idx {
        let kind = cluster::classify_block(&members, &clines);
        let texts: Vec<String> = members.iter().map(|&i| lines[i].text.clone()).collect();
        let (text_flow, flow_map) = schema::build_text_flow(&texts);
        let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &i in &members {
            let b = lines[i].bbox;
            bb[0] = bb[0].min(b[0]);
            bb[1] = bb[1].min(b[1]);
            bb[2] = bb[2].max(b[2]);
            bb[3] = bb[3].max(b[3]);
        }
        let line_jsons: Vec<schema::LineJson> = members
            .iter()
            .map(|&i| {
                let l = &lines[i];
                schema::LineJson {
                    text: l.text.clone(),
                    bbox: schema::norm_bbox(l.bbox, w0, h0),
                    chars: l
                        .chars
                        .iter()
                        .map(|(c, b)| schema::CharJson {
                            c: c.clone(),
                            bbox: schema::norm_bbox(*b, w0, h0),
                        })
                        .collect(),
                    amount_suspect: if l.amount_suspect { Some(true) } else { None },
                    score: Some(l.score),
                }
            })
            .collect();
        blocks.push(schema::BlockJson {
            kind: kind.as_str().to_string(),
            bbox: schema::norm_bbox(bb, w0, h0),
            text_flow,
            flow_map,
            lines: line_jsons,
        });
    }

    Ok(schema::PageJson {
        page_idx,
        width: w0,
        height: h0,
        rotation: deg,
        deskew_angle: applied,
        blocks,
    })
}

/// 单文件解析：渲染全部页 → 逐页 OCR → 写 `{stem}_ocr.json` 与 `{stem}.md`。
/// `on_page(done, total)` 每页完成回调一次。
pub fn parse_pdf_file(
    engine: &mut OrtEngine,
    pdfium: &Pdfium,
    pdf_path: &Path,
    out_dir: &Path,
    stem: &str,
    mut on_page: impl FnMut(u32, u32),
) -> Result<ParseFileOutput, AppError> {
    let doc = pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|e| AppError::Internal(format!("打开 PDF 失败 {}: {e}", pdf_path.display())))?;
    let n = doc.pages().len() as u32;
    std::fs::create_dir_all(out_dir)?;

    let mut pages = Vec::with_capacity(n as usize);
    for idx in 0..n {
        let page = doc
            .pages()
            .get(idx as u16)
            .map_err(|e| AppError::Internal(format!("读取第 {} 页失败: {e}", idx + 1)))?;
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new().scale_page_by_factor(DEFAULT_DPI as f32 / 72.0),
            )
            .map_err(|e| AppError::Internal(format!("渲染第 {} 页失败: {e}", idx + 1)))?;
        let img = bitmap.as_image().to_rgb8();
        pages.push(process_page(engine, &img, idx as usize)?);
        on_page(idx + 1, n);
    }

    let ocr = schema::OcrJson {
        version: 1,
        engine: "pp-ocrv6".to_string(),
        lang: "ch".to_string(),
        page_count: n as usize,
        pages,
    };
    let json_path = out_dir.join(format!("{stem}_ocr.json"));
    let json = serde_json::to_string(&ocr)
        .map_err(|e| AppError::Internal(format!("序列化 ocr.json 失败: {e}")))?;
    std::fs::write(&json_path, json)?;

    let md_path = out_dir.join(format!("{stem}.md"));
    let mut md = String::new();
    for p in &ocr.pages {
        for b in &p.blocks {
            md.push_str(&b.text_flow);
            md.push_str("\n\n");
        }
    }
    std::fs::write(&md_path, md)?;

    Ok(ParseFileOutput {
        json_path,
        md_path,
        page_count: n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_suspect_patterns() {
        // 正常金额不误报
        assert!(!amount_suspect("1,234,567.89"));
        assert!(!amount_suspect("人民币 1,234,567.89 元"));
        assert!(!amount_suspect("共 3,000 元"));
        assert!(!amount_suspect("余额：9,999,999.99"));
        assert!(!amount_suspect("1,234.56"));
        assert!(!amount_suspect("44689999101000302926"));
        // 可疑模式命中
        assert!(amount_suspect("1.234.567"));
        assert!(amount_suspect("1,234,56"));
        assert!(amount_suspect("1,2345"));
        assert!(amount_suspect("12,34"));
        assert!(amount_suspect("价格 5,67 元"));
    }
}

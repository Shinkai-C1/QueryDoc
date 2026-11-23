//! PP-OCRv6 det：预处理（DetResizeForTest 默认参数 + ImageNet 归一化，BGR）与 DB 后处理
//! （纯 Rust：8 连通域 → 凸包 → 最小外接矩形 → 矩形双边外扩近似 pyclipper unclip）。
//! 移植自 Spike det.rs。

use image::{imageops::FilterType, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::error::AppError;
use crate::ocr::geo::{convex_hull, min_area_rect, order_quad, poly_area, poly_perimeter, Quad};

/// DBPostProcess 参数（来自 PP-OCRv6_small_det_onnx.yml）。
pub struct DbParams {
    pub thresh: f32,       // 0.2
    pub box_thresh: f32,   // 0.45
    pub unclip_ratio: f32, // 1.4
}

impl Default for DbParams {
    fn default() -> Self {
        DbParams {
            thresh: 0.2,
            box_thresh: 0.45,
            unclip_ratio: 1.4,
        }
    }
}

/// DetResizeForTest（yml 无参 → 默认 limit_side_len=736, limit_type=min）。
/// 返回 (CHW 输入张量, 缩放后宽, 缩放后高)。
pub fn preprocess(img: &RgbImage) -> (Array4<f32>, u32, u32) {
    let (w, h) = img.dimensions();
    let min_side = w.min(h) as f32;
    let ratio = if min_side < 736.0 {
        736.0 / min_side
    } else {
        1.0
    };
    let rw = ((w as f32 * ratio) / 32.0).round().max(1.0) as u32 * 32;
    let rh = ((h as f32 * ratio) / 32.0).round().max(1.0) as u32 * 32;
    let resized = image::imageops::resize(img, rw, rh, FilterType::Triangle);

    // DecodeImage img_mode=BGR；NormalizeImage 按 BGR 通道顺序施加 ImageNet mean/std。
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let (rwu, rhu) = (rw as usize, rh as usize);
    let mut data = vec![0f32; 3 * rwu * rhu];
    for y in 0..rhu {
        for x in 0..rwu {
            let p = resized.get_pixel(x as u32, y as u32);
            let bgr = [p[2] as f32, p[1] as f32, p[0] as f32];
            for c in 0..3 {
                data[c * rwu * rhu + y * rwu + x] = (bgr[c] / 255.0 - MEAN[c]) / STD[c];
            }
        }
    }
    (
        Array4::from_shape_vec((1, 3, rhu, rwu), data).unwrap(),
        rw,
        rh,
    )
}

/// det 推理，返回概率图（按输出实际形状）。
pub fn infer(sess: &mut Session, input: Array4<f32>) -> Result<(Vec<f32>, usize, usize), AppError> {
    let tensor =
        Tensor::from_array(input).map_err(|e| AppError::Internal(format!("det 输入张量: {e}")))?;
    let outputs = sess
        .run(ort::inputs![tensor])
        .map_err(|e| AppError::Internal(format!("det 推理: {e}")))?;
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| AppError::Internal(format!("det 输出解析: {e}")))?;
    let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
    let (mh, mw) = (dims[dims.len() - 2], dims[dims.len() - 1]);
    Ok((data.to_vec(), mw, mh))
}

/// DB 后处理：阈值二值化 → 8 连通域 → 凸包 → 最小外接矩形 → 外扩 → 映射回原图。
/// 返回按阅读顺序（先上后下、同行先左后右）排序的 (行四边形, 得分)。
pub fn postprocess(
    prob: &[f32],
    map_w: usize,
    map_h: usize,
    orig_w: u32,
    orig_h: u32,
    params: &DbParams,
) -> Vec<(Quad, f32)> {
    let n = map_w * map_h;
    let mut bitmap = vec![false; n];
    for (i, b) in bitmap.iter_mut().enumerate() {
        *b = prob[i] > params.thresh;
    }
    // 8 连通域标记（两趟并查集）
    let mut labels = vec![0u32; n];
    let mut parent: Vec<u32> = vec![0];
    let find = |parent: &mut Vec<u32>, mut x: u32| -> u32 {
        while parent[x as usize] != x {
            x = parent[x as usize];
        }
        x
    };
    let mut next_label = 1u32;
    for y in 0..map_h {
        for x in 0..map_w {
            let i = y * map_w + x;
            if !bitmap[i] {
                continue;
            }
            let mut neigh = [0u32; 4];
            let mut nn = 0usize;
            if x > 0 && bitmap[i - 1] {
                neigh[nn] = labels[i - 1];
                nn += 1;
            }
            if y > 0 {
                if bitmap[i - map_w] {
                    neigh[nn] = labels[i - map_w];
                    nn += 1;
                }
                if x > 0 && bitmap[i - map_w - 1] {
                    neigh[nn] = labels[i - map_w - 1];
                    nn += 1;
                }
                if x + 1 < map_w && bitmap[i - map_w + 1] {
                    neigh[nn] = labels[i - map_w + 1];
                    nn += 1;
                }
            }
            if nn == 0 {
                parent.push(next_label);
                labels[i] = next_label;
                next_label += 1;
            } else {
                let mut roots: Vec<u32> = neigh[..nn].to_vec();
                for r in roots.iter_mut() {
                    *r = find(&mut parent, *r);
                }
                let min_root = *roots.iter().min().unwrap();
                labels[i] = min_root;
                for &r in &roots {
                    if r != min_root {
                        parent[r as usize] = min_root;
                    }
                }
            }
        }
    }
    let mut comps: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, &lb) in labels.iter().enumerate() {
        if lb > 0 {
            let r = find(&mut parent, lb);
            comps.entry(r).or_default().push(i);
        }
    }

    let sx = orig_w as f32 / map_w as f32;
    let sy = orig_h as f32 / map_h as f32;
    let mut out = Vec::new();
    for pixels in comps.into_values() {
        if pixels.len() < 8 {
            continue;
        }
        let score: f32 = pixels.iter().map(|&i| prob[i]).sum::<f32>() / pixels.len() as f32;
        if score < params.box_thresh {
            continue;
        }
        let pts: Vec<(f32, f32)> = pixels
            .iter()
            .map(|&i| ((i % map_w) as f32, (i / map_w) as f32))
            .collect();
        let hull = convex_hull(&pts);
        if hull.len() < 4 {
            continue;
        }
        // unclip：d = ratio * area / perimeter，最小外接矩形两边各外扩 d
        let d = params.unclip_ratio * poly_area(&hull) / poly_perimeter(&hull).max(1e-6);
        let quad = order_quad(min_area_rect(&hull)).expand(d, d);
        let mut q = quad.pts;
        for p in q.iter_mut() {
            p.0 = (p.0 * sx).clamp(0.0, orig_w as f32 - 1.0);
            p.1 = (p.1 * sy).clamp(0.0, orig_h as f32 - 1.0);
        }
        let quad = Quad { pts: q };
        if quad.width() < 3.0 || quad.height() < 3.0 {
            continue;
        }
        out.push((quad, score));
    }
    out.sort_by(|a, b| {
        let (ba, bb) = (a.0.bbox(), b.0.bbox());
        ba[1]
            .partial_cmp(&bb[1])
            .unwrap()
            .then(ba[0].partial_cmp(&bb[0]).unwrap())
    });
    out
}

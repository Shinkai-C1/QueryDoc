//! 方向分类与精确 90° 旋转：doc_ori（整页 0/90/180/270）、textline_ori（行 180°）。
//! 旋转为整数像素重排（无重采样误差），坐标逆映射精确。

use image::{imageops::FilterType, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::error::AppError;
use crate::ocr::geo::Pt;

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

fn chw_bgr(img: &RgbImage) -> Array4<f32> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut data = vec![0f32; 3 * w * h];
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x as u32, y as u32);
            let bgr = [p[2] as f32, p[1] as f32, p[0] as f32];
            for c in 0..3 {
                data[c * w * h + y * w + x] = (bgr[c] / 255.0 - MEAN[c]) / STD[c];
            }
        }
    }
    Array4::from_shape_vec((1, 3, h, w), data).unwrap()
}

/// 分类推理，返回 (argmax 类别, 置信度)。模型输出多为已归一化概率（行和≈1），
/// 非概率输出则按 logits softmax。
fn run_cls(sess: &mut Session, input: Array4<f32>) -> Result<(usize, f32), AppError> {
    let tensor =
        Tensor::from_array(input).map_err(|e| AppError::Internal(format!("cls 输入张量: {e}")))?;
    let outputs = sess
        .run(ort::inputs![tensor])
        .map_err(|e| AppError::Internal(format!("cls 推理: {e}")))?;
    let (_shape, logits) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| AppError::Internal(format!("cls 输出解析: {e}")))?;
    let mut best = 0usize;
    let mut minv = f32::MAX;
    let mut sum = 0f64;
    for (i, &v) in logits.iter().enumerate() {
        if v > logits[best] {
            best = i;
        }
        minv = minv.min(v);
        sum += v as f64;
    }
    let conf = if minv >= -1e-6 && (sum - 1.0).abs() < 0.05 {
        logits[best]
    } else {
        let mut s = 0f64;
        for &v in logits {
            s += (v as f64 - logits[best] as f64).exp();
        }
        (1.0 / s) as f32
    };
    Ok((best, conf))
}

/// 整页方向分类（PP-LCNet_x1_0_doc_ori）：resize short 256 → center crop 224 →
/// ImageNet 归一化（BGR），返回 0/90/180/270。
pub fn classify_doc_ori(sess: &mut Session, img: &RgbImage) -> Result<u32, AppError> {
    let (w, h) = img.dimensions();
    let scale = 256.0 / w.min(h) as f32;
    let rw = ((w as f32 * scale).round() as u32).max(256);
    let rh = ((h as f32 * scale).round() as u32).max(256);
    let resized = image::imageops::resize(img, rw, rh, FilterType::Triangle);
    let crop =
        image::imageops::crop_imm(&resized, (rw - 224) / 2, (rh - 224) / 2, 224, 224).to_image();
    const DEG: [u32; 4] = [0, 90, 180, 270];
    Ok(DEG[run_cls(sess, chw_bgr(&crop))?.0])
}

/// 行方向分类（PP-LCNet_x0_25_textline_ori）：resize 160x80 → ImageNet 归一化（BGR）。
/// 返回 (是否倒置 180°, 置信度)。调用方应按置信度门控翻转——对非文字/印章等
/// 歧义裁剪，模型会给出低置信的 180°，盲目翻转会把竖排噪声翻成"可读"垃圾。
pub fn is_line_upside_down(sess: &mut Session, crop: &RgbImage) -> Result<(bool, f32), AppError> {
    let resized = image::imageops::resize(crop, 160, 80, FilterType::Triangle);
    let (idx, conf) = run_cls(sess, chw_bgr(&resized))?;
    Ok((idx == 1, conf))
}

/// 精确旋转（逆时针 deg 度，deg ∈ {90, 180, 270}），用于按 doc_ori 结果扶正页面。
/// 正向映射（原图 → 旋转后）见各分支注释；逆映射用 [`unrotate_point`]。
pub fn rotate_exact(img: &RgbImage, deg: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    match deg {
        90 => {
            // fwd: (x, y) → (y, w-1-x)；新尺寸 (h, w)
            let mut out = RgbImage::new(h, w);
            for y in 0..h {
                for x in 0..w {
                    out.put_pixel(y, w - 1 - x, *img.get_pixel(x, y));
                }
            }
            out
        }
        180 => {
            // fwd: (x, y) → (w-1-x, h-1-y)
            let mut out = RgbImage::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    out.put_pixel(w - 1 - x, h - 1 - y, *img.get_pixel(x, y));
                }
            }
            out
        }
        270 => {
            // fwd: (x, y) → (h-1-y, x)；新尺寸 (h, w)
            let mut out = RgbImage::new(h, w);
            for y in 0..h {
                for x in 0..w {
                    out.put_pixel(h - 1 - y, x, *img.get_pixel(x, y));
                }
            }
            out
        }
        _ => img.clone(),
    }
}

/// 180° 旋转（行图翻转用）。
pub fn rotate180(img: &RgbImage) -> RgbImage {
    rotate_exact(img, 180)
}

/// 旋转后坐标 → 原始页面坐标（[`rotate_exact`] 的逆）。
/// `orig_w/orig_h` 为旋转前图像尺寸；deg 为当时施加的旋转角。
pub fn unrotate_point(p: Pt, deg: u32, orig_w: u32, orig_h: u32) -> Pt {
    let (w, h) = (orig_w as f32, orig_h as f32);
    match deg {
        // fwd: x' = y, y' = w-1-x → x = w-1-y', y = x'
        90 => (w - 1.0 - p.1, p.0),
        // fwd: x' = w-1-x, y' = h-1-y
        180 => (w - 1.0 - p.0, h - 1.0 - p.1),
        // fwd: x' = h-1-y, y' = x → x = y', y = h-1-x'
        270 => (p.1, h - 1.0 - p.0),
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_unrotate_round_trip() {
        let (w, h) = (1654u32, 2339u32);
        let pts = [
            (0.0, 0.0),
            (1653.0, 0.0),
            (0.0, 2338.0),
            (1653.0, 2338.0),
            (800.0, 1200.0),
        ];
        for deg in [90, 180, 270] {
            // 构造带标记点的图，验证像素级 fwd 与 unrotate 一致
            let mut img = RgbImage::new(w, h);
            for (i, &(x, y)) in pts.iter().enumerate() {
                img.put_pixel(x as u32, y as u32, image::Rgb([i as u8 + 1, 0, 0]));
            }
            let rot = rotate_exact(&img, deg);
            for (i, &(x, y)) in pts.iter().enumerate() {
                // 标记点经 unrotate 应回到原坐标
                // 先求 fwd 坐标：在旋转图中找到标记像素
                let mut fwd = None;
                for yy in 0..rot.height() {
                    for xx in 0..rot.width() {
                        if rot.get_pixel(xx, yy).0[0] == i as u8 + 1 {
                            fwd = Some((xx as f32, yy as f32));
                        }
                    }
                }
                let fwd = fwd.unwrap();
                let back = unrotate_point(fwd, deg, w, h);
                assert!(
                    (back.0 - x).abs() < 1e-3 && (back.1 - y).abs() < 1e-3,
                    "deg={deg} 点 ({x},{y}) 逆映射得到 ({},{})",
                    back.0,
                    back.1
                );
            }
        }
    }
}

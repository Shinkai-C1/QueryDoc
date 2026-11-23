//! PP-OCRv6 rec：预处理（RecResizeImg 3x48 动态宽**不封顶**，(x/255-0.5)/0.5，BGR）
//! + CTC 贪心解码 + 单字时间步对齐（中点切分）。移植自 Spike rec.rs，保留关键结论：
//! - 宽度封顶 320 会压扁长行导致数字串识别错误 → 动态宽（上限 3200）
//! - yml 未写归一化但实测必须 (x/255-0.5)/0.5（raw 输出乱码）
//! - ONNX 输出已含 softmax（行和≈1），勿重复 softmax
//! - 18710 类 = blank + 18708 dict + 尾部空格类

use image::{imageops::FilterType, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::error::AppError;

/// RecResizeImg：高 48，宽按长宽比缩放（上限 3200，不压扁长行）。
/// 返回 (CHW 输入, resized_w)。
pub fn preprocess(crop: &RgbImage) -> (Array4<f32>, u32) {
    const IMG_H: u32 = 48;
    const MAX_W: u32 = 3200;
    let (w, h) = crop.dimensions();
    let ratio = w as f32 / h.max(1) as f32;
    let resized_w = ((IMG_H as f32 * ratio).ceil() as u32).clamp(1, MAX_W);
    let resized = image::imageops::resize(crop, resized_w, IMG_H, FilterType::Triangle);

    let (rwu, rhu) = (resized_w as usize, IMG_H as usize);
    let mut data = vec![0f32; 3 * rwu * rhu];
    for y in 0..rhu {
        for x in 0..rwu {
            let p = resized.get_pixel(x as u32, y as u32);
            let bgr = [p[2] as f32, p[1] as f32, p[0] as f32];
            for c in 0..3 {
                data[c * rwu * rhu + y * rwu + x] = (bgr[c] / 255.0 - 0.5) / 0.5;
            }
        }
    }
    (
        Array4::from_shape_vec((1, 3, rhu, rwu), data).unwrap(),
        resized_w,
    )
}

/// rec 推理，返回 (logits 或已归一化概率, T, C)。
pub fn infer(sess: &mut Session, input: Array4<f32>) -> Result<(Vec<f32>, usize, usize), AppError> {
    let tensor =
        Tensor::from_array(input).map_err(|e| AppError::Internal(format!("rec 输入张量: {e}")))?;
    let outputs = sess
        .run(ort::inputs![tensor])
        .map_err(|e| AppError::Internal(format!("rec 推理: {e}")))?;
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| AppError::Internal(format!("rec 输出解析: {e}")))?;
    let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
    let (t, c) = (dims[dims.len() - 2], dims[dims.len() - 1]);
    Ok((data.to_vec(), t, c))
}

pub struct DecodedChar {
    pub ch: String,
    pub prob: f32,
    /// 在缩放后行图（宽 resized_w）中的 x 区间
    pub x0: f32,
    pub x1: f32,
}

pub struct RecResult {
    pub text: String,
    /// 行置信度（发射步概率均值）；executor 低置信行过滤用，Phase 4 写 confidence 用
    pub score: f32,
    pub chars: Vec<DecodedChar>,
}

/// CTC 贪心解码 + 单字对齐：
/// 每个字符记录"发射时间步" e_i（新一轮非 blank argmax 的起点），
/// 字符 x 边界取相邻发射时间步的中点，再线性映射到 [0, resized_w]。
pub fn decode(
    logits: &[f32],
    t_steps: usize,
    n_cls: usize,
    dict: &[String],
    resized_w: u32,
) -> RecResult {
    // 类别映射：0=blank；1..=18708=dict[cls-1]；18709（最后一类）=" "（use_space_char）
    let has_space_tail = n_cls == dict.len() + 2;
    debug_assert!(
        has_space_tail || n_cls == dict.len() + 1,
        "CTC 类别数与字典不符: {n_cls}"
    );
    let map_char = |cls: usize| -> String {
        if cls == 0 {
            String::new()
        } else if has_space_tail && cls == n_cls - 1 {
            " ".to_string()
        } else {
            dict.get(cls - 1).cloned().unwrap_or_default()
        }
    };
    // 每步 argmax 与其概率。该 ONNX 图输出已是 softmax 概率（行和≈1）：
    // 若再套 softmax 会把分布压向均匀，故先探测、仅在为 logits 时才 softmax。
    let mut argmax = vec![0usize; t_steps];
    let mut am_prob = vec![0f32; t_steps];
    for t in 0..t_steps {
        let row = &logits[t * n_cls..(t + 1) * n_cls];
        let mut best = 0usize;
        let mut bestv = f32::MIN;
        let mut minv = f32::MAX;
        let mut sum64 = 0f64;
        for (i, &v) in row.iter().enumerate() {
            if v > bestv {
                bestv = v;
                best = i;
            }
            if v < minv {
                minv = v;
            }
            sum64 += v as f64;
        }
        let prob = if minv >= -1e-6 && (sum64 - 1.0).abs() < 0.05 {
            bestv
        } else {
            let mut s = 0f64;
            for &v in row {
                s += (v as f64 - bestv as f64).exp();
            }
            (1.0 / s) as f32
        };
        argmax[t] = best;
        am_prob[t] = prob;
    }
    // 贪心合并：c != prev 且 c != blank(0) 时发射新字符
    let mut emissions: Vec<(usize, usize, f32)> = Vec::new();
    let mut prev = usize::MAX;
    for t in 0..t_steps {
        let c = argmax[t];
        if c != 0 && c != prev {
            emissions.push((c, t, am_prob[t]));
        }
        prev = c;
    }
    let k = emissions.len();
    let mut chars = Vec::with_capacity(k);
    for i in 0..k {
        let (cls, t_i, prob) = emissions[i];
        let left = if i == 0 {
            0.0
        } else {
            (emissions[i - 1].1 + t_i) as f32 / 2.0
        };
        let right = if i + 1 == k {
            t_steps as f32
        } else {
            (t_i + emissions[i + 1].1) as f32 / 2.0
        };
        let scale = resized_w as f32 / t_steps as f32;
        chars.push(DecodedChar {
            ch: map_char(cls),
            prob,
            x0: left * scale,
            x1: right * scale,
        });
    }
    let text: String = chars.iter().map(|c| c.ch.as_str()).collect();
    let score = if chars.is_empty() {
        0.0
    } else {
        chars.iter().map(|c| c.prob).sum::<f32>() / chars.len() as f32
    };
    RecResult { text, score, chars }
}

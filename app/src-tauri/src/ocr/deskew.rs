//! 歪斜估算与仿射矫正：det 行框下边缘角度中位数 → 绕图心旋转（双线性采样），
//! 坐标经逆映射回原始页面坐标系（§3.2 硬约束）。

use image::{Rgb, RgbImage};

use crate::ocr::geo::{Pt, Quad};

/// 绕图心的旋转（图像坐标系 y 向下；R(θ) 为 [[cos,-sin],[sin,cos]]，正值视觉顺时针）。
/// fwd: 原图 → 矫正图（p' = R(θ)(p-c)+c）；inv: 矫正图 → 原图。
/// deskew 用法：测得行倾斜角 s 后，以 θ=-s 构造（输出中文本方向角 s+θ=0）。
#[derive(Clone, Copy, Debug)]
pub struct Rot {
    cos: f32,
    sin: f32,
    cx: f32,
    cy: f32,
}

impl Rot {
    /// angle_deg 为图像施加的旋转角（正值视觉顺时针；deskew 时传 -skew）。
    pub fn new(width: u32, height: u32, angle_deg: f32) -> Rot {
        let a = angle_deg.to_radians();
        Rot {
            cos: a.cos(),
            sin: a.sin(),
            cx: (width as f32 - 1.0) / 2.0,
            cy: (height as f32 - 1.0) / 2.0,
        }
    }

    pub fn fwd(&self, p: Pt) -> Pt {
        let dx = p.0 - self.cx;
        let dy = p.1 - self.cy;
        (
            self.cos * dx - self.sin * dy + self.cx,
            self.sin * dx + self.cos * dy + self.cy,
        )
    }

    pub fn inv(&self, p: Pt) -> Pt {
        let dx = p.0 - self.cx;
        let dy = p.1 - self.cy;
        (
            self.cos * dx + self.sin * dy + self.cx,
            -self.sin * dx + self.cos * dy + self.cy,
        )
    }
}

/// 由 det 行框下边缘（bl→br）角度估算整页倾斜角（中位数，度）。
pub fn estimate_angle(quads: &[Quad]) -> f32 {
    let mut angles: Vec<f32> = quads
        .iter()
        .map(|q| {
            let bl = q.pts[3];
            let br = q.pts[2];
            (br.1 - bl.1).atan2(br.0 - bl.0).to_degrees()
        })
        .collect();
    if angles.is_empty() {
        return 0.0;
    }
    angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
    angles[angles.len() / 2]
}

fn sample_bilinear(img: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = (img.width() as i32, img.height() as i32);
    if x < 0.0 || y < 0.0 || x > (w - 1) as f32 || y > (h - 1) as f32 {
        return Rgb([255, 255, 255]);
    }
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let mut out = [0f32; 3];
    for (px, wt) in [
        (img.get_pixel(x0 as u32, y0 as u32), (1.0 - fx) * (1.0 - fy)),
        (img.get_pixel(x1 as u32, y0 as u32), fx * (1.0 - fy)),
        (img.get_pixel(x0 as u32, y1 as u32), (1.0 - fx) * fy),
        (img.get_pixel(x1 as u32, y1 as u32), fx * fy),
    ] {
        for (o, &pc) in out.iter_mut().zip(px.0.iter()) {
            *o += pc as f32 * wt;
        }
    }
    Rgb([out[0] as u8, out[1] as u8, out[2] as u8])
}

/// 按 rot 旋转图像（输出同尺寸；越界采样为白）。
pub fn rotate_image(img: &RgbImage, rot: &Rot) -> RgbImage {
    let (w, h) = img.dimensions();
    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = rot.inv((x as f32, y as f32));
            out.put_pixel(x, y, sample_bilinear(img, p.0, p.1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rot_fwd_inv_round_trip_under_1px() {
        let rot = Rot::new(1654, 2339, -2.5);
        for &(x, y) in &[
            (0.0, 0.0),
            (1653.0, 2338.0),
            (800.0, 1200.0),
            (100.5, 55.25),
        ] {
            let back = rot.inv(rot.fwd((x, y)));
            assert!(
                (back.0 - x).abs() < 1e-3 && (back.1 - y).abs() < 1e-3,
                "({x},{y}) → ({},{})",
                back.0,
                back.1
            );
        }
    }

    #[test]
    fn rotate_image_then_inverse_maps_coords_under_1px() {
        // 在图上放标记点，旋转后红色质心应在 fwd 位置 ±1px，且 inv 还原误差 <1px
        let mut img = RgbImage::new(400, 300);
        let mark = (123.0f32, 45.0f32);
        img.put_pixel(mark.0 as u32, mark.1 as u32, Rgb([255, 0, 0]));
        let rot = Rot::new(400, 300, -3.0);
        let rotated = rotate_image(&img, &rot);
        let fwd = rot.fwd(mark);
        // 双线性会把单点扩散到 ~2x2，用红色像素的加权质心定位
        let (mut sx, mut sy, mut sw) = (0f64, 0f64, 0f64);
        for y in 0..rotated.height() {
            for x in 0..rotated.width() {
                let p = rotated.get_pixel(x, y);
                if p.0[0] > 100 && p.0[0] as i16 - p.0[1] as i16 > 50 {
                    let wt = p.0[0] as f64;
                    sx += x as f64 * wt;
                    sy += y as f64 * wt;
                    sw += wt;
                }
            }
        }
        assert!(sw > 0.0, "旋转后未找到标记点");
        let (cx, cy) = ((sx / sw) as f32, (sy / sw) as f32);
        assert!(
            (cx - fwd.0).abs() < 1.0 && (cy - fwd.1).abs() < 1.0,
            "质心 ({cx},{cy}) 应 ≈ fwd ({},{})",
            fwd.0,
            fwd.1
        );
        let back = rot.inv(fwd);
        assert!((back.0 - mark.0).abs() < 1.0 && (back.1 - mark.1).abs() < 1.0);
    }

    #[test]
    fn estimate_angle_median() {
        let mk = |ang_deg: f32| {
            let a = ang_deg.to_radians();
            let (c, s) = (a.cos(), a.sin());
            let f = |x: f32, y: f32| (500.0 + c * x - s * y, 500.0 + s * x + c * y);
            Quad {
                pts: [
                    f(-50.0, -10.0),
                    f(50.0, -10.0),
                    f(50.0, 10.0),
                    f(-50.0, 10.0),
                ],
            }
        };
        let quads = vec![mk(-2.4), mk(-2.6), mk(-2.5), mk(0.2)]; // 一个离群
        let est = estimate_angle(&quads);
        assert!((est - (-2.4)).abs() < 0.2, "est={est}");
        assert_eq!(estimate_angle(&[]), 0.0);
    }
}

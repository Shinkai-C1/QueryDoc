//! 几何工具：点、四边形、凸包、最小外接矩形、单应映射（移植自 Spike geo.rs）。

pub type Pt = (f32, f32);

/// 文本行四边形，顶点顺序固定为 tl, tr, br, bl。
#[derive(Clone, Copy, Debug)]
pub struct Quad {
    pub pts: [Pt; 4],
}

fn dist(a: Pt, b: Pt) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

impl Quad {
    /// 上/下边长度的较大者（近似行宽）。
    pub fn width(&self) -> f32 {
        dist(self.pts[0], self.pts[1]).max(dist(self.pts[3], self.pts[2]))
    }
    /// 左/右边长度的较大者（近似行高）。
    pub fn height(&self) -> f32 {
        dist(self.pts[0], self.pts[3]).max(dist(self.pts[1], self.pts[2]))
    }
    /// 轴对齐包围盒 [x0, y0, x1, y1]。
    pub fn bbox(&self) -> [f32; 4] {
        let mut x0 = f32::MAX;
        let mut y0 = f32::MAX;
        let mut x1 = f32::MIN;
        let mut y1 = f32::MIN;
        for p in &self.pts {
            x0 = x0.min(p.0);
            y0 = y0.min(p.1);
            x1 = x1.max(p.0);
            y1 = y1.max(p.1);
        }
        [x0, y0, x1, y1]
    }
    /// 在局部坐标系（沿行方向 u、垂直方向 v）向外扩张 (dx, dy)。
    pub fn expand(&self, dx: f32, dy: f32) -> Quad {
        let [tl, tr, br, bl] = self.pts;
        let wl = dist(tl, tr).max(1e-6);
        let hl = dist(tl, bl).max(1e-6);
        let u = ((tr.0 - tl.0) / wl, (tr.1 - tl.1) / wl);
        let v = ((bl.0 - tl.0) / hl, (bl.1 - tl.1) / hl);
        let f = |p: Pt, su: f32, sv: f32| {
            (
                p.0 + su * dx * u.0 + sv * dy * v.0,
                p.1 + su * dx * u.1 + sv * dy * v.1,
            )
        };
        Quad {
            pts: [
                f(tl, -1.0, -1.0),
                f(tr, 1.0, -1.0),
                f(br, 1.0, 1.0),
                f(bl, -1.0, 1.0),
            ],
        }
    }
    /// 对所有顶点施加映射。
    pub fn map_pts(&self, f: impl Fn(Pt) -> Pt) -> Quad {
        Quad {
            pts: self.pts.map(f),
        }
    }
}

/// Andrew monotone chain 凸包。返回逆时针凸包（不含重复端点）。
pub fn convex_hull(pts: &[Pt]) -> Vec<Pt> {
    let mut pts: Vec<Pt> = pts.to_vec();
    pts.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap()
            .then(a.1.partial_cmp(&b.1).unwrap())
    });
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
    if pts.len() <= 2 {
        return pts;
    }
    let cross = |o: Pt, a: Pt, b: Pt| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut lower: Vec<Pt> = Vec::with_capacity(pts.len());
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<Pt> = Vec::with_capacity(pts.len());
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

pub fn poly_area(pts: &[Pt]) -> f32 {
    let mut a = 0.0f32;
    for i in 0..pts.len() {
        let p = pts[i];
        let q = pts[(i + 1) % pts.len()];
        a += p.0 * q.1 - q.0 * p.1;
    }
    a.abs() / 2.0
}

pub fn poly_perimeter(pts: &[Pt]) -> f32 {
    let mut s = 0.0f32;
    for i in 0..pts.len() {
        s += dist(pts[i], pts[(i + 1) % pts.len()]);
    }
    s
}

/// 旋转卡壳求最小外接矩形，返回 4 角点（顺序未规范化）。
pub fn min_area_rect(hull: &[Pt]) -> [Pt; 4] {
    let n = hull.len();
    let mut best_area = f32::MAX;
    let mut best = [hull[0], hull[0], hull[0], hull[0]];
    for i in 0..n {
        let p0 = hull[i];
        let p1 = hull[(i + 1) % n];
        let ex = p1.0 - p0.0;
        let ey = p1.1 - p0.1;
        let len = (ex * ex + ey * ey).sqrt();
        if len < 1e-6 {
            continue;
        }
        let u = (ex / len, ey / len);
        let v = (-u.1, u.0);
        let (mut min_u, mut max_u, mut min_v, mut max_v) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for &p in hull {
            let du = p.0 * u.0 + p.1 * u.1;
            let dv = p.0 * v.0 + p.1 * v.1;
            min_u = min_u.min(du);
            max_u = max_u.max(du);
            min_v = min_v.min(dv);
            max_v = max_v.max(dv);
        }
        let area = (max_u - min_u) * (max_v - min_v);
        if area < best_area {
            best_area = area;
            let c = |cu: f32, cv: f32| (u.0 * cu + v.0 * cv, u.1 * cu + v.1 * cv);
            best = [
                c(min_u, min_v),
                c(max_u, min_v),
                c(max_u, max_v),
                c(min_u, max_v),
            ];
        }
    }
    best
}

/// 按 PaddleOCR get_mini_boxes 规则排序 4 点：tl, tr, br, bl。
pub fn order_quad(pts: [Pt; 4]) -> Quad {
    let mut idx_tl = 0;
    let mut idx_br = 0;
    let mut idx_tr = 0;
    let mut idx_bl = 0;
    for (i, p) in pts.iter().enumerate() {
        if p.0 + p.1 < pts[idx_tl].0 + pts[idx_tl].1 {
            idx_tl = i;
        }
        if p.0 + p.1 > pts[idx_br].0 + pts[idx_br].1 {
            idx_br = i;
        }
        if p.0 - p.1 > pts[idx_tr].0 - pts[idx_tr].1 {
            idx_tr = i;
        }
        if p.0 - p.1 < pts[idx_bl].0 - pts[idx_bl].1 {
            idx_bl = i;
        }
    }
    Quad {
        pts: [pts[idx_tl], pts[idx_tr], pts[idx_br], pts[idx_bl]],
    }
}

/// 3x3 单应矩阵（行主序），用于坐标映射。
pub struct Homography {
    m: [f64; 9],
}

impl Homography {
    /// 由 4 组对应点求解 from -> to 的单应。求解失败（退化）返回 None。
    pub fn from_control_points(from: [Pt; 4], to: [Pt; 4]) -> Option<Homography> {
        let mut a = [[0f64; 9]; 8];
        for i in 0..4 {
            let (x, y) = (from[i].0 as f64, from[i].1 as f64);
            let (u, v) = (to[i].0 as f64, to[i].1 as f64);
            a[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            a[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        for col in 0..8 {
            let mut piv = col;
            for r in col + 1..8 {
                if a[r][col].abs() > a[piv][col].abs() {
                    piv = r;
                }
            }
            if a[piv][col].abs() < 1e-12 {
                return None;
            }
            a.swap(col, piv);
            let d = a[col][col];
            for v in a[col][col..].iter_mut() {
                *v /= d;
            }
            let pivot = a[col]; // 归一化后的主元行（复制以避开借用冲突）
            for (r, row) in a.iter_mut().enumerate() {
                if r == col {
                    continue;
                }
                let f = row[col];
                if f == 0.0 {
                    continue;
                }
                for (av, pv) in row[col..].iter_mut().zip(pivot[col..].iter()) {
                    *av -= f * pv;
                }
            }
        }
        let mut m = [0f64; 9];
        for (i, item) in m.iter_mut().enumerate().take(8) {
            *item = a[i][8];
        }
        m[8] = 1.0;
        Some(Homography { m })
    }

    pub fn apply(&self, p: Pt) -> Pt {
        let (x, y) = (p.0 as f64, p.1 as f64);
        let w = self.m[6] * x + self.m[7] * y + self.m[8];
        let nx = (self.m[0] * x + self.m[1] * y + self.m[2]) / w;
        let ny = (self.m[3] * x + self.m[4] * y + self.m[5]) / w;
        (nx as f32, ny as f32)
    }
}

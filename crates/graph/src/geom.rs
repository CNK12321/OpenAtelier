/// 2D affine transform: `x' = a·x + c·y + e`, `y' = b·x + d·y + f` (y down).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Affine2 {
    pub m: [f64; 6], // a b c d e f
}

impl Affine2 {
    pub const IDENTITY: Affine2 = Affine2 { m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] };

    pub fn translate(x: f64, y: f64) -> Self {
        Affine2 { m: [1.0, 0.0, 0.0, 1.0, x, y] }
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Affine2 { m: [sx, 0.0, 0.0, sy, 0.0, 0.0] }
    }

    /// Clockwise on screen (y down).
    pub fn rotate_degrees(deg: f64) -> Self {
        let (s, c) = deg.to_radians().sin_cos();
        Affine2 { m: [c, s, -s, c, 0.0, 0.0] }
    }

    /// Apply `self`, then `next`.
    pub fn then(&self, next: &Affine2) -> Affine2 {
        let [a1, b1, c1, d1, e1, f1] = self.m;
        let [a2, b2, c2, d2, e2, f2] = next.m;
        Affine2 {
            m: [
                a2 * a1 + c2 * b1,
                b2 * a1 + d2 * b1,
                a2 * c1 + c2 * d1,
                b2 * c1 + d2 * d1,
                a2 * e1 + c2 * f1 + e2,
                b2 * e1 + d2 * f1 + f2,
            ],
        }
    }

    pub fn apply(&self, [x, y]: [f64; 2]) -> [f64; 2] {
        let [a, b, c, d, e, f] = self.m;
        [a * x + c * y + e, b * x + d * y + f]
    }

    /// The transform that undoes this one, or `None` if it collapses the plane (a zero
    /// scale), in which case no point maps back.
    pub fn invert(&self) -> Option<Affine2> {
        let [a, b, c, d, e, f] = self.m;
        let det = a * d - b * c;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
        Some(Affine2 { m: [ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)] })
    }

    /// Largest stretch along either source axis — how many output pixels one source
    /// pixel can cover. Used to pick raster resolution.
    pub fn max_axis_scale(&self) -> f64 {
        let [a, b, c, d, ..] = self.m;
        (a * a + b * b).sqrt().max((c * c + d * d).sqrt())
    }

    /// No rotation or skew, so rectangles stay rectangles (needed for occlusion).
    pub fn is_axis_aligned(&self) -> bool {
        self.m[1].abs() < 1e-12 && self.m[2].abs() < 1e-12
    }

    pub fn is_identity(&self) -> bool {
        self.m.iter().zip(Self::IDENTITY.m).all(|(x, y)| (x - y).abs() < 1e-12)
    }

    pub fn map_rect(&self, r: Rect) -> Rect {
        let pts = [[r.x0, r.y0], [r.x1, r.y0], [r.x0, r.y1], [r.x1, r.y1]].map(|p| self.apply(p));
        let (mut out, rest) = (Rect::new(pts[0][0], pts[0][1], pts[0][0], pts[0][1]), &pts[1..]);
        for p in rest {
            out.x0 = out.x0.min(p[0]);
            out.y0 = out.y0.min(p[1]);
            out.x1 = out.x1.max(p[0]);
            out.y1 = out.y1.max(p[1]);
        }
        out
    }
}

/// Axis-aligned rectangle in pixels, `[x0, x1) × [y0, y1)`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect { x0, y0, x1, y1 }
    }

    pub fn from_size(w: f64, h: f64) -> Self {
        Rect::new(0.0, 0.0, w, h)
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }

    /// With a small tolerance for float error at edges.
    pub fn contains_rect(&self, o: &Rect) -> bool {
        const EPS: f64 = 1e-6;
        self.x0 <= o.x0 + EPS && self.y0 <= o.y0 + EPS && self.x1 >= o.x1 - EPS && self.y1 >= o.y1 - EPS
    }

    pub fn expand(&self, by: f64) -> Rect {
        Rect::new(self.x0 - by, self.y0 - by, self.x1 + by, self.y1 + by)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn composition_order() {
        let t = Affine2::scale(2.0, 3.0).then(&Affine2::translate(10.0, 0.0));
        assert!(close(t.apply([1.0, 1.0]), [12.0, 3.0]));
        let r = Affine2::rotate_degrees(90.0);
        assert!(close(r.apply([1.0, 0.0]), [0.0, 1.0]));
        assert!(!r.is_axis_aligned());
        assert!((Affine2::scale(0.5, 4.0).max_axis_scale() - 4.0).abs() < 1e-12);
    }

    #[test]
    fn inverse_round_trips() {
        let t = Affine2::scale(2.0, 0.5)
            .then(&Affine2::rotate_degrees(33.0))
            .then(&Affine2::translate(-40.0, 12.5));
        let inv = t.invert().unwrap();
        for p in [[0.0, 0.0], [100.0, -3.0], [7.5, 64.0]] {
            assert!(close(inv.apply(t.apply(p)), p));
        }
        assert!(Affine2::scale(0.0, 1.0).invert().is_none());
    }

    #[test]
    fn rect_mapping() {
        let r = Affine2::scale(2.0, 2.0).map_rect(Rect::from_size(10.0, 5.0));
        assert_eq!(r, Rect::new(0.0, 0.0, 20.0, 10.0));
        assert!(r.contains_rect(&Rect::from_size(20.0, 10.0)));
    }
}

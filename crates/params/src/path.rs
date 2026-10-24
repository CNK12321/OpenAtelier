//! A bezier path as a property value ([`Value::Path`](crate::Value::Path)): points with
//! two handles each and numbers of their own (an effect's per-point width, say),
//! keyframed as a whole like any other value.
//!
//! Blending two keys matches points by their `id`, not their place in the list, so a
//! point added between keys never scrambles the in-between frames: a point one key has
//! and the other doesn't is first put into the other — on its curve, splitting the
//! segment it falls in without changing that curve's shape — and then both are blended
//! point by point. Handles blend by angle (the shorter way round) and length, so one
//! that turns doesn't shrink to nothing halfway and kink the curve.

use serde::{Deserialize, Serialize};

/// One point of a [`BezierPath`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathPoint {
    /// Which point it is, for matching it between keys: never reused within a path.
    pub id: u32,
    pub at: [f64; 2],
    /// The handles, as offsets from `at`; zero for a corner.
    #[serde(default, rename = "in", skip_serializing_if = "is_origin")]
    pub handle_in: [f64; 2],
    #[serde(default, rename = "out", skip_serializing_if = "is_origin")]
    pub handle_out: [f64; 2],
    /// The point's own numbers, in the order the parameter declares them
    /// (`ParamSchema::point_attrs`); missing ones are the declared defaults.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<f64>,
    /// Its handles stay mirrored while one is dragged (an editing rule; not blended).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub smooth: bool,
}

fn is_origin(p: &[f64; 2]) -> bool {
    *p == [0.0, 0.0]
}

/// A bezier path: open, or `closed` back to its first point.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BezierPath {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    pub points: Vec<PathPoint>,
}

fn lerp2(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// A handle blended by angle (the shorter way round) and length.
fn lerp_handle(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    let (la, lb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
    // A zero-length handle has no angle: it takes the other's.
    if la < 1e-12 || lb < 1e-12 {
        return lerp2(a, b, t);
    }
    let (aa, ab) = (a[1].atan2(a[0]), b[1].atan2(b[0]));
    let d = (ab - aa + std::f64::consts::PI * 3.0).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    let (angle, len) = (aa + d * t, la + (lb - la) * t);
    [angle.cos() * len, angle.sin() * len]
}

impl BezierPath {
    /// Most points a path keeps (and a shader is handed samples of).
    pub const MAX_POINTS: usize = 64;
    /// Samples a shader gets ([`BezierPath::pack`]).
    pub const SAMPLES: usize = 64;
    /// Point numbers a shader gets with each sample (the first two declared).
    pub const PACKED_ATTRS: usize = 2;
    /// Floats [`BezierPath::pack`] produces: sample count, closed, attribute count, then
    /// (x, y, two numbers) per sample.
    pub const PACKED_LEN: usize = 3 + Self::SAMPLES * (2 + Self::PACKED_ATTRS);

    /// An open path through `points`, corners all, ids 1, 2, 3…
    pub fn through(points: &[[f64; 2]]) -> BezierPath {
        let points = points.iter().enumerate().map(|(i, &at)| PathPoint { id: i as u32 + 1, at, handle_in: [0.0; 2], handle_out: [0.0; 2], attrs: Vec::new(), smooth: false }).collect();
        BezierPath { closed: false, points }
    }

    /// An id no point has.
    pub fn free_id(&self) -> u32 {
        self.points.iter().map(|p| p.id).max().unwrap_or(0) + 1
    }

    /// Segments: each point to the next (and the last back to the first, closed).
    pub fn segment_count(&self) -> usize {
        match self.points.len() {
            0 | 1 => 0,
            n if self.closed => n,
            n => n - 1,
        }
    }

    /// Segment `i`'s four control points.
    pub fn segment(&self, i: usize) -> [[f64; 2]; 4] {
        let n = self.points.len();
        let (a, b) = (&self.points[i % n], &self.points[(i + 1) % n]);
        [a.at, add(a.at, a.handle_out), add(b.at, b.handle_in), b.at]
    }

    /// The point at `t` (0 → 1) along segment `i`.
    pub fn point_on(&self, i: usize, t: f64) -> [f64; 2] {
        let [p0, p1, p2, p3] = self.segment(i);
        let u = 1.0 - t;
        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
        [w[0] * p0[0] + w[1] * p1[0] + w[2] * p2[0] + w[3] * p3[0], w[0] * p0[1] + w[1] * p1[1] + w[2] * p2[1] + w[3] * p3[1]]
    }

    /// Segment `i`'s length (measured in `scale` units: a path in fractions of a
    /// picture measures true lengths with the picture's size).
    pub fn segment_length(&self, i: usize, scale: [f64; 2]) -> f64 {
        let s = |p: [f64; 2]| [p[0] * scale[0], p[1] * scale[1]];
        (1..=16).map(|k| dist(s(self.point_on(i, (k - 1) as f64 / 16.0)), s(self.point_on(i, k as f64 / 16.0)))).sum()
    }

    /// Point `i`'s number `k` (`defaults` where it has none).
    pub fn attr(&self, i: usize, k: usize, defaults: &[f64]) -> f64 {
        self.points[i].attrs.get(k).copied().unwrap_or(defaults.get(k).copied().unwrap_or(0.0))
    }

    /// A point added at `t` along segment `i`, the curve keeping its shape (the
    /// segment split in two, its ends' handles shortened to match); its numbers
    /// blended from the two ends'. Returns its index.
    pub fn insert(&mut self, i: usize, t: f64, id: u32, defaults: &[f64]) -> usize {
        let n = self.points.len();
        let [p0, p1, p2, p3] = self.segment(i);
        // de Casteljau.
        let (a, b, c) = (lerp2(p0, p1, t), lerp2(p1, p2, t), lerp2(p2, p3, t));
        let (d, e) = (lerp2(a, b, t), lerp2(b, c, t));
        let at = lerp2(d, e, t);
        let (ia, ib) = (i % n, (i + 1) % n);
        let attrs = (0..defaults.len().max(self.points[ia].attrs.len()).max(self.points[ib].attrs.len()))
            .map(|k| {
                let (x, y) = (self.attr(ia, k, defaults), self.attr(ib, k, defaults));
                x + (y - x) * t
            })
            .collect();
        let smooth = sub(d, at) != [0.0; 2] || sub(e, at) != [0.0; 2];
        self.points[ia].handle_out = sub(a, p0);
        self.points[ib].handle_in = sub(c, p3);
        let point = PathPoint { id, at, handle_in: sub(d, at), handle_out: sub(e, at), attrs, smooth };
        let at_index = ia + 1;
        self.points.insert(at_index, point);
        at_index
    }

    /// Where a point of `other` (`id`, lying between its neighbours there) would be on
    /// this path: the segment between the same neighbours here and how far along it,
    /// by length; `None` if they aren't next to each other here.
    fn place_from(&self, other: &BezierPath, id: u32) -> Option<(usize, f64)> {
        let at = other.points.iter().position(|p| p.id == id)?;
        let n = other.points.len();
        // Its nearest neighbours in `other` that this path also has.
        let has = |pid: u32| self.points.iter().position(|p| p.id == pid);
        let back = (1..n).map(|k| (at + n - k) % n).take_while(|&j| other.closed || j < at).find(|&j| has(other.points[j].id).is_some())?;
        let ahead = (1..n).map(|k| (at + k) % n).take_while(|&j| other.closed || j > at).find(|&j| has(other.points[j].id).is_some())?;
        let (here_a, here_b) = (has(other.points[back].id)?, has(other.points[ahead].id)?);
        let m = self.points.len();
        let seg = if (here_a + 1) % m == here_b && (self.closed || here_a + 1 == here_b) { here_a } else { return None };
        // How far along, by the lengths of `other`'s segments from `back` to `ahead`.
        let lengths = |from: usize, to: usize| -> f64 {
            let mut s = 0.0;
            let mut j = from;
            while j != to {
                s += other.segment_length(j, [1.0, 1.0]);
                j = (j + 1) % n;
            }
            s
        };
        let (before, total) = (lengths(back, at), lengths(back, ahead));
        let t = if total > 1e-12 { before / total } else { 0.5 };
        Some((seg, t.clamp(0.0, 1.0)))
    }

    /// This path with every point of `other` it lacks put in: on its curve where `other`
    /// has it between the same neighbours (the shape unchanged), else at its nearer end
    /// (it grows out of the end).
    fn with_points_of(&self, other: &BezierPath, defaults: &[f64]) -> BezierPath {
        let mut out = self.clone();
        for (k, p) in other.points.iter().enumerate() {
            if out.points.iter().any(|q| q.id == p.id) {
                continue;
            }
            match out.place_from(other, p.id) {
                Some((seg, t)) => {
                    let at = out.insert(seg, t, p.id, defaults);
                    out.points[at].smooth = p.smooth;
                }
                None => {
                    // At the end it's nearer to in `other`.
                    let first = k * 2 < other.points.len();
                    let end = if first { out.points.first() } else { out.points.last() };
                    let at = end.map_or(p.at, |e| e.at);
                    let point = PathPoint { id: p.id, at, handle_in: [0.0; 2], handle_out: [0.0; 2], attrs: end.map_or_else(|| p.attrs.clone(), |e| e.attrs.clone()), smooth: p.smooth };
                    if first {
                        out.points.insert(0, point);
                    } else {
                        out.points.push(point);
                    }
                }
            }
        }
        out
    }

    /// The path `t` of the way (0 → 1) from this one to `other`.
    pub fn blend(&self, other: &BezierPath, t: f64) -> BezierPath {
        if t <= 0.0 {
            return self.clone();
        }
        if t >= 1.0 {
            return other.clone();
        }
        // Each with the other's points, then point by point (in this one's order).
        let (a, b) = (self.with_points_of(other, &[]), other.with_points_of(self, &[]));
        let points = a
            .points
            .iter()
            .map(|p| {
                let Some(q) = b.points.iter().find(|q| q.id == p.id) else { return p.clone() };
                let attrs = (0..p.attrs.len().max(q.attrs.len()))
                    .map(|k| {
                        let (x, y) = (p.attrs.get(k).or(q.attrs.get(k)).copied().unwrap_or(0.0), q.attrs.get(k).or(p.attrs.get(k)).copied().unwrap_or(0.0));
                        x + (y - x) * t
                    })
                    .collect();
                PathPoint { id: p.id, at: lerp2(p.at, q.at, t), handle_in: lerp_handle(p.handle_in, q.handle_in, t), handle_out: lerp_handle(p.handle_out, q.handle_out, t), attrs, smooth: p.smooth }
            })
            .collect();
        BezierPath { closed: self.closed, points }
    }

    /// `samples` points along the path, evenly by length (measured at `scale`), each
    /// with its numbers (`defaults` where a point has none) blended along its segment.
    pub fn sample(&self, samples: usize, scale: [f64; 2], defaults: &[f64]) -> Vec<([f64; 2], Vec<f64>)> {
        let segs = self.segment_count();
        if segs == 0 || samples < 2 {
            return self.points.first().map(|p| vec![(p.at, (0..defaults.len()).map(|k| self.attr(0, k, defaults)).collect())]).unwrap_or_default();
        }
        let n = self.points.len();
        let lengths: Vec<f64> = (0..segs).map(|i| self.segment_length(i, scale)).collect();
        let total: f64 = lengths.iter().sum();
        let attrs = |i: usize, t: f64| -> Vec<f64> {
            (0..defaults.len())
                .map(|k| {
                    let (x, y) = (self.attr(i % n, k, defaults), self.attr((i + 1) % n, k, defaults));
                    x + (y - x) * t
                })
                .collect()
        };
        (0..samples)
            .map(|k| {
                let want = total * k as f64 / (samples - 1) as f64;
                let mut run = 0.0;
                for (i, l) in lengths.iter().enumerate() {
                    if want <= run + l || i + 1 == segs {
                        let t = if *l > 1e-12 { ((want - run) / l).clamp(0.0, 1.0) } else { 0.0 };
                        // Evenly by length within the segment too (its parameter isn't).
                        let t = self.even_t(i, t, scale);
                        return (self.point_on(i, t), attrs(i, t));
                    }
                    run += l;
                }
                unreachable!("the last segment takes the rest")
            })
            .collect()
    }

    /// The parameter on segment `i` a share `share` of its length along.
    fn even_t(&self, i: usize, share: f64, scale: [f64; 2]) -> f64 {
        const STEPS: usize = 16;
        let s = |p: [f64; 2]| [p[0] * scale[0], p[1] * scale[1]];
        let marks: Vec<f64> = (0..=STEPS)
            .scan(0.0, |run, k| {
                if k > 0 {
                    *run += dist(s(self.point_on(i, (k - 1) as f64 / STEPS as f64)), s(self.point_on(i, k as f64 / STEPS as f64)));
                }
                Some(*run)
            })
            .collect();
        let want = share * marks[STEPS];
        let k = marks.partition_point(|m| *m < want).clamp(1, STEPS);
        let span = marks[k] - marks[k - 1];
        let f = if span > 1e-12 { (want - marks[k - 1]) / span } else { 0.0 };
        ((k - 1) as f64 + f) / STEPS as f64
    }

    /// The floats a shader reads: the sample count, 1 if closed, the attribute count,
    /// then per sample its position and its first two numbers (see
    /// [`BezierPath::PACKED_LEN`]). Sampled evenly by length as fractions of a square.
    pub fn pack(&self, defaults: &[f64]) -> Vec<f32> {
        let mut out = vec![0f32; Self::PACKED_LEN];
        let samples = self.sample(Self::SAMPLES, [1.0, 1.0], defaults);
        out[0] = samples.len() as f32;
        out[1] = self.closed as u8 as f32;
        out[2] = defaults.len().min(Self::PACKED_ATTRS) as f32;
        for (i, (at, attrs)) in samples.iter().enumerate() {
            let b = 3 + i * (2 + Self::PACKED_ATTRS);
            out[b] = at[0] as f32;
            out[b + 1] = at[1] as f32;
            for k in 0..Self::PACKED_ATTRS {
                out[b + 2 + k] = attrs.get(k).copied().unwrap_or(0.0) as f32;
            }
        }
        out
    }

    /// The box its points and handles fill (a curve never leaves it).
    pub fn bounds(&self) -> Option<[[f64; 2]; 2]> {
        let mut it = self.points.iter().flat_map(|p| [p.at, add(p.at, p.handle_in), add(p.at, p.handle_out)]);
        let first = it.next()?;
        Some(it.fold([first, first], |[lo, hi], q| [[lo[0].min(q[0]), lo[1].min(q[1])], [hi[0].max(q[0]), hi[1].max(q[1])]]))
    }

    pub fn is_finite(&self) -> bool {
        self.points.iter().all(|p| p.at.iter().chain(&p.handle_in).chain(&p.handle_out).chain(&p.attrs).all(|x| x.is_finite()))
    }

    /// Made safe to use: at most [`BezierPath::MAX_POINTS`] points, each id once (a
    /// repeated one renumbered), no NaN or infinity (such a point dropped).
    pub fn repaired(mut self) -> BezierPath {
        self.points.retain(|p| p.at.iter().chain(&p.handle_in).chain(&p.handle_out).chain(&p.attrs).all(|x| x.is_finite()));
        self.points.truncate(Self::MAX_POINTS);
        let mut seen = std::collections::HashSet::new();
        let mut next = self.free_id();
        for p in &mut self.points {
            if !seen.insert(p.id) {
                p.id = next;
                next += 1;
                seen.insert(p.id);
            }
        }
        self
    }

    /// Feeds an exact byte form of it to `out` (cache keys; `-0.0` and `0.0` alike, as
    /// `Value::write_canonical` has them).
    pub fn write_canonical(&self, out: &mut dyn FnMut(&[u8])) {
        let f = |x: f64, out: &mut dyn FnMut(&[u8])| out(&(if x == 0.0 { 0u64 } else if x.is_nan() { f64::NAN.to_bits() } else { x.to_bits() }).to_le_bytes());
        out(&[self.closed as u8]);
        out(&(self.points.len() as u64).to_le_bytes());
        for p in &self.points {
            out(&p.id.to_le_bytes());
            out(&[p.smooth as u8]);
            p.at.iter().chain(&p.handle_in).chain(&p.handle_out).for_each(|x| f(*x, out));
            out(&(p.attrs.len() as u64).to_le_bytes());
            p.attrs.iter().for_each(|x| f(*x, out));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) -> bool {
        dist(a, b) < 1e-9
    }

    /// An arc from (0, 0.5) up through the middle to (1, 0.5).
    fn arc() -> BezierPath {
        let mut p = BezierPath::through(&[[0.0, 0.5], [1.0, 0.5]]);
        p.points[0].handle_out = [0.3, -0.4];
        p.points[1].handle_in = [-0.3, -0.4];
        p
    }

    #[test]
    fn adding_a_point_keeps_the_shape() {
        let before = arc();
        let mut after = before.clone();
        let at = after.insert(0, 0.3, 9, &[]);
        assert_eq!((at, after.points.len()), (1, 3));
        assert!(close(after.points[1].at, before.point_on(0, 0.3)), "the new point is on the curve");
        // Anywhere along the old curve is on the new one: its first part, then its second.
        for k in 0..=10 {
            let t = k as f64 / 10.0;
            assert!(close(after.point_on(0, t), before.point_on(0, 0.3 * t)), "first part at {t}");
            assert!(close(after.point_on(1, t), before.point_on(0, 0.3 + 0.7 * t)), "second part at {t}");
        }
    }

    #[test]
    fn a_point_only_one_key_has_grows_out_of_the_curve() {
        let a = arc();
        let mut b = arc();
        b.insert(0, 0.5, 7, &[]);
        b.points[1].at[1] -= 0.3; // the new point pulled up
        // At the very start it's there, on the first key's curve, which keeps its shape.
        let start = a.blend(&b, 1e-9);
        assert_eq!(start.points.len(), 3, "the point is there from the start…");
        let off_curve = |p: [f64; 2]| (0..=1000).map(|k| dist(p, a.point_on(0, k as f64 / 1000.0))).fold(f64::MAX, f64::min);
        assert!(off_curve(start.points[1].at) < 1e-3, "…sitting on the first key's curve");
        for k in 0..=10 {
            let p = start.point_on(if k < 5 { 0 } else { 1 }, (k % 5) as f64 / 5.0);
            assert!(off_curve(p) < 1e-3, "the curve itself unchanged");
        }
        // Then it moves straight to where the second key has it.
        let mid = a.blend(&b, 0.5);
        assert!(close(mid.points[1].at, lerp2(start.points[1].at, b.points[1].at, 0.5)), "{:?}", mid.points[1].at);
        assert_eq!(a.blend(&b, 1.0), b);
    }

    #[test]
    fn points_are_matched_by_id_not_by_place() {
        let a = BezierPath::through(&[[0.0, 0.0], [1.0, 0.0]]);
        // The same two points, plus one before them: the old ones still go to themselves.
        let mut b = a.clone();
        b.points.insert(0, PathPoint { id: 5, at: [-1.0, 0.0], handle_in: [0.0; 2], handle_out: [0.0; 2], attrs: Vec::new(), smooth: false });
        let mid = a.blend(&b, 0.5);
        let by_id = |id: u32| mid.points.iter().find(|p| p.id == id).unwrap().at;
        assert!(close(by_id(1), [0.0, 0.0]) && close(by_id(2), [1.0, 0.0]), "{mid:?}");
        assert!(close(by_id(5), [-0.5, 0.0]), "the new one grows out of the start");
    }

    #[test]
    fn handles_turn_rather_than_shrink() {
        let mut a = BezierPath::through(&[[0.0, 0.0], [1.0, 0.0]]);
        let mut b = a.clone();
        a.points[0].handle_out = [0.2, 0.0];
        b.points[0].handle_out = [0.0, 0.2];
        let mid = a.blend(&b, 0.5).points[0].handle_out;
        assert!((mid[0].hypot(mid[1]) - 0.2).abs() < 1e-9, "its length kept: {mid:?}");
        // Opposite ways: still its length, not nothing.
        b.points[0].handle_out = [-0.2, 1e-6];
        let mid = a.blend(&b, 0.5).points[0].handle_out;
        assert!((mid[0].hypot(mid[1]) - 0.2).abs() < 1e-6, "{mid:?}");
    }

    #[test]
    fn samples_are_even_along_the_curve_with_their_numbers() {
        let mut p = BezierPath::through(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
        p.points[0].attrs = vec![0.0];
        p.points[1].attrs = vec![1.0];
        p.points[2].attrs = vec![1.0];
        let s = p.sample(5, [1.0, 1.0], &[0.5]);
        let at: Vec<[f64; 2]> = s.iter().map(|s| s.0).collect();
        assert!(close(at[0], [0.0, 0.0]) && close(at[2], [1.0, 0.0]) && close(at[4], [1.0, 1.0]), "{at:?}");
        assert!(close(at[1], [0.5, 0.0]) && close(at[3], [1.0, 0.5]));
        assert!((s[1].1[0] - 0.5).abs() < 1e-9, "the number blends along: {:?}", s[1].1);
        let packed = p.pack(&[0.5, 0.0]);
        assert_eq!(packed.len(), BezierPath::PACKED_LEN);
        assert_eq!(packed[0] as usize, BezierPath::SAMPLES);
    }

    #[test]
    fn repairs_ids_and_bad_numbers() {
        let mut p = BezierPath::through(&[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]]);
        p.points[2].id = 1;
        p.points[1].at[0] = f64::NAN;
        let r = p.repaired();
        assert_eq!(r.points.len(), 2);
        assert_ne!(r.points[0].id, r.points[1].id);
    }
}

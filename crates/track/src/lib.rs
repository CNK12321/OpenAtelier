//! Point tracks: a point's path over time, however it was made — clicked in by hand,
//! drawn with the mouse while the timeline plays (then smoothed), or followed by
//! CoTracker ([`engine`]) from one start point through the footage. A track becomes
//! keyframes on a position property; [`simplify`] keeps only the keys the path needs.

pub mod engine;

/// One point of a track: timeline seconds and a position (any 2D space — canvas px,
/// footage px, fractions).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Sample {
    pub t: f64,
    pub p: [f64; 2],
}

impl Sample {
    pub fn new(t: f64, p: [f64; 2]) -> Self {
        Sample { t, p }
    }
}

/// Takes the jitter out of a hand-drawn track: each point becomes a Gaussian-weighted
/// average of its neighbors in time (`seconds` is the spread; 0 leaves it as it is).
/// Works on unevenly spaced samples, as mouse input is.
pub fn smooth(samples: &[Sample], seconds: f64) -> Vec<Sample> {
    if seconds <= 0.0 || samples.len() < 3 {
        return samples.to_vec();
    }
    let reach = 3.0 * seconds;
    let mut out = Vec::with_capacity(samples.len());
    let mut lo = 0;
    for s in samples {
        while samples[lo].t < s.t - reach {
            lo += 1;
        }
        let (mut sum, mut total) = ([0.0; 2], 0.0);
        for n in samples[lo..].iter().take_while(|n| n.t <= s.t + reach) {
            let d = (n.t - s.t) / seconds;
            let w = (-0.5 * d * d).exp();
            sum[0] += w * n.p[0];
            sum[1] += w * n.p[1];
            total += w;
        }
        out.push(Sample::new(s.t, [sum[0] / total, sum[1] / total]));
    }
    out
}

/// The fewest samples whose straight-line (in time) interpolation stays within
/// `tolerance` of every original point (Ramer–Douglas–Peucker, with the error measured
/// at each sample's own time, so speed changes are kept too). First and last stay.
pub fn simplify(samples: &[Sample], tolerance: f64) -> Vec<Sample> {
    if samples.len() <= 2 {
        return samples.to_vec();
    }
    let mut keep = vec![false; samples.len()];
    keep[0] = true;
    keep[samples.len() - 1] = true;
    let mut stack = vec![(0, samples.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (sa, sb) = (samples[a], samples[b]);
        let span = (sb.t - sa.t).max(1e-12);
        let (worst, at) = (a + 1..b)
            .map(|i| {
                let f = (samples[i].t - sa.t) / span;
                let q = [sa.p[0] + (sb.p[0] - sa.p[0]) * f, sa.p[1] + (sb.p[1] - sa.p[1]) * f];
                ((samples[i].p[0] - q[0]).hypot(samples[i].p[1] - q[1]), i)
            })
            .fold((0.0, a), |best, x| if x.0 > best.0 { x } else { best });
        if worst > tolerance {
            keep[at] = true;
            stack.push((a, at));
            stack.push((at, b));
        }
    }
    samples.iter().zip(keep).filter(|(_, k)| *k).map(|(s, _)| *s).collect()
}

/// Where a track is at `t`: linear between its samples, held before and after.
pub fn at(samples: &[Sample], t: f64) -> Option<[f64; 2]> {
    let first = samples.first()?;
    if t <= first.t {
        return Some(first.p);
    }
    let i = samples.partition_point(|s| s.t <= t);
    let Some(b) = samples.get(i) else { return samples.last().map(|s| s.p) };
    let a = samples[i - 1];
    let f = (t - a.t) / (b.t - a.t).max(1e-12);
    Some([a.p[0] + (b.p[0] - a.p[0]) * f, a.p[1] + (b.p[1] - a.p[1]) * f])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize, jitter: f64) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let t = i as f64 / 60.0;
                // A deterministic wobble standing in for a shaky hand.
                let j = jitter * ((i as f64 * 12.9898).sin() * 43758.5453).fract();
                Sample::new(t, [100.0 * t + j, 50.0 - j])
            })
            .collect()
    }

    #[test]
    fn smoothing_takes_out_jitter_and_keeps_the_path() {
        let rough = line(120, 6.0);
        let smoothed = smooth(&rough, 0.08);
        let wobble = |s: &[Sample]| s.windows(3).map(|w| ((w[0].p[0] + w[2].p[0]) / 2.0 - w[1].p[0]).abs()).sum::<f64>();
        assert!(wobble(&smoothed) < wobble(&rough) * 0.2, "{} vs {}", wobble(&smoothed), wobble(&rough));
        // Still going the same way, at the same times.
        assert_eq!(smoothed.len(), rough.len());
        assert!((smoothed[60].p[0] - 100.0).abs() < 5.0, "{:?}", smoothed[60]);
        assert_eq!(smooth(&rough, 0.0), rough);
    }

    #[test]
    fn simplifying_keeps_only_the_corners() {
        // Right for a second, then down for a second: three keys say it all.
        let mut s: Vec<Sample> = (0..=60).map(|i| Sample::new(i as f64 / 60.0, [i as f64 * 2.0, 0.0])).collect();
        s.extend((1..=60).map(|i| Sample::new(1.0 + i as f64 / 60.0, [120.0, i as f64 * 2.0])));
        let keys = simplify(&s, 0.5);
        assert_eq!(keys.len(), 3, "{keys:?}");
        assert_eq!(keys[1].p, [120.0, 0.0]);
        // And the simplified track is never far from the original.
        for x in &s {
            let p = at(&keys, x.t).unwrap();
            assert!((p[0] - x.p[0]).hypot(p[1] - x.p[1]) <= 0.5 + 1e-9);
        }
        // A pause is a corner in time, even on a straight line.
        let pause = vec![Sample::new(0.0, [0.0, 0.0]), Sample::new(1.0, [10.0, 0.0]), Sample::new(2.0, [10.0, 0.0]), Sample::new(3.0, [20.0, 0.0])];
        assert_eq!(simplify(&pause, 0.5).len(), 4);
    }

    #[test]
    fn a_track_is_read_between_its_samples() {
        let s = [Sample::new(1.0, [0.0, 0.0]), Sample::new(2.0, [10.0, 20.0])];
        assert_eq!(at(&s, 0.0), Some([0.0, 0.0]));
        assert_eq!(at(&s, 1.5), Some([5.0, 10.0]));
        assert_eq!(at(&s, 9.0), Some([10.0, 20.0]));
        assert_eq!(at(&[], 1.0), None);
    }
}

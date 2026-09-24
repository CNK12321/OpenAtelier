//! Exact time for the editor.
//!
//! * Timeline time is an `i64` count of **flicks** (1/705,600,000 s). Every common
//!   frame rate — including NTSC 24000/1001, 30000/1001, 60000/1001 — and every common
//!   audio sample rate has an integer duration in flicks.
//! * Media timestamps are *not* guaranteed to divide into flicks (FFmpeg MP4s often use
//!   a 1/15360 timebase), so media keeps its own timebase and is compared exactly with
//!   [`Rational`] math.
//! * There is exactly **one** rule for turning a time into a frame: frames own the
//!   half-open interval `[pts, next_pts)`. See [`select_frame`] and [`FrameRate::frame_at`].
//!   Nothing else in the codebase may round time to frames.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

pub const FLICKS_PER_SECOND: i64 = 705_600_000;

/// A point or span on the timeline, in flicks.
#[derive(Copy, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Time(pub i64);

impl Time {
    pub const ZERO: Time = Time(0);
    pub const MAX: Time = Time(i64::MAX);

    pub const fn from_flicks(flicks: i64) -> Self {
        Time(flicks)
    }

    pub const fn from_seconds(seconds: i64) -> Self {
        Time(seconds * FLICKS_PER_SECOND)
    }

    /// For UI input only. Rounds toward negative infinity.
    /// Seconds (as typed or computed from user input) to a time. Out-of-range input is
    /// clamped to ±[`Time::MAX_SECONDS`] and NaN is zero, so later arithmetic on the
    /// result can never overflow.
    pub fn from_seconds_f64(seconds: f64) -> Self {
        let s = if seconds.is_nan() { 0.0 } else { seconds.clamp(-Self::MAX_SECONDS, Self::MAX_SECONDS) };
        Time((s * FLICKS_PER_SECOND as f64).floor() as i64)
    }

    /// The longest time made from seconds (about three years): large enough for any
    /// edit, small enough that sums of several stay far from `i64` overflow.
    pub const MAX_SECONDS: f64 = 1e8;

    pub fn as_seconds_f64(self) -> f64 {
        self.0 as f64 / FLICKS_PER_SECOND as f64
    }

    /// Exact conversion to seconds.
    pub fn as_rational(self) -> Rational {
        Rational::new(self.0, FLICKS_PER_SECOND)
    }

    /// Exact seconds → flicks, rounding toward negative infinity (the only time
    /// quantization rule for derived times such as speed-ramped source times).
    pub fn from_rational_floor(seconds: Rational) -> Self {
        let n = seconds.num as i128 * FLICKS_PER_SECOND as i128;
        Time(n.div_euclid(seconds.den as i128) as i64)
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.6}s", self.as_seconds_f64())
    }
}

impl Add for Time {
    type Output = Time;
    fn add(self, rhs: Time) -> Time {
        Time(self.0 + rhs.0)
    }
}
impl Sub for Time {
    type Output = Time;
    fn sub(self, rhs: Time) -> Time {
        Time(self.0 - rhs.0)
    }
}
impl Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time(-self.0)
    }
}
impl AddAssign for Time {
    fn add_assign(&mut self, rhs: Time) {
        self.0 += rhs.0;
    }
}
impl SubAssign for Time {
    fn sub_assign(&mut self, rhs: Time) {
        self.0 -= rhs.0;
    }
}

/// An exact fraction. Always normalized: `den > 0`, `gcd(num, den) == 1`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "(i64, i64)", into = "(i64, i64)")]
pub struct Rational {
    num: i64,
    den: i64,
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Rational {
    pub const ZERO: Rational = Rational { num: 0, den: 1 };
    pub const ONE: Rational = Rational { num: 1, den: 1 };

    /// Panics if `den == 0` or the reduced value does not fit in `i64`.
    pub fn new(num: i64, den: i64) -> Self {
        Self::from_i128(num as i128, den as i128)
    }

    pub fn integer(n: i64) -> Self {
        Rational { num: n, den: 1 }
    }

    fn from_i128(num: i128, den: i128) -> Self {
        assert!(den != 0, "rational with zero denominator");
        let g = gcd(num, den).max(1);
        let (mut num, mut den) = (num / g, den / g);
        if den < 0 {
            num = -num;
            den = -den;
        }
        Rational {
            num: i64::try_from(num).expect("rational overflow"),
            den: i64::try_from(den).expect("rational overflow"),
        }
    }

    pub fn num(self) -> i64 {
        self.num
    }
    pub fn den(self) -> i64 {
        self.den
    }

    pub fn recip(self) -> Rational {
        Self::from_i128(self.den as i128, self.num as i128)
    }

    pub fn is_zero(self) -> bool {
        self.num == 0
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

impl Mul for Rational {
    type Output = Rational;
    fn mul(self, rhs: Rational) -> Rational {
        Self::from_i128(self.num as i128 * rhs.num as i128, self.den as i128 * rhs.den as i128)
    }
}

impl Add for Rational {
    type Output = Rational;
    fn add(self, rhs: Rational) -> Rational {
        Self::from_i128(
            self.num as i128 * rhs.den as i128 + rhs.num as i128 * self.den as i128,
            self.den as i128 * rhs.den as i128,
        )
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.num as i128 * other.den as i128).cmp(&(other.num as i128 * self.den as i128))
    }
}
impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<(i64, i64)> for Rational {
    type Error = &'static str;
    fn try_from((num, den): (i64, i64)) -> Result<Self, Self::Error> {
        if den == 0 {
            Err("rational with zero denominator")
        } else {
            Ok(Rational::new(num, den))
        }
    }
}
impl From<Rational> for (i64, i64) {
    fn from(r: Rational) -> Self {
        (r.num, r.den)
    }
}

impl fmt::Debug for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// Frames per second as an exact fraction (e.g. 30000/1001).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const FPS_23_976: FrameRate = FrameRate { num: 24000, den: 1001 };
    pub const FPS_24: FrameRate = FrameRate { num: 24, den: 1 };
    pub const FPS_25: FrameRate = FrameRate { num: 25, den: 1 };
    pub const FPS_29_97: FrameRate = FrameRate { num: 30000, den: 1001 };
    pub const FPS_30: FrameRate = FrameRate { num: 30, den: 1 };
    pub const FPS_50: FrameRate = FrameRate { num: 50, den: 1 };
    pub const FPS_59_94: FrameRate = FrameRate { num: 60000, den: 1001 };
    pub const FPS_60: FrameRate = FrameRate { num: 60, den: 1 };

    pub const fn new(num: u32, den: u32) -> Self {
        FrameRate { num, den }
    }

    /// Duration of one frame in flicks, if it is an exact integer (true for all
    /// standard rates).
    pub fn frame_duration_exact(self) -> Option<Time> {
        let n = FLICKS_PER_SECOND as i128 * self.den as i128;
        (n % self.num as i128 == 0).then(|| Time((n / self.num as i128) as i64))
    }

    /// The frame index that owns time `t`: frame `n` owns `[n/fps, (n+1)/fps)`.
    pub fn frame_at(self, t: Time) -> i64 {
        let n = t.0 as i128 * self.num as i128;
        let d = FLICKS_PER_SECOND as i128 * self.den as i128;
        n.div_euclid(d) as i64
    }

    /// Exact start time of frame `n`, rounded toward negative infinity for rates
    /// that are not exact in flicks.
    pub fn frame_start(self, n: i64) -> Time {
        let v = n as i128 * FLICKS_PER_SECOND as i128 * self.den as i128;
        Time(v.div_euclid(self.num as i128) as i64)
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

/// The frame-selection rule for media with arbitrary timestamps.
///
/// `pts` are presentation timestamps in the stream's own `timebase` (seconds per
/// tick), sorted ascending. Frame `i` owns `[pts[i], pts[i+1])`; the last frame owns
/// everything after it. Times before the first frame select frame 0. Ties go to the
/// frame that *starts* at that instant. Comparison is exact — no floats.
pub fn select_frame(pts: &[i64], timebase: Rational, t: Rational) -> Option<usize> {
    if pts.is_empty() {
        return None;
    }
    // pts * tb <= t  <=>  pts * tb.num * t.den <= t.num * tb.den
    let rhs = t.num as i128 * timebase.den as i128;
    let k = timebase.num as i128 * t.den as i128;
    let after = pts.partition_point(|&p| p as i128 * k <= rhs);
    Some(after.saturating_sub(1))
}

/// A half-open span `[start, start + duration)` on the timeline.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Time,
    pub duration: Time,
}

impl TimeRange {
    pub const fn new(start: Time, duration: Time) -> Self {
        TimeRange { start, duration }
    }

    pub fn end(self) -> Time {
        self.start + self.duration
    }

    pub fn contains(self, t: Time) -> bool {
        t >= self.start && t < self.end()
    }

    pub fn overlaps(self, other: TimeRange) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_rates_are_exact_in_flicks() {
        for r in [
            FrameRate::FPS_23_976,
            FrameRate::FPS_24,
            FrameRate::FPS_25,
            FrameRate::FPS_29_97,
            FrameRate::FPS_30,
            FrameRate::FPS_50,
            FrameRate::FPS_59_94,
            FrameRate::FPS_60,
            FrameRate::new(120, 1),
        ] {
            assert!(r.frame_duration_exact().is_some(), "{r:?}");
        }
        assert_eq!(FrameRate::FPS_29_97.frame_duration_exact(), Some(Time(23_543_520)));
        for sr in [22050, 44100, 48000, 96000, 192000] {
            assert_eq!(FLICKS_PER_SECOND % sr, 0);
        }
    }

    #[test]
    fn frame_at_is_half_open_and_round_trips() {
        let r = FrameRate::FPS_29_97;
        for n in [0, 1, 29, 30, 1_000_000] {
            let start = r.frame_start(n);
            assert_eq!(r.frame_at(start), n);
            assert_eq!(r.frame_at(start - Time(1)), n - 1);
        }
        assert_eq!(r.frame_at(Time(-1)), -1);
    }

    #[test]
    fn timebase_15360_is_not_flick_exact_but_selection_is_exact() {
        let tb = Rational::new(1, 15360);
        // 30 fps in a 1/15360 timebase: pts step 512.
        let pts: Vec<i64> = (0..100).map(|i| i * 512).collect();
        // A tick that does not divide into flicks.
        assert_ne!((FLICKS_PER_SECOND as i128 * tb.num() as i128) % tb.den() as i128, 0);
        // Exactly at frame 10's pts → frame 10; one flick earlier → frame 9.
        let t10 = Rational::new(10 * 512, 15360);
        assert_eq!(select_frame(&pts, tb, t10), Some(10));
        let just_before = Time::from_rational_floor(t10) - Time(1);
        assert_eq!(select_frame(&pts, tb, just_before.as_rational()), Some(9));
        assert_eq!(select_frame(&pts, tb, Rational::new(-1, 1)), Some(0));
        assert_eq!(select_frame(&pts, tb, Rational::integer(3600)), Some(99));
        assert_eq!(select_frame(&[], tb, Rational::ZERO), None);
    }

    #[test]
    fn rational_normalizes_and_orders() {
        assert_eq!(Rational::new(2, -4), Rational::new(-1, 2));
        assert!(Rational::new(1, 3) < Rational::new(1, 2));
        assert_eq!(Rational::new(1, 3) + Rational::new(1, 6), Rational::new(1, 2));
        assert_eq!(Time::from_rational_floor(Rational::new(-1, 3)).0, -235_200_000);
    }

    #[test]
    fn ranges_are_half_open() {
        let a = TimeRange::new(Time::from_seconds(1), Time::from_seconds(2));
        assert!(a.contains(Time::from_seconds(1)));
        assert!(!a.contains(Time::from_seconds(3)));
        assert!(!a.overlaps(TimeRange::new(Time::from_seconds(3), Time::from_seconds(1))));
        assert!(a.overlaps(TimeRange::new(Time::from_seconds(2), Time::from_seconds(5))));
    }
}

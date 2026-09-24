//! Live sound levels that [`crate::Modulator::Follow`] reads while a frame is planned.
//!
//! Evaluating a parameter stays a pure function of its settings and the instant: the
//! caller that plans a frame installs a *provider* for that frame's timeline time (from
//! loudness envelopes analyzed ahead of time), so preview and export see the same
//! numbers. With no provider installed every level is silence and a following property
//! shows its base value.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;

/// Which part of the sound a property follows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SoundBand {
    /// Everything.
    Loudness,
    /// Below about 200 Hz: kicks, bass lines.
    Bass,
    /// About 200 Hz – 4 kHz: voices, most instruments.
    Mids,
    /// Above about 4 kHz: hi-hats, sibilance, sparkle.
    Treble,
}

impl SoundBand {
    pub const ALL: [SoundBand; 4] = [SoundBand::Loudness, SoundBand::Bass, SoundBand::Mids, SoundBand::Treble];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            SoundBand::Loudness => "Loudness",
            SoundBand::Bass => "Bass",
            SoundBand::Mids => "Mids",
            SoundBand::Treble => "Treble",
        }
    }
}

/// Whose sound a property follows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SoundSource {
    /// Everything playing at that instant (the program mix).
    Mix,
    /// One timeline item (its id), wherever it is.
    Item(u64),
}

type Provider = dyn Fn(SoundSource, SoundBand) -> f64;

thread_local! {
    static PROVIDER: RefCell<Option<Rc<Provider>>> = const { RefCell::new(None) };
}

/// Restores the previous provider, even if planning panics.
struct Restore(Option<Rc<Provider>>);

impl Drop for Restore {
    fn drop(&mut self) {
        let prev = self.0.take();
        PROVIDER.with(|p| *p.borrow_mut() = prev);
    }
}

/// Runs `f` with `provider` answering [`level`] on this thread: the linear RMS level
/// (1.0 = full scale) of `source`'s `band` at the instant being planned.
pub fn with<R>(provider: Rc<Provider>, f: impl FnOnce() -> R) -> R {
    let prev = PROVIDER.with(|p| p.borrow_mut().replace(provider));
    let _restore = Restore(prev);
    f()
}

/// The current level of `source`'s `band` (linear RMS; 0 when nothing provides one).
pub fn level(source: SoundSource, band: SoundBand) -> f64 {
    let provider = PROVIDER.with(|p| p.borrow().clone());
    provider.map_or(0.0, |p| p(source, band).max(0.0))
}

/// Where a level lands between `floor_db` (0) and [`CEILING_DB`] (1).
pub fn normalized(level: f64, floor_db: f64) -> f64 {
    if level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.log10();
    let floor = floor_db.min(CEILING_DB - 1.0);
    ((db - floor) / (CEILING_DB - floor)).clamp(0.0, 1.0)
}

/// The level (dBFS RMS) a following property reaches its full offset at.
pub const CEILING_DB: f64 = -6.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_come_from_the_installed_provider_only() {
        assert_eq!(level(SoundSource::Mix, SoundBand::Bass), 0.0);
        let got = with(Rc::new(|s, b| if s == SoundSource::Item(7) && b == SoundBand::Bass { 0.5 } else { 0.1 }), || level(SoundSource::Item(7), SoundBand::Bass));
        assert_eq!(got, 0.5);
        assert_eq!(level(SoundSource::Item(7), SoundBand::Bass), 0.0);
        assert_eq!(normalized(0.0, -48.0), 0.0);
        assert!((normalized(10f64.powf(-6.0 / 20.0), -48.0) - 1.0).abs() < 1e-9);
        assert!((normalized(10f64.powf(-27.0 / 20.0), -48.0) - 0.5).abs() < 1e-9);
    }
}

#[cfg(test)]
mod follow_tests {
    use super::*;
    use crate::{EvalContext, ParamSource, Value};
    use oa_time::Time;

    #[test]
    fn a_followed_property_moves_with_the_level_and_round_trips() {
        let src = ParamSource::Static(Value::Float(1.0)).follow(SoundSource::Mix, SoundBand::Bass, 0.5, -48.0);
        let ctx = EvalContext::at(Time::ZERO, Time::ZERO);
        assert!(src.follows_sound());
        assert_eq!(src.eval(&ctx), Value::Float(1.0), "silent without a provider");
        let loud = with(Rc::new(|_, _| 1.0), || src.eval(&ctx));
        assert_eq!(loud, Value::Float(1.5));
        let json = serde_json::to_string(&src).unwrap();
        let back: ParamSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, src);
        assert_eq!(src.clone().remove_follow(), ParamSource::Static(Value::Float(1.0)));
        assert!(src.is_valid());
    }
}

#[cfg(test)]
mod track_tests {
    use crate::{EvalContext, ParamSource, PointTrack, Value};
    use oa_time::Time;
    use std::sync::Arc;

    fn close(v: Value, want: [f64; 2]) -> bool {
        matches!(v, Value::Vec2([a, b]) if (a - want[0]).abs() < 1e-9 && (b - want[1]).abs() < 1e-9)
    }

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    #[test]
    fn a_position_on_a_track_is_the_track_plus_its_offset() {
        let mut track = PointTrack::new(9, "Ball");
        track.set(secs(10.0), [0.0, 0.0]);
        track.set(secs(12.0), [0.2, -0.1]);
        let track = Arc::new(track);
        // A clip starting at 9 s: clip time 2 s is timeline 11 s, half way.
        let src = ParamSource::Static(Value::Vec2([0.5, 0.5])).wiggle(0.0, 1.0, 1).with_track(track.clone(), secs(9.0), [0.05, 0.0]);
        let at = |t: f64| src.eval(&EvalContext::at(secs(t), secs(t)));
        assert!(close(at(2.0), [0.15, -0.05]), "{:?}", at(2.0));
        assert!(close(at(0.0), [0.05, 0.0]), "held before the track starts");
        assert_eq!(src.find_track().map(|(t, c)| (t.id, c)), Some((9, secs(9.0))));
        assert_eq!(src.track_offset(), Some(&ParamSource::Static(Value::Vec2([0.05, 0.0]))));
        // Motion stays on top: still a wiggle outermost.
        assert!(matches!(&src, ParamSource::Modulated { modulator: crate::Modulator::Wiggle { .. }, .. }));

        // A split moves the clip clock; the same instant keeps its place.
        let mut split = src.clone();
        split.shift_clip_clock(secs(1.0));
        assert!(close(split.eval(&EvalContext::at(secs(1.0), secs(1.0))), [0.15, -0.05]));

        // An edited track reaches every property following it.
        let mut edited = (*track).clone();
        edited.set(secs(12.0), [0.4, -0.1]);
        let moved = src.clone().retracked(&Arc::new(edited));
        assert!(close(moved.eval(&EvalContext::at(secs(3.0), secs(3.0))), [0.45, -0.1]));

        // Letting go keeps it where it is at the chosen moment.
        let free = src.clone().without_track(track.at(secs(11.0)).unwrap());
        assert!(close(free.eval(&EvalContext::at(secs(2.0), secs(2.0))), [0.15, -0.05]));
        assert!(free.find_track().is_none());

        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(serde_json::from_str::<ParamSource>(&json).unwrap(), src);
    }

    #[test]
    fn tracks_take_new_points_in_place() {
        let mut t = PointTrack::new(1, "t");
        for i in 0..5 {
            t.set(secs(i as f64), [i as f64, 0.0]);
        }
        t.replace(&[(secs(1.5), [9.0, 9.0]), (secs(3.0), [8.0, 8.0])]);
        assert_eq!(t.points.iter().map(|p| p.0.as_seconds_f64()).collect::<Vec<_>>(), vec![0.0, 1.0, 1.5, 3.0, 4.0]);
        assert_eq!(t.at(secs(3.5)), Some([6.0, 4.0]));
    }
}

#[cfg(test)]
mod stabilize_tests {
    use crate::{EvalContext, ParamSource, PointTrack, Stabilize, TrackUse, Value};
    use oa_time::Time;
    use std::sync::Arc;

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    /// A slow pan to the right with a fast shake on top, 30 points a second for 4 s.
    fn shaky_pan() -> PointTrack {
        let mut t = PointTrack::new(3, "Shaky");
        for i in 0..120 {
            let s = i as f64 / 30.0;
            let shake = 0.01 * (s * 2.0 * std::f64::consts::PI * 8.0).sin();
            t.set(secs(s), [0.05 * s + shake, shake]);
        }
        t
    }

    fn value(src: &ParamSource, s: f64) -> [f64; 2] {
        src.eval(&EvalContext::at(secs(s), secs(s))).as_vec2().unwrap()
    }

    #[test]
    fn locking_holds_the_tracked_point_still() {
        let track = Arc::new(shaky_pan());
        let anchor = track.at(secs(0.0)).unwrap();
        let mode = TrackUse::Stabilize(Stabilize::new(&track, 1.0, true, 1.0, anchor));
        let src = ParamSource::Static(Value::Vec2([0.0, 0.0])).with_track_as(track.clone(), Time::ZERO, [0.0, 0.0], mode);
        // Where the tracked point ends up = where it was + the clip's move: always the anchor.
        for i in 0..119 {
            let s = i as f64 / 30.0;
            let p = track.at(secs(s)).unwrap();
            let v = value(&src, s);
            assert!((p[0] + v[0] - anchor[0]).abs() < 1e-9 && (p[1] + v[1] - anchor[1]).abs() < 1e-9, "at {s}: {:?}", [p[0] + v[0], p[1] + v[1]]);
        }
    }

    #[test]
    fn smoothing_takes_out_the_shake_and_keeps_the_pan() {
        let track = Arc::new(shaky_pan());
        let mode = TrackUse::Stabilize(Stabilize::new(&track, 1.0, false, 0.3, [0.0; 2]));
        let src = ParamSource::Static(Value::Vec2([0.0, 0.0])).with_track_as(track.clone(), Time::ZERO, [0.0, 0.0], mode);
        let seen = |s: f64| {
            let (p, v) = (track.at(secs(s)).unwrap(), value(&src, s));
            [p[0] + v[0], p[1] + v[1]]
        };
        // Away from the ends: the shake (±0.01 vertically) is almost gone...
        let wobble = (30..90).map(|i| seen(i as f64 / 30.0)[1].abs()).fold(0.0, f64::max);
        assert!(wobble < 0.002, "vertical shake left: {wobble}");
        // ...and the pan (0.05 a second) is still there.
        let pan = seen(2.5)[0] - seen(1.5)[0];
        assert!((pan - 0.05).abs() < 0.005, "pan over a second: {pan}");
        // Half strength: half the correction.
        let half = TrackUse::Stabilize(Stabilize::new(&track, 0.5, false, 0.3, [0.0; 2]));
        let (full_c, half_c) = (TrackUse::Stabilize(Stabilize::new(&track, 1.0, false, 0.3, [0.0; 2])).contribution(&track, secs(1.0)).unwrap(), half.contribution(&track, secs(1.0)).unwrap());
        assert!((half_c[1] - full_c[1] / 2.0).abs() < 1e-12);
    }

    #[test]
    fn switching_between_follow_and_stabilize_keeps_the_value_there() {
        let track = Arc::new(shaky_pan());
        let src = ParamSource::Static(Value::Vec2([0.2, 0.1])).with_track(track.clone(), Time::ZERO, [0.2, 0.1]);
        let at = EvalContext::at(secs(2.0), secs(2.0));
        let before = src.eval(&at);
        let mode = TrackUse::Stabilize(Stabilize::new(&track, 1.0, false, 0.5, [0.0; 2]));
        let stab = src.clone().with_track_use(mode, &at);
        assert!(matches!(stab.track_use(), Some(TrackUse::Stabilize(_))));
        let (a, b) = (before.as_vec2().unwrap(), stab.eval(&at).as_vec2().unwrap());
        assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{a:?} vs {b:?}");
        // An edited track is stabilized against again; the settings stay.
        let mut edited = (*track).clone();
        edited.set(secs(1.0), [0.3, 0.3]);
        let again = stab.clone().retracked(&Arc::new(edited));
        let Some(TrackUse::Stabilize(s)) = again.track_use() else { panic!() };
        assert_eq!((s.strength, s.lock, s.smoothing), (1.0, false, 0.5));
        assert_ne!(again.eval(&EvalContext::at(secs(1.0), secs(1.0))), stab.eval(&EvalContext::at(secs(1.0), secs(1.0))));
        // Saved and loaded as it was (to the last digit JSON keeps).
        let loaded: ParamSource = serde_json::from_str(&serde_json::to_string(&stab).unwrap()).unwrap();
        assert!(matches!(loaded.track_use(), Some(TrackUse::Stabilize(s)) if !s.lock && s.smoothing == 0.5));
        for i in 0..40 {
            let (a, b) = (value(&stab, i as f64 / 10.0), value(&loaded, i as f64 / 10.0));
            assert!((a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12);
        }
    }
}

#[cfg(test)]
mod hold_point_tests {
    use crate::{EvalContext, ParamSource, PointTrack, Stabilize, TrackUse, Value};
    use oa_time::Time;
    use std::sync::Arc;

    #[test]
    fn a_picked_point_is_where_the_tracked_point_is_held() {
        let secs = Time::from_seconds_f64;
        let mut track = PointTrack::new(4, "t");
        for i in 0..60 {
            let s = i as f64 / 30.0;
            track.set(secs(s), [0.2 + 0.03 * (s * 9.0).sin(), -0.1 + 0.02 * (s * 7.0).cos()]);
        }
        let track = Arc::new(track);
        // Smoothed first (the clip resting where it is), then locked at the center: the
        // resting place stays, and the tracked point sits exactly on the picked spot.
        let rest = [0.0, 0.0];
        let smooth = TrackUse::Stabilize(Stabilize::new(&track, 1.0, false, 0.5, [0.0; 2]));
        let src = ParamSource::Static(Value::Vec2(rest)).with_track_as(track.clone(), Time::ZERO, rest, smooth);
        let centered = src.clone().replace_track_use(TrackUse::Stabilize(Stabilize::new(&track, 1.0, true, 0.5, [0.0, 0.0])));
        assert_eq!(centered.track_offset(), Some(&ParamSource::Static(Value::Vec2(rest))), "the resting place is kept");
        for i in 0..59 {
            let s = i as f64 / 30.0;
            let p = track.at(secs(s)).unwrap();
            let v = centered.eval(&EvalContext::at(secs(s), secs(s))).as_vec2().unwrap();
            // Where the tracked point shows = where it was + how far the clip moved.
            assert!((p[0] + v[0]).abs() < 1e-9 && (p[1] + v[1]).abs() < 1e-9, "at {s}: {:?}", [p[0] + v[0], p[1] + v[1]]);
        }
    }
}

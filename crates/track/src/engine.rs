//! The tracking engine: CoTracker (Meta's point tracker) in a private Python,
//! downloaded on request — the same kind of self-contained folder the caption engine
//! uses, run by the same steps ([`oa_captions::engine`]):
//!
//! ```text
//! tracker/
//!   uv/, python/, env/   uv, its Python, and a virtual environment with PyTorch (CPU)
//!   torch/hub/           CoTracker's code and weights (torch.hub's cache)
//!   work/                the frames of the footage being tracked
//!   oa_track.py, READY
//! ```
//!
//! CoTracker is licensed CC-BY-NC 4.0 (non-commercial use); it's fetched from its own
//! repository when the user sets it up, never shipped with the editor.

use crate::Sample;
use oa_captions::engine::{uv_asset, Engine, Step};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use oa_captions::engine::{run, Job, Msg};

/// The bridge script (see `oa_track.py`).
pub const SCRIPT: &str = include_str!("oa_track.py");
const READY: &str = "READY";
const LAYOUT: &str = "cotracker engine 1";
const PYTHON_VERSION: &str = "3.12";
/// Names the PyTorch build installed ("cpu" or "cuda").
const COMPUTE: &str = "COMPUTE";

/// Where tracking runs: which PyTorch build the tracker installs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Compute {
    /// Works everywhere; uses every core.
    Cpu,
    /// CUDA (NVIDIA graphics cards): many times faster, a much bigger download.
    Nvidia,
}

impl Compute {
    fn name(self) -> &'static str {
        match self {
            Compute::Cpu => "cpu",
            Compute::Nvidia => "cuda",
        }
    }

    fn index(self) -> &'static str {
        match self {
            Compute::Cpu => "https://download.pytorch.org/whl/cpu",
            Compute::Nvidia => "https://download.pytorch.org/whl/cu128",
        }
    }

    /// Roughly what setting up with it downloads (PyTorch, then CoTracker's weights), MB.
    pub fn download_mb(self) -> u32 {
        match self {
            Compute::Cpu => 400,
            Compute::Nvidia => 3200,
        }
    }
}

/// The NVIDIA graphics card's name, if there is one with a working driver (asks
/// `nvidia-smi`, which the driver installs).
pub fn nvidia_gpu() -> Option<String> {
    let mut c = std::process::Command::new("nvidia-smi");
    c.args(["--query-gpu=name", "--format=csv,noheader"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c.output().ok().filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout).lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// The footage is handed over at most this wide or tall (CoTracker works at 512×384).
pub const MAX_SIDE: u32 = 512;
/// At most this many frames per run: the tracker walks them in small windows, so this
/// only bounds the frames file (about 0.44 MB a frame at 512×288).
pub const MAX_FRAMES: usize = 900;

/// The tracker's folder.
#[derive(Clone, Debug)]
pub struct Tracker {
    engine: Engine,
}

/// What to follow a point through: a stretch of a file, sampled evenly.
#[derive(Clone, Debug, PartialEq)]
pub struct Footage {
    pub path: PathBuf,
    /// Where in the file (seconds) and for how long.
    pub start: f64,
    pub duration: f64,
    /// Frames to hand over, and their size (even numbers).
    pub frames: usize,
    pub size: [u32; 2],
}

impl Footage {
    /// `frames` evenly over `duration`, at most [`MAX_FRAMES`], scaled from `native` to fit
    /// [`MAX_SIDE`].
    pub fn new(path: PathBuf, start: f64, duration: f64, frames: usize, native: [f64; 2]) -> Footage {
        let k = (MAX_SIDE as f64 / native[0].max(native[1]).max(1.0)).min(1.0);
        let even = |x: f64| (((x * k).round() as u32).max(2) / 2) * 2;
        Footage { path, start, duration: duration.max(1e-3), frames: frames.clamp(2, MAX_FRAMES), size: [even(native[0]), even(native[1])] }
    }

    /// The file time (seconds) of frame `i`.
    pub fn time_of(&self, i: usize) -> f64 {
        self.start + self.duration * i as f64 / self.frames as f64
    }

    /// The frame nearest file time `t`.
    pub fn frame_at(&self, t: f64) -> usize {
        (((t - self.start) / self.duration * self.frames as f64).round().max(0.0) as usize).min(self.frames - 1)
    }
}

impl Tracker {
    pub fn new(root: PathBuf) -> Self {
        Tracker { engine: Engine::new(root) }
    }

    /// `OA_TRACKER_DIR`, else `tracker` beside the caption engine's folder.
    pub fn default_root() -> PathBuf {
        if let Some(dir) = std::env::var_os("OA_TRACKER_DIR") {
            return PathBuf::from(dir);
        }
        let captions = Engine::default_root();
        captions.parent().map_or_else(|| captions.join("tracker"), |p| p.join("tracker"))
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn root(&self) -> &Path {
        self.engine.root()
    }

    /// Set up completely.
    pub fn is_installed(&self) -> bool {
        std::fs::read_to_string(self.root().join(READY)).is_ok_and(|s| s.trim() == LAYOUT) && self.engine.python().exists()
    }

    /// Deletes the tracker: back to how it was before setup.
    pub fn remove(&self) -> std::io::Result<()> {
        self.engine.remove()
    }

    fn script(&self) -> PathBuf {
        self.root().join("oa_track.py")
    }

    /// Which PyTorch it has: the CPU build unless the NVIDIA one was chosen.
    pub fn compute(&self) -> Compute {
        match std::fs::read_to_string(self.root().join(COMPUTE)).as_deref().map(str::trim) {
            Ok("cuda") => Compute::Nvidia,
            _ => Compute::Cpu,
        }
    }

    /// PyTorch for `compute`, replacing whichever build is there.
    fn torch_steps(&self, compute: Compute) -> Vec<Step> {
        let os = |s: &str| OsString::from(s);
        let python = self.engine.python().into_os_string();
        vec![
            Step::Uv {
                what: match compute {
                    Compute::Cpu => "PyTorch".into(),
                    Compute::Nvidia => "PyTorch for NVIDIA GPUs".into(),
                },
                args: vec![os("pip"), os("install"), os("--python"), python, os("--reinstall-package"), os("torch"), os("torch"), os("--index-url"), os(compute.index())],
            },
            Step::Write { path: self.root().join(COMPUTE), text: compute.name().into() },
        ]
    }

    /// uv, Python, PyTorch for `compute`, then CoTracker.
    pub fn install_steps(&self, compute: Compute) -> Vec<Step> {
        let root = self.root();
        let archive = root.join(uv_asset());
        let python = self.engine.python().into_os_string();
        let os = |s: &str| OsString::from(s);
        let mut steps = vec![
            Step::Download { what: "uv, the Python installer".into(), url: format!("https://github.com/astral-sh/uv/releases/latest/download/{}", uv_asset()), to: archive.clone() },
            Step::Extract { archive, into: root.join("uv") },
            Step::Uv { what: format!("Python {PYTHON_VERSION}"), args: vec![os("venv"), os("--clear"), os("--python"), os(PYTHON_VERSION), root.join("env").into_os_string()] },
        ];
        steps.extend(self.torch_steps(compute));
        steps.extend([
            Step::Uv { what: "NumPy".into(), args: vec![os("pip"), os("install"), os("--python"), python, os("numpy")] },
            Step::Uv { what: "Tidying up".into(), args: vec![os("cache"), os("clean")] },
            Step::Write { path: self.script(), text: SCRIPT.into() },
            Step::Python { what: "the CoTracker model".into(), args: vec![self.script().into_os_string(), os("download")] },
            Step::Write { path: root.join(READY), text: LAYOUT.into() },
        ]);
        steps
    }

    /// Swaps an installed tracker's PyTorch to `compute`'s build.
    pub fn switch_steps(&self, compute: Compute) -> Vec<Step> {
        let mut steps = self.torch_steps(compute);
        steps.push(Step::Uv { what: "Tidying up".into(), args: vec![OsString::from("cache"), OsString::from("clean")] });
        steps
    }

    /// The frames file a run hands over.
    pub fn frames_path(&self) -> PathBuf {
        self.root().join("work").join("frames.rgb")
    }

    /// Cuts `footage` into frames (with ffmpeg, `ffmpeg` naming it), then follows the
    /// point at `at` (footage px, at file time `time`) through them.
    pub fn track_steps(&self, ffmpeg: &str, footage: &Footage, time: f64, at: [f64; 2]) -> Vec<Step> {
        let out = self.frames_path();
        let [w, h] = footage.size;
        let fps = footage.frames as f64 / footage.duration;
        let os = |s: String| OsString::from(s);
        let ffmpeg_args = vec![
            os("-v".into()),
            os("error".into()),
            os("-nostdin".into()),
            // "frame=  123" lines as it goes: how far along reading the footage is.
            os("-stats".into()),
            os("-y".into()),
            os("-ss".into()),
            os(format!("{:.6}", footage.start.max(0.0))),
            os("-i".into()),
            footage.path.clone().into_os_string(),
            os("-t".into()),
            os(format!("{:.6}", footage.duration)),
            os("-vf".into()),
            os(format!("fps={fps:.6},scale={w}:{h}")),
            os("-frames:v".into()),
            os(footage.frames.to_string()),
            os("-an".into()),
            os("-f".into()),
            os("rawvideo".into()),
            os("-pix_fmt".into()),
            os("rgb24".into()),
            out.clone().into_os_string(),
        ];
        vec![
            Step::Write { path: self.script(), text: SCRIPT.into() },
            // Makes the work folder (and clears the last run's frames).
            Step::Write { path: out.clone(), text: String::new() },
            Step::Tool { what: "Reading the footage".into(), program: ffmpeg.into(), args: ffmpeg_args },
            Step::Python {
                what: "Following the point".into(),
                args: vec![
                    self.script().into_os_string(),
                    os("track".into()),
                    os("--frames".into()),
                    out.into_os_string(),
                    os("--width".into()),
                    os(w.to_string()),
                    os("--height".into()),
                    os(h.to_string()),
                    os("--frame".into()),
                    os(footage.frame_at(time).to_string()),
                    os("--x".into()),
                    os(format!("{:.3}", at[0])),
                    os("--y".into()),
                    os(format!("{:.3}", at[1])),
                ],
            },
        ]
    }
}

/// Where the bridge said it's running (its `device` line): "NVIDIA GeForce …",
/// "CPU, 8 threads".
pub fn parse_device(line: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Line {
        r#type: String,
        name: String,
    }
    let l: Line = serde_json::from_str(line.trim()).ok()?;
    (l.r#type == "device").then_some(l.name)
}

/// Where the point is on one frame, as the bridge reports it while it runs (its `at`
/// lines): (frame, footage px).
pub fn parse_at(line: &str) -> Option<(usize, [f64; 2])> {
    #[derive(serde::Deserialize)]
    struct Line {
        r#type: String,
        frame: usize,
        x: f64,
        y: f64,
    }
    let l: Line = serde_json::from_str(line.trim()).ok()?;
    (l.r#type == "at").then_some((l.frame, [l.x, l.y]))
}

/// How many frames ffmpeg has written, from one of its `-stats` lines ("frame=  123 …").
pub fn ffmpeg_frames(line: &str) -> Option<usize> {
    let rest = line.trim_start().strip_prefix("frame=")?;
    rest.split_whitespace().next()?.parse().ok()
}

/// A tracked point per frame: where (footage px) and whether it was seen there.
#[derive(Clone, Debug, PartialEq)]
pub struct Tracked {
    pub points: Vec<([f64; 2], bool)>,
}

impl Tracked {
    /// The bridge's `track` line, if `line` is one.
    pub fn parse(line: &str) -> Option<Tracked> {
        #[derive(serde::Deserialize)]
        struct Line {
            r#type: String,
            points: Vec<(f64, f64, bool)>,
        }
        let l: Line = serde_json::from_str(line.trim()).ok()?;
        (l.r#type == "track").then(|| Tracked { points: l.points.into_iter().map(|(x, y, v)| ([x, y], v)).collect() })
    }

    /// As samples at file times (`footage`'s frames), in the footage's native px (the
    /// frames were scaled down from `native`).
    pub fn samples(&self, footage: &Footage, native: [f64; 2]) -> Vec<(f64, [f64; 2], bool)> {
        let k = [native[0] / footage.size[0] as f64, native[1] / footage.size[1] as f64];
        self.points.iter().enumerate().map(|(i, (p, seen))| (footage.time_of(i), [p[0] * k[0], p[1] * k[1]], *seen)).collect()
    }
}

/// Samples from a tracked run, positions mapped by `place` (file time, footage px →
/// timeline seconds and the target space); frames `place` rejects are skipped.
pub fn to_samples(tracked: &[(f64, [f64; 2], bool)], mut place: impl FnMut(f64, [f64; 2]) -> Option<(f64, [f64; 2])>) -> Vec<Sample> {
    tracked.iter().filter_map(|(t, p, _)| place(*t, *p)).map(|(t, p)| Sample::new(t, p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footage_is_sampled_evenly_and_scaled_down() {
        let f = Footage::new(PathBuf::from("a.mp4"), 2.0, 4.0, 1000, [1920.0, 1080.0]);
        assert_eq!(f.frames, MAX_FRAMES);
        assert_eq!(f.size, [512, 288]);
        assert_eq!(f.time_of(0), 2.0);
        assert_eq!(f.frame_at(2.0 + 2.0), MAX_FRAMES / 2);
        assert_eq!(f.frame_at(99.0), MAX_FRAMES - 1);
        let small = Footage::new(PathBuf::from("a.mp4"), 0.0, 1.0, 30, [320.0, 241.0]);
        assert_eq!(small.size, [320, 240], "never enlarged, always even");
    }

    #[test]
    fn setup_stays_in_its_folder_and_tracking_hands_frames_over() {
        let t = Tracker::new(std::env::temp_dir().join("oa-tracker-steps"));
        let steps = t.install_steps(Compute::Cpu);
        assert!(t.install_steps(Compute::Nvidia).iter().any(|s| matches!(s, Step::Uv { args, .. } if args.iter().any(|a| a == "https://download.pytorch.org/whl/cu128"))));
        assert_eq!(t.compute(), Compute::Cpu);
        assert!(steps.iter().any(|s| matches!(s, Step::Uv { args, .. } if args.iter().any(|a| a == "https://download.pytorch.org/whl/cpu"))));
        assert!(matches!(steps.last(), Some(Step::Write { path, .. }) if path.ends_with(READY)));
        assert!(!t.is_installed());
        let f = Footage::new(PathBuf::from("clip.mp4"), 1.0, 2.0, 60, [1280.0, 720.0]);
        let run = t.track_steps("ffmpeg", &f, 2.0, [100.0, 50.0]);
        let Step::Tool { args, .. } = &run[2] else { panic!("{run:?}") };
        assert!(args.iter().any(|a| a == "fps=30.000000,scale=512:288"), "{args:?}");
        let Step::Python { args, .. } = &run[3] else { panic!() };
        let frame = args.iter().position(|a| a == "--frame").unwrap();
        assert_eq!(args[frame + 1], "30");
    }

    #[test]
    fn tracks_come_back_in_native_pixels_at_file_times() {
        let line = r#"{"type": "track", "points": [[10.0, 20.0, true], [12.0, 20.0, false]]}"#;
        let tracked = Tracked::parse(line).unwrap();
        assert!(Tracked::parse(r#"{"type": "done", "path": ""}"#).is_none());
        assert_eq!(parse_device(r#"{"type": "device", "name": "CPU, 8 threads"}"#).as_deref(), Some("CPU, 8 threads"));
        assert_eq!(parse_device(line), None);
        assert_eq!(parse_at(r#"{"type": "at", "frame": 57, "x": 211.5, "y": 98.0}"#), Some((57, [211.5, 98.0])));
        assert_eq!(parse_at(line), None);
        assert_eq!(ffmpeg_frames("frame=  123 fps= 45 q=-0.0 size=..."), Some(123));
        assert_eq!(ffmpeg_frames("frame=9"), Some(9));
        assert_eq!(ffmpeg_frames("Stream mapping:"), None);
        let f = Footage::new(PathBuf::from("a.mp4"), 1.0, 1.0, 2, [1024.0, 576.0]);
        let s = tracked.samples(&f, [1024.0, 576.0]);
        assert_eq!(s[0], (1.0, [20.0, 40.0], true));
        assert_eq!(s[1], (1.5, [24.0, 40.0], false));
        let samples = to_samples(&s, |t, p| (t < 1.2).then_some((t + 10.0, p)));
        assert_eq!(samples, vec![Sample::new(11.0, [20.0, 40.0])]);
    }
}

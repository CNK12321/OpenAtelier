//! Automatic rotoscoping: SAM 2 (Meta's Segment Anything 2, Apache-2.0) finds an object's
//! outline on every frame from a few clicks on one.
//!
//! It runs in the AI engine's folder, on the Python and PyTorch the point tracker set up
//! (or sets them up itself first: [`Tracker::base_steps`]), and adds its own part:
//!
//! ```text
//! tracker/
//!   sam2/src/sam2-main/   SAM 2's code (its repository; the PyPI package only comes
//!                         as source that compiles an optional CUDA extension)
//!   sam2/<model>.pt       the models downloaded (each its own choice)
//!   oa_roto.py, SAM2_READY
//! ```
//!
//! A run hands ffmpeg's frames to the bridge (`oa_roto.py`) with the clicked points and
//! gets back one byte of coverage a pixel per frame ([`read_mattes`]).

use crate::engine::{Footage, Tracker};
use oa_captions::engine::Step;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The bridge script (see `oa_roto.py`).
pub const SCRIPT: &str = include_str!("oa_roto.py");
const READY: &str = "SAM2_READY";
const LAYOUT: &str = "sam2 1";
const SOURCE_URL: &str = "https://github.com/facebookresearch/sam2/archive/refs/heads/main.zip";
const MODELS_URL: &str = "https://dl.fbaipublicfiles.com/segment_anything_2/092824";
/// Left in its folder: what it is.
const NOTE: &str = "SAM 2 (github.com/facebookresearch/sam2), Apache-2.0: its code and the models downloaded.\n";

/// Frames go to SAM 2 at most this wide or tall (it works at 1024 inside; the matte
/// comes back at this size and is stretched over the clip).
pub const MAX_SIDE: u32 = 768;
/// At most this many frames per run (the frames file is about 1 MB a frame).
pub const MAX_FRAMES: usize = 900;

/// The SAM 2.1 models: bigger is more exact, slower and a bigger download.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SamModel {
    Tiny,
    Small,
    BasePlus,
    Large,
}

impl SamModel {
    pub const ALL: [SamModel; 4] = [SamModel::Tiny, SamModel::Small, SamModel::BasePlus, SamModel::Large];

    pub fn name(self) -> &'static str {
        match self {
            SamModel::Tiny => "Tiny",
            SamModel::Small => "Small",
            SamModel::BasePlus => "Base+",
            SamModel::Large => "Large",
        }
    }

    /// Its key in settings.
    pub fn key(self) -> &'static str {
        match self {
            SamModel::Tiny => "tiny",
            SamModel::Small => "small",
            SamModel::BasePlus => "base_plus",
            SamModel::Large => "large",
        }
    }

    pub fn from_key(key: &str) -> Option<SamModel> {
        SamModel::ALL.into_iter().find(|m| m.key() == key)
    }

    pub fn describe(self) -> &'static str {
        match self {
            SamModel::Tiny => "fastest; fine for clear subjects",
            SamModel::Small => "fast, a little more exact",
            SamModel::BasePlus => "a good balance",
            SamModel::Large => "most exact edges; slowest (a GPU helps)",
        }
    }

    /// The checkpoint's file name (and its name on Meta's server).
    pub fn file(self) -> String {
        format!("sam2.1_hiera_{}.pt", self.key())
    }

    /// Its model config, inside SAM 2's code.
    pub fn config(self) -> &'static str {
        match self {
            SamModel::Tiny => "configs/sam2.1/sam2.1_hiera_t.yaml",
            SamModel::Small => "configs/sam2.1/sam2.1_hiera_s.yaml",
            SamModel::BasePlus => "configs/sam2.1/sam2.1_hiera_b+.yaml",
            SamModel::Large => "configs/sam2.1/sam2.1_hiera_l.yaml",
        }
    }

    pub fn url(self) -> String {
        format!("{MODELS_URL}/{}", self.file())
    }

    /// About how big its download is, MB.
    pub fn download_mb(self) -> u32 {
        match self {
            SamModel::Tiny => 156,
            SamModel::Small => 184,
            SamModel::BasePlus => 323,
            SamModel::Large => 898,
        }
    }
}

/// A point clicked on the start frame: where (frame px) and whether it's the thing to
/// follow (`true`) or something to leave out.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Click {
    pub at: [f64; 2],
    pub include: bool,
}

/// SAM 2 in the AI engine's folder.
#[derive(Clone, Debug)]
pub struct Roto {
    tracker: Tracker,
}

impl Roto {
    pub fn new(tracker: Tracker) -> Self {
        Roto { tracker }
    }

    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    fn root(&self) -> &Path {
        self.tracker.root()
    }

    fn dir(&self) -> PathBuf {
        self.root().join("sam2")
    }

    fn source(&self) -> PathBuf {
        self.dir().join("src").join("sam2-main")
    }

    fn script(&self) -> PathBuf {
        self.root().join("oa_roto.py")
    }

    pub fn checkpoint(&self, model: SamModel) -> PathBuf {
        self.dir().join(model.file())
    }

    /// SAM 2's code is set up (models are each their own download).
    pub fn is_installed(&self) -> bool {
        self.tracker.has_base() && std::fs::read_to_string(self.root().join(READY)).is_ok_and(|s| s.trim() == LAYOUT) && self.source().join("sam2").is_dir()
    }

    pub fn has_model(&self, model: SamModel) -> bool {
        self.checkpoint(model).is_file()
    }

    /// The models downloaded.
    pub fn models(&self) -> Vec<SamModel> {
        SamModel::ALL.into_iter().filter(|m| self.has_model(*m)).collect()
    }

    /// Deletes a model's download.
    pub fn remove_model(&self, model: SamModel) -> std::io::Result<()> {
        std::fs::remove_file(self.checkpoint(model))
    }

    /// Deletes SAM 2 and its models (the shared Python stays for the tracker).
    pub fn remove(&self) -> std::io::Result<()> {
        let _ = std::fs::remove_file(self.root().join(READY));
        let _ = std::fs::remove_file(self.script());
        match std::fs::remove_dir_all(self.dir()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Everything to rotoscope with `model`: the shared Python and PyTorch for
    /// `compute` if they aren't there, SAM 2's code and what it needs, and the model.
    pub fn install_steps(&self, compute: crate::engine::Compute, model: SamModel) -> Vec<Step> {
        let os = |s: &str| OsString::from(s);
        let python = self.tracker.engine().python().into_os_string();
        let mut steps = if self.tracker.has_base() { Vec::new() } else { self.tracker.base_steps(compute) };
        // torchvision must match the PyTorch build: from the same index (while setting up
        // from nothing, the build just chosen).
        let index = if self.tracker.has_base() { self.tracker.torch_index() } else { compute.index() };
        let archive = self.dir().join("sam2-main.zip");
        steps.extend([
            Step::Uv { what: "torchvision".into(), args: vec![os("pip"), os("install"), os("--python"), python.clone(), os("torchvision"), os("--index-url"), os(index)] },
            Step::Uv {
                what: "what SAM 2 needs".into(),
                args: vec![os("pip"), os("install"), os("--python"), python, os("hydra-core>=1.3.2"), os("iopath>=0.1.10"), os("pillow>=9.4.0"), os("tqdm>=4.66.1")],
            },
            Step::Uv { what: "Tidying up".into(), args: vec![os("cache"), os("clean")] },
            // (Makes the folder: curl doesn't.)
            Step::Write { path: self.dir().join("README.txt"), text: NOTE.into() },
            Step::Download { what: "SAM 2".into(), url: SOURCE_URL.into(), to: archive.clone() },
            // Unpacked by Python, not tar: the repository has symbolic links (old config
            // names), which Windows' tar can't make from a zip; Python writes them as small
            // files, and they aren't used.
            Step::Python {
                what: "Unpacking SAM 2".into(),
                args: vec![
                    os("-c"),
                    OsString::from(format!(
                        "import os, shutil, zipfile; src = {:?}; shutil.rmtree(src, ignore_errors=True); z = zipfile.ZipFile({:?}); z.extractall(src); z.close(); os.remove({:?})",
                        self.dir().join("src").to_string_lossy(),
                        archive.to_string_lossy(),
                        archive.to_string_lossy()
                    )),
                ],
            },
            Step::Write { path: self.script(), text: SCRIPT.into() },
            Step::Python { what: "Checking SAM 2".into(), args: vec![self.script().into_os_string(), os("check"), os("--src"), self.source().into_os_string()] },
            Step::Write { path: self.root().join(READY), text: LAYOUT.into() },
        ]);
        steps.extend(self.model_steps(model));
        steps
    }

    /// Downloads `model` (if it isn't there yet).
    pub fn model_steps(&self, model: SamModel) -> Vec<Step> {
        if self.has_model(model) {
            return Vec::new();
        }
        // Downloaded aside and moved into place, so a canceled download isn't taken
        // for the model.
        let part = self.dir().join(format!("{}.part", model.file()));
        vec![
            Step::Write { path: self.dir().join("README.txt"), text: NOTE.into() },
            Step::Download { what: format!("the SAM 2.1 {} model", model.name()), url: model.url(), to: part.clone() },
            Step::Python {
                what: "Finishing".into(),
                args: vec![OsString::from("-c"), OsString::from(format!("import os; os.replace({:?}, {:?})", part.to_string_lossy(), self.checkpoint(model).to_string_lossy()))],
            },
        ]
    }

    /// The frames file and the result of a run.
    pub fn frames_path(&self) -> PathBuf {
        self.root().join("work").join("roto.rgb")
    }

    pub fn mattes_path(&self) -> PathBuf {
        self.root().join("work").join("roto.u8")
    }

    /// Cuts `footage` into frames, then finds the outline clicked on at file time `time`
    /// (`clicks` in the frames' px) through them with `model`.
    pub fn segment_steps(&self, ffmpeg: &str, footage: &Footage, time: f64, clicks: &[Click], model: SamModel) -> Vec<Step> {
        let frames = self.frames_path();
        let os = |s: String| OsString::from(s);
        let points: Vec<String> = clicks.iter().map(|c| format!("[{:.2}, {:.2}, {}]", c.at[0], c.at[1], c.include as u8)).collect();
        vec![
            Step::Write { path: self.script(), text: SCRIPT.into() },
            Step::Write { path: frames.clone(), text: String::new() },
            footage.read_step(ffmpeg, &frames),
            Step::Python {
                what: "Finding the outline".into(),
                args: vec![
                    self.script().into_os_string(),
                    os("segment".into()),
                    os("--src".into()),
                    self.source().into_os_string(),
                    os("--checkpoint".into()),
                    self.checkpoint(model).into_os_string(),
                    os("--config".into()),
                    os(model.config().into()),
                    os("--frames".into()),
                    frames.into_os_string(),
                    os("--width".into()),
                    os(footage.size[0].to_string()),
                    os("--height".into()),
                    os(footage.size[1].to_string()),
                    os("--frame".into()),
                    os(footage.frame_at(time).to_string()),
                    os("--points".into()),
                    os(format!("[{}]", points.join(", "))),
                    os("--out".into()),
                    self.mattes_path().into_os_string(),
                    os("--work".into()),
                    self.root().join("work").into_os_string(),
                ],
            },
        ]
    }
}

/// What a finished run said: the mattes' file, how many frames and their size.
#[derive(Clone, Debug, PartialEq)]
pub struct Mattes {
    pub path: PathBuf,
    pub frames: usize,
    pub size: [u32; 2],
}

impl Mattes {
    /// The bridge's `matte` line, if `line` is one.
    pub fn parse(line: &str) -> Option<Mattes> {
        #[derive(serde::Deserialize)]
        struct Line {
            r#type: String,
            path: String,
            frames: usize,
            width: u32,
            height: u32,
        }
        let l: Line = serde_json::from_str(line.trim()).ok()?;
        (l.r#type == "matte").then(|| Mattes { path: PathBuf::from(l.path), frames: l.frames, size: [l.width, l.height] })
    }

    /// Each frame's coverage, in order.
    pub fn read(&self) -> Result<Vec<Vec<u8>>, String> {
        let bytes = std::fs::read(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        let each = self.size[0] as usize * self.size[1] as usize;
        if each == 0 || bytes.len() < each * self.frames {
            return Err("the rotoscoped frames are incomplete".into());
        }
        Ok(bytes.chunks_exact(each).take(self.frames).map(<[u8]>::to_vec).collect())
    }
}

/// Which frame the bridge has just done (its `at` lines), for following along.
pub fn parse_at(line: &str) -> Option<usize> {
    #[derive(serde::Deserialize)]
    struct Line {
        r#type: String,
        frame: usize,
    }
    let l: Line = serde_json::from_str(line.trim()).ok()?;
    (l.r#type == "at").then_some(l.frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Compute;

    #[test]
    fn models_have_their_files_and_configs() {
        for m in SamModel::ALL {
            assert_eq!(SamModel::from_key(m.key()), Some(m));
            assert!(m.url().starts_with("https://dl.fbaipublicfiles.com/") && m.url().ends_with(".pt"));
            assert!(m.config().starts_with("configs/sam2.1/"));
        }
        assert_eq!(SamModel::BasePlus.file(), "sam2.1_hiera_base_plus.pt");
    }

    #[test]
    fn setup_adds_to_the_shared_python_and_runs_hand_frames_over() {
        let root = std::env::temp_dir().join(format!("oa-roto-steps-{}", std::process::id()));
        let roto = Roto::new(Tracker::new(root.clone()));
        assert!(!roto.is_installed() && roto.models().is_empty());
        let steps = roto.install_steps(Compute::Cpu, SamModel::Tiny);
        // Nothing shared yet: the base first (uv's download), then SAM 2, then the model.
        assert!(matches!(&steps[0], Step::Download { url, .. } if url.contains("astral-sh/uv")));
        assert!(steps.iter().any(|s| matches!(s, Step::Download { url, .. } if url == SOURCE_URL)));
        assert!(matches!(steps.iter().rev().nth(1), Some(Step::Download { url, .. }) if url.ends_with("sam2.1_hiera_tiny.pt")));
        assert!(steps.iter().any(|s| matches!(s, Step::Uv { args, .. } if args.iter().any(|a| a == "torchvision") && args.iter().any(|a| a == "https://download.pytorch.org/whl/cpu"))));
        let f = Footage::fit(PathBuf::from("clip.mp4"), 1.0, 2.0, 60, [1920.0, 1080.0], MAX_SIDE, MAX_FRAMES);
        assert_eq!(f.size, [768, 432]);
        let run = roto.segment_steps("ffmpeg", &f, 2.0, &[Click { at: [10.0, 20.0], include: true }, Click { at: [5.5, 6.0], include: false }], SamModel::Small);
        let Step::Python { args, .. } = &run[3] else { panic!("{run:?}") };
        let arg = |name: &str| args[args.iter().position(|a| a == name).unwrap() + 1].to_string_lossy().to_string();
        assert_eq!(arg("--points"), "[[10.00, 20.00, 1], [5.50, 6.00, 0]]");
        assert_eq!(arg("--frame"), "30");
        assert_eq!(arg("--config"), "configs/sam2.1/sam2.1_hiera_s.yaml");
        assert!(arg("--checkpoint").ends_with("sam2.1_hiera_small.pt"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn results_are_read_frame_by_frame() {
        let path = std::env::temp_dir().join(format!("oa-roto-mattes-{}.u8", std::process::id()));
        std::fs::write(&path, [0u8, 255, 255, 0, 9, 9]).unwrap();
        let line = format!(r#"{{"type": "matte", "path": {:?}, "frames": 3, "width": 2, "height": 1}}"#, path.to_string_lossy());
        let m = Mattes::parse(&line).unwrap();
        assert_eq!(m.read().unwrap(), vec![vec![0, 255], vec![255, 0], vec![9, 9]]);
        assert_eq!(parse_at(r#"{"type": "at", "frame": 7}"#), Some(7));
        assert_eq!(parse_at(&line), None);
        let short = Mattes { frames: 4, ..m };
        assert!(short.read().is_err());
        let _ = std::fs::remove_file(&path);
    }
}

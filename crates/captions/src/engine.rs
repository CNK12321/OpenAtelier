//! The caption engine: faster-whisper in a private Python, downloaded on request.
//!
//! Everything lives in one folder ([`Engine::root`], `%LOCALAPPDATA%/OpenAtelier/captions`
//! by default, `OA_CAPTIONS_DIR` to move it) and nothing is installed anywhere else:
//!
//! ```text
//! captions/
//!   uv/           uv, Astral's Python installer (one small download)
//!   python/       the Python uv fetched for us
//!   env/          a virtual environment with faster-whisper in it
//!   hf/           Hugging Face's cache (while a model downloads)
//!   models/<name> the Whisper models, converted for faster-whisper
//!   oa_captions.py, READY
//! ```
//!
//! Setting up is a list of [`Step`]s run in order by [`run`] on a worker thread, which
//! streams what's happening back as [`Msg`]s; so is a transcription. "Remove" deletes the
//! folder, and that's the whole uninstall.

use crate::Event;
use std::ffi::OsString;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// The bridge script the engine runs (see `oa_captions.py`).
pub const SCRIPT: &str = include_str!("oa_captions.py");
/// Written last during setup: the engine is complete. Its contents name the layout, so
/// a future change can tell an old install apart.
const READY: &str = "READY";
const LAYOUT: &str = "faster-whisper engine 1";
const PYTHON_VERSION: &str = "3.12";

/// The engine's folder and what's in it.
#[derive(Clone, Debug)]
pub struct Engine {
    root: PathBuf,
}

impl Engine {
    pub fn new(root: PathBuf) -> Self {
        Engine { root }
    }

    /// `OA_CAPTIONS_DIR`, else `%LOCALAPPDATA%/OpenAtelier/captions` (the platform's data
    /// folder elsewhere).
    pub fn default_root() -> PathBuf {
        if let Some(dir) = std::env::var_os("OA_CAPTIONS_DIR") {
            return PathBuf::from(dir);
        }
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        // The app folder (as `oa_media::app_dir`, which this crate doesn't depend on).
        let base = if cfg!(windows) {
            var("LOCALAPPDATA")
        } else if cfg!(target_os = "macos") {
            var("HOME").map(|h| h.join("Library").join("Application Support"))
        } else {
            var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local").join("share")))
        }
        .unwrap_or_else(std::env::temp_dir);
        base.join("OpenAtelier").join("captions")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Set up completely (a failed or canceled setup doesn't count).
    pub fn is_installed(&self) -> bool {
        std::fs::read_to_string(self.root.join(READY)).is_ok_and(|s| s.trim() == LAYOUT) && self.python().exists()
    }

    pub fn has_model(&self, name: &str) -> bool {
        self.model_dir(name).join("model.bin").exists()
    }

    /// The models downloaded so far.
    pub fn models(&self) -> Vec<String> {
        crate::MODELS.iter().map(|m| m.name.to_string()).filter(|m| self.has_model(m)).collect()
    }

    fn model_dir(&self, name: &str) -> PathBuf {
        self.root.join("models").join(name)
    }

    /// The environment's Python.
    pub fn python(&self) -> PathBuf {
        if cfg!(windows) { self.root.join("env").join("Scripts").join("python.exe") } else { self.root.join("env").join("bin").join("python") }
    }

    /// uv, wherever its archive put it under `uv/`.
    fn uv(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { "uv.exe" } else { "uv" };
        find(&self.root.join("uv"), name, 3)
    }

    /// How much the engine takes on disk, in bytes.
    pub fn disk_usage(&self) -> u64 {
        fn walk(p: &Path) -> u64 {
            let Ok(entries) = std::fs::read_dir(p) else { return 0 };
            entries
                .flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&e.path()),
                    Ok(_) => e.metadata().map_or(0, |m| m.len()),
                    Err(_) => 0,
                })
                .sum()
        }
        walk(&self.root)
    }

    /// Deletes the engine and every model: back to how it was before setup.
    pub fn remove(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(&self.root) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Everything setup does, then `model`'s download.
    pub fn install_steps(&self, model: &str) -> Vec<Step> {
        let archive = self.root.join(uv_asset());
        let mut steps = vec![
            Step::Download { what: "uv, the Python installer".into(), url: format!("https://github.com/astral-sh/uv/releases/latest/download/{}", uv_asset()), to: archive.clone() },
            Step::Extract { archive, into: self.root.join("uv") },
            Step::Uv { what: format!("Python {PYTHON_VERSION}"), args: args(["venv", "--clear", "--python", PYTHON_VERSION]).chain([self.root.join("env").into_os_string()]).collect() },
            Step::Uv { what: "faster-whisper and what it needs".into(), args: args(["pip", "install", "--python"]).chain([self.python().into_os_string()]).chain(args(["faster-whisper"])).collect() },
            // The downloaded packages aren't needed once they're installed.
            Step::Uv { what: "Tidying up".into(), args: args(["cache", "clean"]).collect() },
        ];
        steps.extend(self.model_steps(model));
        steps.push(Step::Mark);
        steps
    }

    /// Downloads `model` into `models/<model>`.
    pub fn model_steps(&self, model: &str) -> Vec<Step> {
        vec![
            self.script_step(),
            Step::Python {
                what: format!("the {model} model"),
                args: vec![
                    self.root.join("oa_captions.py").into_os_string(),
                    "download".into(),
                    "--model".into(),
                    model.into(),
                    "--models".into(),
                    self.root.join("models").into_os_string(),
                ],
            },
        ]
    }

    /// Transcribes `audio` (a WAV) with `model`; `language` empty to detect it.
    pub fn transcribe_steps(&self, model: &str, audio: &Path, language: &str) -> Vec<Step> {
        let mut a: Vec<OsString> = vec![
            self.root.join("oa_captions.py").into_os_string(),
            "transcribe".into(),
            "--model".into(),
            model.into(),
            "--models".into(),
            self.root.join("models").into_os_string(),
            "--audio".into(),
            audio.as_os_str().to_owned(),
        ];
        if !language.trim().is_empty() {
            a.extend(["--language".into(), language.trim().into()]);
        }
        vec![self.script_step(), Step::Python { what: "Listening".into(), args: a }]
    }

    fn script_step(&self) -> Step {
        Step::Write { path: self.root.join("oa_captions.py"), text: SCRIPT.into() }
    }
}

fn args<const N: usize>(a: [&str; N]) -> impl Iterator<Item = OsString> {
    a.into_iter().map(OsString::from)
}

/// uv's release archive for this platform.
pub fn uv_asset() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "aarch64") => "uv-aarch64-pc-windows-msvc.zip",
        ("windows", _) => "uv-x86_64-pc-windows-msvc.zip",
        ("macos", "aarch64") => "uv-aarch64-apple-darwin.tar.gz",
        ("macos", _) => "uv-x86_64-apple-darwin.tar.gz",
        (_, "aarch64") => "uv-aarch64-unknown-linux-gnu.tar.gz",
        _ => "uv-x86_64-unknown-linux-gnu.tar.gz",
    }
}

/// A file named `name` under `dir`, at most `depth` folders down.
fn find(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_dir()).find_map(|e| find(&e.path(), name, depth - 1))
}

/// One thing setup or transcription does.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Fetches `url` into `to` (with curl, which Windows 10 and later ship with).
    Download { what: String, url: String, to: PathBuf },
    /// Unpacks a .zip or .tar.gz (with tar, likewise built in) and deletes the archive.
    Extract { archive: PathBuf, into: PathBuf },
    /// Runs uv, confined to the engine's folder.
    Uv { what: String, args: Vec<OsString> },
    /// Runs the environment's Python.
    Python { what: String, args: Vec<OsString> },
    /// Runs another program (ffmpeg, say) the same way, its output streamed.
    Tool { what: String, program: OsString, args: Vec<OsString> },
    Write { path: PathBuf, text: String },
    /// Marks the engine complete.
    Mark,
}

impl Step {
    /// What the UI says while it runs.
    pub fn label(&self) -> String {
        match self {
            Step::Download { what, .. } => format!("Downloading {what}"),
            Step::Extract { .. } => "Unpacking".into(),
            Step::Uv { what, .. } if what == "Tidying up" => what.clone(),
            Step::Uv { what, .. } | Step::Python { what, .. } if what.starts_with("the ") => format!("Downloading {what}"),
            Step::Uv { what, .. } => format!("Installing {what}"),
            Step::Python { what, .. } | Step::Tool { what, .. } => what.clone(),
            Step::Write { .. } => "Preparing".into(),
            Step::Mark => "Finishing".into(),
        }
    }
}

/// What a running job reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    /// Step `index` (of the job's steps) starts.
    Step { index: usize, label: String },
    /// A line of a tool's output, for the log.
    Log(String),
    /// A percentage seen in a tool's progress output (0–1).
    Progress(f32),
    /// Something the bridge script reported.
    Event(Event),
    /// A JSON line that isn't a caption event: another bridge's (the tracker's) output.
    Data(String),
    Failed(String),
    Finished,
}

/// A job running on a worker thread.
pub struct Job {
    pub rx: Receiver<Msg>,
    pub steps: usize,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Job {
    /// Stops it: the running tool is killed, and no further steps start.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = child.kill();
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Runs `steps` in order on a worker thread.
pub fn run(engine: &Engine, steps: Vec<Step>) -> Job {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let job = Job { rx, steps: steps.len(), cancel: cancel.clone(), child: child.clone() };
    let engine = engine.clone();
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            std::fs::create_dir_all(engine.root()).map_err(|e| format!("{}: {e}", engine.root().display()))?;
            for (index, step) in steps.into_iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Canceled.".into());
                }
                let _ = tx.send(Msg::Step { index, label: step.label() });
                execute(&engine, step, &tx, &child)?;
            }
            Ok(())
        })();
        let _ = tx.send(match result {
            Ok(()) => Msg::Finished,
            Err(_) if cancel.load(Ordering::SeqCst) => Msg::Failed("Canceled.".into()),
            Err(e) => Msg::Failed(e),
        });
    });
    job
}

fn execute(engine: &Engine, step: Step, tx: &Sender<Msg>, child: &Arc<Mutex<Option<Child>>>) -> Result<(), String> {
    let root = engine.root();
    match step {
        Step::Download { url, to, .. } => {
            let mut c = Command::new("curl");
            // `--progress-bar` writes "####  23.4%" lines to stderr: the window's progress.
            c.args(["-L", "--fail", "--show-error", "--progress-bar", "--retry", "2", "-o"]).arg(&to).arg(&url);
            spawn(c, tx, child).map_err(|e| format!("couldn't download {url}: {e}"))
        }
        Step::Extract { archive, into } => {
            std::fs::create_dir_all(&into).map_err(|e| e.to_string())?;
            let mut c = Command::new("tar");
            c.arg("-xf").arg(&archive).arg("-C").arg(&into);
            spawn(c, tx, child).map_err(|e| format!("couldn't unpack {}: {e}", archive.display()))?;
            let _ = std::fs::remove_file(&archive);
            Ok(())
        }
        Step::Uv { args, .. } => {
            let uv = engine.uv().ok_or("uv isn't there — set the engine up again")?;
            let mut c = Command::new(uv);
            c.args(&args)
                .env("UV_PYTHON_INSTALL_DIR", root.join("python"))
                .env("UV_CACHE_DIR", root.join("cache"))
                .env("UV_PYTHON_PREFERENCE", "only-managed")
                .env("UV_NO_PROGRESS", "1");
            spawn(c, tx, child)
        }
        Step::Python { args, .. } => {
            let python = engine.python();
            if !python.exists() {
                return Err("the engine isn't set up".into());
            }
            let mut c = Command::new(python);
            c.args(&args)
                .env("HF_HOME", root.join("hf"))
                .env("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
                .env("TORCH_HOME", root.join("torch"))
                .env("PYTHONUNBUFFERED", "1")
                .env("PYTHONIOENCODING", "utf-8");
            spawn(c, tx, child)
        }
        Step::Tool { program, args, .. } => {
            let mut c = Command::new(program);
            c.args(&args);
            spawn(c, tx, child)
        }
        Step::Write { path, text } => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
        }
        Step::Mark => std::fs::write(root.join(READY), LAYOUT).map_err(|e| e.to_string()),
    }
}

/// Runs a tool to the end, passing its output on: stdout lines that are events as
/// events, everything else (and all of stderr, split on `\r` too, for progress bars) as
/// log lines, and any percentage in them as progress.
fn spawn(mut command: Command, tx: &Sender<Msg>, slot: &Arc<Mutex<Option<Child>>>) -> Result<(), String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);

    let err_tx = tx.clone();
    let tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let err_tail = tail.clone();
    let errors = std::thread::spawn(move || {
        let Some(mut stderr) = stderr else { return };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let flush = |line: &[u8]| {
            let line = String::from_utf8_lossy(line).trim().to_string();
            if line.is_empty() {
                return;
            }
            if let Some(p) = percent(&line) {
                let _ = err_tx.send(Msg::Progress(p));
            }
            let mut t = err_tail.lock().unwrap_or_else(|e| e.into_inner());
            t.push(line.clone());
            if t.len() > 8 {
                t.remove(0);
            }
            let _ = err_tx.send(Msg::Log(line));
        };
        while let Ok(n) = stderr.read(&mut chunk) {
            if n == 0 {
                break;
            }
            for &b in &chunk[..n] {
                if b == b'\n' || b == b'\r' {
                    flush(&buf);
                    buf.clear();
                } else {
                    buf.push(b);
                }
            }
        }
        flush(&buf);
    });

    let mut reported = None;
    if let Some(stdout) = stdout {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            match Event::parse(&line) {
                Some(Event::Error { message }) => reported = Some(message),
                Some(event) => {
                    let _ = tx.send(Msg::Event(event));
                }
                None if line.trim_start().starts_with('{') => {
                    let _ = tx.send(Msg::Data(line));
                }
                None if !line.trim().is_empty() => {
                    let _ = tx.send(Msg::Log(line));
                }
                None => {}
            }
        }
    }
    let _ = errors.join();
    let status = {
        let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        let status = guard.as_mut().map(|c| c.wait());
        *guard = None;
        status
    };
    match status {
        Some(Ok(s)) if s.success() => Ok(()),
        _ => Err(reported.unwrap_or_else(|| {
            let t = tail.lock().unwrap_or_else(|e| e.into_inner());
            if t.is_empty() { "it stopped with an error".into() } else { t.join("\n") }
        })),
    }
}

/// The last "NN%" (or "NN.N%") in a progress line, as 0–1.
fn percent(line: &str) -> Option<f32> {
    let bytes = line.as_bytes();
    let at = line.rfind('%')?;
    let len = bytes[..at].iter().rev().take_while(|b| b.is_ascii_digit() || **b == b'.').count();
    if len == 0 || len > 6 {
        return None;
    }
    line[at - len..at].parse::<f32>().ok().filter(|p| *p <= 100.0).map(|p| p / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> Engine {
        let dir = std::env::temp_dir().join(format!("oa-captions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Engine::new(dir)
    }

    fn drain(job: Job) -> Vec<Msg> {
        let mut out = Vec::new();
        while let Ok(m) = job.rx.recv_timeout(std::time::Duration::from_secs(20)) {
            let end = matches!(m, Msg::Finished | Msg::Failed(_));
            out.push(m);
            if end {
                break;
            }
        }
        out
    }

    /// Setup downloads uv and fetches everything else through it, all inside the
    /// engine's folder; the engine only counts as installed once it's all done.
    #[test]
    fn setup_stays_in_its_folder() {
        let e = temp("steps");
        let steps = e.install_steps("small");
        assert!(matches!(&steps[0], Step::Download { url, .. } if url.contains("astral-sh/uv") && url.ends_with(uv_asset())));
        assert_eq!(steps.last(), Some(&Step::Mark));
        for s in &steps {
            let paths: Vec<PathBuf> = match s {
                Step::Download { to, .. } => vec![to.clone()],
                Step::Extract { archive, into } => vec![archive.clone(), into.clone()],
                Step::Write { path, .. } => vec![path.clone()],
                _ => vec![],
            };
            assert!(paths.iter().all(|p| p.starts_with(e.root())), "{s:?}");
        }
        assert!(steps.iter().any(|s| matches!(s, Step::Python { args, .. } if args.iter().any(|a| a == "download"))));
        assert!(!e.is_installed());
        assert!(e.models().is_empty());
        assert_eq!(steps[2].label(), "Installing Python 3.12");
    }

    /// Steps run in order and report; a missing tool fails the job with a reason; the
    /// folder can be removed whole.
    #[test]
    fn runs_steps_and_reports_failures() {
        let e = temp("run");
        let msgs = drain(run(&e, vec![Step::Write { path: e.root().join("x.txt"), text: "hi".into() }, Step::Mark]));
        assert_eq!(msgs.last(), Some(&Msg::Finished), "{msgs:?}");
        // Into a folder that isn't there yet (the tracker's work folder, first run).
        let deep = e.root().join("work").join("frames.rgb");
        let msgs = drain(run(&e, vec![Step::Write { path: deep.clone(), text: String::new() }]));
        assert_eq!(msgs.last(), Some(&Msg::Finished), "{msgs:?}");
        assert!(deep.exists());
        assert!(matches!(&msgs[0], Msg::Step { index: 0, .. }));
        assert_eq!(std::fs::read_to_string(e.root().join("x.txt")).unwrap(), "hi");
        assert!(!e.is_installed(), "marked, but there's no Python yet");

        let msgs = drain(run(&e, vec![Step::Uv { what: "x".into(), args: vec![] }]));
        assert!(matches!(msgs.last(), Some(Msg::Failed(m)) if m.contains("uv")), "{msgs:?}");
        let msgs = drain(run(&e, e.transcribe_steps("small", Path::new("a.wav"), "")));
        assert!(matches!(msgs.last(), Some(Msg::Failed(m)) if m.contains("isn't set up")), "{msgs:?}");

        e.remove().unwrap();
        assert!(!e.root().exists());
        e.remove().unwrap();
    }

    /// Tools' output comes through: a real command's stdout events and stderr lines.
    #[test]
    fn tool_output_is_streamed() {
        let (tx, rx) = channel();
        let slot = Arc::new(Mutex::new(None));
        // A stand-in tool: prints an event to stdout and a progress line to stderr.
        let dir = temp("tool").root().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let (out, err) = (dir.join("out.txt"), dir.join("err.txt"));
        std::fs::write(&out, "{\"type\": \"info\", \"language\": \"en\", \"duration\": 2.0}\n").unwrap();
        std::fs::write(&err, "42% done\n").unwrap();
        #[cfg(windows)]
        let c = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("cmd");
            c.raw_arg(format!("/C type \"{}\" & type \"{}\" 1>&2", out.display(), err.display()));
            c
        };
        #[cfg(not(windows))]
        let c = {
            let mut c = Command::new("sh");
            c.arg("-c").arg(format!("cat '{}'; cat '{}' 1>&2", out.display(), err.display()));
            c
        };
        spawn(c, &tx, &slot).expect("runs");
        drop(tx);
        let msgs: Vec<Msg> = rx.iter().collect();
        assert!(msgs.contains(&Msg::Event(Event::Info { language: "en".into(), duration: 2.0 })), "{msgs:?}");
        assert!(msgs.contains(&Msg::Progress(0.42)), "{msgs:?}");
    }

    #[test]
    fn percentages() {
        assert_eq!(percent("model.bin:  45%|####      | 220M/484M"), Some(0.45));
        assert_eq!(percent("100%"), Some(1.0));
        assert_eq!(percent("######################                          23.5%"), Some(0.235));
        assert_eq!(percent("no progress here"), None);
        assert_eq!(percent("5000%"), None);
    }
}

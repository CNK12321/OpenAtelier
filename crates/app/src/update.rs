//! Updates from GitHub: the app reads the repository's releases (the GitHub API, through
//! `curl`, the same way the engines download), tells the user when a newer version than
//! the one running is out, and — with one button — downloads the package for this
//! platform, checks it against the release's `SHA256SUMS.txt`, puts it in place next to
//! the running app and restarts into it.
//!
//! * Versions are semantic (`0.1.0-beta.2`); the **Beta** channel also offers pre-releases,
//!   **Stable** only full releases. Beta builds start on the Beta channel.
//! * Checked at every start and every few hours while open (Settings → Updates: on/off,
//!   channel, Check now).
//! * Installing replaces files in the app's own folder: a file in use (the running
//!   program) is renamed aside (`*.old`) first — Windows allows that — and the leftovers
//!   are cleared at the next start. Installed from the `.deb`, it installs the new `.deb`
//!   with apt behind the system's password prompt (`pkexec`). A build run from a
//!   `target` folder (a developer's), or an install the app can't write to, gets a link
//!   to the download page instead.
//! * News comes as a dialog: "Beta 5 is available!", the release notes' points
//!   (`release_features`), **Restart and Update Now** or **Later**.
//!
//! Packages are what `.github/workflows/release.yml` makes:
//! `OpenAtelier-<version>-<platform>.zip|.tar.gz`, one folder inside with the programs.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_captions::engine::{Engine, Job, Msg, Step};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

/// Where releases come from (`OA_UPDATE_REPO` overrides, for forks and testing).
pub const REPO: &str = "CNK12321/OpenAtelier";
/// The version running.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A version as people see it (and as releases are tagged): a patch of 0 left off,
/// `0.1.0-beta.5` → `0.1-beta.5`. Cargo needs all three parts; the tags don't.
pub fn short_version(v: &str) -> String {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let core = core.strip_suffix(".0").filter(|c| c.contains('.')).unwrap_or(core);
    match pre {
        Some(p) => format!("{core}-{p}"),
        None => core.to_string(),
    }
}
/// The checksum file every release carries.
const SUMS: &str = "SHA256SUMS.txt";

fn repo() -> String {
    std::env::var("OA_UPDATE_REPO").ok().filter(|r| r.contains('/')).unwrap_or_else(|| REPO.to_string())
}

// ---- versions ----

/// A semantic version: `1.2.3`, `0.1.0-beta.2` (build metadata ignored).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub core: [u64; 3],
    /// Pre-release identifiers (`beta`, `2`); empty for a release.
    pub pre: Vec<Ident>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ident {
    Num(u64),
    Text(String),
}

impl Version {
    /// From `v0.1.0-beta.2`, `0.1.0`, `1.2` (missing parts are 0).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = [0u64; 3];
        let parts: Vec<&str> = core.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        for (i, p) in parts.iter().enumerate() {
            nums[i] = p.parse().ok()?;
        }
        let pre = match pre {
            Some(p) if !p.is_empty() => p.split('.').map(|id| id.parse().map(Ident::Num).unwrap_or_else(|_| Ident::Text(id.to_string()))).collect(),
            _ => Vec::new(),
        };
        Some(Version { core: nums, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    /// Semantic-versioning precedence: the numbers, then a release above any of its
    /// pre-releases, then pre-release identifiers left to right (numbers below words,
    /// numbers by value, words alphabetically, fewer identifiers first).
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    let o = match (a, b) {
                        (Ident::Num(x), Ident::Num(y)) => x.cmp(y),
                        (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
                        (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
                        (Ident::Text(x), Ident::Text(y)) => x.cmp(y),
                    };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ---- releases ----

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub name: String,
    pub prerelease: bool,
    /// The release's page on GitHub.
    pub page: String,
    pub notes: String,
    pub assets: Vec<Asset>,
}

/// The releases in GitHub's `/releases` answer (drafts and tags that aren't versions left
/// out).
pub fn parse_releases(json: &str) -> Result<Vec<Release>, String> {
    #[derive(serde::Deserialize)]
    struct R {
        tag_name: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        html_url: String,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        assets: Vec<A>,
    }
    #[derive(serde::Deserialize)]
    struct A {
        name: String,
        browser_download_url: String,
        #[serde(default)]
        size: u64,
    }
    let list: Vec<R> = serde_json::from_str(json).map_err(|e| {
        // GitHub answers errors (rate limits…) as an object with a message.
        serde_json::from_str::<serde_json::Value>(json).ok().and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string)).unwrap_or_else(|| e.to_string())
    })?;
    Ok(list
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = Version::parse(&r.tag_name)?;
            Some(Release {
                name: r.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| r.tag_name.clone()),
                prerelease: r.prerelease || version.is_prerelease(),
                tag: r.tag_name,
                version,
                page: r.html_url,
                notes: r.body.unwrap_or_default(),
                assets: r.assets.into_iter().map(|a| Asset { name: a.name, url: a.browser_download_url, size: a.size }).collect(),
            })
        })
        .collect())
}

/// The newest release on the channel (`beta`: pre-releases too) that has a package for
/// this platform — `None` when nothing is newer than `current`.
pub fn newer<'a>(releases: &'a [Release], current: &Version, beta: bool) -> Option<&'a Release> {
    releases
        .iter()
        .filter(|r| beta || !r.prerelease)
        .filter(|r| package_for(r, platform()).is_some())
        .max_by(|a, b| a.version.cmp(&b.version))
        .filter(|r| r.version > *current)
}

/// This platform's name in package file names.
pub fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "aarch64") => "windows-arm64",
        ("windows", _) => "windows-x64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", _) => "macos-x64",
        (_, "aarch64") => "linux-arm64",
        _ => "linux-x64",
    }
}

/// The package in `release` for `platform` (`OpenAtelier-<v>-<platform>.zip|.tar.gz`).
pub fn package_for<'a>(release: &'a Release, platform: &str) -> Option<&'a Asset> {
    release.assets.iter().find(|a| a.name.contains(platform) && (a.name.ends_with(".zip") || a.name.ends_with(".tar.gz")))
}

/// The checksum listed for `file` in a `sha256sum`-style file ("<hex>  <name>").
pub fn checksum_in(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == file && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

/// A file's SHA-256, as lowercase hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ---- installing ----

/// How this copy updates itself (in the folder it runs from, which the variants carry).
#[derive(Clone, Debug, PartialEq)]
pub enum Method {
    /// The new files put in place of the old ones: the Windows install, an unpacked zip
    /// or tarball, `install.sh`'s `~/.local`.
    Folder(PathBuf),
    /// Installed from the `.deb` (apt owns `/opt/openatelier`): the new `.deb`, installed
    /// by apt behind the system's password prompt (`pkexec`).
    Deb(PathBuf),
}

impl Method {
    pub fn dir(&self) -> &Path {
        match self {
            Method::Folder(d) | Method::Deb(d) => d,
        }
    }
}

/// How the running app can update itself, if it can: not a developer build (run from a
/// `target` folder), and either installed by the `.deb` or in a folder it can write to.
/// Worked out once (it writes a file to find out), not on every frame the dialog shows.
pub fn install_method() -> Option<Method> {
    static METHOD: std::sync::OnceLock<Option<Method>> = std::sync::OnceLock::new();
    METHOD.get_or_init(find_install_method).clone()
}

fn find_install_method() -> Option<Method> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    if dir.components().any(|c| c.as_os_str() == "target") {
        return None;
    }
    if cfg!(target_os = "linux") && dpkg_owns(&exe) {
        return Some(Method::Deb(dir));
    }
    let probe = dir.join(".oa-write-test");
    std::fs::write(&probe, b"").ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(Method::Folder(dir))
}

/// Whether `exe` is one of the files the `openatelier` package installed.
fn dpkg_owns(exe: &Path) -> bool {
    let Ok(list) = std::fs::read_to_string("/var/lib/dpkg/info/openatelier.list") else { return false };
    let exe = exe.to_string_lossy();
    list.lines().any(|l| l == exe)
}

/// The folder the running app can replace its own files in (not a `.deb` install's).
pub fn install_dir() -> Option<PathBuf> {
    match install_method()? {
        Method::Folder(dir) => Some(dir),
        Method::Deb(_) => None,
    }
}

/// The file in `release` that `method` installs: the `.deb`, or the zip/tarball.
pub fn package_for_method<'a>(release: &'a Release, method: &Method) -> Option<&'a Asset> {
    match method {
        Method::Deb(_) => release.assets.iter().find(|a| a.name.ends_with(&format!("-{}.deb", platform()))),
        Method::Folder(_) => package_for(release, platform()),
    }
}

/// The program to start after updating, in `dir`.
pub fn main_program(dir: &Path) -> PathBuf {
    dir.join(if cfg!(windows) { "OpenAtelier.exe" } else { "openatelier" })
}

/// Copies everything under `from` (the unpacked package's folder) into `to`, replacing
/// what's there: a file that's there is renamed aside first (`name.old`, or `.old2`… if
/// an older one is still in use), since a running program can be renamed but not
/// overwritten on Windows. Returns how many files were put in place.
pub fn put_in_place(from: &Path, to: &Path) -> std::io::Result<usize> {
    let mut n = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&dst)?;
            n += put_in_place(&src, &dst)?;
            continue;
        }
        if dst.exists() {
            let aside = (1..100)
                .map(|i| {
                    let mut name = dst.file_name().unwrap_or_default().to_os_string();
                    name.push(if i == 1 { ".old".to_string() } else { format!(".old{i}") });
                    dst.with_file_name(name)
                })
                .find(|p| !p.exists() || std::fs::remove_file(p).is_ok())
                .ok_or_else(|| std::io::Error::other(format!("{} is in use", dst.display())))?;
            std::fs::rename(&dst, &aside)?;
        }
        std::fs::copy(&src, &dst)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&src)?.permissions().mode();
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))?;
        }
        n += 1;
    }
    Ok(n)
}

/// Clears what a previous update set aside (`*.old`, `*.old2`…), where it can.
pub fn clean_up(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let old = name.rsplit_once(".old").is_some_and(|(_, rest)| rest.chars().all(|c| c.is_ascii_digit()));
        if e.path().is_dir() {
            clean_up(&e.path());
        } else if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The single folder a package unpacks to (or the staging folder itself).
fn package_root(staging: &Path) -> PathBuf {
    let dirs: Vec<PathBuf> = std::fs::read_dir(staging).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    match dirs.as_slice() {
        [only] if only.is_dir() => only.clone(),
        _ => staging.to_path_buf(),
    }
}

// ---- the app's side ----

/// Where an update stands.
#[derive(Default)]
pub struct Updater {
    checking: Option<Receiver<Result<Vec<Release>, String>>>,
    /// What the last check said (for Settings).
    pub last: Option<Result<String, String>>,
    /// A newer release for this channel.
    pub available: Option<Release>,
    /// "Later" for this release (this session).
    dismissed: Option<String>,
    install: Option<Install>,
    /// Installed; the program to start when the app closes (Restart now).
    installed: bool,
    relaunch: Option<PathBuf>,
    /// "Restart and Update Now": restart as soon as it's installed.
    restart_when_done: bool,
}

enum Install {
    /// Downloading the package and the checksums.
    Download { job: Job, step: String, progress: Option<f32>, archive: PathBuf, sums: PathBuf, method: Method },
    /// Checking, unpacking and putting in place, on a thread.
    Place(Receiver<Result<usize, String>>),
    Failed(String),
}

fn workdir() -> PathBuf {
    oa_media::app_dir().join("updates")
}

fn fetch_releases() -> Result<Vec<Release>, String> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page=30", repo());
    let out = oa_media::tool("curl")
        .args(["-sS", "-L", "--fail-with-body", "--max-time", "20", "-H", "Accept: application/vnd.github+json"])
        .arg("-H")
        .arg(format!("User-Agent: OpenAtelier/{VERSION}"))
        .arg(&url)
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    let body = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let why = parse_releases(&body).err().unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
        return Err(if why.is_empty() { tr("GitHub didn't answer").into() } else { why });
    }
    parse_releases(&body)
}

/// Starting again within this many seconds of a check doesn't check again.
const START_CHECK_GAP: u64 = 10 * 60;
/// While the app is open, it checks again this often (seconds).
const RUNNING_CHECK_GAP: u64 = 6 * 3600;

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl App {
    /// Starting up: clear what the last update left behind, and check — at every start
    /// (the settings are shared by every copy, so a daily limit let one copy's check
    /// hide a release from another), just not twice within a few minutes.
    pub(crate) fn start_updates(&mut self) {
        if let Some(dir) = install_dir() {
            clean_up(&dir);
        }
        let _ = std::fs::remove_dir_all(workdir());
        self.check_if_due(START_CHECK_GAP);
    }

    /// Checks again when the last check is older than `gap` seconds (and checking is on).
    fn check_if_due(&mut self, gap: u64) {
        if self.settings.check_updates && self.script.is_none() && self.updater.checking.is_none() && now_secs().saturating_sub(self.settings.update_checked_at) > gap {
            self.check_for_updates();
        }
    }

    /// Asks GitHub for the releases (on a thread).
    pub(crate) fn check_for_updates(&mut self) {
        if self.updater.checking.is_some() {
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_releases());
        });
        self.updater.checking = Some(rx);
    }

    fn beta_channel(&self) -> bool {
        self.settings.update_channel == "beta"
    }

    /// Every frame: answers from GitHub, and the install's progress.
    pub(crate) fn poll_updates(&mut self, ctx: &egui::Context) {
        // Left open for hours, it looks again; once something is offered, there's no need.
        if self.updater.available.is_none() {
            self.check_if_due(RUNNING_CHECK_GAP);
        }
        if let Some(rx) = &self.updater.checking {
            match rx.try_recv() {
                Ok(result) => {
                    self.updater.checking = None;
                    self.settings.update_checked_at = now_secs();
                    self.settings.save();
                    let current = Version::parse(VERSION).unwrap_or(Version { core: [0; 3], pre: Vec::new() });
                    self.updater.last = Some(match result {
                        Ok(releases) => {
                            self.updater.available = newer(&releases, &current, self.beta_channel()).cloned();
                            Ok(match &self.updater.available {
                                Some(r) => format!("{} is available.", r.name),
                                None => tr("You have the latest version.").into(),
                            })
                        }
                        Err(e) => Err(trf("Couldn't check for updates: {e}", &[("e", &(e).to_string())])),
                    });
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
                Err(_) => self.updater.checking = None,
            }
        }
        let mut next = None;
        match &mut self.updater.install {
            Some(Install::Download { job, step, progress, archive, sums, method }) => {
                while let Ok(m) = job.rx.try_recv() {
                    match m {
                        Msg::Step { label, .. } => {
                            *step = label;
                            *progress = None;
                        }
                        Msg::Progress(p) => *progress = Some(p),
                        Msg::Failed(e) => next = Some(Install::Failed(e)),
                        Msg::Finished => {
                            // Checked, unpacked and put in place off the UI thread.
                            let (archive, sums, method) = (archive.clone(), sums.clone(), method.clone());
                            let (tx, rx) = channel();
                            std::thread::spawn(move || {
                                let _ = tx.send(verify_and_install(&archive, &sums, &method));
                            });
                            next = Some(Install::Place(rx));
                        }
                        _ => {}
                    }
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Some(Install::Place(rx)) => match rx.try_recv() {
                Ok(Ok(_)) => {
                    self.updater.installed = true;
                    self.updater.install = None;
                    // "Restart and Update Now" was asked for: that's the restart.
                    if self.updater.restart_when_done {
                        self.restart_into_update(ctx);
                    }
                }
                Ok(Err(e)) => next = Some(Install::Failed(e)),
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                Err(_) => next = Some(Install::Failed("the installer stopped".into())),
            },
            _ => {}
        }
        if let Some(n) = next {
            self.updater.install = Some(n);
        }
    }

    /// The Update button: downloads, checks and installs — or, where the app can't
    /// update itself, opens the release's page.
    fn start_install(&mut self, ctx: &egui::Context) {
        let Some(release) = self.updater.available.clone() else { return };
        let Some(method) = install_method() else {
            ctx.open_url(egui::OpenUrl::new_tab(release.page));
            return;
        };
        let Some(package) = package_for_method(&release, &method).cloned() else {
            self.updater.install = Some(Install::Failed(format!("{} has no package for this install", release.name)));
            return;
        };
        let Some(sums_asset) = release.assets.iter().find(|a| a.name == SUMS).cloned() else {
            self.updater.install = Some(Install::Failed("the release has no checksums to check the download against".into()));
            return;
        };
        let work = workdir();
        let _ = std::fs::remove_dir_all(&work);
        let (archive, sums) = (work.join(&package.name), work.join(SUMS));
        let steps = vec![
            Step::Download { what: release.name.clone(), url: package.url.clone(), to: archive.clone() },
            Step::Download { what: "the checksums".into(), url: sums_asset.url.clone(), to: sums.clone() },
        ];
        let job = oa_captions::engine::run(&Engine::new(work), steps);
        self.updater.install = Some(Install::Download { job, step: "Starting".into(), progress: None, archive, sums, method });
    }

    /// Restart now: close the normal way (unsaved work is asked about), then start the
    /// new version.
    fn restart_into_update(&mut self, ctx: &egui::Context) {
        if let Some(method) = install_method() {
            self.updater.relaunch = Some(main_program(method.dir()));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Called when the app exits: starts the updated version if a restart was asked for.
    pub(crate) fn relaunch_after_update(&mut self) {
        if let Some(program) = self.updater.relaunch.take()
            && program.exists()
        {
            let _ = std::process::Command::new(program).spawn();
        }
    }

    /// When a newer version is out: a dialog that says so plainly — "Beta 5 is
    /// available!", what's in it, and two choices: restart and update now, or later.
    /// After "Later" it stays away for this session (Settings → Updates → Check now
    /// brings it back); an update installed but not yet restarted into keeps a small bar
    /// at the top.
    pub(crate) fn update_banner(&mut self, root: &mut egui::Ui) {
        let u = &self.updater;
        let Some(release) = u.available.clone() else { return };
        let ctx = root.ctx().clone();
        let dismissed = u.dismissed.as_deref() == Some(release.tag.as_str());
        if dismissed {
            if self.updater.installed {
                egui::Panel::top("update-banner").show(root, |ui| {
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(trf("{0} is installed. Restart to use it.", &[("0", &(release.name).to_string())])).color(crate::style::ACCENT));
                        if ui.button(tr("Restart now")).on_hover_text(tr("Closes (asking about unsaved work first) and opens the new version")).clicked() {
                            self.restart_into_update(&ctx);
                        }
                    });
                    ui.add_space(3.0);
                });
            }
            return;
        }
        // Closing asks about unsaved work in its own dialog: that one goes on top.
        if self.closing {
            return;
        }
        let can = install_method().is_some();
        let busy = matches!(self.updater.install, Some(Install::Download { .. } | Install::Place(_)));
        let lines = release_features(&release.notes);
        let mut later = false;
        egui::Modal::new(egui::Id::new("update-available")).show(&ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(egui::RichText::new(trf("{0} is available!", &[("0", &(release.name).to_string())])).strong());
            ui.label(egui::RichText::new(trf("You have {0}.", &[("0", &(short_version(VERSION)).to_string())])).weak());
            ui.add_space(crate::style::GAP);
            if lines.is_empty() {
                ui.label(tr("New features and fixes."));
                if ui.link(tr("See what's new")).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(release.page.clone()));
                }
            } else {
                egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                    for line in &lines {
                        match line {
                            Feature::Heading(h) => {
                                ui.add_space(crate::style::GAP_S);
                                ui.label(egui::RichText::new(h).strong());
                            }
                            Feature::Item(text, depth) => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(8.0 + 16.0 * *depth as f32);
                                    ui.label(if *depth == 0 { "•" } else { "◦" });
                                    ui.label(text);
                                });
                            }
                        }
                    }
                });
            }
            ui.add_space(crate::style::GAP_L);
            match &self.updater.install {
                Some(Install::Download { step, progress, .. }) => {
                    ui.label(trf("{step}…", &[("step", &step.to_string())]));
                    ui.add(egui::ProgressBar::new(progress.unwrap_or(0.0)).show_percentage().animate(progress.is_none()));
                }
                Some(Install::Place(_)) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(if matches!(install_method(), Some(Method::Deb(_))) {
                            tr("Installing — your system may ask for your password…")
                        } else {
                            tr("Checking and installing…")
                        });
                    });
                }
                Some(Install::Failed(e)) => {
                    ui.label(egui::RichText::new(trf("The update didn't install: {e}", &[("e", &e.to_string())])).color(crate::style::ERROR));
                }
                None if self.updater.installed => {
                    ui.label(tr("Installed. Restarting…"));
                }
                None => {}
            }
            ui.add_space(crate::style::GAP_S);
            ui.horizontal(|ui| {
                let failed = matches!(self.updater.install, Some(Install::Failed(_)));
                let label = match () {
                    _ if !can => tr("Open the Download Page"),
                    _ if failed => "Try Again",
                    _ => tr("Restart and Update Now"),
                };
                let tip = if can {
                    tr("Downloads it, checks it and installs it, then restarts (asking about unsaved work first)")
                } else {
                    tr("This copy can't update itself (a developer build, or its folder isn't writable): opens the download page")
                };
                let button = egui::Button::new(egui::RichText::new(label).strong()).fill(crate::style::ACCENT.gamma_multiply(0.45));
                if ui.add_enabled(!busy, button).on_hover_text(tip).clicked() {
                    if self.updater.installed {
                        self.restart_into_update(&ctx);
                    } else {
                        self.updater.restart_when_done = true;
                        self.start_install(&ctx);
                    }
                }
                if failed && ui.button(tr("Download Page")).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(release.page.clone()));
                }
                if ui.button(tr("Later")).on_hover_text(tr("Keeps working with this version; the download, if it started, finishes in the background")).clicked() {
                    later = true;
                }
            });
        });
        if later {
            self.updater.dismissed = Some(release.tag.clone());
            self.updater.restart_when_done = false;
        }
    }

    /// Settings → Updates.
    pub(crate) fn update_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(trf("OpenAtelier {0} · {1}", &[("0", &(short_version(VERSION)).to_string()), ("1", (platform()))])).small());
        let s = &mut self.settings;
        ui.horizontal(|ui| {
            ui.label(tr("Channel"));
            let before = s.update_channel.clone();
            ui.selectable_value(&mut s.update_channel, "stable".to_string(), "Stable").on_hover_text(tr("Full releases only"));
            ui.selectable_value(&mut s.update_channel, "beta".to_string(), "Beta").on_hover_text(tr("Pre-releases too: new features sooner, less tested"));
            if s.update_channel != before {
                s.update_checked_at = 0;
            }
        });
        ui.checkbox(&mut s.check_updates, tr("Check for updates")).on_hover_text(tr("At start and every few hours while open, from the project's GitHub releases"));
        ui.horizontal(|ui| {
            let checking = self.updater.checking.is_some();
            if ui.add_enabled(!checking, egui::Button::new(tr("Check now"))).clicked() {
                self.updater.dismissed = None;
                self.check_for_updates();
            }
            if checking {
                ui.spinner();
            } else {
                match &self.updater.last {
                    Some(Ok(m)) => {
                        ui.label(egui::RichText::new(m).small());
                    }
                    Some(Err(e)) => {
                        ui.label(egui::RichText::new(e).small().color(crate::style::ERROR));
                    }
                    None => {}
                }
            }
        });
    }
}

/// The downloaded package against its listed checksum, then installed the way `method`
/// says.
fn verify_and_install(archive: &Path, sums: &Path, method: &Method) -> Result<usize, String> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let listed = std::fs::read_to_string(sums).map_err(|e| e.to_string())?;
    let want = checksum_in(&listed, name).ok_or_else(|| format!("{name} isn't in the release's checksums"))?;
    let got = sha256_file(archive).map_err(|e| e.to_string())?;
    if got != want {
        return Err("the download is damaged (its checksum doesn't match); try again".into());
    }
    match method {
        Method::Folder(dir) => place(archive, dir),
        Method::Deb(dir) => install_deb(archive, dir),
    }
}

/// Installs a downloaded `.deb` with apt (dpkg if there's no apt), as root through
/// `pkexec`, which puts up the system's own password prompt.
fn install_deb(deb: &Path, dir: &Path) -> Result<usize, String> {
    let deb = std::fs::canonicalize(deb).map_err(|e| e.to_string())?;
    let has = |program: &str| std::process::Command::new(program).arg("--version").output().is_ok();
    if !has("pkexec") {
        return Err(format!("this system can't ask for your password to install it (no pkexec) — in a terminal: sudo apt install {}", deb.display()));
    }
    let mut command = std::process::Command::new("pkexec");
    if has("apt-get") {
        command.args(["apt-get", "install", "-y", "--allow-downgrades"]).arg(&deb);
    } else {
        command.args(["dpkg", "-i"]).arg(&deb);
    }
    let out = command.output().map_err(|e| format!("couldn't run pkexec: {e}"))?;
    match out.status.code() {
        Some(0) => {}
        // pkexec: the prompt was closed, or the password wasn't accepted.
        Some(126 | 127) => return Err("installing needs your password, and the prompt was closed".into()),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("apt failed").trim().to_string();
            return Err(format!("apt couldn't install it: {last}"));
        }
    }
    if !main_program(dir).exists() {
        return Err("the package has no program in it".into());
    }
    Ok(1)
}

/// Unpacks a package and puts its files in place of the ones in `dir`.
fn place(archive: &Path, dir: &Path) -> Result<usize, String> {
    let staging = archive.with_extension("unpacked");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    // tar reads .zip too (Windows 10 and later ship bsdtar).
    let out = oa_media::tool("tar").arg("-xf").arg(archive).arg("-C").arg(&staging).output().map_err(|e| format!("couldn't run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("couldn't unpack it: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let n = put_in_place(&package_root(&staging), dir).map_err(|e| format!("couldn't put the new files in place: {e}"))?;
    if !main_program(dir).exists() {
        return Err("the package has no program in it".into());
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(n)
}

/// A line of a release's feature list.
#[derive(Clone, Debug, PartialEq)]
pub enum Feature {
    /// A section ("New", "Fixed").
    Heading(String),
    /// A point, and how far it's nested.
    Item(String, usize),
}

/// What's in a release, from its notes (Markdown): the bullet points and the headings over
/// them, as plain text. The download instructions and GitHub's generated list of changes
/// (commits and contributors) are left out.
pub fn release_features(notes: &str) -> Vec<Feature> {
    // `**x**`, `` `x` ``, `[x](url)` → x.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('[') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            match after.find("](").and_then(|close| after[close..].find(')').map(|end| (close, close + end))) {
                Some((close, end)) => {
                    out.push_str(&after[..close]);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('[');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out.replace("**", "").replace("__", "").replace('`', "").trim().to_string()
    }
    let skipped = |heading: &str| {
        let h = heading.to_lowercase();
        ["which file", "what's changed", "what’s changed", "new contributors", "download", "install"].iter().any(|s| h.contains(s))
    };
    let mut out = Vec::new();
    let mut skipping = false;
    // Whether the next plain line carries on the last point (wrapped in the source).
    let mut open = false;
    for line in notes.lines() {
        let trimmed = line.trim_start();
        if let Some(h) = trimmed.strip_prefix('#') {
            let h = plain(h.trim_start_matches('#'));
            skipping = skipped(&h);
            open = false;
            if !skipping && !h.is_empty() {
                out.push(Feature::Heading(h));
            }
            continue;
        }
        if skipping || trimmed.starts_with("**Full Changelog**") {
            open = false;
            continue;
        }
        let Some(text) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) else {
            match out.last_mut() {
                Some(Feature::Item(text, _)) if open && !trimmed.is_empty() => {
                    text.push(' ');
                    text.push_str(&plain(trimmed));
                }
                _ => open = false,
            }
            continue;
        };
        let indent = line.len() - trimmed.len();
        let text = plain(text);
        open = !text.is_empty();
        if open {
            out.push(Feature::Item(text, indent / 2));
        }
    }
    // Headings with nothing under them go.
    let mut kept: Vec<Feature> = Vec::new();
    for f in out {
        if matches!(f, Feature::Heading(_)) && matches!(kept.last(), Some(Feature::Heading(_))) {
            kept.pop();
        }
        kept.push(f);
    }
    if matches!(kept.last(), Some(Feature::Heading(_))) {
        kept.pop();
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dialog lists what's new from the release notes, without the download help or
    /// GitHub's list of commits.
    #[test]
    fn features_come_from_the_notes() {
        let notes = "## Beta 5\n### New\n- **Masks.** Turn on under Settings.\n  - Draw with a `brush`.\n* Tabs in the [bin](https://x.y/z).\n### Fixed\n- Flicker\n\n### Which file?\n- **Windows:** the setup.exe\n\n## What's Changed\n* Fix a thing by @someone in https://github.com/a/b/pull/1\n\n**Full Changelog**: https://github.com/a/b/compare/v1...v2\n### Empty\n";
        let f = release_features(notes);
        let item = |s: &str, d: usize| Feature::Item(s.into(), d);
        let head = |s: &str| Feature::Heading(s.into());
        assert_eq!(
            f,
            vec![head("New"), item("Masks. Turn on under Settings.", 0), item("Draw with a brush.", 1), item("Tabs in the bin.", 0), head("Fixed"), item("Flicker", 0)]
        );
        assert!(release_features("").is_empty());
        // A point wrapped over lines is one point; a paragraph after a blank line isn't
        // part of it.
        let wrapped = release_features("- **Masks:** rectangle,\n  ellipse and `paths`.\n\nSome words.\n- Next");
        assert_eq!(wrapped, vec![item("Masks: rectangle, ellipse and paths.", 0), item("Next", 0)]);
        // This repository's own changelog reads as a list.
        let changelog = include_str!("../../../CHANGELOG.md");
        let beta5 = changelog.split("\n## ").find(|s| s.starts_with("0.1-beta.5")).expect("a Beta 5 section");
        let points = release_features(beta5);
        assert!(points.iter().filter(|f| matches!(f, Feature::Item(..))).count() >= 10, "{points:?}");
        assert!(points.contains(&head("Fixed")));
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_order_by_semantic_versioning() {
        let mut list = ["1.0.0", "0.1.0-beta.10", "0.1.0-alpha", "0.1.0", "0.1.0-beta.2", "v0.2.0-rc.1", "0.1.0-beta", "0.1.1"].map(v).to_vec();
        list.sort();
        let names: Vec<String> = list.iter().map(|x| format!("{:?}{:?}", x.core, x.pre)).collect();
        let expect = ["0.1.0-alpha", "0.1.0-beta", "0.1.0-beta.2", "0.1.0-beta.10", "0.1.0", "0.1.1", "0.2.0-rc.1", "1.0.0"].map(v).to_vec();
        assert_eq!(list, expect, "{names:?}");
        assert!(v("0.1.0-beta.2") > v("0.1.0-beta.1"));
        assert!(v("0.1.0") > v("0.1.0-beta.9"));
        assert_eq!(v("v1.2"), v("1.2.0"));
        assert_eq!(v("1.2.3+build.7"), v("1.2.3"));
        assert!(Version::parse("latest").is_none());
        assert!(Version::parse(VERSION).is_some(), "the running version parses");
        // Tags leave a patch of 0 off, and still mean the same version.
        assert_eq!(v("v0.1-beta.5"), v("0.1.0-beta.5"));
        assert!(v("v0.1-beta.5") > v("v0.1.0-beta.4"));
        assert_eq!(short_version("0.1.0-beta.5"), "0.1-beta.5");
        assert_eq!(short_version("0.2.0"), "0.2");
        assert_eq!(short_version("1.2.3-rc.1"), "1.2.3-rc.1");
        assert_eq!(short_version("1.0"), "1.0");
        assert_eq!(Version::parse(&short_version(VERSION)), Version::parse(VERSION));
    }

    fn release(tag: &str, pre: bool, platforms: &[&str]) -> Release {
        Release {
            tag: tag.into(),
            version: v(tag),
            name: tag.into(),
            prerelease: pre,
            page: String::new(),
            notes: String::new(),
            assets: platforms.iter().map(|p| Asset { name: format!("OpenAtelier-{tag}-{p}.zip"), url: String::new(), size: 1 }).collect(),
        }
    }

    #[test]
    fn the_channel_decides_what_counts_as_newer() {
        let here = platform();
        let list = vec![release("v0.1.0-beta.3", true, &[here]), release("v0.1.0-beta.2", true, &[here]), release("v0.0.9", false, &[here]), release("v0.1.0-beta.4", true, &["no-such-platform"])];
        let current = v("0.1.0-beta.2");
        assert_eq!(newer(&list, &current, true).map(|r| r.tag.as_str()), Some("v0.1.0-beta.3"), "beta: the newest pre-release with a package here");
        assert_eq!(newer(&list, &current, false), None, "stable: 0.0.9 isn't newer");
        assert_eq!(newer(&list, &v("0.1.0-beta.3"), true), None, "already the latest");
    }

    #[test]
    fn github_answers_are_read() {
        let json = r#"[
            {"tag_name": "v0.1.0-beta.2", "name": "Beta 2", "prerelease": true, "draft": false, "html_url": "https://x/2", "body": "notes",
             "assets": [{"name": "OpenAtelier-0.1.0-beta.2-windows-x64.zip", "browser_download_url": "https://x/a.zip", "size": 5},
                        {"name": "SHA256SUMS.txt", "browser_download_url": "https://x/s", "size": 1}]},
            {"tag_name": "v0.1.0-beta.3", "draft": true, "assets": []},
            {"tag_name": "nightly", "assets": []}
        ]"#;
        let list = parse_releases(json).unwrap();
        assert_eq!(list.len(), 1, "drafts and non-versions left out");
        assert_eq!(list[0].name, "Beta 2");
        assert!(list[0].prerelease);
        assert_eq!(package_for(&list[0], "windows-x64").map(|a| a.url.as_str()), Some("https://x/a.zip"));
        assert_eq!(package_for(&list[0], "linux-x64"), None);
        assert_eq!(parse_releases(r#"{"message": "API rate limit exceeded"}"#), Err("API rate limit exceeded".into()));
    }

    #[test]
    fn downloads_are_checked_against_the_listed_checksums() {
        let dir = std::env::temp_dir().join(format!("oa-update-sum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pkg.zip");
        std::fs::write(&file, b"abc").unwrap();
        // SHA-256 of "abc".
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_file(&file).unwrap(), abc);
        let sums = format!("{abc}  pkg.zip\n{}  other.tar.gz\n", "0".repeat(64));
        assert_eq!(checksum_in(&sums, "pkg.zip").as_deref(), Some(abc));
        assert_eq!(checksum_in(&format!("{}  *pkg.zip", abc.to_uppercase()), "pkg.zip").as_deref(), Some(abc), "binary-mode marker, upper case");
        assert_eq!(checksum_in(&sums, "missing.zip"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A package as the release workflow makes it — one folder, zipped (Windows) or
    /// tarred — is checked, unpacked and put in place; a damaged one is refused.
    #[test]
    fn a_package_installs_end_to_end() {
        let root = std::env::temp_dir().join(format!("oa-update-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = format!("OpenAtelier-9.9.9-{}", platform());
        let inner = root.join("build").join(&folder);
        std::fs::create_dir_all(inner.join("plugins")).unwrap();
        let program = main_program(&inner);
        std::fs::write(&program, b"new program").unwrap();
        std::fs::write(inner.join("plugins").join("README.md"), b"plugins").unwrap();
        let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
        let archive = root.join(format!("{folder}.{ext}"));
        let flags = if cfg!(windows) { "-a" } else { "-z" };
        let made = oa_media::tool("tar").arg(flags).arg("-cf").arg(&archive).arg("-C").arg(root.join("build")).arg(&folder).status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipping: no tar to make a package with");
            return;
        }
        let name = archive.file_name().unwrap().to_str().unwrap().to_string();
        let sums = root.join(SUMS);
        std::fs::write(&sums, format!("{}  {name}\n", sha256_file(&archive).unwrap())).unwrap();
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(main_program(&app), b"old program").unwrap();

        assert_eq!(verify_and_install(&archive, &sums, &Method::Folder(app.clone())), Ok(2));
        assert_eq!(std::fs::read(main_program(&app)).unwrap(), b"new program");
        assert!(app.join("plugins").join("README.md").exists());

        // A download that doesn't match its checksum is refused before anything moves.
        std::fs::write(&sums, format!("{}  {name}\n", "0".repeat(64))).unwrap();
        std::fs::write(main_program(&app), b"current").unwrap();
        assert!(verify_and_install(&archive, &sums, &Method::Folder(app.clone())).unwrap_err().contains("damaged"));
        assert_eq!(std::fs::read(main_program(&app)).unwrap(), b"current");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_files_go_in_place_and_old_ones_step_aside() {
        let root = std::env::temp_dir().join(format!("oa-update-place-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (new, app) = (root.join("new"), root.join("app"));
        std::fs::create_dir_all(new.join("plugins")).unwrap();
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(new.join("OpenAtelier.exe"), b"v2").unwrap();
        std::fs::write(new.join("plugins").join("readme.md"), b"docs").unwrap();
        std::fs::write(app.join("OpenAtelier.exe"), b"v1").unwrap();
        std::fs::write(app.join("OpenAtelier.exe.old"), b"v0").unwrap();
        assert_eq!(put_in_place(&new, &app).unwrap(), 2);
        assert_eq!(std::fs::read(app.join("OpenAtelier.exe")).unwrap(), b"v2");
        assert_eq!(std::fs::read(app.join("OpenAtelier.exe.old")).unwrap(), b"v1", "the replaced one is kept aside");
        assert_eq!(std::fs::read(app.join("plugins").join("readme.md")).unwrap(), b"docs");
        clean_up(&app);
        assert!(!app.join("OpenAtelier.exe.old").exists(), "cleared at the next start");
        assert!(app.join("OpenAtelier.exe").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

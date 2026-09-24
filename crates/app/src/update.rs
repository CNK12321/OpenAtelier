//! Updates from GitHub: the app reads the repository's releases (the GitHub API, through
//! `curl`, the same way the engines download), tells the user when a newer version than
//! the one running is out, and — with one button — downloads the package for this
//! platform, checks it against the release's `SHA256SUMS.txt`, puts it in place next to
//! the running app and restarts into it.
//!
//! * Versions are semantic (`0.1.0-beta.2`); the **Beta** channel also offers pre-releases,
//!   **Stable** only full releases. Beta builds start on the Beta channel.
//! * Checked at start at most once a day (Settings → Updates: on/off, channel, Check now).
//! * Installing replaces files in the app's own folder: a file in use (the running
//!   program) is renamed aside (`*.old`) first — Windows allows that — and the leftovers
//!   are cleared at the next start. A build run from a `target` folder (a developer's), or
//!   an install the app can't write to, gets a link to the download page instead.
//!
//! Packages are what `.github/workflows/release.yml` makes:
//! `OpenAtelier-<version>-<platform>.zip|.tar.gz`, one folder inside with the programs.

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

/// Where the running app lives, if it can update itself there: not a developer build
/// (run from a `target` folder) and a folder it can write to.
pub fn install_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    if dir.components().any(|c| c.as_os_str() == "target") {
        return None;
    }
    let probe = dir.join(".oa-write-test");
    std::fs::write(&probe, b"").ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(dir)
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
}

enum Install {
    /// Downloading the package and the checksums.
    Download { job: Job, step: String, progress: Option<f32>, archive: PathBuf, sums: PathBuf, dir: PathBuf },
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
        return Err(if why.is_empty() { "GitHub didn't answer".into() } else { why });
    }
    parse_releases(&body)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl App {
    /// Starting up: clear what the last update left behind, and check (at most daily).
    pub(crate) fn start_updates(&mut self) {
        if let Some(dir) = install_dir() {
            clean_up(&dir);
        }
        let _ = std::fs::remove_dir_all(workdir());
        if self.settings.check_updates && self.script.is_none() && now_secs().saturating_sub(self.settings.update_checked_at) > 20 * 3600 {
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
                                None => "You have the latest version.".into(),
                            })
                        }
                        Err(e) => Err(format!("Couldn't check for updates: {e}")),
                    });
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
                Err(_) => self.updater.checking = None,
            }
        }
        let mut next = None;
        match &mut self.updater.install {
            Some(Install::Download { job, step, progress, archive, sums, dir }) => {
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
                            let (archive, sums, dir) = (archive.clone(), sums.clone(), dir.clone());
                            let (tx, rx) = channel();
                            std::thread::spawn(move || {
                                let _ = tx.send(verify_and_place(&archive, &sums, &dir));
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
        let (Some(dir), Some(package)) = (install_dir(), package_for(&release, platform()).cloned()) else {
            ctx.open_url(egui::OpenUrl::new_tab(release.page));
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
        self.updater.install = Some(Install::Download { job, step: "Starting".into(), progress: None, archive, sums, dir });
    }

    /// Restart now: close the normal way (unsaved work is asked about), then start the
    /// new version.
    fn restart_into_update(&mut self, ctx: &egui::Context) {
        if let Some(dir) = install_dir() {
            self.updater.relaunch = Some(main_program(&dir));
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

    /// The bar across the top when there's news: a newer version, the download, done.
    pub(crate) fn update_banner(&mut self, root: &mut egui::Ui) {
        let u = &self.updater;
        let offer = u.available.as_ref().filter(|r| u.dismissed.as_deref() != Some(r.tag.as_str()) || u.install.is_some() || u.installed);
        let Some(release) = offer.cloned() else { return };
        let ctx = root.ctx().clone();
        egui::Panel::top("update-banner").show(root, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("⬆").color(crate::style::ACCENT));
                match &self.updater.install {
                    _ if self.updater.installed => {
                        ui.label(format!("{} is installed. Restart to use it.", release.name));
                        if ui.button("Restart now").on_hover_text("Closes (asking about unsaved work first) and opens the new version").clicked() {
                            self.restart_into_update(&ctx);
                        }
                    }
                    Some(Install::Download { step, progress, .. }) => {
                        ui.label(format!("{step}…"));
                        ui.add(egui::ProgressBar::new(progress.unwrap_or(0.0)).desired_width(180.0).show_percentage().animate(progress.is_none()));
                    }
                    Some(Install::Place(_)) => {
                        ui.spinner();
                        ui.label("Checking and installing…");
                    }
                    Some(Install::Failed(e)) => {
                        ui.label(egui::RichText::new(format!("The update didn't install: {e}")).color(crate::style::ERROR));
                        if ui.button("Download page").clicked() {
                            ctx.open_url(egui::OpenUrl::new_tab(release.page.clone()));
                        }
                        if ui.button("Try again").clicked() {
                            self.start_install(&ctx);
                        }
                    }
                    None => {
                        let kind = if release.prerelease { "beta" } else { "version" };
                        ui.label(format!("A new {kind} is out: {} — you have {VERSION}.", release.name));
                        let can = install_dir().is_some();
                        let tip = if can { "Download it, check it and install it; then restart" } else { "Opens the download page (this copy can't update itself: it's a developer build, or its folder isn't writable)" };
                        if ui.add(egui::Button::new(egui::RichText::new("Update").strong()).fill(crate::style::ACCENT.gamma_multiply(0.35))).on_hover_text(tip).clicked() {
                            self.start_install(&ctx);
                        }
                        if ui.button("What's new").on_hover_text(release.notes.chars().take(600).collect::<String>()).clicked() {
                            ctx.open_url(egui::OpenUrl::new_tab(release.page.clone()));
                        }
                        if ui.button("Later").clicked() {
                            self.updater.dismissed = Some(release.tag.clone());
                        }
                    }
                }
            });
            ui.add_space(3.0);
        });
    }

    /// Settings → Updates.
    pub(crate) fn update_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(format!("OpenAtelier {VERSION} · {}", platform())).small());
        let s = &mut self.settings;
        ui.horizontal(|ui| {
            ui.label("Channel");
            let before = s.update_channel.clone();
            ui.selectable_value(&mut s.update_channel, "stable".to_string(), "Stable").on_hover_text("Full releases only");
            ui.selectable_value(&mut s.update_channel, "beta".to_string(), "Beta").on_hover_text("Pre-releases too: new features sooner, less tested");
            if s.update_channel != before {
                s.update_checked_at = 0;
            }
        });
        ui.checkbox(&mut s.check_updates, "Check for updates at start").on_hover_text("At most once a day, from the project's GitHub releases");
        ui.horizontal(|ui| {
            let checking = self.updater.checking.is_some();
            if ui.add_enabled(!checking, egui::Button::new("Check now")).clicked() {
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

/// The downloaded package against its listed checksum, then unpacked and put in place.
fn verify_and_place(archive: &Path, sums: &Path, dir: &Path) -> Result<usize, String> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let listed = std::fs::read_to_string(sums).map_err(|e| e.to_string())?;
    let want = checksum_in(&listed, name).ok_or_else(|| format!("{name} isn't in the release's checksums"))?;
    let got = sha256_file(archive).map_err(|e| e.to_string())?;
    if got != want {
        return Err("the download is damaged (its checksum doesn't match); try again".into());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

        assert_eq!(verify_and_place(&archive, &sums, &app), Ok(2));
        assert_eq!(std::fs::read(main_program(&app)).unwrap(), b"new program");
        assert!(app.join("plugins").join("README.md").exists());

        // A download that doesn't match its checksum is refused before anything moves.
        std::fs::write(&sums, format!("{}  {name}\n", "0".repeat(64))).unwrap();
        std::fs::write(main_program(&app), b"current").unwrap();
        assert!(verify_and_place(&archive, &sums, &app).unwrap_err().contains("damaged"));
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

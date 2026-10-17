//! Proxies: small, easy-to-decode copies of heavy footage (4K, 10-bit, long-GOP) that the
//! viewer plays instead of the originals. Exports always read the originals.
//!
//! A proxy is H.264, at most 540 pixels on its short side, a keyframe every 12 frames and
//! no B-frames (so scrubbing lands quickly), in Matroska with the original's timestamps
//! (`-copyts`), one frame for each of the original's — so a time in the original is the
//! same time in the proxy. It's kept in `<app dir>/proxies/<fingerprint>.mkv`, named by
//! the file's content: every project using the file shares it, and a file that moves
//! keeps it.
//!
//! One thread does the work, in order: finding a proxy made earlier (the fingerprint
//! reads a few megabytes), making one (ffmpeg, with progress). None of it is shown: heavy
//! footage gets a proxy by itself, and the viewer plays it once it's ready.

use oa_media::VideoTrack;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// The short side of a proxy, at most.
pub const SHORT_SIDE: u32 = 540;

/// Where a file's proxy stands.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Being looked for.
    Checking,
    /// None made.
    Missing,
    /// Being made: how far (0–1).
    Making(f32),
    Ready { path: PathBuf, track: Box<VideoTrack> },
    Failed(String),
}

enum Request {
    /// Look for a proxy made earlier; make one if there's none and `make`.
    Find { file: PathBuf, make: bool },
}

pub struct Proxies {
    requests: Sender<Request>,
    events: Receiver<(PathBuf, State)>,
    /// By the file decoded (`PoolItem::decode_path`).
    states: HashMap<PathBuf, State>,
    /// The ffmpeg making a proxy now, stopped when the app closes.
    running: Arc<Mutex<Option<Child>>>,
}

impl Default for Proxies {
    fn default() -> Self {
        Self::new(oa_media::app_dir().join("proxies"))
    }
}

impl Drop for Proxies {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Proxies {
    pub fn new(dir: PathBuf) -> Self {
        let (requests, jobs) = channel();
        let (tell, events) = channel();
        let running = Arc::new(Mutex::new(None));
        let child = running.clone();
        std::thread::Builder::new()
            .name("oa-proxies".into())
            .spawn(move || work(&dir, jobs, tell, child))
            .expect("start the proxy thread");
        Proxies { requests, events, states: HashMap::new(), running }
    }

    /// Stops the proxy being made (the app is closing).
    pub fn stop(&mut self) {
        if let Some(mut child) = self.running.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Where `file`'s proxy stands (`None`: never asked about).
    #[cfg(test)]
    pub fn state(&self, file: &Path) -> Option<&State> {
        self.states.get(file)
    }

    /// The proxy to play instead of `file`, if one is ready.
    pub fn ready(&self, file: &Path) -> Option<(&Path, &VideoTrack)> {
        match self.states.get(file) {
            Some(State::Ready { path, track }) => Some((path, track)),
            _ => None,
        }
    }

    /// Looks for `file`'s proxy the first time it's asked about (and makes one if there's
    /// none and `make`).
    pub fn find(&mut self, file: &Path, make: bool) {
        if self.states.contains_key(file) {
            return;
        }
        self.states.insert(file.to_path_buf(), State::Checking);
        let _ = self.requests.send(Request::Find { file: file.to_path_buf(), make });
    }

    /// Takes in the thread's news. Returns whether a proxy became ready or went away
    /// (what the viewer plays changes).
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((file, state)) = self.events.try_recv() {
            let was_ready = matches!(self.states.get(&file), Some(State::Ready { .. }));
            changed |= was_ready != matches!(state, State::Ready { .. });
            self.states.insert(file, state);
        }
        changed
    }

    /// Whether any proxy is being made or waiting to be (the UI keeps repainting).
    pub fn busy(&self) -> bool {
        self.states.values().any(|s| matches!(s, State::Making(_) | State::Checking))
    }
}

/// Whether a file is heavy enough to want a proxy by default: bigger than 1440p, deeper
/// than 8 bits, or with its keyframes far apart (more than [`SPARSE_KEYFRAMES`] frames:
/// game and screen recordings, 60 fps phone video) — scrubbing, stepping back and
/// playing it sped up decode from the keyframe before each frame, and a proxy has one
/// every 12. Video with transparency bigger than 720p is too: no decoder the hardware has
/// keeps alpha, so it's decoded by ffmpeg and copied over a frame at a time (a 1080p
/// ProRes 4444 frame is 16 MB) — its proxy packs the transparency below the picture
/// ([`VideoTrack::packed_alpha`]). Stills are left alone.
pub fn heavy(track: &VideoTrack) -> bool {
    let pixels = track.width as u64 * track.height as u64;
    let big = pixels > 2560 * 1440 || track.bit_depth() > 8;
    let transparent = track.has_alpha && pixels > 1280 * 720;
    !track.still && (big || transparent || track.index.max_gop() > SPARSE_KEYFRAMES)
}

/// Keyframes further apart than this many frames make a file worth a proxy.
pub const SPARSE_KEYFRAMES: usize = 45;

/// The proxy's name for `file`: its content fingerprint (`.packed` for one holding its
/// transparency below the picture).
fn proxy_path(dir: &Path, file: &Path, packed: bool) -> Result<PathBuf, String> {
    let fp = oa_media::fingerprint(file).map_err(|e| e.to_string())?;
    Ok(dir.join(if packed { format!("{fp}.packed.mkv") } else { format!("{fp}.mkv") }))
}

fn work(dir: &Path, jobs: Receiver<Request>, tell: Sender<(PathBuf, State)>, running: Arc<Mutex<Option<Child>>>) {
    let _ = std::fs::create_dir_all(dir);
    for job in jobs {
        let Request::Find { file, make } = job;
        // Transparent video: a packed proxy (its alpha below the picture).
        let packed = oa_media::probe(&file).ok().and_then(|p| p.video).is_some_and(|v| v.has_alpha);
        let path = match proxy_path(dir, &file, packed) {
            Ok(p) => p,
            Err(e) => {
                let _ = tell.send((file, State::Failed(e)));
                continue;
            }
        };
        let found = path.exists().then(|| probe_proxy(&path, packed)).and_then(Result::ok);
        let state = match found {
            Some(track) => State::Ready { path, track: Box::new(track) },
            None if !make => State::Missing,
            None => {
                let _ = tell.send((file.clone(), State::Making(0.0)));
                match make_proxy(&file, &path, packed, &running, |p| {
                    let _ = tell.send((file.clone(), State::Making(p)));
                })
                .and_then(|()| probe_proxy(&path, packed))
                {
                    Ok(track) => State::Ready { path, track: Box::new(track) },
                    Err(e) => State::Failed(e),
                }
            }
        };
        let _ = tell.send((file, state));
    }
}

fn probe_proxy(path: &Path, packed: bool) -> Result<VideoTrack, String> {
    let mut track = oa_media::probe(path).map_err(|e| e.to_string())?.video.ok_or("the proxy has no picture")?;
    track.packed_alpha = packed;
    Ok(track)
}

/// Encodes `file`'s proxy into `to` (written aside, then moved into place), reporting
/// progress (0–1) as it goes.
/// `packed`: the file has transparency, kept below the picture as brightness (the frame
/// twice as tall) — H.264 has no alpha, and this way the hardware decoder still plays it.
pub fn make_proxy(file: &Path, to: &Path, packed: bool, running: &Mutex<Option<Child>>, progress: impl Fn(f32)) -> Result<(), String> {
    let duration = oa_media::probe(file).map_err(|e| e.to_string())?.duration.as_seconds_f64().max(1e-3);
    let part = to.with_extension("part.mkv");
    // Below normal priority and on half the cores at most: it runs while you edit.
    let threads = (std::thread::available_parallelism().map_or(4, |n| n.get()) / 2).max(1).to_string();
    let mut c = oa_media::background_tool("ffmpeg");
    c.args(["-hide_banner", "-nostdin", "-v", "error", "-y", "-copyts", "-threads", &threads, "-i"]).arg(file);
    // The short side down to SHORT_SIDE (never up), even sizes; one frame for each of the
    // original's, at the original's times.
    let short = SHORT_SIDE;
    let scale = format!("scale='if(gt(iw,ih),-2,min({short},iw))':'if(gt(iw,ih),min({short},ih),-2)':flags=bicubic");
    if packed {
        // The picture (BT.709, video range — what the shader reads) on top, its alpha
        // below as brightness.
        let graph = format!(
            "[0:v:0]{scale}:out_color_matrix=bt709:out_range=tv,format=yuva420p,split[c][a];[a]alphaextract,format=yuv420p[alpha];[c]format=yuv420p[picture];[picture][alpha]vstack,format=yuv420p[v]"
        );
        c.args(["-filter_complex", &graph, "-map", "[v]", "-an", "-sn", "-dn"]);
        c.args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]);
    } else {
        c.args(["-map", "0:v:0", "-an", "-sn", "-dn"]);
        c.args(["-vf", &format!("{scale},format=yuv420p")]);
    }
    c.args(["-fps_mode", "passthrough"]);
    c.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-g", "12", "-bf", "0", "-tune", "fastdecode", "-threads", &threads]);
    c.args(["-progress", "pipe:1", "-nostats"]).arg(&part);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *running.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = std::io::Read::read_to_string(&mut e, &mut s);
        }
        s
    });
    if let Some(out) = stdout {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            // "out_time_us=1234567" (or out_time_ms, which ffmpeg also gives in µs).
            if let Some(us) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")).and_then(|v| v.trim().parse::<f64>().ok()) {
                progress((us / 1e6 / duration).clamp(0.0, 1.0) as f32);
            }
        }
    }
    let status = running.lock().unwrap_or_else(|e| e.into_inner()).take().map(|mut c| c.wait());
    let errors = errors.join().unwrap_or_default();
    match status {
        Some(Ok(s)) if s.success() => {}
        // Taken by `Drop` (the app closing): stopped, not failed.
        None => {
            let _ = std::fs::remove_file(&part);
            return Err("stopped".into());
        }
        _ => {
            let _ = std::fs::remove_file(&part);
            let why = errors.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ffmpeg failed").trim().to_string();
            return Err(why);
        }
    }
    std::fs::rename(&part, to).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ffmpeg() -> bool {
        oa_media::tool("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
    }

    /// Transparent footage (here ProRes 4444) gets a packed proxy: H.264 twice as tall,
    /// marked so the viewer reads its transparency from the bottom half; and transparent
    /// video bigger than 720p counts as heavy.
    #[test]
    fn transparent_footage_gets_a_packed_proxy() {
        if !ffmpeg() {
            return;
        }
        let root = std::env::temp_dir().join(format!("oa-proxies-packed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("alpha.mov");
        let made = oa_media::tool("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x240:r=25:d=1,format=yuva444p10le"])
            .args(["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le"])
            .arg(&file)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let original = oa_media::probe(&file).unwrap().video.unwrap();
        assert!(original.has_alpha);
        let mut big = original.clone();
        (big.width, big.height) = (1920, 1080);
        assert!(heavy(&big), "1080p with transparency counts as heavy");
        let mut proxies = Proxies::new(root.join("proxies"));
        proxies.find(&file, true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !matches!(proxies.state(&file), Some(State::Ready { .. } | State::Failed(_))) {
            assert!(std::time::Instant::now() < deadline, "made in time: {:?}", proxies.state(&file));
            proxies.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (path, track) = proxies.ready(&file).map(|(p, t)| (p.to_path_buf(), t.clone())).unwrap_or_else(|| panic!("{:?}", proxies.state(&file)));
        assert!(path.to_string_lossy().ends_with(".packed.mkv"), "{}", path.display());
        assert!(track.packed_alpha, "marked as packed");
        assert_eq!([track.width, track.height], [320, 480], "the picture on top of its transparency");
        assert_eq!(track.codec, "h264");
        assert_eq!(track.index.len(), original.index.len(), "a frame for each of the original's");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A proxy is small, has a frame for each of the original's at the same times, and is
    /// found again (by content) instead of made twice.
    #[test]
    fn proxies_are_small_same_timed_and_found_again() {
        if !ffmpeg() {
            return;
        }
        let root = std::env::temp_dir().join(format!("oa-proxies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("big.mp4");
        // 10-bit, 1920×1080 (portrait: 1080×1920 to check the short side), 25 fps, 1 s,
        // starting at 0.5 s.
        let made = oa_media::tool("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1080x1920:r=25:d=1", "-vf", "setpts=PTS+0.5/TB"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p10le", "-preset", "ultrafast"])
            .arg(&file)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let original = oa_media::probe(&file).unwrap().video.unwrap();
        assert!(heavy(&original), "10-bit counts as heavy");
        let mut proxies = Proxies::new(root.join("proxies"));
        proxies.find(&file, true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut saw_progress = false;
        while !matches!(proxies.state(&file), Some(State::Ready { .. } | State::Failed(_))) {
            assert!(std::time::Instant::now() < deadline, "made in time: {:?}", proxies.state(&file));
            proxies.poll();
            saw_progress |= matches!(proxies.state(&file), Some(State::Making(_)));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (path, track) = proxies.ready(&file).map(|(p, t)| (p.to_path_buf(), t.clone())).unwrap_or_else(|| panic!("{:?}", proxies.state(&file)));
        assert!(saw_progress);
        assert_eq!([track.width, track.height], [540, 960], "the short side down to 540, the shape kept");
        assert_eq!(track.bit_depth(), 8);
        assert!(!heavy(&track), "the proxy itself: small, 8-bit, a keyframe every 12 frames");
        let mut sparse = track.clone();
        (sparse.index.pts, sparse.index.keyframes) = ((0..120).collect(), vec![0, 60]);
        assert!(heavy(&sparse), "keyframes 60 frames apart: scrubbing would decode up to that many each time");
        // The same frames at the same times (Matroska keeps milliseconds).
        assert_eq!(track.index.pts.len(), original.index.pts.len());
        let secs = |t: &VideoTrack, i: usize| t.index.pts[i] as f64 * t.index.time_base.num() as f64 / t.index.time_base.den() as f64;
        for i in [0, 7, 24] {
            assert!((secs(&track, i) - secs(&original, i)).abs() < 0.002, "frame {i}: {} vs {}", secs(&track, i), secs(&original, i));
        }
        // Found again, not made again.
        let made_at = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut again = Proxies::new(root.join("proxies"));
        again.find(&file, true);
        while !matches!(again.state(&file), Some(State::Ready { .. } | State::Failed(_))) {
            again.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), made_at);
        drop((proxies, again));
        let _ = std::fs::remove_dir_all(&root);
    }
}

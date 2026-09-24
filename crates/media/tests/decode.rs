//! Frame-accuracy tests against real encoded files.
//!
//! Each generated clip encodes its frame number in brightness: the left half of the
//! picture holds `N mod 16`, the right half `N / 16`, in steps of 12 luma codes, which
//! survives lossy encoding. After GPU decode → linear → sRGB readback we recover N and
//! compare it with the frame the project-wide selection rule says should be shown.
//!
//! Requires `ffmpeg`/`ffprobe` on PATH, Windows, and a DX12 GPU; otherwise tests skip.

#![cfg(windows)]

use oa_gpu::readback::read_srgb8;
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer};
use oa_graph::registry::Registry;
use oa_graph::*;
use oa_media::windows::{hardware_source, MfDecoder};
use oa_media::{probe, MediaFrameSource, MediaProbe, VideoTrack};
use oa_time::Time;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

struct Clip {
    name: &'static str,
    /// Frames are generated (and numbered) at this rate, so painted numbers equal indices.
    rate: &'static str,
    args: &'static [&'static str],
}

const NUMBERED: &str = "geq=lum='if(lt(X\\,W/2)\\,16+mod(N\\,16)*12\\,16+floor(N/16)*12)':cb=128:cr=128";

const CLIPS: &[Clip] = &[
    // 30 fps, GOP 30, B-frames, FFmpeg's default 1/15360 MP4 time base.
    Clip { name: "h264_30_gop30_bframes.mp4", rate: "30", args: &["-c:v", "libx264", "-g", "30", "-bf", "2"] },
    // NTSC 29.97 with a long GOP.
    Clip { name: "h264_2997_gop120.mp4", rate: "30000/1001", args: &["-c:v", "libx264", "-g", "120", "-bf", "3"] },
    // Variable frame rate: every third frame arrives 10 ms late.
    Clip {
        name: "h264_vfr.mp4",
        rate: "30",
        args: &[
            "-vf", "settb=1/1000,setpts='floor((N/30+if(eq(mod(N\\,3)\\,1)\\,0.01\\,0))*1000)'",
            "-fps_mode", "passthrough", "-video_track_timescale", "1000", "-c:v", "libx264", "-g", "45",
        ],
    },
    Clip { name: "hevc_25.mp4", rate: "25", args: &["-c:v", "libx265", "-x265-params", "log-level=error:keyint=50", "-tag:v", "hvc1"] },
];

fn tool_available(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok_and(|o| o.status.success())
}

fn make_clip(clip: &Clip) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("oa-media-tests");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("v2_{}", clip.name));
    if path.exists() {
        return Some(path);
    }
    // 8 seconds of 640x360 numbered frames.
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("nullsrc=s=640x360:r={}:d=8,{NUMBERED}", clip.rate)]);
    let mut args: Vec<&str> = clip.args.to_vec();
    if let Some(vf) = args.iter().position(|a| *a == "-vf") {
        let filter = args.remove(vf + 1);
        args.remove(vf);
        cmd.args(["-vf", filter]);
    }
    cmd.args(&args).args(["-pix_fmt", "yuv420p", "-crf", "12", "-preset", "veryfast"]).arg(&path);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        eprintln!("could not create {}: {}", clip.name, String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some(path)
}

struct Harness {
    ctx: Arc<GpuContext>,
    renderer: Renderer,
    source: MediaFrameSource<MfDecoder>,
    probe: MediaProbe,
    probe_path: PathBuf,
    /// Which media id `shown_frame` renders (tests may add more files).
    media: u64,
}

fn harness(clip: &Clip) -> Option<Harness> {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return None;
    }
    let ctx = Arc::new(GpuContext::new_headless().ok()?);
    let mut source = match hardware_source(&ctx) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skipping: {e}");
            return None;
        }
    };
    let path = make_clip(clip)?;
    let probe = probe(&path).expect("probe");
    source.add(1, &path, probe.video.clone().expect("video track"));
    let renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, cache_budget: 0, ..Default::default() });
    Some(Harness { ctx, renderer, source, probe, probe_path: path, media: 1 })
}

impl Harness {
    fn video(&self) -> &VideoTrack {
        self.probe.video.as_ref().expect("video track")
    }

    /// Renders source time `t` and reads the painted number from two normalized points
    /// (the low digit's half and the high digit's half of the picture).
    fn shown_frame_at(&mut self, t: Time, size: [u32; 2], low: [f32; 2], high: [f32; 2]) -> Result<u32, String> {
        let mut b = GraphBuilder::new(KeyContext::default());
        let src = b.add(
            NodeOp::Source { media: self.media, fingerprint: None, source_time: t, rep: Representation::Original, decode_scale: 1.0, size, yuv: [0, 0] },
            vec![],
            Rect::from_size(size[0] as f64, size[1] as f64),
            true,
        );
        let out = b.add(
            NodeOp::Composite { size, background: [0.0, 0.0, 0.0, 1.0], layers: vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }] },
            vec![src],
            Rect::from_size(size[0] as f64, size[1] as f64),
            true,
        );
        let img = self.renderer.render(&b.finish(out), &Registry::with_builtins(), &mut self.source).map_err(|e| e.to_string())?;
        let rgba = read_srgb8(&self.ctx, self.renderer.pipelines(), &img).map_err(|e| e.to_string())?;
        let digit = |p: [f32; 2]| {
            let x = (p[0] * size[0] as f32) as u32;
            let y = (p[1] * size[1] as f32) as u32;
            let px = rgba[((y * size[0] + x) * 4) as usize] as f64;
            let luma_code = 16.0 + px / 255.0 * 219.0;
            ((luma_code - 16.0) / 12.0).round() as u32
        };
        Ok(digit(low) + 16 * digit(high))
    }

    fn shown_frame(&mut self, t: Time, size: [u32; 2]) -> Result<u32, String> {
        self.shown_frame_at(t, size, [0.25, 0.5], [0.75, 0.5])
    }

    fn check(&mut self, t: Time, what: &str) {
        let expected = self.video().index.frame_at(t).unwrap() as u32;
        let shown = self.shown_frame(t, [self.video().width, self.video().height]).unwrap();
        assert_eq!(shown, expected, "{what}: t={t:?}");
    }
}

fn run_clip(clip: &Clip) {
    let Some(mut h) = harness(clip) else { return };
    let index = h.video().index.clone();
    eprintln!("{}: {} frames, codec {}, time base {:?}, GOP {}, VFR {}", clip.name, index.len(), h.video().codec, index.time_base, index.max_gop(), index.is_variable_rate());
    if let Err(e) = h.shown_frame(Time::ZERO, [h.video().width, h.video().height]) {
        if e.contains("unsupported") || e.contains("no NV12 hardware decode") {
            eprintln!("skipping {}: {e}", clip.name);
            return;
        }
        panic!("{}: {e}", clip.name);
    }

    // Playback: every frame in order, including exact boundaries and one flick before.
    let seeks_before = h.source.stats().seeks;
    for i in 0..index.len().min(90) {
        h.check(index.time_of(i), "playback exact");
    }
    assert!(h.source.stats().seeks - seeks_before <= 1, "playback should not seek ({} seeks)", h.source.stats().seeks - seeks_before);
    for i in [1, 17, 31, 60] {
        h.check(index.time_of(i) - Time(1), "one flick before a frame");
    }

    // Scrubbing: jumps forward, backward, across GOPs, to the end.
    let n = index.len();
    for i in [n / 2, 3, n - 1, n / 3 + 7, index.max_gop(), index.max_gop() - 1, 0, n - 2, 5 * n / 7] {
        h.check(index.time_of(i), "scrub");
    }
    // Stepping backwards frame by frame.
    for i in (n / 2 - 10..n / 2).rev() {
        h.check(index.time_of(i), "step back");
    }
    eprintln!("{}: {:?}", clip.name, h.source.stats());
    assert_eq!(h.source.stats().inexact, 0);
}

#[test]
fn h264_30fps_with_bframes() {
    run_clip(&CLIPS[0]);
}

#[test]
fn h264_ntsc_long_gop() {
    run_clip(&CLIPS[1]);
}

#[test]
fn h264_variable_frame_rate() {
    run_clip(&CLIPS[2]);
}

#[test]
fn hevc_if_the_system_can_decode_it() {
    run_clip(&CLIPS[3]);
}

/// A clip tagged to display rotated 90° (as phones record) must come out upright: the
/// picture is 360x640 and the painted halves appear rotated with it.
#[test]
fn rotation_metadata_is_applied() {
    let Some(h) = harness(&CLIPS[0]) else { return };
    let source = h.probe_path.clone();
    let rotated = source.with_file_name("v2_h264_rot90.mp4");
    if !rotated.exists() {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-display_rotation", "90", "-i"])
            .arg(&source)
            .args(["-c", "copy"])
            .arg(&rotated)
            .output()
            .expect("ffmpeg");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let probe = probe(&rotated).expect("probe");
    let video = probe.video.clone().expect("video");
    assert_eq!((video.width, video.height), (360, 640), "display size swaps");
    assert_eq!((video.coded_width, video.coded_height), (640, 360));
    assert_eq!(video.rotation_quarter_turns, 3);

    let Some(mut h) = harness(&CLIPS[0]) else { return };
    h.source.add(2, &rotated, probe.video.clone().expect("video"));
    h.media = 2;
    for i in [0usize, 7, 40, 91] {
        let t = video.index.time_of(i);
        // 90° counter-clockwise display: the source's left half is now at the bottom.
        let shown = h.shown_frame_at(t, [360, 640], [0.5, 0.75], [0.5, 0.25]).unwrap();
        assert_eq!(shown, i as u32, "rotated frame at {t:?}");
    }
}

/// Stepping backwards over frames that were just played must not re-decode them.
#[test]
fn stepping_back_reuses_recent_frames() {
    let Some(mut h) = harness(&CLIPS[0]) else { return };
    let index = h.video().index.clone();
    let size = [h.video().width, h.video().height];
    for i in 40..60 {
        assert_eq!(h.shown_frame(index.time_of(i), size).unwrap(), i as u32);
    }
    // Let the decode thread finish working ahead before counting.
    assert!(h.source.wait_idle(std::time::Duration::from_secs(5)));
    let (seeks, decoded, hits) = (h.source.stats().seeks, h.source.stats().decoded, h.source.stats().frame_cache_hits);
    for i in (40..60).rev() {
        assert_eq!(h.shown_frame(index.time_of(i), size).unwrap(), i as u32, "stepping back to {i}");
    }
    assert_eq!(h.source.stats().seeks, seeks, "no seeks when stepping back through cached frames");
    assert_eq!(h.source.stats().decoded, decoded, "no re-decoding either");
    assert_eq!(h.source.stats().frame_cache_hits, hits + 20);
}

#[test]
fn downscaled_decode_matches() {
    let Some(mut h) = harness(&CLIPS[0]) else { return };
    let t = h.video().index.time_of(77);
    let full = h.shown_frame(t, [640, 360]).unwrap();
    let half = h.shown_frame(t, [320, 180]).unwrap();
    assert_eq!((full, half), (77, 77));
}

/// One file shown at two places at once — both sides of a transition inside one clip —
/// playing forward together must not make a decoder seek back and forth every frame.
#[test]
fn two_places_in_one_file_play_without_seeking() {
    let Some(mut h) = harness(&CLIPS[0]) else { return };
    let index = h.video().index.clone();
    let size = [h.video().width, h.video().height];
    let rect = Rect::from_size(size[0] as f64, size[1] as f64);
    let registry = Registry::with_builtins();
    let render_pair = |h: &mut Harness, a: usize, b: usize| {
        let mut g = GraphBuilder::new(KeyContext::default());
        let source = |g: &mut GraphBuilder, i: usize| {
            let op = NodeOp::Source { media: 1, fingerprint: None, source_time: index.time_of(i), rep: Representation::Original, decode_scale: 1.0, size, yuv: [0, 0] };
            g.add(op, vec![], rect, true)
        };
        let (sa, sb) = (source(&mut g, a), source(&mut g, b));
        let half = LayerInfo { opacity: 0.5, blend: BlendMode::Normal, pixelated: false };
        let out = g.add(NodeOp::Composite { size, background: [0.0, 0.0, 0.0, 1.0], layers: vec![half, half] }, vec![sa, sb], rect, true);
        h.renderer.render(&g.finish(out), &registry, &mut h.source).expect("render");
    };
    render_pair(&mut h, 20, 120);
    let seeks = h.source.stats().seeks;
    for i in 1..30 {
        render_pair(&mut h, 20 + i, 120 + i);
    }
    let extra = h.source.stats().seeks - seeks;
    assert!(extra <= 1, "{extra} seeks while two streams of one file played together");
}

/// Scrubbing (interactive mode): jumping around the file, no render waits for a decode —
/// a stand-in is shown at once — and when the scrub stops the exact frame arrives.
#[test]
fn scrubbing_never_waits_and_settles_on_the_exact_frame() {
    let Some(mut h) = harness(&CLIPS[1]) else { return }; // long GOP: seeks are expensive
    let index = h.video().index.clone();
    let size = [h.video().width, h.video().height];
    // First frame of a file: nothing on hand yet, so this one waits.
    h.shown_frame(index.time_of(0), size).unwrap();
    h.source.interactive = true;
    let mut slowest = std::time::Duration::ZERO;
    let n = index.len();
    for step in 0..60 {
        let i = (step * 37 + 11) % n;
        let started = std::time::Instant::now();
        let _ = h.shown_frame(index.time_of(i), size).unwrap();
        slowest = slowest.max(started.elapsed());
    }
    eprintln!("slowest interactive render while scrubbing: {slowest:?}");
    assert!(slowest < std::time::Duration::from_millis(60), "a scrub render waited on the decoder: {slowest:?}");

    // Stop on a frame: within a moment the exact frame is what's shown.
    let target = n / 2 + 3;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let shown = h.shown_frame(index.time_of(target), size).unwrap();
        if oa_gpu::FrameSource::settled(&mut h.source) && shown == target as u32 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "never settled on frame {target} (showing {shown})");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

//! Export round-trip: render a project to a file, then decode that file back and check
//! that the frames are the ones the timeline said they would be.
//!
//! The source clip paints its frame number into the picture (low digit on the left half,
//! high digit on the right), so a decoded frame proves *which* frame was exported.
//!
//! Requires ffmpeg/ffprobe, Windows and a DX12 GPU; skips otherwise.

#![cfg(windows)]

use oa_doc::*;
use oa_export::{export, Encoder, ExportOptions, VideoCodec};
use oa_gpu::readback::read_srgb8;
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer};
use oa_graph::registry::Registry;
use oa_graph::*;
use oa_media::windows::hardware_source;
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const SIZE: [u32; 2] = [320, 180];
const FRAMES: usize = 90;

fn tool(name: &str) -> bool {
    Command::new(name).arg("-version").output().is_ok_and(|o| o.status.success())
}

/// 3 seconds at 30 fps with the frame number painted in.
fn numbered_clip() -> Option<PathBuf> {
    if !tool("ffmpeg") || !tool("ffprobe") {
        eprintln!("skipping: ffmpeg not on PATH");
        return None;
    }
    let dir = std::env::temp_dir().join("oa-media-tests");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("export_source.mp4");
    if path.exists() {
        return Some(path);
    }
    let filter = "geq=lum='if(lt(X\\,W/2)\\,16+mod(N\\,16)*12\\,16+floor(N/16)*12)':cb=128:cr=128";
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("nullsrc=s={}x{}:r=30:d=3,{filter}", SIZE[0], SIZE[1]))
        .args(["-c:v", "libx264", "-g", "15", "-bf", "2", "-pix_fmt", "yuv420p", "-crf", "12", "-preset", "veryfast"])
        .arg(&path)
        .output()
        .ok()?;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    Some(path)
}

/// A one-clip project at the media's own size and rate.
fn project(media_path: &Path, probe: &oa_media::MediaProbe) -> (Project, SeqId, MediaId) {
    let video = probe.video.as_ref().expect("video");
    let (seq, track, media, clip) = (SeqId(1), TrackId(2), MediaId(3), ItemId(4));
    let mut p = Project::new("export test");
    let variant = FormatVariant {
        id: VariantId(5),
        name: "Native".into(),
        size: CanvasSize::new(video.width, video.height),
        overrides: BTreeMap::new(),
    };
    let rate = video.avg_rate.unwrap_or(FrameRate::FPS_30);
    let mut sequence = Sequence::new(seq, "Main", rate, variant);
    let mut t = Track::new(track, "V1", TrackKind::Video);
    let duration = rate.frame_start(FRAMES as i64);
    t.items.push(Item::new(clip, "clip", ItemKind::Media { media }, TimeRange::new(Time::ZERO, duration)));
    sequence.tracks.push(Arc::new(t));
    p.sequences.insert(seq, Arc::new(sequence));
    let info = MediaInfo {
        width: video.width,
        height: video.height,
        duration: probe.duration,
        rate: video.avg_rate,
        has_video: true,
        has_audio: probe.has_audio(),
        still: false,
        ..Default::default()
    };
    p.media.insert(
        media,
        Arc::new(MediaRef {
            id: media,
            path: media_path.to_string_lossy().to_string(),
            fingerprint: Some("export-source".into()),
            info: Some(info),
            scaling: Default::default(),
            folder: String::new(),
            color: Default::default(),
        }),
    );
    (p, seq, media)
}

/// Decodes `path` and reads the painted frame number at each of `times`.
fn read_numbers(ctx: &Arc<GpuContext>, path: &Path, times: &[usize]) -> Vec<u32> {
    let probe = oa_media::probe(path).expect("probe");
    let video = probe.video.clone().expect("video");
    let mut source = hardware_source(ctx).expect("decoder");
    source.add(1, path, video.clone());
    let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, cache_budget: 0, ..Default::default() });
    let registry = Registry::with_builtins();
    let size = [video.width, video.height];

    times
        .iter()
        .map(|&frame| {
            let t = video.index.time_of(frame.min(video.index.len() - 1));
            let mut b = GraphBuilder::new(KeyContext::default());
            let src = b.add(
                NodeOp::Source { media: 1, fingerprint: None, source_time: t, rep: Representation::Original, decode_scale: 1.0, size, yuv: [0, 0] },
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
            let img = renderer.render(&b.finish(out), &registry, &mut source).expect("render");
            let rgba = read_srgb8(ctx, renderer.pipelines(), &img).expect("readback");
            let digit = |x: u32| {
                let px = rgba[(((size[1] / 2) * size[0] + x) * 4) as usize] as f64;
                (((16.0 + px / 255.0 * 219.0) - 16.0) / 12.0).round() as u32
            };
            digit(size[0] / 4) + 16 * digit(size[0] * 3 / 4)
        })
        .collect()
}

/// A 3 second tone to mux in.
fn tone() -> Option<PathBuf> {
    let path = std::env::temp_dir().join("oa-media-tests").join("export_tone.wav");
    if !path.exists() {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=3:sample_rate=48000", "-ac", "2"])
            .arg(&path)
            .output()
            .ok()?;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    Some(path)
}

/// Exports the numbered clip with `encoder` and checks the file frame by frame.
fn round_trip(encoder: Encoder, name: &str) -> Option<String> {
    let source_path = numbered_clip()?;
    let Ok(ctx) = GpuContext::new_headless() else {
        eprintln!("skipping: no GPU");
        return None;
    };
    let ctx = Arc::new(ctx);
    if hardware_source(&ctx).is_err() {
        eprintln!("skipping: no hardware decoding");
        return None;
    }
    let probe = oa_media::probe(&source_path).expect("probe");
    let (project, seq, _) = project(&source_path, &probe);

    let out = std::env::temp_dir().join("oa-media-tests").join(name);
    let _ = std::fs::remove_file(&out);
    let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, ..Default::default() });
    let registry = Registry::with_builtins();
    let mut sources = hardware_source(&ctx).expect("decoder");
    sources.add(3, &source_path, probe.video.clone().expect("video"));

    let audio = vec![oa_audio::AudioClip::new(
        99,
        98,
        tone()?,
        TimeRange::new(Time::ZERO, Time::from_seconds(3)),
        Time::ZERO,
    )];
    let options = ExportOptions { codec: VideoCodec::H264, crf: 12, encoder, audio, ..Default::default() };
    let summary = export(&project, seq, &out, &options, &ctx, &mut renderer, &registry, &mut sources, |_, _| true).expect("export");
    eprintln!("{name}: {} at {:.0} fps", summary.encoder, summary.frames_per_second());

    assert_eq!(summary.frames, FRAMES as u64);
    assert_eq!(summary.size, SIZE);
    assert!(out.is_file());

    // The exported file must be readable, the right length, tagged BT.709, with sound.
    let exported = oa_media::probe(&out).expect("probe export");
    let video = exported.video.as_ref().expect("video");
    assert_eq!(video.index.len(), FRAMES, "one encoded frame per timeline frame");
    assert_eq!((video.width, video.height), (SIZE[0], SIZE[1]));
    assert_eq!(video.color.matrix, oa_gpu::YuvMatrix::Bt709, "color metadata should be written, not guessed");
    assert!(!video.color.full_range);
    let sound = exported.audio.as_ref().expect("the sound was muxed in");
    assert_eq!(sound.sample_rate, 48_000);
    assert!((exported.duration.as_seconds_f64() - 3.0).abs() < 0.1, "{:?}", exported.duration);

    // Every sampled frame of the export carries the number the timeline had there.
    let wanted: Vec<usize> = vec![0, 1, 7, 29, 30, 45, 60, 88, 89];
    let got = read_numbers(&ctx, &out, &wanted);
    let expected: Vec<u32> = wanted.iter().map(|f| *f as u32).collect();
    assert_eq!(got, expected, "exported frames must match the timeline");
    Some(summary.encoder)
}

#[test]
fn exported_file_contains_the_frames_the_timeline_showed() {
    round_trip(Encoder::Ffmpeg, "exported_roundtrip.mp4");
}

#[test]
fn media_foundation_export_matches_too() {
    if let Some(encoder) = round_trip(Encoder::MediaFoundation, "exported_roundtrip_mf.mp4") {
        assert!(encoder.starts_with("Media Foundation"), "{encoder}");
    }
}

/// A part of the timeline, as a GIF: only that part's frames are rendered (starting on
/// the frame it asks for), and ffmpeg writes a looping GIF at the GIF's own rate.
#[test]
fn gif_of_a_range() {
    let Some(source_path) = numbered_clip() else { return };
    let Ok(ctx) = GpuContext::new_headless() else { return };
    let ctx = Arc::new(ctx);
    if hardware_source(&ctx).is_err() {
        return;
    }
    let probe = oa_media::probe(&source_path).expect("probe");
    let (project, seq, _) = project(&source_path, &probe);
    let out = std::env::temp_dir().join("oa-media-tests").join("exported_range.gif");
    let _ = std::fs::remove_file(&out);
    let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, ..Default::default() });
    let registry = Registry::with_builtins();
    let mut sources = hardware_source(&ctx).expect("decoder");
    sources.add(3, &source_path, probe.video.clone().expect("video"));

    // Frames 30..60 (one second from 1 s), as a 10 fps GIF.
    let range = TimeRange::new(Time::from_seconds(1), Time::from_seconds(1));
    let options = ExportOptions { codec: VideoCodec::Gif { fps: 10 }, encoder: Encoder::Ffmpeg, range: Some(range), ..Default::default() };
    let mut exporter = oa_export::Exporter::start(&project, seq, &out, &options).expect("start");
    exporter.step(&project, &ctx, &mut renderer, &registry, &mut sources, 1).expect("first frame");
    let first_seen = exporter.last_frame().map(|(_, t)| *t);
    while !exporter.step(&project, &ctx, &mut renderer, &registry, &mut sources, 8).expect("step") {}
    assert_eq!(first_seen, Some(Time::from_seconds(1)), "rendering starts where the range does");
    let summary = exporter.finish().expect("finish");
    assert_eq!(summary.frames, 30, "only the range's frames");

    let out_probe = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,nb_read_frames", "-of", "csv=p=0"])
        .arg(&out)
        .output()
        .expect("ffprobe");
    let text = String::from_utf8_lossy(&out_probe.stdout).trim().to_string();
    let mut parts = text.split(',');
    assert_eq!(parts.next(), Some("gif"), "{text}");
    let frames: u32 = parts.next().and_then(|n| n.trim().parse().ok()).unwrap_or(0);
    assert!((9..=11).contains(&frames), "a second at 10 fps: {text}");
}

/// First frame of `path` as raw RGBA bytes (via ffmpeg).
fn first_rgba(path: &Path) -> Vec<u8> {
    // ffmpeg's own VP9 decoder drops the alpha channel; libvpx keeps it.
    let decoder: &[&str] = if path.extension().is_some_and(|e| e == "webm") { &["-c:v", "libvpx-vp9"] } else { &[] };
    let out = Command::new("ffmpeg")
        .args(["-v", "error"])
        .args(decoder)
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// A transparent background stays transparent in formats that can keep it: the clip,
/// shrunk to half size, is opaque, and the corners around it are see-through.
#[test]
fn transparency_survives_export() {
    let Some(source_path) = numbered_clip() else { return };
    let Ok(ctx) = GpuContext::new_headless() else { return };
    let ctx = Arc::new(ctx);
    if hardware_source(&ctx).is_err() {
        return;
    }
    let probe = oa_media::probe(&source_path).expect("probe");
    let (mut project, seq, _) = project(&source_path, &probe);
    {
        let s = Arc::make_mut(project.sequences.get_mut(&seq).unwrap());
        s.params.set(schema::BG_COLOR, oa_params::ParamSource::Static(oa_params::Value::Gradient(oa_params::Gradient::solid([0.0, 0.0, 0.0, 0.0]))));
        let track = Arc::make_mut(&mut s.tracks[0]);
        track.items[0].params.set(schema::SCALE, oa_params::ParamSource::Static(oa_params::Value::Vec2([0.5, 0.5])));
    }
    for (codec, name) in [(VideoCodec::ProRes4444, "transparent.mov"), (VideoCodec::Gif { fps: 10 }, "transparent.gif"), (VideoCodec::WebmAlpha, "transparent.webm")] {
        let out = std::env::temp_dir().join("oa-media-tests").join(name);
        let _ = std::fs::remove_file(&out);
        let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, ..Default::default() });
        let registry = Registry::with_builtins();
        let mut sources = hardware_source(&ctx).expect("decoder");
        sources.add(3, &source_path, probe.video.clone().expect("video"));
        let range = Some(TimeRange::new(Time::ZERO, Time::from_seconds_f64(0.5)));
        let options = ExportOptions { codec, encoder: Encoder::Ffmpeg, range, ..Default::default() };
        export(&project, seq, &out, &options, &ctx, &mut renderer, &registry, &mut sources, |_, _| true).expect("export");
        let rgba = first_rgba(&out);
        let alpha = |x: u32, y: u32| rgba[((y * SIZE[0] + x) * 4 + 3) as usize];
        assert_eq!(alpha(4, 4), 0, "{name}: the corner is see-through");
        assert_eq!(alpha(SIZE[0] / 2, SIZE[1] / 2), 255, "{name}: the clip is opaque");
    }
}

/// A compound clip as the background texture really draws: a red compound tiled behind
/// a small clip shows red in the corners, even past the compound's own end (it loops).
#[test]
fn compound_clip_background_renders() {
    let Ok(ctx) = GpuContext::new_headless() else { return };
    let ctx = Arc::new(ctx);
    let (seq, inner) = (SeqId(1), SeqId(10));
    let variant = |id| FormatVariant { id: VariantId(id), name: "v".into(), size: CanvasSize::new(64, 64), overrides: BTreeMap::new() };
    let solid = |id, color: [f64; 4], secs: i64| {
        let mut item = Item::new(ItemId(id), "solid", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(secs)));
        item.params.set(schema::SOLID_COLOR, oa_params::ParamSource::Static(oa_params::Value::Color(color)));
        item
    };
    let mut p = Project::new("bg");
    let mut pattern = Sequence::new(inner, "Pattern", FrameRate::FPS_30, variant(11));
    let mut t = Track::new(TrackId(12), "V1", TrackKind::Video);
    t.items.push(solid(13, [1.0, 0.0, 0.0, 1.0], 1));
    pattern.tracks.push(Arc::new(t));
    p.sequences.insert(inner, Arc::new(pattern));
    let mut main = Sequence::new(seq, "Main", FrameRate::FPS_30, variant(2));
    let mut t = Track::new(TrackId(3), "V1", TrackKind::Video);
    let mut small = solid(4, [0.0, 0.0, 1.0, 1.0], 5);
    small.params.set(schema::SCALE, oa_params::ParamSource::Static(oa_params::Value::Vec2([0.25, 0.25])));
    t.items.push(small);
    main.tracks.push(Arc::new(t));
    main.params.set(schema::BG_MODE, oa_params::ParamSource::Static(oa_params::Value::Enum("texture".into())));
    main.params.set(schema::BG_TEXTURE, oa_params::ParamSource::Static(oa_params::Value::Media(Some(inner.0))));
    p.sequences.insert(seq, Arc::new(main));

    let registry = Registry::with_builtins();
    let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, ..Default::default() });
    let mut sources = oa_gpu::TestPatternSource { frames_generated: 0 };
    for at in [0.5, 3.5] {
        let planned = oa_plan::plan_frame(&p, seq, Time::from_seconds_f64(at), &Default::default(), &registry).expect("plan");
        let graph = optimize(&planned.graph, OptLevel::Full, KeyContext::default());
        let image = renderer.render(&graph, &registry, &mut sources).expect("render");
        let px = read_srgb8(&ctx, renderer.pipelines(), &image).expect("read");
        let at_xy = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 3];
        assert_eq!(at_xy(2, 2), [255, 0, 0], "at {at}s: the red compound in the corner");
        assert_eq!(at_xy(32, 32), [0, 0, 255], "at {at}s: the clip in front");
    }
}

/// The app's renderer doesn't wait for shaders or glyphs (it keeps the last frame up
/// instead); an export on it still gets every frame, because the exporter waits itself.
/// It used to fail with "rendering failed: still preparing".
#[test]
fn export_waits_for_what_the_preview_would_skip() {
    if !tool("ffmpeg") {
        return;
    }
    let Ok(ctx) = GpuContext::new_headless() else { return };
    let ctx = Arc::new(ctx);
    let seq = SeqId(1);
    let variant = FormatVariant { id: VariantId(2), name: "v".into(), size: CanvasSize::new(64, 64), overrides: BTreeMap::new() };
    let mut p = Project::new("text");
    let mut main = Sequence::new(seq, "Main", FrameRate::FPS_30, variant);
    let mut t = Track::new(TrackId(3), "V1", TrackKind::Video);
    let mut title = Item::new(ItemId(4), "title", ItemKind::Text, TimeRange::new(Time::ZERO, Time::from_seconds_f64(0.3)));
    title.params.set(schema::TEXT_CONTENT, oa_params::ParamSource::Static(oa_params::Value::Text("Hi".into())));
    t.items.push(title);
    main.tracks.push(Arc::new(t));
    p.sequences.insert(seq, Arc::new(main));

    let registry = Registry::with_builtins();
    let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Async, wait: false, ..Default::default() });
    let mut sources = oa_gpu::TestPatternSource { frames_generated: 0 };
    let out = std::env::temp_dir().join("oa-media-tests").join("waits.gif");
    let _ = std::fs::create_dir_all(out.parent().expect("dir"));
    let options = ExportOptions { codec: VideoCodec::Gif { fps: 10 }, encoder: Encoder::Ffmpeg, ..Default::default() };
    let mut exporter = oa_export::Exporter::start(&p, seq, &out, &options).expect("start");
    while !exporter.step(&p, &ctx, &mut renderer, &registry, &mut sources, 4).expect("every frame renders") {}
    exporter.finish().expect("finish");
    assert!(!renderer.options.wait, "the renderer is left as it was");
}

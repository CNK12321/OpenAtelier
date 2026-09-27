//! Where an export's time goes (not a test of correctness; `cargo test --release -p
//! oa-export --test bench_export -- --ignored --nocapture`). 10 s of 1080p30 footage with a
//! title, a blur and a color effect over it, and sound, into a 1080p file — through Media
//! Foundation's encoder and x264. Knobs: `OA_BENCH_4K` (4K footage), `OA_BENCH_FFMPEG`
//! (decode with ffmpeg, as on Linux), `OA_BENCH_BLUR=80` (a heavier picture).

#![cfg(windows)]

use oa_doc::*;
use oa_export::{export, Encoder, ExportOptions, VideoCodec};
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer};
use oa_graph::registry::Registry;
use oa_params::{ParamSource, Value};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;

#[test]
#[ignore]
fn where_the_time_goes() {
    let dir = std::env::temp_dir().join("oa-media-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let uhd = std::env::var("OA_BENCH_4K").is_ok();
    let clip = dir.join(if uhd { "bench_2160p.mp4" } else { "bench_1080p.mp4" });
    if !clip.exists() {
        assert!(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={}:r=30:d=10", if uhd { "3840x2160" } else { "1920x1080" }), "-f", "lavfi", "-i", "sine=frequency=330:duration=10"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&clip)
            .status()
            .unwrap()
            .success());
    }
    let ctx = Arc::new(GpuContext::new_headless().unwrap());
    let probe = oa_media::probe(&clip).unwrap();
    let video = probe.video.clone().unwrap();

    let mut p = Project::new("bench");
    let variant = FormatVariant { id: VariantId(5), name: "HD".into(), size: CanvasSize::new(1920, 1080), overrides: BTreeMap::new() };
    let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_30, variant);
    let ten = TimeRange::new(Time::ZERO, Time::from_seconds(10));
    let mut v1 = Track::new(TrackId(2), "V1", TrackKind::Video);
    let mut footage = Item::new(ItemId(4), "clip", ItemKind::Media { media: MediaId(3) }, ten);
    let mut blur = EffectInstance::new(EffectId(10), "oa.blur.gaussian");
    blur.params.set("radius", ParamSource::Static(Value::Float(std::env::var("OA_BENCH_BLUR").ok().and_then(|v| v.parse().ok()).unwrap_or(6.0))));
    footage.effects.push(blur);
    footage.effects.push(EffectInstance::new(EffectId(11), "oa.color.contrast"));
    v1.items.push(footage);
    let mut v2 = Track::new(TrackId(6), "V2", TrackKind::Video);
    let mut title = Item::new(ItemId(7), "title", ItemKind::Text, ten);
    title.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text("Where the time goes".into())));
    title.params.set(schema::TEXT_SIZE, ParamSource::Static(Value::Float(110.0)));
    v2.items.push(title);
    seq.tracks.push(Arc::new(v1));
    seq.tracks.push(Arc::new(v2));
    p.sequences.insert(SeqId(1), Arc::new(seq));
    let info = MediaInfo { width: video.width, height: video.height, duration: probe.duration, rate: video.avg_rate, has_video: true, has_audio: true, still: false, ..Default::default() };
    p.media.insert(MediaId(3), Arc::new(MediaRef { id: MediaId(3), path: clip.to_string_lossy().to_string(), fingerprint: Some("bench".into()), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
    let audio = vec![oa_audio::AudioClip::new(4, 3, clip.clone(), ten, Time::ZERO)];

    for (encoder, name) in [(Encoder::MediaFoundation, "bench_mf.mp4"), (Encoder::Ffmpeg, "bench_x264.mp4")] {
        let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, wait: true, ..Default::default() });
        let registry = Registry::with_builtins();
        let mut sources = if std::env::var("OA_BENCH_FFMPEG").is_ok() { oa_media::frame_source(&ctx, oa_media::DecoderChoice { force_ffmpeg: true, ..Default::default() }) } else { oa_media::frame_source(&ctx, oa_media::DecoderChoice { bridge: if ctx.is_dx12() { oa_media::windows::D3D11Bridge::new(&ctx).ok() } else { None }, force_ffmpeg: false }) };
        sources.add(3, &clip, video.clone());
        let options = ExportOptions { codec: VideoCodec::H264, encoder, audio: audio.clone(), ..Default::default() };
        let s = export(&p, SeqId(1), &dir.join(name), &options, &ctx, &mut renderer, &registry, &mut sources, |_, _| true).unwrap();
        eprintln!("{name}: {} frames in {:.2}s = {:.0} fps with {} — {}", s.frames, s.seconds_elapsed, s.frames_per_second(), s.encoder, s.timings);
    }
}

//! `oa` — inspect projects, render plans and rendered frames.
//!
//!   oa presets
//!   oa demo [out.oaproj.json]
//!   oa plan <project> <seconds> [--variant <name>] [--scale <0..1>] [--reference]
//!   oa render <project> <seconds> <out.png> [--variant <name>] [--scale <0..1>] [--reference]
//!   oa bench <project> <seconds> [...]
//!   oa export <project> <out.mp4> [--variant <name>] [--scale <0..1>] [--codec ...] [--crf <n>]
//!   oa stress <minutes> [--intensity 0..1] [--media <files>...]   (how long would it take?)
//!
//!   oa gpu                                                          (which GPU is used?)
//!
//! Media is decoded by Media Foundation on Windows where it can, by ffmpeg elsewhere; files that aren't loaded (or
//! can't be decoded) render as a GPU test pattern.

use oa_doc::*;
use oa_gpu::{readback, FusionMode, GpuContext, RenderOptions, Renderer, TestPatternSource};
use oa_graph::registry::Registry;
use oa_graph::{optimize, Graph, KeyContext, OptLevel};
use oa_params::{Curve, Keyframe, KeyframeAnchor, ParamId, ParamSource, Value};
use oa_plan::{plan_frame, PlanOptions};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("presets") => {
            presets();
            Ok(())
        }
        Some("demo") => demo(&args[1..]),
        Some("probe") => probe_cmd(&args[1..]),
        Some("plan") => plan(&args[1..]),
        Some("render") => render(&args[1..]),
        Some("bench") => bench(&args[1..]),
        Some("export") => export(&args[1..]),
        Some("stress") => stress(&args[1..]),
        Some("gpu") => gpu_cmd(),
        _ => {
            eprintln!(
                "usage:\n  oa presets\n  oa probe <video>\n  oa demo [out.oaproj.json] [--media <video>]\n  oa plan <project> <seconds> [--variant <name>] [--scale <s>] [--reference]\n  oa render <project> <seconds> <out.png> [--variant <name>] [--scale <s>] [--reference]\n  oa bench <project> <seconds> [--variant <name>] [--scale <s>] [--reference]
  oa export <project> <out.mp4> [--variant <name>] [--scale <s>] [--codec h264|hevc|prores] [--crf <n>] [--encoder auto|mf|ffmpeg] [--reference]
  oa stress <minutes> [--intensity 0..1] [--media <files>...] [--sample <s> | --full] [--seed <n>] [--size WxH] [--fps <n>] [--encoder auto|mf|ffmpeg] [--out project.json] [--export out.mp4]"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type Res = Result<(), Box<dyn std::error::Error>>;

fn presets() {
    println!("{:<16} {:<20} {:>11} {:>11}  platforms", "id", "name", "1080", "2160");
    for p in PRESETS {
        let (a, b) = (p.size(1080), p.size(2160));
        println!(
            "{:<16} {:<20} {:>11} {:>11}  {}",
            p.id,
            p.name,
            format!("{}x{}", a.width, a.height),
            format!("{}x{}", b.width, b.height),
            p.platforms
        );
    }
}

fn variant(doc: &mut Document, preset: &str) -> FormatVariant {
    let p = AspectPreset::by_id(preset).expect("known preset");
    FormatVariant { id: VariantId(doc.alloc_id()), name: p.name.into(), size: p.size(1080), overrides: BTreeMap::new() }
}

fn probe_cmd(args: &[String]) -> Res {
    let [path] = args else {
        return Err("usage: oa probe <video>".into());
    };
    let path = std::path::Path::new(path);
    let started = std::time::Instant::now();
    let p = oa_media::probe(path)?;
    let fp = oa_media::fingerprint(path)?;
    println!("{}", path.display());
    println!("  container    {}   duration {:?}", p.container, p.duration);
    if let Some(v) = &p.video {
        let index = &v.index;
        let still = if v.still { "  [still image]" } else { "" };
        println!("  video        {} ({}), {}x{}{still}", v.codec, v.pixel_format, v.width, v.height);
        println!("  frames       {} ({} keyframes, max GOP {}, variable rate: {})", index.len(), index.keyframes.len(), index.max_gop(), index.is_variable_rate());
        println!("  time base    {:?}   average rate {:?}", v.time_base, v.avg_rate);
        let rotation = if v.rotation_quarter_turns > 0 { format!("  [rotated {} quarter turns]", v.rotation_quarter_turns) } else { String::new() };
        println!(
            "  color        {:?}{}{}{rotation}",
            v.color,
            if v.hdr { "  [HDR: 8-bit decode, may band]" } else { "" },
            if v.has_alpha { "  [alpha]" } else { "" }
        );
    }
    match &p.audio {
        Some(a) => println!("  audio        {} {} ch {} Hz", a.codec, a.channels, a.sample_rate),
        None => println!("  audio        none"),
    }
    println!("  fingerprint  {fp}");
    println!("  probed in    {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}

/// Builds a small project entirely through edit ops, as the UI would.
fn demo(args: &[String]) -> Res {
    let mut out = "demo.oaproj.json".to_string();
    let mut media_file = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--media" => media_file = Some(it.next().ok_or("--media needs a file")?.clone()),
            other => out = other.to_string(),
        }
    }
    let out = out.as_str();
    let mut doc = Document::new(Project::new("Demo"));
    let secs = Time::from_seconds;
    let (seq, v1, v2, media, clip, title) = (
        SeqId(doc.alloc_id()),
        TrackId(doc.alloc_id()),
        TrackId(doc.alloc_id()),
        MediaId(doc.alloc_id()),
        ItemId(doc.alloc_id()),
        ItemId(doc.alloc_id()),
    );
    let wide = variant(&mut doc, "landscape-16x9");
    let tall = variant(&mut doc, "vertical-9x16");
    let tall_id = tall.id;

    let mut item = Item::new(clip, "Drone shot", ItemKind::Media { media }, TimeRange::new(secs(0), secs(8)));
    item.effects.push(EffectInstance {
        id: EffectId(doc.alloc_id()),
        type_id: "oa.color.exposure".into(),
        type_version: 1,
        enabled: true,
        params: Default::default(),
        role: Default::default(),
    });
    item.effects.push(EffectInstance {
        id: EffectId(doc.alloc_id()),
        type_id: "oa.color.saturation".into(),
        type_version: 1,
        enabled: true,
        params: Default::default(),
        role: Default::default(),
    });
    // Blur intensity is keyframed: sharp at 0s, soft by 6s.
    let mut blur = oa_params::ParamSet::default();
    blur.set(
        "radius",
        ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::ease(secs(0), Value::Float(0.0)), Keyframe::linear(secs(6), Value::Float(24.0))],
        )),
    );
    item.effects.push(EffectInstance {
        id: EffectId(doc.alloc_id()),
        type_id: "oa.blur.gaussian".into(),
        type_version: 1,
        enabled: true,
        params: blur,
        role: Default::default(),
    });
    let mut card = Item::new(title, "Color card", ItemKind::Solid, TimeRange::new(secs(2), secs(3)));
    card.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color([0.9, 0.3, 0.1, 1.0])));
    // The card shakes around the center (1.5% of the canvas at 9 Hz).
    card.params.set(schema::POSITION, ParamSource::Static(Value::Vec2([0.0, 0.0])).wiggle(0.015, 9.0, 7));
    card.params.set(schema::FIT, ParamSource::Static(Value::Enum("fit".into())));
    card.params.set(
        schema::SCALE,
        ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::ease(secs(0), Value::Vec2([0.2, 0.2])), Keyframe::linear(secs(1), Value::Vec2([0.4, 0.4]))],
        )),
    );
    card.params.set(
        schema::SQUASH,
        ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::ease(secs(0), Value::Float(0.4)), Keyframe::linear(secs(1), Value::Float(0.0))],
        )),
    );

    let (media_ref, rate) = match &media_file {
        Some(file) => {
            let path = std::fs::canonicalize(file)?;
            let p = oa_media::probe(&path)?;
            let v = p.video.clone().ok_or("file has no video track")?;
            let info = MediaInfo { width: v.width, height: v.height, duration: p.duration, rate: v.avg_rate, has_video: true, has_audio: p.has_audio(), still: v.still, color: oa_doc::color::ColorTags { transfer: v.transfer_tag.clone(), primaries: v.primaries_tag.clone() } };
            let fingerprint = Some(oa_media::fingerprint(&path)?);
            let path = path.to_string_lossy().trim_start_matches(r"\\?\").to_string();
            (MediaRef { id: media, path, fingerprint, info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }, v.avg_rate.unwrap_or(FrameRate::FPS_29_97))
        }
        None => {
            let info = MediaInfo { width: 3840, height: 2160, duration: secs(30), rate: Some(FrameRate::FPS_29_97), has_video: true, has_audio: true, still: false, ..Default::default() };
            (MediaRef { id: media, path: "drone.mp4".into(), fingerprint: Some("demo-fingerprint".into()), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }, FrameRate::FPS_29_97)
        }
    };
    let mut sequence = Sequence::new(seq, "Main", rate, wide);
    sequence.variants.push(tall);
    doc.edit(
        "Create demo",
        vec![
            Op::AddSequence(Arc::new(sequence)),
            Op::AddMedia(Arc::new(media_ref)),
            Op::InsertTrack { seq, index: 0, track: Arc::new(Track::new(v1, "V1", TrackKind::Video)) },
            Op::InsertTrack { seq, index: 1, track: Arc::new(Track::new(v2, "V2", TrackKind::Video)) },
            Op::InsertItem { seq, track: v1, item },
            Op::InsertItem { seq, track: v2, item: card },
            // Vertical cut keeps the subject on the right side of the frame.
            Op::SetParam {
                seq,
                item: clip,
                target: ParamTarget::VariantOverride(tall_id),
                param: ParamId::new(schema::FOCUS),
                source: Some(ParamSource::Static(Value::Vec2([0.7, 0.5]))),
            },
        ],
    )?;
    std::fs::write(out, ProjectFile::to_json(doc.project())?)?;
    println!("wrote {out}");
    println!("try:  oa plan {out} 2.5 --variant \"Vertical 9:16\" --scale 0.5");
    println!("      oa render {out} 2.5 frame.png --variant \"Vertical 9:16\"");
    Ok(())
}

/// A loaded project plus the command-line planning options.
struct Session {
    project: Project,
    seq: SeqId,
    opts: PlanOptions,
    level: OptLevel,
}

struct Planned {
    graph: Graph,
    unoptimized_nodes: usize,
    report: oa_plan::PlanReport,
}

fn session(path: &str, rest: &[String]) -> Result<Session, Box<dyn std::error::Error>> {
    let (project, report) = ProjectFile::load(&std::fs::read_to_string(path)?)?;
    for m in report.repaired.iter().map(|m| format!("repaired: {m}")).chain(report.warnings) {
        eprintln!("{path}: {m}");
    }
    let (seq, s) = project.sequences.iter().next().ok_or("project has no sequences")?;
    let seq = *seq;
    let mut opts = PlanOptions::default();
    let mut level = OptLevel::Full;
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--variant" => {
                let name = it.next().ok_or("--variant needs a name")?;
                let v = s.variants.iter().find(|v| &v.name == name).ok_or_else(|| {
                    let names: Vec<_> = s.variants.iter().map(|v| v.name.as_str()).collect();
                    format!("no variant {name:?}; have {names:?}")
                })?;
                opts.variant = Some(v.id);
            }
            "--scale" => opts.render_scale = it.next().ok_or("--scale needs a value")?.parse()?,
            "--reference" => level = OptLevel::Reference,
            other => return Err(format!("unknown flag {other}").into()),
        }
    }
    Ok(Session { project, seq, opts, level })
}

impl Session {
    fn sequence(&self) -> &Sequence {
        self.project.sequence(self.seq).expect("validated when loading")
    }

    fn plan(&self, t: Time, registry: &Registry) -> Result<Planned, Box<dyn std::error::Error>> {
        let planned = plan_frame(&self.project, self.seq, t, &self.opts, registry)?;
        Ok(Planned {
            graph: optimize(&planned.graph, self.level, KeyContext::default()),
            unoptimized_nodes: planned.graph.live_count(),
            report: planned.report,
        })
    }

    fn print_header(&self, t: Time, p: &Planned) {
        let seq = self.sequence();
        println!(
            "frame {} of {:?} ({} nodes planned, {} after {:?})",
            seq.rate.frame_at(t),
            seq.name,
            p.unoptimized_nodes,
            p.graph.live_count(),
            self.level
        );
        if p.report != Default::default() {
            println!("warnings: {:?}", p.report);
        }
    }

    fn renderer(&self, ctx: &Arc<GpuContext>, registry: &Registry) -> Result<Renderer, Box<dyn std::error::Error>> {
        let fusion = if self.level == OptLevel::Reference { FusionMode::Off } else { FusionMode::Blocking };
        let renderer = Renderer::new(ctx.clone(), RenderOptions { fusion, ..Default::default() });
        renderer.warm_up(registry)?;
        Ok(renderer)
    }
}

/// Decoded media for every file that exists (Media Foundation's hardware decoder on
/// Windows where it can, ffmpeg otherwise); the test pattern for the rest.
fn media_source(project: &Project, ctx: &Arc<GpuContext>) -> Box<dyn oa_gpu::FrameSource> {
    let files: Vec<_> = project.media.values().filter(|m| std::path::Path::new(&m.path).exists()).collect();
    if files.is_empty() {
        return Box::new(TestPatternSource::default());
    }
    let choice = oa_media::DecoderChoice {
        #[cfg(windows)]
        bridge: oa_media::windows::D3D11Bridge::new(ctx).ok(),
        force_ffmpeg: std::env::var("OA_DECODER").is_ok_and(|v| v == "ffmpeg"),
    };
    let mut source = oa_media::frame_source(ctx, choice);
    // Pictures go to the still decoder (uploaded once), videos to the video one; the
    // video source hands anything it doesn't know to the stills.
    let mut stills = oa_media::StillSource::default();
    for m in files {
        match oa_media::probe(std::path::Path::new(&m.path)) {
            Ok(p) => match p.video {
                Some(video) if video.still => {
                    if let Err(e) = stills.add(ctx, m.id.0, std::path::Path::new(&m.path), &video) {
                        eprintln!("warning: {}: {e}", m.path);
                    }
                }
                Some(video) => source.add(m.id.0, &m.path, video),
                None => {}
            },
            Err(e) => eprintln!("warning: {}: {e}", m.path),
        }
    }
    source.fallback = Some(if stills.is_empty() { Box::new(TestPatternSource::default()) } else { Box::new(stills) });
    Box::new(source)
}

/// Every GPU this machine offers, what (if anything) keeps each from running the
/// renderer, and which one would be used.
fn gpu_cmd() -> Res {
    let adapters = pollster::block_on(async {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::all();
        wgpu::Instance::new(desc).enumerate_adapters(wgpu::Backends::all()).await
    });
    if adapters.is_empty() {
        println!("no GPU adapters found (a software renderer is used)");
    }
    for a in &adapters {
        let problems = oa_gpu::select::shortcomings(a);
        let info = a.get_info();
        println!("{}", oa_gpu::select::describe(&info));
        println!("    driver: {} {}", info.driver, info.driver_info);
        println!("    {}", if problems.is_empty() { "can run everything".to_string() } else { format!("lacks: {}", problems.join("; ")) });
    }
    let ctx = GpuContext::new_preferred(&oa_gpu::GpuPreference::new("auto", ""))?;
    println!("\nwould use: {}", ctx.describe());
    println!("choose with OA_GPU_BACKEND=dx12|vulkan|metal|gl or OA_GPU_ADAPTER=<part of a name>; OA_DECODER=ffmpeg decodes video with ffmpeg");
    Ok(())
}

fn plan(args: &[String]) -> Res {
    let [path, seconds, rest @ ..] = args else {
        return Err("usage: oa plan <project> <seconds> [--variant <name>] [--scale <s>] [--reference]".into());
    };
    let s = session(path, rest)?;
    let t = Time::from_seconds_f64(seconds.parse()?);
    let planned = s.plan(t, &Registry::with_builtins())?;
    s.print_header(t, &planned);
    print!("{}", planned.graph.describe());
    Ok(())
}

fn render(args: &[String]) -> Res {
    let [path, seconds, out, rest @ ..] = args else {
        return Err("usage: oa render <project> <seconds> <out.png> [--variant <name>] [--scale <s>] [--reference]".into());
    };
    let s = session(path, rest)?;
    let t = Time::from_seconds_f64(seconds.parse()?);
    let registry = Registry::with_builtins();
    let planned = s.plan(t, &registry)?;
    s.print_header(t, &planned);

    let started = std::time::Instant::now();
    let ctx = Arc::new(GpuContext::new_headless()?);
    let mut renderer = s.renderer(&ctx, &registry)?;
    let mut source = media_source(&s.project, &ctx);
    let setup = started.elapsed();

    let started = std::time::Instant::now();
    let image = renderer.render(&planned.graph, &registry, source.as_mut())?;
    let rgba = readback::read_srgb8(&ctx, renderer.pipelines(), &image)?;
    let elapsed = started.elapsed();

    let file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut encoder = png::Encoder::new(file, image.size[0], image.size[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;

    let st = &renderer.stats;
    println!(
        "{} ({:?}): {}x{} rendered + read back in {:.1} ms ({:.0} ms device and pipeline setup), {} passes, {} fused chains",
        ctx.info.name,
        ctx.info.backend,
        image.size[0],
        image.size[1],
        elapsed.as_secs_f64() * 1000.0,
        setup.as_secs_f64() * 1000.0,
        st.passes,
        st.fused_chains
    );
    for u in &st.unsupported {
        println!("not rendered: {u}");
    }
    println!("wrote {out}");
    Ok(())
}

/// Sequential playback benchmark: plan, decode and render every frame (no readback),
/// waiting for the GPU after each frame so timings are real.
fn bench(args: &[String]) -> Res {
    let [path, seconds, rest @ ..] = args else {
        return Err("usage: oa bench <project> <seconds> [--variant <name>] [--scale <s>] [--reference]".into());
    };
    let s = session(path, rest)?;
    let registry = Registry::with_builtins();
    let ctx = Arc::new(GpuContext::new_headless()?);
    let mut renderer = s.renderer(&ctx, &registry)?;
    let mut source = media_source(&s.project, &ctx);
    let rate = s.sequence().rate;
    let frames = (seconds.parse::<f64>()? * rate.as_f64()).round() as i64;

    let mut times = Vec::with_capacity(frames as usize);
    let mut size = [0, 0];
    let total = std::time::Instant::now();
    for f in 0..frames {
        let started = std::time::Instant::now();
        let planned = s.plan(rate.frame_start(f), &registry)?;
        let image = renderer.render(&planned.graph, &registry, source.as_mut())?;
        ctx.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None })?;
        size = image.size;
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let total = total.elapsed().as_secs_f64();
    times.sort_by(f64::total_cmp);
    let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
    println!(
        "{} ({:?}): {frames} frames at {}x{} in {:.2} s = {:.1} fps (target {:.2})",
        ctx.info.name,
        ctx.info.backend,
        size[0],
        size[1],
        total,
        frames as f64 / total,
        rate.as_f64()
    );
    println!("  per frame: median {:.1} ms, p95 {:.1} ms, max {:.1} ms (first frame includes decoder start-up)", pct(0.5), pct(0.95), pct(1.0));
    Ok(())
}

/// Renders the whole sequence to a file, sound included.
fn export(args: &[String]) -> Res {
    let [path, out, rest @ ..] = args else {
        return Err("usage: oa export <project> <out.mp4> [--variant <name>] [--scale <s>] [--codec h264|hevc|prores] [--crf <n>] [--encoder auto|mf|ffmpeg] [--reference]".into());
    };
    // `--codec` and `--crf` are ours; everything else is the shared session parsing.
    let (mut codec, mut crf, mut encoder) = (oa_export::VideoCodec::H264, 18, oa_export::Encoder::Auto);
    let mut session_args = Vec::new();
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--codec" => {
                codec = match it.next().map(String::as_str) {
                    Some("h264") => oa_export::VideoCodec::H264,
                    Some("hevc") => oa_export::VideoCodec::Hevc,
                    Some("prores") => oa_export::VideoCodec::ProRes,
                    other => return Err(format!("unknown codec {other:?}").into()),
                }
            }
            "--crf" => crf = it.next().ok_or("--crf needs a value")?.parse()?,
            "--encoder" => {
                encoder = match it.next().map(String::as_str) {
                    Some("auto") => oa_export::Encoder::Auto,
                    Some("mf") => oa_export::Encoder::MediaFoundation,
                    Some("ffmpeg") => oa_export::Encoder::Ffmpeg,
                    other => return Err(format!("unknown encoder {other:?}").into()),
                }
            }
            other => {
                session_args.push(other.to_string());
                if other == "--variant" || other == "--scale" {
                    session_args.push(it.next().ok_or_else(|| format!("{other} needs a value"))?.clone());
                }
            }
        }
    }
    let s = session(path, &session_args)?;
    let registry = Registry::with_builtins();
    let ctx = Arc::new(GpuContext::new_headless()?);
    let mut renderer = s.renderer(&ctx, &registry)?;
    let mut source = media_source(&s.project, &ctx);

    let (audio, buses) = audio_clips(&s);
    let options = oa_export::ExportOptions {
        variant: s.opts.variant,
        scale: s.opts.render_scale,
        codec,
        crf,
        reference: s.level == OptLevel::Reference,
        audio,
        buses,
        encoder,
        range: None,
    };
    let out_path = std::path::Path::new(out);
    let mut last_percent = -1i64;
    let summary = oa_export::export(&s.project, s.seq, out_path, &options, &ctx, &mut renderer, &registry, source.as_mut(), |frame, total| {
        let percent = (frame * 100 / total.max(1)) as i64;
        if percent != last_percent {
            last_percent = percent;
            print!("\rexporting {percent}% ({frame}/{total})");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        true
    })?;
    println!();
    println!(
        "wrote {out}: {} frames at {}x{}, {:.2}s of video{}, in {:.1}s ({:.1} fps) with {}",
        summary.frames,
        summary.size[0],
        summary.size[1],
        summary.duration.as_seconds_f64(),
        if summary.audio_seconds > 0.0 { format!(" and {:.2}s of sound", summary.audio_seconds) } else { String::new() },
        summary.seconds_elapsed,
        summary.frames_per_second(),
        summary.encoder
    );
    println!("  {}", summary.timings);
    Ok(())
}

/// For `oa_export::audio_clips`: the file to decode for a media id, if it has sound. The
/// project's own record says so when it has one; otherwise the file is probed — once per
/// file, not once per clip (a long edit has thousands of clips of a few files).
fn sound_of(project: &Project) -> impl Fn(MediaId) -> Option<std::path::PathBuf> + '_ {
    let probed = std::cell::RefCell::new(std::collections::HashMap::<MediaId, bool>::new());
    move |media| {
        let m = project.media(media)?;
        let path = std::path::PathBuf::from(&m.path);
        let audible = *probed.borrow_mut().entry(media).or_insert_with(|| {
            path.is_file() && m.info.as_ref().map_or_else(|| oa_media::probe(&path).is_ok_and(|p| p.has_audio()), |i| i.has_audio)
        });
        audible.then_some(path)
    }
}

/// Audible clips in the project, for muxing into an export.
fn audio_clips(s: &Session) -> (Vec<oa_audio::AudioClip>, Vec<oa_audio::AudioBus>) {
    let (clips, buses, skipped) = oa_export::audio_mix(&s.project, s.seq, sound_of(&s.project));
    if !skipped.is_empty() {
        eprintln!("note: {} clip(s) with speed changes are silent in this build", skipped.len());
    }
    (clips, buses)
}

/// `oa stress <minutes>`: how long would a video this long, edited this hard, take to
/// export? Builds a stand-in project (`oa_edit::stress`) from the media given — cuts,
/// transitions, overlays, titles, sound, effects, keyframes and waves, more of all of it
/// the higher the intensity — exports a slice of it for real and projects the whole.
fn stress(args: &[String]) -> Res {
    const USAGE: &str = "usage: oa stress <minutes> [--intensity 0..1] [--media <file>...] [--seed <n>] [--sample <seconds> | --full] [--size WxH] [--fps <n>] [--encoder auto|mf|ffmpeg] [--out project.oaproj.json] [--export out.mp4]";
    let [minutes, rest @ ..] = args else { return Err(USAGE.into()) };
    let minutes: f64 = minutes.parse().map_err(|_| USAGE)?;
    let mut recipe = oa_edit::stress::Recipe { length: Time::from_seconds_f64(minutes * 60.0), ..Default::default() };
    let (mut files, mut sample, mut full, mut out, mut keep) = (Vec::new(), None::<f64>, false, None, None);
    let mut encoder = oa_export::Encoder::Auto;
    let mut it = rest.iter().peekable();
    while let Some(flag) = it.next() {
        let flag = flag.as_str();
        if flag == "--full" {
            full = true;
            continue;
        }
        let value = it.next().cloned().ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?;
        match flag {
            "--intensity" => recipe.intensity = value.parse()?,
            "--seed" => recipe.seed = value.parse()?,
            "--sample" => sample = Some(value.parse()?),
            "--fps" => recipe.rate = FrameRate::new(value.parse::<u32>()?, 1),
            "--size" => {
                let (w, h) = value.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
                recipe.size = CanvasSize { width: w.parse()?, height: h.parse()? };
            }
            "--out" => out = Some(value),
            "--export" => keep = Some(value),
            "--encoder" => {
                encoder = match value.as_str() {
                    "auto" => oa_export::Encoder::Auto,
                    "mf" => oa_export::Encoder::MediaFoundation,
                    "ffmpeg" => oa_export::Encoder::Ffmpeg,
                    other => return Err(format!("unknown encoder {other}").into()),
                }
            }
            "--media" => {
                files.push(value);
                // Any more files, up to the next flag.
                while let Some(next) = it.next_if(|a| !a.starts_with("--")) {
                    files.push(next.clone());
                }
            }
            other => return Err(format!("unknown option {other}\n{USAGE}").into()),
        }
    }

    // The pool: the files given, probed; or, with none, stand-ins that render as the
    // test pattern (the GPU and the encoder measured without any decoding).
    use oa_edit::stress::{Source, SourceKind};
    let mut doc = Document::new(Project::new("Stress test"));
    let mut sources = Vec::new();
    let mut media_ops = Vec::new();
    for file in &files {
        let path = std::fs::canonicalize(file).map_err(|e| format!("{file}: {e}"))?;
        let p = oa_media::probe(&path).map_err(|e| format!("{file}: {e}"))?;
        let id = MediaId(doc.alloc_id());
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (kind, width, height, rate, still) = match &p.video {
            Some(v) if v.still => (SourceKind::Still, v.width, v.height, None, true),
            Some(v) => (SourceKind::Video, v.width, v.height, v.avg_rate, false),
            None if p.has_audio() => (SourceKind::Audio, 0, 0, None, false),
            None => continue,
        };
        let info = MediaInfo { width, height, duration: p.duration, rate, has_video: p.video.is_some(), has_audio: p.has_audio(), still, ..Default::default() };
        let path = path.to_string_lossy().trim_start_matches(r"\\?\").to_string();
        media_ops.push(Op::AddMedia(Arc::new(MediaRef { id, path, fingerprint: None, info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() })));
        sources.push(Source { media: id, kind, duration: p.duration, name: name.clone() });
        // A video with sound is also something for the sound tracks.
        if kind == SourceKind::Video && p.has_audio() {
            sources.push(Source { media: id, kind: SourceKind::Audio, duration: p.duration, name });
        }
    }
    if files.is_empty() {
        for i in 0..3 {
            let id = MediaId(doc.alloc_id());
            let duration = Time::from_seconds(120);
            let info = MediaInfo { width: 1920, height: 1080, duration, rate: Some(FrameRate::FPS_30), has_video: true, ..Default::default() };
            let path = format!("stand-in-{i}.mp4");
            media_ops.push(Op::AddMedia(Arc::new(MediaRef { id, path, fingerprint: None, info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() })));
            sources.push(Source { media: id, kind: SourceKind::Video, duration, name: format!("stand-in {i}") });
        }
    }
    doc.edit("Add media", media_ops)?;
    let registry = Registry::with_builtins();
    let built = oa_edit::stress::build(&mut doc, &sources, &recipe, &registry)?;
    let project = doc.project().clone();
    println!(
        "built {minutes:.1} min at {}x{} {} fps, intensity {:.2}: {} clips on {} picture and {} sound tracks, {} effects, {} transitions, {} keyframed and {} waving settings",
        recipe.size.width,
        recipe.size.height,
        recipe.rate.as_f64(),
        recipe.intensity,
        built.clips,
        built.video_tracks,
        built.audio_tracks,
        built.effects,
        built.transitions,
        built.keyframed,
        built.modulated
    );
    if files.is_empty() {
        println!("  (no --media given: the pictures are the test pattern, and there's no sound)");
    }
    if let Some(out) = &out {
        std::fs::write(out, ProjectFile::to_json(&project)?)?;
        println!("  wrote {out} (open it in the editor to look it over)");
    }

    // A slice from a quarter of the way in (or the whole thing), exported for real.
    let length = recipe.length;
    let range = (!full).then(|| {
        // Long enough to average over what happens to fall in it: 20 s, or a tenth of a
        // long project (up to a minute).
        let seconds = sample.unwrap_or_else(|| (length.as_seconds_f64() * 0.1).clamp(20.0, 60.0));
        let span = Time::from_seconds_f64(seconds.max(1.0)).min(length);
        let start = Time::from_seconds_f64(length.as_seconds_f64() * 0.25).min(length - span);
        TimeRange::new(start, span)
    });
    let (clips, buses, _) = oa_export::audio_mix(&project, built.seq, sound_of(&project));
    let options = oa_export::ExportOptions { audio: clips, buses, encoder, range, ..Default::default() };
    let ctx = Arc::new(GpuContext::new_headless()?);
    let mut renderer = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, wait: true, ..Default::default() });
    let _ = renderer.warm_up(&registry);
    let mut source = media_source(&project, &ctx);
    let target = keep.clone().map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join(format!("oa-stress-{}.mp4", std::process::id())));
    let started = std::time::Instant::now();
    // The first frames pay once for what later ones reuse (shaders built, decoders
    // opened and seeked): the steady rate is measured after them.
    let warm = |total: u64| (total / 5).clamp(1, 60);
    let mut warmed: Option<(u64, std::time::Instant)> = None;
    let summary = oa_export::export(&project, built.seq, &target, &options, &ctx, &mut renderer, &registry, source.as_mut(), |frame, total| {
        if warmed.is_none() && frame >= warm(total) {
            warmed = Some((frame, std::time::Instant::now()));
        }
        print!("\r  exporting {frame}/{total}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        true
    })?;
    let wall = started.elapsed().as_secs_f64();
    let finished = std::time::Instant::now();
    // Besides the frames: the sound mixdown (grows with the length) and opening and
    // closing the file (doesn't).
    let sound = summary.timings.sound;
    let fixed = (wall - summary.seconds_elapsed - sound).max(0.0);
    println!();
    if keep.is_none() {
        let _ = std::fs::remove_file(&target);
    }
    let fps = summary.frames_per_second();
    let what = if full { "exported all" } else { "sampled" };
    println!("  {what} {} frames in {:.1}s: {fps:.0} fps with {}", summary.frames, summary.seconds_elapsed, summary.encoder);
    println!("  {}", summary.timings);
    let total_frames = (length.as_seconds_f64() * recipe.rate.as_f64()).ceil();
    let clock = |s: f64| {
        if s >= 3600.0 {
            format!("{}h {:02}m {:02}s", (s / 3600.0) as u64, (s % 3600.0 / 60.0) as u64, (s % 60.0) as u64)
        } else {
            format!("{}m {:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
        }
    };
    if fps > 0.0 {
        // The sound mixdown grows with the length, like the frames.
        let sampled = range.map_or(length, |r| r.duration).as_seconds_f64();
        let sound_whole = sound * length.as_seconds_f64() / sampled.max(1e-3);
        // The start (the file opened, the first frames building shaders and opening
        // decoders) happens once; every frame after it runs at the steady rate. That rate
        // runs to the file being closed: a hardware encoder queues frames and does much
        // of its work when the file is finished, so a rate measured to the last frame
        // handed over would be far too quick.
        let estimate = match warmed.filter(|(n, _)| summary.frames > *n) {
            _ if full => wall,
            Some((n, at)) => {
                let steady = (summary.frames - n) as f64 / finished.duration_since(at).as_secs_f64().max(1e-6);
                let start = (at.duration_since(started).as_secs_f64() - sound).max(0.0);
                start + sound_whole + (total_frames - n as f64).max(0.0) / steady
            }
            None => total_frames / fps + sound_whole + fixed,
        };
        let verb = if full { "took" } else { "about" };
        let mixing = if sound_whole > 0.05 { format!(", {sound_whole:.1}s of it mixing the sound") } else { String::new() };
        println!(
            "  {verb} {} for the whole {minutes:.1} min ({total_frames} frames{mixing}): {:.1}× real time",
            clock(estimate),
            length.as_seconds_f64() / estimate.max(1e-3)
        );
    }
    Ok(())
}

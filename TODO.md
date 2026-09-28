# OpenAtelier — where things stand and what's next

Where things stand and what's next. Architecture and rationale live in
[DESIGN.md](DESIGN.md); this file is the working list.

## State

16 crates, ~46k lines of Rust, **374 tests passing, clippy clean** (as of this note):

| Crate | What works |
|---|---|
| `oa-time` | Flicks, exact rationals, the one frame-selection rule |
| `oa-params` | Schemas, keyframes w/ easing, clip/source anchoring, wiggle (w/ clock offset), `set_at` (keyframe-aware writes), `shift_clip_clock` |
| `oa-script` | **New.** OA script: lexer, AST, compiler (steady `let`s hoisted per block), stack machine; hosts give it inputs, outputs and functions |
| `oa-doc` | Project model, format variants + presets, reframe, ops w/ undo (capped at 1000)/coalescing, JSON file; **`repair()` on load** (overlaps → new track, dup ids, order, empty curves, missing formats…); `MediaInfo.still`; `schema::audio()`; `Item::transition_in/out`, `Op::SetTransition`, track/clip on-off ops |
| `oa-graph` | Render graph IR, cache keys, registry with WGSL, optimizer; `Affine2::invert`; `NodeOp::Transition` + built-in transitions (dissolve, dip, wipe, push) |
| `oa-plan` | Snapshot + time → graph; **`scene`**: placements, `layers_at`, `hit_test` (same math as rendering); **`transitions`**: timing windows (`window`, `active`) |
| `oa-edit` | `sections`: timeline-wide dividers/sections (add, move, clear, delete, reorder), paste into a track's free space. `timeline`: trim/split/split_all/ripple_delete/close_gap/move_item/slip/Snapper/snap_to_frame. `transform`: Handles, Gesture (move/scale/rotate/anchor), snapping guides, nudge, reset, `write_param` (variant-aware) |
| `oa-gpu` | wgpu executor, pool, node cache, async fusion, NV12 in/out, blit, readback; `FrameSource::submitted` hook; **text pass** (`text.rs`: glyph atlas, per-letter vertex chain, per-pixel fragment chain) |
| `oa-text` | **New.** System font index (mmap'd name tables), fallback chain + bundled font, harfrust shaping, line layout (`TextSpec` → `Layout`, cached), glyph signed distance fields |
| `oa-media` | Probe, import/conform, stills, relink, MF hardware decode **on per-file decode threads with 4-frame lookahead**, leased shared textures |
| `oa-audio` | Ring, ffmpeg decoder, cpal engine (built for the device's format), clock; **mixer**: overlapping clips, per-clip decoders, keyframable gain, live `MixHandle` updates; **`fx`**: sound effects with latency-primed chains, roles (intro/outro) and plugin **sound shaders** (`shader.rs`) |
| `oa-export` | Stepwise exporter; **Media Foundation sink** (NVIDIA HW H.264/HEVC + AAC mux, default) or ffmpeg; `audio_clips()` shared with playback; sound mixed on its own thread beside the frames (`mixdown.rs`) |
| `oa-cli` | `oa presets/probe/demo/plan/render/bench/export` (binary is `oa`) |
| `oa-app` | Viewer with direct manipulation, timeline editing, inspector with keyframes, shortcuts, `--script` input driver, **autosave + crash recovery**, atomic saves |

## Running & testing

```bash
cargo test -- --test-threads=4        # GPU/decoder tests skip without ffmpeg/DX12
cargo clippy --all-targets
cargo run -p oa-app -- my_clip.mp4 my_title.png   # or drop files on the window
cargo run --release -p oa-cli -- export project.oaproj.json out.mp4   # binary: target/release/oa.exe
```

UI can be driven from a script for debugging (`oa-app <files> --script steps.txt`, see
`crates/app/src/script.rs`). Set `OA_AUTOSAVE_DIR` to a scratch folder when doing that,
so test runs don't leave recovery banners in your real autosave folder.

## Next, in priority order

0. **Plugins & the start page** (✅ 2026-09-20, DESIGN §8 and §11b): `oa_graph::plugin`
   (manifest + WGSL, Atelier Core holds every built-in), `Registry::from_plugins`, the
   app's `plugins.rs`/`settings.rs`/`home.rs`, `plugins/example-looks`,
   `plugins/README.md`. Plugins also bring sound effects (`.oasound` scripts), motion
   effects (`.oamotion`), text effects (`glyph`/`glyph_pixel` shaders), overlays and
   actions (OA scripts, behind a security confirmation); the Plugins page opens each
   plugin's effect list with hover previews (2026-09-27). Left: installing a plugin from
   a zip, and versioned migration when a plugin changes its parameters.

   Also done: a **Depth** effect (`oa.depth.slab`: the clip as a sheet with
   thickness, turned in 3D, optionally shaped by a depth map), a searchable/filterable effect picker (`picker.rs`), a font menu with
   search and per-font previews (`fontpick.rs`), media-bin folders, and pixel art
   rasterized at the size it is shown at (enlarged with nearest neighbor first) so its
   effects run over every displayed pixel.
   Also done: notifications in the bottom-right corner (`notify.rs`) and the start of
   localization (`i18n.rs` + `crates/app/locales/en.json`). Left there: move the
   editor's own labels onto `t()` keys, and ship a second language to prove the
   fallback path.

1. **UI/UX overhaul** (started 2026-09-20, DESIGN §11b). ✅ Phase 1–2: design tokens
   (`style.rs`), icon buttons with shortcut tooltips, a single command list
   (`command.rs`) behind the menu bar's Edit/Clip/Timeline/View menus (`menu.rs`), a
   CapCut-style contextual action bar, a transport row that is only transport, a timeline
   tool row, **Material Symbols icons** (`icons.rs` + a 28 KB subset in `assets/fonts`,
   with drawn fallbacks), **panel clamps** so the bin can't squeeze the viewer shut,
   a **resizable timeline that scrolls** with four track heights, and **project cards**
   with thumbnails and real names on the start page (`thumbnail.rs`).
   The command palette was tried and removed — the menus carry the same list. Also
   done since: the cut-transition UI removed, menus that stay open while you use them
   (search boxes and filter chips inside them), a three-column effect browser that fills
   its width, bin folders you make with a **New folder** card (`Project::bin_folders`),
   the bin clamped so it can't widen the panel, and add-track buttons that look like
   what they add. Left, in order:
   **(3)** ✅ inspector tabs — Properties / Transitions / Effects / Sound, scene shown
   with nothing selected. Left in this phase: a **Monitor** tab (frame time, GPU memory,
   messages, "no optimizer") to get diagnostics out of the clip panel, and collapsible
   effect rows with a value summary and drag-to-reorder;
   **(4)** a left dock with Media / Effects / Plugins tabs, so the effect browser is a
   panel you drag from (Premiere) rather than a menu;
   **(5)** ✅ viewer toolbar (fit/zoom, thirds, center, safe areas, vertical layout) and
   fullscreen playback (F, 2026-09-27);
   **(6)** empty states, focus rings, full keyboard navigation, reduced motion;
   **(7)** workspaces (Edit/Color/Audio/Export, Resolve's pages) with saved panel sizes;
   plus: route the rest of the menu/command strings through `t()` (35 files use it so
   far), a shortcut editor (Premiere), and export presets (CapCut). (Per-property reset is
   there: right-click a value → Reset to default.)

   Also done 2026-09-20: 14 more core effects (pixelate, scanlines, vignette with colour,
   film grain, halftone, sharpen, edges, fisheye, swirl, mirror, zoom blur, chromatic
   aberration, hue shift, contrast, invert, duotone), wider ranges everywhere with
   sliders that only clamp dragging, **Bit Crush**, clip speed in Properties (sound
   follows the picture), **save as you go** and a save prompt when the window closes,
   and the **asset library** (`assets.rs`): a per-computer folder the bin shows in an
   Assets tab, shared by every project. Left there: dragging an asset straight onto the
   timeline, and moving assets between the library's folders from inside the app.
   Also: a play button on audio cards (`audition.rs`, a second output stream), and clip
   volume relabelled and documented as the keyframable property it already was.

2. **GPU memory & robustness** (✅ 2026-09-20, DESIGN §10, `crates/gpu/src/health.rs`):
   a VRAM budget the renderer works to (pool + cache, 2 GB default, slider in the
   Performance panel, kept in settings.json), trimming and preview back-off under
   pressure, `OutOfMemory` handling, and device-lost detection that autosaves and stops
   rendering. Left: rebuilding a lost device in place (eframe shares the device, so it
   means a restart today) and limits on plugin shaders.

3. **GPU surfaces into the encoder**: feed the MF sink writer D3D11 NV12 textures via
   `MF_SINK_WRITER_D3D_MANAGER` so frames never leave the GPU (needs rendering the two
   NV12 planes into a shared D3D11 NV12 texture). Export is pipelined already: 305 fps
   1080p60 via MF/NVIDIA.
   Measured 2026-09-27 (`bench_export`, 10 s): 1080p is **encoder-bound** (~275 fps; the
   NVIDIA MFT paces the writer; planning is 0.13 ms a frame, masks cost clips without
   them nothing; a Blur of 80 px adds ~5 %). 4K footage is **decode-bound** (~157 fps,
   with or without effects; MF's hardware decode — ffmpeg's d3d11va is no faster, its
   software decoder is 2.5× faster on the CPU but our ffmpeg pipe path is slower still).
   A separate encoder thread was tried and was slower (MF and ffmpeg already encode
   beside the render loop): don't redo it. Next lever for 4K: a second decoder working
   ahead on the next GOP during export.
4. **Timeline UX** (overhaul ✅ 2026-09-19, DESIGN §11b: multi-select, groups, clipboard,
   menus, track rename/reorder/delete, filmstrip/waveform, keyframe line, extract audio,
   effect/value copy-paste, compound clips "As media"/"Nest", crop, media bin cards with
   sort/search, drag from bin to timeline, transform copy/paste, sequence background
   (solid/gradient, blurred content, texture); also chroma key, smear blur, wobble, formats moved into the top dropdown,
   loading skeletons instead of stalls everywhere); compound clips open as their own
   timeline (breadcrumbs, `compound.rs`), bulk Arrange and a Length property
   (2026-09-26); compound clips' own pictures in the bin and filmstrips on the timeline,
   imports that wait in the bin, reverse playback with sound (2026-09-27). Left: solo,
   dragging a transition band's edges, media-level crop (crop is per clip now).
5. **Text, next steps** (text layers ✅ 2026-09-18, DESIGN §17; on-canvas editing ✅,
   its double-click made dependable 2026-09-26): wrapping to a box width, rich text (per-range
   style), color emoji, font weight picker beyond bold (faces have weights), glyph atlas
   eviction. (Text effects as shader plugins: done — `glyph`/`glyph_pixel`.) ✅ Done this session: text layers with
   the three-level shader controller (per pixel `GlyphPixel`: Shimmer, Glow; per letter
   `Glyph`: Letter Wiggle, Wave, Rainbow Letters, Typewriter, Letters Rise/Pop/Scatter,
   Letter Fade; whole object: transform/motion), "+ Text"/Ctrl+T, Text inspector
   section; **several intros/outros per clip**; effect previews that **play on hover**,
   render from a frozen frame (no decoder waits) within a 12 ms/frame budget and are
   framed on the clip; smooth scrubbing (stand-ins), Auto preview resolution, viewer
   zoom/pan and guides. Earlier: effect categories/roles, motion effects, media inputs. Also this session: `Unit::Direction` + pinwheel dial (Fly any angle, wipes, push, shimmer), `Value::Gradient` directional gradients (tint, text fill/outline, glow) with a stop editor, Shimmer for any clip. Old projects: Fly/Push saved with the old left/right/up/down choice fall back to the default direction. Later: simple/advanced Color (right-click → gradient) and Scale (right-click → X/Y), smaller dial snapping to right angles, outro "Reverse" (`Item::active_effects`, `EffectRole::Reversed`), script `rclick`.
6. **Audio**: ✅ sound effects (Bass Boost, Pitch Shift, Echo, Reverb, Threshold, Bit Crush, Denoise; DESIGN §12). ✅ Speed with keep-pitch. ✅ Effect tracks (picture and sound buses), tails, EQ/compressor/limiter/de-esser with live meters, formants, reverb rooms, properties following the sound (2026-09-24). ✅ Reversed clips play their sound backwards (2026-09-27). ✅ Compound clips: their volume (keyframes too) and fades reach the sound inside, compound clips inside compound clips keep their sound effects (groups nest in the mixer), keyframes inside retimed compounds run at the right rate, sound-only compounds go on sound tracks and draw as waveforms (timeline, bin; the viewer says so inside one) (2026-09-27). Left: freeze frames and reversed compound clips are silent; per-track gain/pan;
   replace the ffmpeg audio process with a platform decoder.
7. **Stateful effects**: the engine reserves `Statefulness::Stateful` (preroll, no cache
   key) but no effect uses it any more and the executor doesn't run them — needed for
   trails, echoes of the picture, temporal denoise.
8. **Color management** ✅ 2026-09-20 (DESIGN §9, `oa_doc::color`, `app/color.rs`): input
   transforms per file (curve incl. PQ/HLG and six log formats, gamut, levels, matrix,
   exposure; automatic from tags), display-space wrapping, output tone map + exposure.
   ✅ The Color tab (2026-09-27): scopes, lift/gamma/gain/offset wheels, tone and white balance, curves, HSL mixer. Left: **10-bit/P010 decode** (HDR/log band at 8 bits today), HDR export, LUTs/OCIO, qualifiers and power windows (secondary grades on part of the picture).
9. ✅ **Batch export of all format variants** (Export window, 2026-09-20). Left: cancel-with-partial-output semantics in the UI.
10. **Editing UI** ✅ 2026-09-20 (DESIGN §5, §11b): curve editor window (`curves.rs`),
    editing inside compound clips with breadcrumbs (`compound.rs`), on-canvas title
    typing (double-click in the viewer), the format picker (`formats.rs`) with platform
    safe zones. Left: curve editor for vector properties and several curves at once;
    selecting several keys on the timeline.
11. **Masks** ✅ 2026-09-27 (DESIGN §17b, `oa_doc::mask`, `app/masks.rs`; Settings →
    Masking): several masks per clip drawn with rectangle, ellipse, brush/eraser, fill
    and magic select, or imported from a black-and-white or see-through picture;
    keyframable center/scale/rotation/softness/harshness; "… on mask" values for
    opacity, position, scale, rotation, squash; "Use with mask" (or outside) for effects;
    copy/paste between clips with fit or crop. ✅ Second pass (same day): bezier paths
    (Pen) with per-point keyframes, an Edit tool with handles for rectangles, ellipses
    and path points, per-shape expand/contract and feather, add/subtract/intersect/
    difference between masks, the center on a point track and scale/rotation on two
    (`Modulator::TrackPair`), pixel masks at full resolution. Left: **automatic
    rotoscoping** (a path that follows its outline by itself, e.g. tracking each point),
    inserting a point on a path segment, a timeline view of path keys, and building
    masks from the Color tab (qualifiers).

## Background backend track

The backend keeps maturing **one small, finished, tested step at a time**, alongside
whatever front-end work is under way. Take
the next unticked item, land it with tests, update DESIGN.md, tick it here. Skip only
when the turn's requested work is already very large (and say so).

- Threading: [x] coordinator thread (the viewer renders on `viewer_render`, 2026-09-26) ·
  [~] single GPU submit thread — decided against: wgpu's queue already is one serialized
  submission point, and routing submits through another thread would reorder them
  against each thread's own uploads (DESIGN "Threading") · [x] request priorities
  (the viewer's frame first: background previews wait while it plays; decoders answer
  frames before scrub wants, 2026-09-26) · [x] pipelining (the next playback frame is
  planned on `plan_ahead` while this one renders) · [x] progressive refinement when idle
  (2× supersampled, smooth text).
- Parameters: [x] LFO driver (`Modulator::Lfo`: sine/triangle/square/saw, phase, decay; wave editor `app/waves.rs`) · [ ] linking params · [x] audio-reactive drivers (`Modulator::Follow`, `oa_audio::envelope`; connection editor `app/connections.rs`) · [ ] spatial bezier motion paths · [ ] squash & stretch from velocity ·
  [ ] motion blur (sub-frame transform samples) · [ ] speed curves.
- GPU: [ ] rebuild a lost device in place · [ ] zero-copy hardware decode · [ ] plugin
  shader loop/time limits.
- Media: [x] ffmpeg and ffprobe run without a console window (`oa_media::tool`; one
  flashed up and took the keyboard on every probe, import and seek in release builds,
  2026-09-21) · [ ] 10-bit/P010 decode · [ ] 4:2:2 · [ ] proxies · [ ] decoder budget · [x] CPU
  decode fallback (`oa_media::ffmpeg`, any platform, 2026-09-24) · [x] Linux · [ ] macOS (VideoToolbox) ·
  [ ] VA-API zero-copy.
- Audio: [x] sound effects on the effect system: `EffectKind::Sound`, roles/clocks, plugin
  sound shaders (2026-09-21) · [x] sound effects on compound clips (a group bus,
  2026-09-26) · [ ] per-track gain/pan · [x] buses and track effects (effect tracks) ·
  [x] time-stretch (WSOLA for "keep pitch") · [x] gain/mute after the ring (ramped) ·
  [x] export frame-count clock · [x] Bluetooth sync offset (Settings → Audio)
  · [x] effect tails past the clip end · [ ] native decoder instead of ffmpeg.
- Render graph / export: [x] exports wait for shaders/glyphs the preview skips
  (2026-09-21) · [x] the sound mixed on its own thread while the frames render
  (2026-09-26) · [x] export queue: jobs keep their project as queued; reorder, remove,
  skip (2026-09-26) · [x] PNG image sequences and audio-only export (WAV/M4A/MP3/FLAC/
  Opus, 2026-09-26) · [ ] on-disk pipeline cache · [x] UV-warp fusion · [x] ROI
  propagation · [x] reuse between frames (uniforms and bind groups kept, 6.8 → 2.2 ms on
  60 layers, 2026-09-26) · [ ] zero-copy encode · [ ] resume a canceled export.
- Color: [ ] OCIO configs (a pure-Rust config reader, transforms evaluated on the CPU and
  baked into 3D LUTs; per-file color space, project display/view) · [ ] .cube LUTs ·
  [ ] 10-bit/P010 decode · [ ] HDR export.
- Media: [x] transparent video fixed (2026-09-27): ffmpeg's `-hwaccel auto` picked its Vulkan compute decoders for FFV1/ProRes, which corrupted frames now and then (black flicker, test pattern) — hardware decoding is now only for H.264/HEVC/VP9/AV1/MPEG-2/VC-1/VP8 without alpha; VP8/VP9 alpha decoded with libvpx and detected from `alpha_mode`; transparent media no longer counts as covering the layers under it (`MediaInfo::alpha`); Matroska millisecond stamps no longer repeat every third frame (half-tick slack in `select_frame` for coarse time bases); clicking the same spot again selects the next clip down.

## Known issues / gaps

- Audio decode is an ffmpeg process per clip (restart on seek, ~50 ms); fine for now.
- Hit testing: titles by their letters and Surfaces by their mesh (2026-09-26), other
  layers still by their rectangle — a PNG's transparent parts still catch clicks.
- Captions: the engine setup (uv → Python → faster-whisper → model) is built and its
  steps are unit-tested, but the real download and a real transcription haven't been run
  yet. First run: watch the Details log in the Captions window. CPU only; no speaker
  detection (color captions per speaker by hand).
- Retyping a caption with a different number of words shifts the highlight (word times
  now follow trims, splits and moves).
- "Highlight when spoken" covers a title's color and outline and text effects' params;
  whole-layer effects (blur…) and the text size can't change per word.
- Background effects are picture effects only (passive); no motion or text effects there.
- Dragging an effect onto another clip failed twice in manual testing with no cause found in
  headless egui tests (they pass); the drop is now decided by the inspector itself. If it
  still fails, check first whether reordering by the same grip works (does the drag even
  start?).
- GPU test binary once hung with ≥3 parallel tests. Not reproduced since (five runs of
  the render tests at 16 threads, 2026-09-26); `crates/gpu/examples/stress.rs` exists to
  chase it. CI uses `--test-threads=4`.

## House style

American English everywhere — code, comments, docs and UI text ("color", "center",
"neighbor", "gray", "catalog").

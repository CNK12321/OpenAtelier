# OpenAtelier — where things stand and what's next

Where things stand and what's next. Architecture and rationale live in
[DESIGN.md](DESIGN.md); this file is the working list.

## State

15 crates, ~45k lines of Rust, **319 tests passing, clippy clean** (as of this note):

| Crate | What works |
|---|---|
| `oa-time` | Flicks, exact rationals, the one frame-selection rule |
| `oa-params` | Schemas, keyframes w/ easing, clip/source anchoring, wiggle (w/ clock offset), `set_at` (keyframe-aware writes), `shift_clip_clock` |
| `oa-doc` | Project model, format variants + presets, reframe, ops w/ undo (capped at 1000)/coalescing, JSON file; **`repair()` on load** (overlaps → new track, dup ids, order, empty curves, missing formats…); `MediaInfo.still`; `schema::audio()`; `Item::transition_in/out`, `Op::SetTransition`, track/clip on-off ops |
| `oa-graph` | Render graph IR, cache keys, registry with WGSL, optimizer; `Affine2::invert`; `NodeOp::Transition` + built-in transitions (dissolve, dip, wipe, push) |
| `oa-plan` | Snapshot + time → graph; **`scene`**: placements, `layers_at`, `hit_test` (same math as rendering); **`transitions`**: timing windows (`window`, `active`) |
| `oa-edit` | `sections`: timeline-wide dividers/sections (add, move, clear, delete, reorder), paste into a track's free space. `timeline`: trim/split/split_all/ripple_delete/close_gap/move_item/slip/Snapper/snap_to_frame. `transform`: Handles, Gesture (move/scale/rotate/anchor), snapping guides, nudge, reset, `write_param` (variant-aware) |
| `oa-gpu` | wgpu executor, pool, node cache, async fusion, NV12 in/out, blit, readback; `FrameSource::submitted` hook; **text pass** (`text.rs`: glyph atlas, per-letter vertex chain, per-pixel fragment chain) |
| `oa-text` | **New.** System font index (mmap'd name tables), fallback chain + bundled font, harfrust shaping, line layout (`TextSpec` → `Layout`, cached), glyph signed distance fields |
| `oa-media` | Probe, import/conform, stills, relink, MF hardware decode **on per-file decode threads with 4-frame lookahead**, leased shared textures |
| `oa-audio` | Ring, ffmpeg decoder, cpal engine (built for the device's format), clock; **mixer**: overlapping clips, per-clip decoders, keyframable gain, live `MixHandle` updates; **`fx`**: sound effects with latency-primed chains, roles (intro/outro) and plugin **sound shaders** (`shader.rs`) |
| `oa-export` | Stepwise exporter; **Media Foundation sink** (NVIDIA HW H.264/HEVC + AAC mux, default) or ffmpeg; `audio_clips()` shared with playback |
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
   `plugins/README.md`. Left: plugin-provided sound effects (DSP needs a host API),
   motion effects (they are Rust fns), installing a plugin from a zip, a per-plugin
   effects list on the Plugins page, and versioned migration when a plugin changes its
   parameters.

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
   **(5)** viewer overlay toolbar (zoom, guides, quality, fullscreen) and moving the
   remaining view controls off the top bar;
   **(6)** empty states, focus rings, full keyboard navigation, reduced motion;
   **(7)** workspaces (Edit/Color/Audio/Export, Resolve's pages) with saved panel sizes;
   plus: route the new menu/command strings through `t()`, per-property reset buttons in
   the inspector (Resolve), a shortcut editor (Premiere), and export presets (CapCut).

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
4. **Timeline UX** (overhaul ✅ 2026-09-19, DESIGN §11b: multi-select, groups, clipboard,
   menus, track rename/reorder/delete, filmstrip/waveform, keyframe line, extract audio,
   effect/value copy-paste, compound clips "As media"/"Nest", crop, media bin cards with
   sort/search, drag from bin to timeline, transform copy/paste, sequence background
   (solid/gradient, blurred content, texture); also chroma key, smear blur, wobble, formats moved into the top dropdown,
   loading skeletons instead of stalls everywhere). **Not yet verified by hand in the
   UI.** Left: open a compound clip as its own timeline, solo, dragging a transition
   band's edges, media-level crop (crop is per
   clip now), rendered thumbnails for compounds (they show their first file's frame).
5. **Text, next steps** (text layers ✅ 2026-09-18, DESIGN §17): on-canvas editing
   (double-click a title in the viewer), wrapping to a box width, rich text (per-range
   style), color emoji, font weight picker beyond bold (faces have weights), text
   effects as shader plugins, atlas eviction. ✅ Done this session: text layers with
   the three-level shader controller (per pixel `GlyphPixel`: Shimmer, Glow; per letter
   `Glyph`: Letter Wiggle, Wave, Rainbow Letters, Typewriter, Letters Rise/Pop/Scatter,
   Letter Fade; whole object: transform/motion), "+ Text"/Ctrl+T, Text inspector
   section; **several intros/outros per clip**; effect previews that **play on hover**,
   render from a frozen frame (no decoder waits) within a 12 ms/frame budget and are
   framed on the clip; smooth scrubbing (stand-ins), Auto preview resolution, viewer
   zoom/pan and guides. Earlier: effect categories/roles, motion effects, media inputs. Also this session: `Unit::Direction` + pinwheel dial (Fly any angle, wipes, push, shimmer), `Value::Gradient` directional gradients (tint, text fill/outline, glow) with a stop editor, Shimmer for any clip. Old projects: Fly/Push saved with the old left/right/up/down choice fall back to the default direction. Later: simple/advanced Color (right-click → gradient) and Scale (right-click → X/Y), smaller dial snapping to right angles, outro "Reverse" (`Item::active_effects`, `EffectRole::Reversed`), script `rclick`.
6. **Audio**: ✅ sound effects (Bass Boost, Pitch Shift, Echo, Reverb, Threshold, Bit Crush, Denoise; DESIGN §12). ✅ Speed with keep-pitch. ✅ Effect tracks (picture and sound buses), tails, EQ/compressor/limiter/de-esser with live meters, formants, reverb rooms, properties following the sound (2026-09-24). Left: reverse/freeze clips are silent; per-track gain/pan;
   replace the ffmpeg audio process with a platform decoder.
7. **Stateful effects** (`oa.time.feedback-trail`) render as passthrough and are reported.
8. **Color management** ✅ 2026-09-20 (DESIGN §9, `oa_doc::color`, `app/color.rs`): input
   transforms per file (curve incl. PQ/HLG and six log formats, gamut, levels, matrix,
   exposure; automatic from tags), display-space wrapping, output tone map + exposure.
   Left: **10-bit/P010 decode** (HDR/log band at 8 bits today), HDR export, LUTs/OCIO.
9. ✅ **Batch export of all format variants** (Export window, 2026-09-20). Left: cancel-with-partial-output semantics in the UI.
10. **Editing UI** ✅ 2026-09-20 (DESIGN §5, §11b): curve editor window (`curves.rs`),
    editing inside compound clips with breadcrumbs (`compound.rs`), on-canvas title
    typing (double-click in the viewer), the format picker (`formats.rs`) with platform
    safe zones. Left: curve editor for vector properties and several curves at once;
    selecting several keys on the timeline.

## Background backend track

The backend keeps maturing **one small, finished, tested step at a time**, alongside
whatever front-end work is under way. Take
the next unticked item, land it with tests, update DESIGN.md, tick it here. Skip only
when the turn's requested work is already very large (and say so).

- Threading: [ ] coordinator thread · [ ] single GPU submit thread · [ ] request priorities
  (current frame → lookahead → scrub → background) · [ ] pipelining (plan N+1 while N
  renders) · [ ] progressive refinement when idle.
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
  sound shaders (2026-09-21) · [ ] sound effects on compound clips (needs a bus) ·
  [ ] per-track gain/pan · [x] buses and track effects (effect tracks) · [ ] time-stretch ·
  [ ] gain/mute after the ring · [ ] export frame-count clock · [ ] Bluetooth sync offset
  · [x] effect tails past the clip end · [ ] native decoder instead of ffmpeg.
- Render graph / export: [x] exports wait for shaders/glyphs the preview skips
  (2026-09-21) · [ ] on-disk pipeline cache · [ ] UV-warp fusion · [ ] ROI
  propagation · [ ] graph reuse between frames · [ ] zero-copy encode · [ ] image
  sequences and audio-only export · [ ] resume a canceled export.

## Known issues / gaps

- Conforming a GIF to H.264 loses its transparency.
- Audio decode is an ffmpeg process per clip (restart on seek, ~50 ms); fine for now.
- Split with non-integer speed can be off by one flick in the back half (floor rounding).
- Anchor moves write position at the playhead only — with keyframed position, other
  times shift (documented in `transform.rs`).
- Hit testing uses the layer rectangle, not alpha (nor a Surface's warped shape).
- Captions: the engine setup (uv → Python → faster-whisper → model) is built and its
  steps are unit-tested, but the real download and a real transcription haven't been run
  yet. First run: watch the Details log in the Captions window. CPU only; no speaker
  detection (color captions per speaker by hand).
- Word times (`Item::word_times`) don't follow a head trim or a split of a caption clip
  yet; retyping a caption with a different number of words shifts the highlight.
- "Highlight when spoken" covers a title's color and outline and text effects' params;
  whole-layer effects (blur…) and the text size can't change per word.
- Sound effects on a compound clip aren't heard (its inner clips' own effects are).
- Background effects are picture effects only (passive); no motion or text effects there.
- Dragging an effect onto another clip failed twice in manual testing with no cause found in
  headless egui tests (they pass); the drop is now decided by the inspector itself. If it
  still fails, check first whether reordering by the same grip works (does the drag even
  start?).
- Inside a compound clip the preview shows it on the compound's own background (black),
  not the see-through look it has where it's used.
- GPU test binary once hung with ≥3 parallel tests (unresolved, not reproduced since);
  `crates/gpu/examples/stress.rs` exists to chase it. Use `--test-threads=4`.
- Rotated layers have no edge anti-aliasing.

## House style

American English everywhere — code, comments, docs and UI text ("color", "center",
"neighbor", "gray", "catalog").

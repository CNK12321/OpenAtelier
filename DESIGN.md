# OpenAtelier — Engine Design (rev 2)

**Core idea:** the user edits a *document*; the engine turns a snapshot of it into a
*pure per-frame render graph*, then optimizes, caches and executes that graph. Preview
and export run the same code at different quality levels.

Revision 2 folds in the review of rev 1: layer space & resolution rules, a stable
plugin boundary, explicit stateful nodes, keyframe anchoring, one frame-selection
rule, cache-key contracts, async shader fusion, working color spaces, memory budgets,
and format variants for aspect ratios.

Status legend: ✅ implemented · 🟡 partly implemented · ⏳ planned

```
 UI ──ops──▶ Document (Arc snapshots, undo/redo, op log)
                 │ snapshot
                 ▼
            Planner ── evaluates params at t, picks variant, raster scales
                 │
                 ▼
            Render graph (pure nodes + cache keys)
                 │
            Optimizer (cull · merge transforms · fuse point ops)   ◀── Reference mode = no rewrites
                 │
     ┌───────────┼──────────────┐
  Decode      Node cache      GPU executor ──▶ preview | encoder
  scheduler   (hash-keyed)
                                  ▲
  Audio graph ── rendered ahead ──┘ clock abstraction drives presentation
```

## Crates

| Crate | Role | Status |
|---|---|---|
| `oa-time` | Flicks, rationals, **the** frame-selection rule | ✅ |
| `oa-params` | Param schemas, values, keyframe curves, anchors, overrides | ✅ |
| `oa-script` | OA script: the small language sound shaders, motion, bounds and pass counts are written in | ✅ |
| `oa-doc` | Project model, format variants & aspect presets, ops, undo, file format | ✅ |
| `oa-graph` | Render graph IR, cache keys, plugin-facing registry (with WGSL), optimizer | ✅ |
| `oa-plan` | Document snapshot + time → render graph; `scene`: where layers land (hit testing) | ✅ |
| `oa-edit` | Editing commands: trim, split, ripple, move, slip, snapping; viewer transform gestures | ✅ |
| `oa-gpu` | wgpu executor, texture pool, node cache, shader codegen, async fusion, readback | ✅ |
| `oa-cli` | `oa presets`, `oa demo`, `oa plan`, `oa render` (PNG via GPU) | ✅ |
| `oa-media` | Probe, frame index, fingerprint, import/conform, stills, **hardware decode into GPU textures** (Windows) | ✅ / ⏳ |
| `oa-text` | System fonts, shaping (HarfBuzz rules via harfrust), line layout, glyph distance fields | ✅ |
| `oa-audio` | Ring buffer, decoding, output device, **the playback clock**, the timeline mixer | ✅ |
| `oa-graph::plugin` | Plugin manifests (JSON + WGSL + scripts), Atelier Core (built in from `plugins/atelier-core`), enable/disable | ✅ |
| `oa-plugin-host` | wasmtime host and native bridge (CPU-side plugin code) | ⏳ |
| `oa-export` | Sequence → file: GPU color conversion, encoder sink, audio mux | ✅ |
| `oa-engine` | Scheduler, proxies (decode lookahead lives in `oa-media`) | ⏳ |
| `oa-captions` | The downloadable speech-to-text engine (a private Python), caption grouping | ✅ |
| `oa-track` | Point tracks (smoothing, thinning to keys) and the downloadable CoTracker engine | ✅ |
| `oa-app` | Preview UI (egui/eframe sharing the renderer's wgpu device) | ✅ |

Dependency rule: `oa-graph` never depends on `oa-doc`. `oa-plan` is the only bridge.

---

## 1. Time ✅

* Timeline time is `i64` **flicks** (705,600,000/s). All standard frame rates —
  including 24000/1001, 30000/1001, 60000/1001 — and sample rates are exact.
* Media keeps its **own timebase**. Not every timebase divides into flicks (FFmpeg MP4s
  often use 1/15360), so comparisons use exact `Rational` math.
* **One rounding rule** (`oa_time::select_frame`, `FrameRate::frame_at`): a frame owns
  `[pts, next_pts)`; ties go to the frame that starts at that instant; times before the
  first frame select frame 0. Derived times (speed maps) round toward −∞
  (`Time::from_rational_floor`). Nothing else may round time to frames.
* Clip time map: `source = source_in + floor(local × speed)`; speed is a `Rational`
  (0 = freeze, negative = reverse). Speed *curves* ⏳ will integrate to source time and
  then pass through the same floor rule.

## 2. Document ✅

* `Project → Sequence → Track → Item`, with `Arc` per sequence/track/media so a snapshot
  clone is cheap; render threads keep old snapshots without blocking the UI.
* All changes are `Op`s that return their inverse. `Document` applies a batch to a copy
  and swaps it in only if **every** op succeeds (no partial edits).
* Undo coalescing: `edit_coalesced(key)` merges a slider drag into one step; `seal()` on
  mouse-up.
* Ops reference **stable ids only**. That keeps logs meaningful and leaves the door open
  to CRDT collaboration later — but invertible ops are *not* collaboration by themselves.
* Validation at edit time: overlaps, track kind, nesting cycles (A ⊃ B ⊃ A), removing
  in-use media/sequences, removing the last variant — and, whatever the UI lets through,
  **no non-finite numbers, keyless curves or zero-size canvases** (`Op::validate`), and
  **never the last timeline** (checked on every edit, undo and redo; the new project
  itself is the starting point of history, not an undoable step).
* The app's last line of defense (`app/guard.rs`): each UI frame runs under
  `catch_unwind`; a panic autosaves, drops half-finished gestures, puts the editor back on
  a timeline that exists and says what happened; repeated failures return to the start
  page. Shared locks tolerate poisoning, and `Time::from_seconds_f64` clamps (NaN → 0,
  ±10⁸ s) so typed times can't overflow later arithmetic.
* Unknown data survives: plugin clip types (`ItemKind::Plugin` keeps raw JSON), unknown
  effects and unknown params round-trip through save/load untouched.
* File format: `{format, version, project}` JSON; older versions migrate step by step on
  raw JSON; newer versions are refused with a clear error.
* ⏳ Dense data (tracking points, automation) as plain arrays, not keyframes. ⏳ Item
  lookup index when projects get large (today `find_item` is linear; `item_at` is
  O(log n)).

## 3. Parameters & keyframes ✅

* Every animatable property uses one system: `ParamSchema { id, type, default, unit,
  animatable, range, default_anchor }`.
* **Units** drive resolution independence: `LayerPixels` values are multiplied by the
  layer's raster scale; `CanvasFraction`, `SourceFraction`, `Degrees`, … are not.
* `ParamSource::Static | Animated(Curve) | Modulated { base, modulator }`
  (`#[non_exhaustive]`). **Every effect parameter goes through this**, so blur intensity,
  glow size, etc. are keyframable by construction and evaluated per frame.
* ✅ **Enum parameters** carry their choices in the schema; shaders receive the option
  index, and UIs list the options (e.g. the blur's `edges` = transparent | clamp).
* ✅ **Modulators** — `Wiggle` (seeded fractal noise per component): shaking text, handheld
  drift, flicker. Its amplitude and frequency are themselves `ParamSource`s, so shake
  intensity can be keyframed. Modulators are pure functions of (settings, seed, time) and
  keep frames cacheable. ✅ `Lfo` (sine, triangle, square, saw; phase; decay; keyframable amplitude
  and rate; survives splits like `Wiggle`), edited in the **wave editor** (right-click a
  number → *Edit wave…*: shape, frequency, min/max or gain over keyframes, phase, decay,
  a preview over the clip; a small wave marks the property's keyframe diamond). ✅ `Follow` (2026-09-24): an **offset
  that follows the sound** — `amount × level`, the level of the mix or one chosen item's
  sound (`SoundSource`), overall or its bass, mids or treble (`SoundBand`), mapped from a
  keyframable floor (dBFS) to −6 dBFS. Evaluation stays pure: whoever plans a frame
  installs a level provider for that instant (`oa_params::signal::with`), fed by
  loudness envelopes (`oa_audio::envelope`: 100 per second per band, analyzed once per
  file — in the background in the app, before rendering in an export) and a `Follower`
  that places them by the clips' timing, speed, gain and fades. Edited in the
  **connection editor** (right-click a number → Animate → *Edit connection offset…*;
  Animate also holds *Edit curve…* and *Edit wave…*): source (everything, a clip from a
  list, or the other selected clip), band, offset, floor, and the result over the clip.
  Clip sound effects aren't heard by the follower. ⏳ Links to other params (with
  dependency ordering and cycle detection at edit time).
* **Keyframe anchoring** (fix for head trims): each curve records
  `KeyframeAnchor::ClipStart` (fades, pops) or `SourceMedia` (masks, tracking, reframe
  focus — glued to the footage). Evaluation receives both clocks.
* Interpolation: hold, linear, cubic-bezier easing per segment (Newton + bisection).
  ⏳ Spatial bezier motion paths with arc-length parameterization.
* Resolution order: **variant override → stored value → schema default**; a wrong-typed
  stored value falls back to the default instead of failing a render.

## 4. Space and resolution rules ✅

This is the contract the optimizer must never change.

1. **Placement order** for a layer: *reframe* (fit mode + focus) → *user transform*
   (anchor pivot, squash & stretch, scale, rotation, position) → *output render scale*.
2. **Effects run in layer space**, before the transform. A blur radius of 10 means 10
   pixels of the layer's native content, regardless of how the layer is scaled.
3. **Raster scale** = the layer's final on-screen scale (`max_axis_scale`), **capped at
   1.0 for media** (no upscaling beyond native), up to 4.0 for nested/vector content,
   **quantized up to a power of two** (…¼, ½, 1, 2…). **Pixel art goes the other way**: it
   rasterizes at the size it is *shown* at (the exact scale in quarter steps, never below
   1:1, capped at a 4096 px side), decoding at its own size and then being enlarged with
   nearest neighbor — so a 16×16 sprite at 10× becomes a 160×160 picture of crisp blocks
   and its effects run over all of those pixels, not over 16×16 that is then stretched.
   Its effect nodes also carry `nearest`, so warps stay on the block grid. Consequences:
   * a 4K clip in a 1080p canvas decodes at ½; in a half-res preview at ¼;
   * animating scale 0.6→0.9 keeps the decode size (and cache key) stable;
   * vector/text/nested content re-rasterizes sharply when scaled up.
4. Transforms merge **only when directly adjacent**; never across an effect.
5. Squash & stretch: `x *= 1+s`, `y /= 1+s` (area preserved). ⏳ Automatic stretch from
   position-curve velocity; ⏳ sub-frame motion blur by sampling the transform curve.

## 5. Format variants & aspect ratios ✅

* A sequence has **one edit** and one or more **format variants** (e.g. "Landscape 16:9"
  and "Vertical 9:16"). Variants share frame rate, timing, clips and effects. Each has
  its own canvas size and **per-item parameter overrides**.
* **Presets** (`oa presets`): Landscape 16:9, Vertical 9:16, Square 1:1, Portrait 4:5,
  Classic 4:3, Portrait 3:4, Ultrawide 21:9, Cinemascope 2.39:1 — sized from any short
  edge (720/1080/1440/2160), long edge rounded to even for encoders.
* **Horizontal ↔ vertical toggle**: `CanvasSize::rotated()` (1920×1080 ↔ 1080×1920).
* **Reframing** per item: `reframe.fit` = `fill` (default: cover canvas, crop) · `fit`
  (letterbox) · `stretch` · `none` (1:1 pixels), and `reframe.focus` — the source point
  `fill` keeps centered, clamped so no empty canvas shows. Focus is keyframeable and
  source-anchored, so it can follow a subject (⏳ auto-follow from tracking data).
* `transform.position` is a **fraction of the canvas**, so layouts survive aspect changes;
  `transform.scale` multiplies the reframe scale.
* Pure nodes don't depend on the canvas, so variants **share cache entries whenever they
  need the same raster scale** (e.g. a 9:16 half-res preview reuses the 16:9 full-res
  decode). Filling a vertical canvas from landscape footage usually crops in ~1.8×, which
  correctly asks for a sharper decode — a real cache miss, not a bug (tested in
  `oa-gpu`).
* ✅ **Format picker** (`app/formats.rs`): the top bar's menu lists every format with a
  shape preview and its size; switch, turn sideways (a preset's name follows its new
  shape), pick the resolution (720p–4K, shape kept), rename (double-click), remove (not
  the main one); add from preset cards that name their platforms, or at a custom size
  (rounded to even). **Safe areas** in the viewer add, on tall formats, the zones phone
  apps cover with their own buttons and captions (the overlap of TikTok, Reels and
  Shorts templates).
* ✅ Export every format at once (Export window → "Every format", one file each, queued).

## 6. Render graph & cache keys ✅

* `NodeOp` is **internal** and free to change. Nodes: `Source`, `Solid`, `Effect`,
  `FusedPointOps`, `Transform`, `Composite`, `Transition` (⏳ `Text`, `Vector`, `Mask`,
  `Temporal`, `Cpu`).
* Each node stores `bounds` (possibly visible region) and `opaque` (all pixels in
  bounds opaque). Rotated/skewed transforms are **never** opaque for occlusion, even if
  their bounding box covers the canvas.
* **Cache-key contract** (`GraphBuilder::key_for`): the key hashes exactly the node's
  declared fields + input keys + `KeyContext` (color config, engine revision), with
  canonical floats (−0 = 0, one NaN) into a 128-bit xxh3.
  * Media is keyed by **content fingerprint**, never path or id. No fingerprint → no key.
  * Effects include `type_id` **and plugin version**.
  * **Stateful** nodes get no key, and neither does anything downstream.
* ✅ **Admission policy**: a node's output is cached on the **second** sighting of its key,
  never the first. Scrubbing and playback produce a new key per frame; caching those on
  sight filled ~660 MB of VRAM that nothing ever read (measured in the UI before the fix,
  0 MB after). Sightings are forgotten after a few frames.
* ✅ **Byte budget** with LRU eviction on the node cache, and a byte-bounded window of
  recently shown decoded frames in `oa-media` (128 MB) so stepping backwards doesn't
  re-decode a GOP per frame.
* ⏳ Per-document-version hashing of large inputs, debug "caches off + image diff" mode.

## 7. Optimizer ✅

* Passes: composite **visibility/occlusion cull**, **transform merge**, **point-op fusion**
  (same working space, fusible, pure), dead-node elimination.
* `OptLevel::Reference` performs no rewrites. It is the ground truth for tests
  (optimized vs. reference image diff on the same GPU, with tolerance) and a user-facing
  fallback if an optimization bug is suspected.
* ✅ **Async shader compilation**: fused pipelines compile on a worker thread; until ready
  the unfused chain renders (per-effect pipelines are precompiled by `Renderer::warm_up`).
  Output is tested equal to the reference path. ⏳ Persist the pipeline cache on disk.
* ⏳ UV-warp fusion with fallback to explicit mip selection / multi-tap filtering when the
  combined transform minifies by more than ~2×.
* ⏳ Region-of-interest propagation, CPU-island grouping (≤1 download + 1 upload per chain).
* ⏳ Structural graph reuse between frames (only uniforms change) for large projects.

## 8. Effects & plugins

**Stable plugin boundary** ✅: plugins describe effects with `EffectDescriptor`
(`PLUGIN_API_VERSION`), never `NodeOp`. Built-ins register through the same `Registry`.

**The core effects** ✅: colour (exposure, saturation, contrast, tint, hue shift,
invert, duotone, posterize), blur (gaussian, smear, zoom), stylize (pixelate, scanlines,
vignette with its own colour, film grain, halftone, sharpen, edges), warp (wobble,
fisheye, swirl, mirror), depth, keying (chroma key), masking, light (shimmer), motion
(shake, wiggle), intros/outros and cut transitions, plus per-letter and per-pixel text
effects. Sound: bass boost, pitch shift, echo, reverb, threshold, **bit crush**, denoise,
and — written as sound shaders — **fade**, **muffle**, **tone**, **tremolo**, **drive**
and **stereo width**. **Tone** adds an oscillator on top of the sound: sine, square
(with pulse width), triangle, saw or noise at a pitch in Hz (keyframe it to glide),
gain, attack, exponential decay, repeat every N seconds (beeps), vibrato rate and depth,
"follow" (sounds only while the clip does) and how much of the original stays.
Ranges are the *comfortable* range, not a limit — sliders clamp what you drag, never what
you type, so an effect can be pushed as far as it will go.

**Everything is a plugin** ✅ (`oa_graph::plugin`; 2026-09-24: truly): the built-ins are
**Atelier Core** (`com.openatelier.core`), an ordinary plugin folder,
`plugins/atelier-core` — `plugin.json`, one `.wgsl` per picture effect, `.oasound` sound
shaders, motion/bounds/pass scripts. `crates/graph/build.rs` builds the folder's files
into the program and `plugin::core` loads them through the same manifest loader as any
plugin; the only thing Atelier Core has that a plugin doesn't is its `oa.*` ids. Nothing
in the host knows an effect by its id to make it work: what used to be Rust (motion
functions, Glow's pass counts, Surface/Depth/Drop Shadow bounds, the native sound
processors, the picker's categories and thumbnail settings, the equalizer card and the
meters) is now declared in the manifest, in shaders and in **OA script** (`oa-script`),
so a plugin can do anything a built-in does. The host keeps only its own plumbing —
`oa.internal.*` (crop, backgrounds, the color transforms, a blur for blurred
backgrounds), registered under `registry::HOST_ID` by `Registry::from_plugins` whether or
not Atelier Core is on. `Registry::from_plugins` builds the registry the whole app
renders with from the enabled plugins, so turning a plugin off takes its effects out of
the menus *and* out of rendering; clips that used them keep their settings and are
reported as missing, naming the plugin that has them.

**OA script** ✅ (`oa-script`, 2026-09-24): the small language for everything that isn't
a GPU shader — sound shaders, motion, bounds, pass counts, sound tails. Statements (`let`,
`state`, assignment, `line` where the host has delay lines, host-function calls), math,
`select`/`choose`, parameters by id (a point's parts as `id_x`…), parsed to an AST and
compiled to a stack machine over one register file. No loops or branches, so every
script finishes; `let`s that only read steady inputs (parameters, the sample rate) are
hoisted to a per-block section. Each host (`oa_script::Env`) names what a script reads,
writes and may call — `oa_audio::shader` (samples, spectra), `oa_graph::script`
(`MotionScript`, `BoundsScript`, `PassScript`).

**Compiled to machine code** ✅ (`oa_script::jit`, 2026-09-24): sound shaders run per
sample, so they're compiled with Cranelift when they load (interpreted, the built-in
effects cost 20–40× what their native Rust versions did). Each op list becomes one
function `(registers, memory, first)`: registers a run reads before writing are loaded
into SSA values once and written ones stored once (`Keep::HostAnd`: a `let` used only
within a run is never stored); `OnFirst` is a branch; math with an instruction is inline
and the rest calls Rust functions with the interpreter's exact behavior. The memory
operations are **intrinsics** (`Intrinsic`), inline on `jit::Memory` laid out as
`oa_script::dsp` describes: delay-line reads (interpolated, wrapping) and writes, filters
(coefficients cached per call site, remade only when their settings change). Other host
functions call back through a `&mut dyn Host`. The interpreter stays the reference and
the fallback where Cranelift can't target the CPU; `shader::tests::
compiled_and_interpreted_agree` checks every Atelier Core sound effect is compiled and
sounds the same both ways.

Other plugins are folders under `<config>/plugins` (and `./plugins` beside the app),
each with a `plugin.json` listing effects: kind, usage, working space, parameters (a
friendly form — `{"id": "amount", "type": "float", "default": 0.5, "min": 0, "max": 1}`
— lowered to `ParamSchema`, so plugin parameters are keyframable and modulatable like
any other) and WGSL, inline or in a file beside the manifest. Nothing executes but
shaders and scripts, through the same path built-ins use. `oa.*` ids are reserved for
Atelier Core; bad effects are skipped and reported per plugin rather than failing the
load, and a manifest written for another `api_version` is refused. `plugins/README.md`
is the guide; `plugins/example-looks` in the repo is a working example (vignette,
scanlines, a sound shader, a motion intro).

Descriptor fields: kind (`PointOp`, `UvWarp`, `Spatial{expand}`, `Transition`,
`Motion`, `Glyph`, `GlyphPixel`, `TextBox`, `Sound`; `Temporal` and `Cpu` exist but no
plugin can declare them yet), **`Statefulness`** (`Pure` | `Stateful{preroll}`),
**`WorkingSpace`** (`Linear` | `Display`), `fusible`, `preserves_opacity`, param
schemas, and what the manifest adds: `category`, `description`, `preview` (thumbnail
settings), `editor` (`surface`, `equalizer`: host editors a plugin opts into by naming
its parameters as they expect), `second_input` (`media` | `original`), and scripts —
`motion`, `bounds`, `pass_count`/`pass_divisor`, and for sound `tail`, `latency`, `meter`.

* Stateful effects (feedback, simulations, audio reverb tails) are never cached; the
  planner reports the required **preroll** so rendering starts early enough to warm
  state. Chunked/parallel export must overlap chunks by the preroll. ✅ (planner) ⏳ (engine)
* Missing plugins: the planner skips them and reports them; project data is untouched. ✅

**Effect categories** ✅ (user direction, 2026-09-18): intros/outros aren't the same thing
as passive effects, so each descriptor declares its `usage`:
* **Passive** — for the whole clip: Exposure, Saturation, Tint, Posterize, Blur (keeps
  the clip's size — no bounds expansion, edges repeat inward), Mask, **Camera Shake**
  and **Wiggle**.
* **In/out** — a clip's intros and outros, each with its own duration: **Fade**,
  **Fly** (from/to left, right, top, bottom; distance; optional fade), **Zoom**,
  **Defocus**, **Wipe**. A clip can have **several** of each; they all run, each over
  its own window (motion effects combine, shader effects chain in clip order), e.g. a
  2 s fly-in with a 4 s fade-in.
* **Cut** — two-input transitions between adjacent clips (§16).
Menus only offer effects that can actually render (`Registry::offered`), and a test
renders every offered effect and checks it changes the picture.

**Motion effects** ✅ (`EffectKind::Motion`): anything that moves the whole layer — fly,
zoom, fade, shake, wiggle — can't be a pixel shader (a shader only sees the pixels inside
the layer). A motion effect is a deterministic script of its params and clock
(`oa_graph::script::MotionScript`: reads the clock, the canvas size and `leaving`; writes
`move_x`/`move_y` in canvas px, `zoom`, `turn`, `opacity`; `noise`/`jitter` give each
instance its own smooth randomness), folded into the layer's transform around its anchor
by the planner; the GPU still draws the moved layer. Camera Shake adds a touch of zoom so
the moving edges stay hidden. Plugins write them the same way (`example-looks`' Drop In).

**Effect roles**: under the hood every effect instance has one of three roles:
* **Passive** — applies for the whole clip (all effects today).
* **Transition in** — plays over the clip's first `duration`; the shader receives
  `progress` (0 → 1) as a uniform, so e.g. a blur-in or a zoom-in is one WGSL function.
* **Transition out** — the same over the clip's last `duration`.

Roles are per instance, not per descriptor: any point/spatial/warp shader can be used in
any role. They're separate from the two-input **cut transitions** (§16), which mix one
clip into the next.

*The clock*: the planner appends three floats to every effect's uniforms — visibility,
progress, seconds — and generated code loads them before each effect function, so
shaders call `visibility()`, `progress()`, `clip_seconds()` (fused chains included:
each effect sees its own clock). Out of its window an in/out effect isn't in the graph
at all. A passive effect gets a still clock (stable cache key) unless its descriptor sets
`time_varying`. Built-ins written against `visibility()` — one shader, both directions:
**Fade**, **Zoom** (from a scale, fading), **Blur**, **Reveal** (soft edge at an angle).

**Property types** ✅: an effect's params can be numbers, vectors, colors, choices
(enum), on/off, text, or **media** — a file from the pool. A media param becomes the
effect's **second input**: the planner adds a `Source` for it at the layer's raster size
(following the clip's clock; stills ignore it), the executor runs the effect on the
two-texture layout, and the shader reads it with `sample_media(pos)` (stretched over the
layer; `has_media()`; the packed param is 1/0 for connected). Built-in **Mask** uses a
matte's luma or alpha, optionally inverted. One media input per effect for now.

**Direction** (`Unit::Direction`, 2026-09-18): a float in degrees on screen — 0 = right,
90 = down, clockwise — edited with a small **dial** (drag to point; it snaps to right
angles within 10°, Shift gives 15° steps; scroll to spin) next to a 0–360° field. Used by **Fly** (the direction of travel:
an intro arrives moving that way, an outro leaves that way — `MotionInput::leaving`;
distance 1 clears the canvas at any angle), Wipe (in/out and cut), Push and Shimmer.

**Directional gradient** (`Value::Gradient`, 2026-09-18): up to 6 color stops along an
angle, laid across whatever it's applied to (the layer, the text box). One stop is a
plain color, and a color stored where a gradient is expected is read as one (and vice
versa: `Value::coerce`), so params can switch type without breaking projects. Keyframes
interpolate stops (same count) and the angle the short way round. Shaders get 32 floats
and call `oa_gradient(base, pos, lo, size)`; point ops get their pixel's position from
`layer_pos()`. Used by **Tint**, text **color** and **outline color**, and **Glow**. The
editor: a bar showing the stops (click to add one there, drag markers to move them),
the selected stop's color and position, ✕/+, and the dial for the direction. **Simple by
default**: the inspector shows one color swatch; right-click → "Advanced: directional
gradient" shows the editor (and it stays shown while there's more than one color);
right-click the label → "Simple" keeps the first color. The swatch is our own
(`widgets::color_swatch`): egui's color button keeps its picker in the same popup slot a
context menu uses, so the two opened together. **Scale** works the same way: one
linked value, right-click for separate X and Y.

**Shimmer** is now a passive point op for any clip — video, image or text: a band of
light sweeping along its direction, repeating.

**Reverse outro** (`Item::outro_reverses_intro`, `Op::SetReverseIntro`): the Outro
section's "Reverse" button makes the outro the intro played backwards. The planner
reads effects through `Item::active_effects()`, which then drops the clip's own outros
and adds each intro again with `EffectRole::Reversed { duration }` — the intro's clock
run backwards over the clip's last `duration` (visibility, progress and seconds all
reversed, and motion isn't `leaving`), so a fly-in from the left leaves back to the
left. While it's on, adding outros is disabled; the clip's own outros are kept for when
it's turned off.

*UI*: the inspector has **Intro** and **Outro** sections (one card per in/out effect:
on/off, duration, ✕, settings; "+ Add intro/outro"), **Cut from previous clip** (only
when a clip sits directly before), and **Effects** — one card per passive effect with
on/off, ↑/↓, ✕ and a widget per property type (sliders with keyframe toggles, color
pickers, menus, checkboxes, text, a media picker). The UI font is Segoe UI with Segoe UI
Symbol as fallback (egui's own fonts lack ▲ ✕ ◆ ↶, which drew as empty boxes).

*Effect previews* (`app/src/thumbs.rs`): every picker shows each effect as a small real
render of the selected clip with that effect added (intros/outros half-way through), and
**hovering a preview plays it** — intros/outros run through and loop after a pause,
animated effects move, still effects sweep from off to full strength. They render from a
scratch copy of the project in which the clip is **frozen on the playhead's frame**
(`TimeMap` speed 0), so every preview and every hover frame reuses one decoded frame
from the node cache — nothing seeks or waits on the decoder. Previews are **framed on
the clip** (its bounds plus room to move), so a small title fills the cell, and render
at a resolution matched to that framing. Still previews are cached per (document,
playhead, format, clip) and rendered within a 12 ms budget per UI frame; the hover
animation draws into one reused texture. **While playing** the previews stay on the
moment playback started from (the moving playhead used to throw them away every frame,
so they flickered to the loading skeleton), and whenever they do need redoing — a new
moment, a slider being dragged — each keeps showing its last picture until the new one
is ready (`Thumbs::stale`); the skeleton only shows for a preview never rendered yet.

*Preview brightness* (fixed 2026-09-18): the preview texture is sRGB-encoded, and egui
reads user textures as raw gamma bytes; handing it the texture's sRGB view gave it linear
light, so the preview looked far too dark (mean 26 vs 82). `readback::display_view` gives
UIs a plain `Rgba8Unorm` view of the same bytes.

**Text shader control** ✅ (§17): text renders from glyph distance fields on the GPU,
and a text effect can act at three levels —
* **per pixel** (`EffectKind::GlyphPixel`): a fragment function over the rendered glyphs
  that sees the pixel's color, its distance to the outline, its glyph and its place in
  the box — **Glow** (plus the built-in gradient fill and outline, and any
  ordinary effect on the text layer: shimmer, blur, tint, wipe…);
* **per letter** (`EffectKind::Glyph`): a function run once per glyph with its index,
  count, line, word, center and em size, returning an offset, scale, rotation and color
  — **Letter Wiggle**, **Wave**, **Rainbow Letters** (passive); **Typewriter**,
  **Letters Rise**, **Letters Pop**, **Letters Scatter**, **Letter Fade** (intro/outro,
  staggered with `letter_progress(g, stagger)`);
* **whole object**: the layer transform, motion effects (Fly, Zoom, Wiggle…), as for
  any clip.
All three take keyframable and procedural params and the in / out / passive roles.
Text effects are `text_only()`: offered only on text clips (`Registry::offered_for`),
listed first there, and ignored by the planner on picture clips.

Plugin tiers:
1. ✅ **Shader plugins** — JSON manifest + WGSL body (`oa_graph::plugin`); host generates bindings (versioned
   preamble); validated by naga; fusible if declared.
2. ⏳ **WASM plugins** (wasmtime, component model/WIT) — clip types, drivers, text animators,
   CPU processing. They return *plugin-facing* graph fragments that the host lowers.
   Seeded RNG + host time only (no wall clock) so they stay deterministic. Memory/fuel
   limits.
3. **Native** (opt-in) — ML, codecs, OFX/CLAP bridges; preferably out-of-process.

## 9. Color ✅ (8-bit decode ⏳)

* **Working space**: scene-linear Rec.709 primaries in `Rgba16Float`, premultiplied. 1.0 is
  SDR reference white — HDR sources map 203 nits there (BT.2408) — and log footage lands
  its 18% gray at 0.18, so highlights simply go past 1.0.
* **Spaces per effect**: effects declare `Linear` (default) or `Display`. Point ops convert
  inside their own pass; any other kind declaring `Display` is wrapped by the planner in
  hidden `oa.internal.to_display` / `to_linear` point ops, which then fuse with their
  neighbors. Fusion never mixes spaces.
* **Input transform** per file (`oa_doc::color::InputColor` on `MediaRef`, `Op::SetMediaColor`),
  every part **overridable** because file tags are often missing or wrong (phones tag
  log as Rec.709):
  * *YCbCr matrix* (601/709/2020) and *range* (video/full) — they reach the decoder's
    YCbCr→RGB pass through `NodeOp::Source::yuv` (in the cache key).
  * *Curve*: sRGB, Rec.709 (2.4), gamma 2.2/2.6, linear, **PQ**, **HLG** (1000-nit OOTF),
    **ARRI LogC3, Sony S-Log3, Panasonic V-Log, Fujifilm F-Log, Canon Log 3, Apple Log**.
  * *Gamut*: Rec.709, Rec.2020, Display P3, ARRI Wide Gamut 3, S-Gamut3(.Cine), V-Gamut,
    Canon Cinema Gamut — matrices derived from the primaries (checked against BT.2087).
  * *Exposure* trim in stops.
  `Automatic` reads the file's tags (`MediaInfo::color`, from ffprobe; old projects get
  them from the fresh probe on open), and a hand-picked log curve brings its camera's
  gamut. The planner adds `oa.internal.input` right after the decode — nothing at all
  for plain sRGB/Rec.709 files. The decoder hands over sRGB-decoded values, so the op
  re-encodes to code values first; tested at each curve's published reference point.
* **Output transform** (`schema::output()` on the sequence): exposure (keyframable) and a
  **tone map** — `auto` (on only when the sequence, or a compound in it, uses HDR or
  log footage, so SDR projects are untouched), `off` (clip), `soft` (identity below 0.8,
  a rational roll-off above, hue kept) or `filmic` (ACES-like S-curve). It's the
  planner's last node (`oa.internal.output`), so preview, thumbnails and export all get
  exactly the same picture. The encoder then writes BT.709 limited range as before.
* **UI**: right-click a file → *Source color* (curve, gamut, levels, matrix, reset); a
  picture clip's Properties tab → *Source color* (curve, gamut, exposure); with nothing
  selected → *Output* (tone map, exposure).
* ⏳ 10-bit decode (P010) so HDR and log don't band; HDR output (PQ/HLG export);
  OCIO configs / LUTs; opt-in `Rgba32Float` for data passes (displacement, depth).

## 10. GPU executor ✅ / memory & robustness ⏳

**Which GPU** ✅ (`oa_gpu::context`, 2026-09-24): every adapter of the platform's APIs
(Windows: DX12, Vulkan, OpenGL; Linux: Vulkan, OpenGL; macOS: Metal) is scored — a real
GPU over software, dedicated over integrated, then the API that suits the platform
(DX12 on Windows, for zero-copy decoded video) — and adapters that can't run the renderer
(`select::shortcomings`: half-float targets that filter and blend, storage buffers in
vertex shaders for text) only win when nothing else is there. With no GPU at all, a
software adapter (WARP, llvmpipe, lavapipe) still gives a picture. The device asks for
the adapter's full limits, falling back to the conservative defaults. The **window**'s
device is made by eframe with the same rules (`WgpuSetupCreateNew` with our adapter
selector and device descriptor, among adapters that can present to the window — which
also hands OpenGL the display connection it needs on Wayland/X11); `GpuContext::from_parts`
wraps it. Settings → Graphics (API, card) or `OA_GPU_BACKEND` / `OA_GPU_ADAPTER` pin one;
`oa gpu` lists every adapter, what it lacks and the one chosen; if no window can be made
at all, a dialog says why and what to try. The whole renderer suite passes on DX12,
Vulkan, OpenGL, Intel integrated graphics and WARP (software). **Display images** are
plain `Rgba8Unorm` holding sRGB bytes encoded by the display shader (OpenGL can't view one
texture as both sRGB and not, which the earlier `Rgba8UnormSrgb` + view format needed).

GPU-first rule: nodes, frame sources and effects exchange **GPU textures**; the CPU only
sees pixels at explicit edges (`readback`: tests, stills, thumbnails; later the encoder).

* **`GpuImage`** = pooled `Rgba16Float` texture + layer-space origin. Effects that grow
  (blur) allocate padded outputs, so glows/blurs extend past the layer instead of
  clipping.
* **`FrameSource`** trait returns GPU images. Hardware decoders will wrap decoder
  surfaces behind it; today `TestPatternSource` renders media stand-ins in a shader.
* **Shader contract** (plugin-facing, versioned with `PLUGIN_API_VERSION`): one bind group
  (64-float uniform block, input texture, linear clamp sampler). Point ops are
  `fn(c: vec4f, base: u32) -> vec4f` on straight color in their declared working space;
  spatial effects are `fn(pos: vec2f, base: u32) -> vec4f` using `sample_input(pos)` and
  `pass_index()`. Built-in effects ship their WGSL in the registry exactly like plugins.
* **Composites draw transforms directly**: a `Transform → Composite` layer is one textured
  quad into the composite target — no intermediate texture per layer. Blend modes are
  fixed-function (Normal, Add, Screen; Multiply exact over opaque backdrops; Darken and
  Lighten as a per-channel min/max, the layer laid over white or black first so its
  transparent parts change nothing — exact where it's opaque, approximate at partial
  opacity). A clip picks one with its `transform.blend` property (Properties → blend).
* **Texture pool**: reuse is decided by reference count (a texture is free when only the
  pool holds it), idle textures are released after a few frames. 60 frames of playback
  stay within 8 textures in tests.
* **Node cache** keyed by `CacheKey`, LRU within a byte budget; an unchanged frame renders
  with zero GPU passes.
* **Raster quantization bounds minification**: media is rasterized at ≥ its on-screen
  scale and < 2× it, so single linear samples in the layer draw never minify by more
  than 2×. ⏳ Mips/multi-tap for nested/vector content and warps.
* **Measured** (RTX 3060, Vulkan, debug build): 1080p demo frame with 4K test source,
  keyframed blur, fused color chain and an animated card: ~20–35 ms including first-frame
  readback.
* ✅ **Edge handling**: spatial effects can sample with edge pixels repeated instead of
  fading to transparent. The blur exposes it as `edges` (default `clamp`), which stops
  full-frame layers darkening at the canvas border.
* **Known gaps**: stateful effects render as passthrough (reported); no anti-aliasing on
  rotated layer edges yet.
* **Open issue**: during development the GPU test binary hung (one core busy) in several
  runs with ≥3 tests in parallel, each with its own device. It hasn't reproduced in 30+
  runs since a racy test assertion was fixed; `examples/stress.rs` exists to chase it
  (per-thread or shared device, `OA_GPU_BACKEND=vulkan|dx12`).

Memory & robustness ✅ / ⏳ (`health.rs`):

* ✅ **Own memory accounting** (wgpu can't report VRAM): the renderer counts what it
  allocated — pool plus node cache — against a **budget** (2 GB by default, a user
  setting). At three quarters it trims the cache; at the budget it releases idle
  textures the same frame, evicts down to what's left, and reports `Pressure::Over`, on
  which the app **renders the preview smaller** (down to a quarter) and says so, easing
  back once there's real room again. An allocation the driver refuses (`OutOfMemory`)
  drops the whole cache at once. Degrading beats failing.
* ✅ **The device going away** is noticed (`set_device_lost_callback`,
  `on_uncaptured_error`): the app autosaves the project *first*, stops rendering rather
  than looping on failed frames, and tells the user to restart. Errors the driver
  reports are counted and shown in the Performance panel.
* ⏳ Rebuilding a lost device in place (eframe shares one device with the UI, so today
  this means restarting), and loop bounds and time limits for plugin shaders.
* ⏳ Hardware decode → GPU textures: ship the copy path first, add zero-copy per platform.

## 11. Media ✅ (Windows, Linux; macOS untried)

**Probe & index** (`ffprobe`, metadata only — never pixels): container, duration, an
optional **video track** (codec, size, coded size, rotation, time base, rate, color,
alpha, still-or-moving, full frame index) and an optional **audio track** (codec, rate,
channels). Audio-only files and single images are first-class, not errors. From that come `frame_at` (the §1 selection rule), `keyframe_before`, `max_gop` and
variable-rate detection. Files are identified by a **content fingerprint** (size + hashes
of three 1 MiB chunks), not by path.

**Decode path, entirely on the GPU** (`oa-media::windows`):
1. Media Foundation decodes with DXVA into D3D11 NV12 surfaces, on a D3D11 device created
   on the **same adapter** as the wgpu device (matched by LUID).
2. Only frames that are actually shown are GPU-copied into a shared NV12 texture
   (`SHARED_NTHANDLE`), synced with a D3D11 event query. Each texture is **leased**: the
   decoder writes it again only after the renderer's GPU work reading it has completed
   (`queue.on_submitted_work_done`), never by guessing from a rotation count.
3. D3D12 opens each shared handle once; wgpu wraps it via `create_texture_from_hal`.
4. `convert_nv12` turns it into linear RGB, cropping decoder padding (e.g. 1088 → 1080)
   and resampling to the requested raster size, supersampling when minifying.

**Frame-accuracy rules**: play forward without seeking; seek to the keyframe at or before
the target, then decode forward (up to 12 frames are reached by decoding forward instead);
if a decoder lands past the target, back up a GOP and retry. Decoder clocks are calibrated
against the index on open, because **Media Foundation ignores MP4 edit lists** while
ffprobe applies them (a 2-frame offset with B-frames). Files are opened as an MF byte
stream, since its URL resolver rejects paths over 260 characters.

**Verified** against generated clips whose frame number is painted into the picture:
H.264 30 fps with B-frames (1/15360 time base), NTSC 29.97 long-GOP, variable frame rate,
and HEVC — playback, exact frame boundaries, one flick before a boundary, scrubbing and
stepping backwards all land on the right frame.

**Measured** (RTX 3060, release, plan + decode + convert + blur + fused color chain +
composite, waiting for the GPU each frame):

| source | canvas | fps | median frame |
|---|---|---|---|
| 1080p30 H.264 | 1920×1080 | 354 | 2.1 ms |
| 4K60 H.264 | 1920×1080 | 182 | 4.9 ms |
| 4K30 HEVC | 1920×1080 | 309 | 2.2 ms |
| 4K60 H.264 | 540×960 (9:16 preview) | 185 | 4.9 ms |

**Importing** ✅ (`oa_media::import`): probe → classify (video / still / audio) →
**conform** when the platform decoder can't open the file as it is. Conforming remuxes to
MP4, re-encoding only if the streams can't be copied, into a cache keyed by fingerprint;
the project keeps pointing at the original file and only decoding uses the copy. Animated
GIF goes down this path on Windows, MKV does not (Media Foundation reads it).

**Stills** ✅: decoded once with ffmpeg to RGBA, uploaded, then reused for every frame that
shows them — with alpha, which the NV12 video path can't carry. The one CPU copy happens
at import, not per frame.

**Relinking** ✅: files are identified by content fingerprint, so a moved file is found by
searching the project's folder (and its `samples/`) and matching content, never just the
name. Missing media keeps its clips and is flagged in the UI.

**Rotation** ✅: display-matrix metadata (phones record sideways) is read by the probe,
which reports the *display* size; the conversion shader un-rotates while converting, so
nothing downstream deals with orientation. Tested against a clip tagged 90°.

**Recently shown frames** ✅: converted frames stay on the GPU in a 128 MB window, so
stepping or playing backwards reuses them — 20 backwards steps cost 0 seeks and 0 decodes
in tests, instead of a seek plus a GOP of decoding each.

**Decode threads** ✅: every file gets a thread that owns its decoder (MF's COM objects
never cross threads). The renderer asks for a frame index and waits only for that frame;
meanwhile the thread decodes up to 4 frames **ahead** unless the viewer is stepping
backwards. Frames cross as `Surface`s (NV12 texture + lease); conversion to the working
format stays in the render graph. Measured in the app: 161 of 162 frames during playback
were already waiting, 0 seeks; export went from 102 to 113 fps (now bound by x264).

**Gaps** ⏳: no decoder budget (threads per open file), no proxies; 8-bit NV12 only (P010/10-bit, 4:2:2 and HDR transfer conversion are
rejected or flagged, not converted); VideoToolbox (macOS) and VA-API zero-copy decoders.

**The portable decoder** ✅ (`oa_media::ffmpeg`, 2026-09-24): an `ffmpeg` process
decodes (`-hwaccel auto`: VA-API, NVDEC, D3D11VA, VideoToolbox when there is one; the CPU
otherwise; `OA_FFMPEG_HWACCEL=0` turns it off) and streams NV12 on stdout, `-noautorotate`
(rotation is applied at conversion, as for hardware frames). Each frame's exact time
comes from the `showinfo` filter on stderr, in step with the frames; `-copyts` keeps the
file's own timestamps (the source re-bases them on the frame index), `-seek_timestamp 1
-noaccurate_seek -ss t` lands on the keyframe at or before `t` as `VideoDecoder::seek`
asks, and `-fps_mode passthrough` (`-vsync` before ffmpeg 5.1, detected once) stops
frames being duplicated or dropped. Frames are uploaded as two plain textures — R8 luma
and RG8 chroma (`Surface::chroma`, `Nv12Frame::chroma`) — which every backend samples,
unlike NV12 textures; the conversion shader already took the planes as two bindings.
The same frame-accuracy suite as Media Foundation's (numbered frames: playback, scrubs,
stepping back, VFR, HEVC, rotation) passes on it on DX12, Vulkan and OpenGL
(`tests/ffmpeg_decode.rs`). `oa_media::frame_source` picks per file: Media Foundation
when the GPU is on DX12 and it can take the file (zero copy), ffmpeg otherwise — so on
Windows, too, a Vulkan/OpenGL device or an unsupported codec still plays (imports are
only conformed to MP4 when Media Foundation is in use and can't read them). Settings →
Graphics → "Decode video with ffmpeg" (or `OA_DECODER=ffmpeg`) forces it.

## 11b. Preview UI ✅ (`oa-app`)

A window for exercising the engine end to end: `oa-app [video]`, or drop a file on it.

* egui/eframe runs on **the same wgpu device** as the renderer and the decoder (eframe
  makes it with our adapter rules, §10, and the app wraps it), so a decoded frame goes
  decoder → graph → display transform → screen without ever leaving the GPU.
* **The window** opens at last time's size and state; since asking for "maximized" at
  creation doesn't always take, it's asked again once the window exists, and a window much
  smaller than the screen (or bigger) is fitted to 85% of it, centered. Sizes under
  800×500 are never remembered.
* **Compact properties panel** (Settings → Interface): the inspector with tighter rows,
  slightly smaller text and controls, a shorter tab bar, snugger effect cards, and each
  section's explanation moved into its heading's tooltip (`inspector::set_compact`).
* Playback (space), frame stepping (arrow keys), scrubbing, format switching (16:9, 9:16,
  1:1, plus presets that can be added live), preview scale (full/½/¼), and a
  "no optimizer" toggle that renders the reference graph for comparison.
* Live controls for exposure, saturation, blur and shake, all going through document ops,
  so undo/redo work and slider drags coalesce into one undo step. "Keyframe blur here"
  writes a keyframe at the playhead.
* A stats panel shows frame time, GPU passes, cache hits, fused chains, pool and cache
  bytes, decode counters and any warnings (unsupported effects, missing media).
* **Idle costs nothing**: the frame is re-rendered only when the playhead, format, scale
  or document changes (1% of one core idle, measured).
* **Measured**: 4K60 H.264 playing at 1920×1080 in the window: 4.3 ms per frame (~233 fps),
  0 seeks, ~1 decode per shown frame.
* Sound plays through `oa-audio`, and while playing **the audio clock drives the
  playhead**; the panel shows the device, buffered milliseconds, underruns and the video
  offset. `--autoplay` starts playback immediately (handy for testing).
* **Media pool**: import files (button, drag & drop, or command line), see what each one
  is, add clips to the timeline, relink missing files. Stills default to 5-second clips.
* **Projects**: New / Open / Save / Save as, with a dirty marker; opening re-imports each
  file (restoring conformed copies and frame indexes) and relinks what moved.
* **Losing work takes effort** ✅: the crash-recovery autosave (every 15 s to
  `<config>/autosave`) *plus* **save as you go** — a project that has a file is written
  back every 20 s once you pause, so the file on disk is never far behind. A write that
  fails turns it off and says so rather than nagging. Closing the window with unsaved
  changes asks first (Save and close / Close without saving / Keep editing), and a lost
  GPU device saves before it reports.
  **Recovering** (`App::recover`, `home::recovery_card`, on Home and as the editor's
  banner): an autosave whose project file still exists is flagged in gold — on its card
  and on that project's recent card — with three choices: *Continue previous session*
  (it opens as the project's unsaved changes; saving writes the project's file), *Move
  to new project* (it opens untitled; the saved file is left alone) and *Discard*. It's
  tied to its project and deleted only once the background open has finished (it used
  to be marked before the load and deleted while it was still being read).
* **Timeline**: tracks top-down (V2 over V1, then audio), a ruler, keyframe diamonds on
  clips. Drag a clip to move it (also onto another track of its kind), drag an edge to
  trim; both snap to clip edges and the playhead (Ctrl turns snapping off) and land on
  frame boundaries. S / Ctrl+K split at the playhead, Delete removes, Shift+Delete
  ripple-deletes, Ctrl+Z / Ctrl+Shift+Z undo and redo — every drag is one undo step.
* **Big projects** (2026-09-24; measured on the hour-long, intensity-1 `oa stress`
  project: 8,871 clips, 31,808 effects): rendering was never the problem (2.7 ms a
  frame median), the editor was.
  * **Clips by id** (`Editor::item`): an index of where each clip is (track, place),
    rebuilt only when the project changes — keyed by `Document::revision` (bumped by
    every edit, undo and redo), the snapshot and the open timeline, and every hit
    checked against the clip's id, so it can't hand back the wrong one. The timeline
    asks for every clip it draws each frame; searching the tracks each time cost ~9 ms.
  * **Only the clips in view**: a track's clips are in time order and never overlap,
    so the first and last in view are a binary search away, and the rest aren't laid
    out at all. Zoomed out so far that clips are under 3 px wide (the whole hour
    fitted), neighbors are drawn as one block per run (`ClipRun`: no names, keyframes,
    thumbnails or waveforms to work out), lit when any clip in it is selected; a
    marquee over a run selects the clips it crosses.
  * **Saving off the UI thread**: the recovery autosave (15 s) and save-as-you-go (20 s)
    turned the whole project into JSON on the UI thread — ~0.2–0.3 s, a regular hitch.
    Both now write an immutable snapshot on a thread of their own, one write at a time;
    once it's on disk, that snapshot is what counts as saved. Writes that must happen
    before the app might go down (a panic, a lost GPU, closing) stay on the spot
    (`Autosave::write_now`); Ctrl+S, opening and new projects wait for a background
    save first, so one file never has two writers and a save never marks the wrong
    project.
* **Breaking a compound clip apart** (`oa_edit::compound`, 2026-09-24; Clip menu and
  right-click: "Break apart"): nesting run backwards. The clips inside come back onto
  the timeline where they play now — shifted by where the compound clip sits and what
  part of its inside it shows, trimmed to that stretch (what was never shown is left
  out). Picture tracks keep their stacking: the lowest onto the compound clip's own
  track, each above onto the next track up with room, or a new track there; sound onto
  the first sound track with room, or a new one. Each piece's per-format settings come
  along. The compound stays in the bin. Its own transform, effects and transitions
  worked on the whole, so they can't be split among the pieces — the app says so when
  there were any. A compound clip at another speed isn't broken apart yet. Several at
  once make one undo step, and the pieces end up selected.
* **Swapping media** (`oa_edit::swap`, 2026-09-22): a media or compound card dragged
  from the bin onto a clip of the same kind (picture onto picture, sound onto sound)
  **swaps** what the clip shows — the clip is outlined with "⇄ Swap for …" — and keeps
  everything done to it: its place and length, speed, transform, crop, effects,
  transitions and keyframes. It starts from the same point in the new media when that
  fits; otherwise early enough to end inside it, and a clip longer than the whole new
  media is cut down to it (a still has no end). Titles and solids aren't swapped, a
  clip's own media dropped on it adds a clip as before, and **Shift** while dropping
  adds a clip instead of swapping. One undo step.
* **Viewer (direct manipulation)**: click a layer to select it, drag to move (snapping
  to the canvas center and edges with guides), corner handles scale about the anchor
  (Shift: free aspect), edge handles scale one axis, the knob above rotates (Ctrl: 15°
  steps), the center dot moves the anchor without moving the picture, Ctrl+arrows nudge.
  In a secondary format, edits go to that format's override unless "edit all formats"
  is on. Hover outlines the layer a click would pick.
* **Speed belongs to the clip** (`Properties`): footage and compound clips carry a speed
  (0.05× to 20×, presets to 4×), which retimes picture and sound together — a video's own
  audio follows it, and the Sound tab says so instead of offering a second control.
  Extracted audio is its own clip and keeps its own speed.
* **Inspector, in tabs** (`inspector.rs`): **Properties** (text, transform, crop),
  **Transitions** (intro and outro), **Effects** and **Sound** — one job per tab, since a
  clip with five effects used to bury its opacity. Tabs a clip has nothing in don't
  appear; with nothing selected the panel shows the **scene** (the background) instead.
  Every property shows its value at the playhead with a keyframe diamond (off / key here
  / keyframed); changing a keyframed value adds a key at the playhead; ◀ key ▶ jumps
  between keys.
* **Scripted input** (`--script steps.txt`): clicks, drags, keys and modifier changes
  injected into egui's raw input, plus `print` to log the selection's transform — how the
  UI is tested without touching the real mouse.
* **Timeline view**: Ctrl+wheel zooms around the pointer, the wheel scrolls, "Fit" goes
  back to the whole sequence, and the view follows the playhead while playing. Track
  headers have on/off toggles; "+V"/"+A" add tracks. Transition windows are drawn as
  bands over the cuts; T adds a cross dissolve at the nearest cut (or a fade on the
  selected clip); the inspector picks the type, duration and settings.
* **Smooth scrubbing**: while paused, the preview never waits on the decoder. Each file
  has up to 3 decoders; a scrub sends "wants" where only the newest counts, and until
  the exact frame is decoded the preview shows the nearest frame on hand (a stand-in —
  never cached). When the playhead stops, the exact frame replaces it within a few ms.
  Slowest scrub render measured 5.5 ms (debug build, long-GOP clip).
* **Preview resolution Auto** (default): renders at the size the viewer shows (physical
  pixels), so a small viewer doesn't render 4K; Full/½/¼ override it.
* **Viewer zoom & guides**: Fit/50%/100%/200% (Ctrl+0 / Ctrl+1), Ctrl+wheel zooms about
  the pointer, wheel or middle-drag pans; toggleable rule-of-thirds, center cross and
  title/action safe areas.
* **Text**: "+ Text" (Ctrl+T) adds a title at the playhead on top of everything and puts
  the cursor in the inspector's text box; the Text section sets font (every installed
  family), bold/italic, alignment, size, directional-gradient color, letter spacing, line height
  and outline. Text clips are purple on the timeline and labeled with their words;
  clips show intro/outro ramps.
* **Timeline editing** (`clips.rs`, `oa_edit::timeline::{move_items, paste}`): a
  multi-selection (click, Ctrl/Shift+click, marquee, Ctrl+A) with a primary clip for the
  inspector; groups (Ctrl+G / Ctrl+Shift+G, `Item.group`) select, move and copy together;
  cut/copy/paste/duplicate, delete and ripple delete, enable/disable, "Extract audio"
  (a linked audio clip; the picture clip goes quiet via `audio.enabled`). Right-click
  menus for clips, track headers (rename, move up/down, delete…) and empty space; a Snap
  toggle. Clips draw a **filmstrip and waveform** (`previews.rs`, extracted by ffmpeg on
  worker threads, one texture per file) and a **keyframe line** (`band.rs`): volume for
  sound, opacity for pictures, or any number property picked with right-click →
  "Default keyframe property"; drag a key or the whole line, Ctrl+click adds/removes keys.
* **Copy/paste of effects and values**: right-click an effect card's name (copy one or
  all, paste settings into the same kind, paste effects onto other clips — also in the
  clip menu); right-click a property to copy/paste/reset its value. **Transforms**
  copy/paste too (clip menu, or copy/paste next to Transform's reset): every transform
  and crop parameter, keyframes and per-format overrides included, onto all selected
  picture clips.
* **Background** (`Sequence::params`, `schema::background()`, `Op::SetSequenceParam`;
  Properties panel with nothing selected, or its Background section): *Solid color*
  (black by default; one color just clears the canvas, a gradient draws the hidden
  `oa.internal.fill` under the clips), *Blurred content* (the finished frame — every layer
  as placed, with transforms and effects — scaled about the center of its content's bounds
  until that covers the canvas, worked at ¼ size, Gaussian-blurred, darkened by
  `oa.internal.dim`, stretched back up under the frame) or *Texture* (a picture or compound, tile height a
  share of the canvas, repeated by `oa.internal.tile`; a compound **loops**, so a short
  one still fills the background after its own end — it used to go black there).
  Keyframe-aware writes; top-level
  sequence only (compounds stay see-through). Spatial passes now grow each side of their
  output independently, to whatever bounds the planner gives.
* **Background effects** ✅ (2026-09-21): the background is a clip of its own —
  `Sequence::background`, id `oa_doc::BACKGROUND` (0; real ids start at 1), on no track
  as long as the timeline (at least a second — derived: every edit, undo and load
  re-syncs it, `Sequence::sync_background`, so keyframe and wave editors span the
  timeline; it was "forever" at first, which squashed them), not written to the file
  while it's plain. So every effect op
  (`InsertEffect`, `SetParam` with `ParamTarget::Effect`…), the inspector's effect cards,
  keyframes, curves and waves work on it unchanged. The planner runs its passive picture
  effects (`Planner::effect_chain`, the same chain clips use) on whatever the background
  drew — a canvas of its color for a plain color (its clear color stays underneath, so a
  blur's edges don't go black), the gradient, the tiles, or the blurred-content backdrop.
  For blurred content they run only on the backdrop (flattened onto a canvas first — it
  arrives placed, as a transform), never also on a canvas under the clips: that canvas was
  opaque, the blur was made of it, and the frame's edges came out black (fixed 2026-09-24;
  before, a background effect with blurred content also failed the frame outright,
  "Transform must feed a Composite layer"). The renderer now also draws a transform that
  feeds anything but a composite into an image of its own instead of failing.
  Inspector: *Background effects* under the Background section when nothing is selected.
* **Depth** (`oa.depth.slab`): turns a clip into a sheet with thickness — a drawing on
  paper — and renders it in 3D. A ray per pixel is turned into the sheet's own frame and
  marched through the block it's cut from: the first point where the picture is opaque
  and the ray is inside the paper is what you see, so faces really do hide the edges
  behind them. `depth` is the thickness as a share of the width, `yaw`/`pitch` turn it,
  `pov` moves the camera in (0 = flat-on, 1 = strong perspective), and a `map` makes the
  thickness follow a picture's brightness (a relief instead of a flat sheet). Cut edges
  are shaded darker. `yaw` and `pitch` go all the way round (±180°): past 90° you see the
  back of the sheet — the picture mirrored, at the `back` brightness. The output grows
  to the turned sheet's outline (`EffectDescriptor::grown_bounds` projects the block's
  eight corners through the shader's own camera), so a sheet swung towards the camera
  isn't cut off at the layer's edge.
* **Surface** (`oa.warp.surface`, 2026-09-21): the picture stretched over a grid of
  points (Corners, 3 × 3 or 4 × 4), each placed on its own. Params are one `Vec2` offset
  per point of a 4 × 4 grid (`p<row><col>`, fractions of the layer, zero at rest), so
  each point keyframes like any property. The shader finds, per output pixel, the grid
  cell covering it and its place in that cell (bilinear interpolation run backwards,
  four samples a pixel for smooth edges). In the viewer (`app/surface.rs`) a clip with a
  switched-on passive Surface shows its points and mesh *instead of* the transform
  handles; dragging a point writes its offset (a key at the playhead when keyframed);
  dragging inside still moves the clip. The inspector shows the grid, Reset, and the
  points in use by name. Points pulled outside grow the output (`grown_bounds`).
* **Glow** (`oa.light.glow`, 2026-09-22): light leaking out around the clip, the clip
  itself left sharp. Not a blur: each pixel outside takes the color of the clip's
  **nearest edge pixel** and fades with the distance to it (`(1 - d/radius)²`, so round
  corners get round halos). The nearest edge pixel comes from a **jump flood**: pass 0
  marks the seeds (pixels more than half opaque), then one pass per halving of the
  radius (steps 2ⁿ⁻¹ … 1, 8 taps each) keeps, per pixel, the way to the nearest seed
  any neighbor knows, and a last pass draws. Its cost grows with log₂ of the radius —
  8 passes at 30 px — and pixels inside the clip skip the drawing. The pass count comes
  from the radius on the host (the manifest's `pass_count` script, `light/glow.passes`; the shader
  counts the same way). The planner hands the effect the clip's picture as its second (`second_input: original`)
  input (a mask's matte's path), which the last pass reads for the edge colors and lays
  over the halo. `mode`: *behind*, or *over* (added on top too).
  **Wide glows run on a coarse grid** (`pass_divisor`, `light/glow.divisor`): from 24 px of radius the flood
  works on cells of 2 × 2 pixels, from 48 px 4 × 4, from 96 px 8 × 8 — a quarter of the
  pixels per halving, and fewer jumps — while the drawing pass stays full size, so the
  picture and its falloff stay smooth. Each seed is an opaque point found inside its
  cell (four looked at), and the flood carries exact positions, so edge colors are
  read from real edge pixels. Passes can run at a fraction of the output's size
  through `EffectDescriptor::pass_divisor`; the shader finds its cell from
  `pos - out_origin()` and reads the coarse grid with `textureLoad`.
* **More effects and cut transitions** (2026-09-22): **Drop Shadow** (`oa.light.shadow`:
  the silhouette moved `distance` towards `direction`, softened over two rings of taps,
  in `color` behind the picture; the bounds grow by distance + softness), **Stroke**
  (an outline `width` px around what's opaque), **Glitch** (bands of rows jumping
  sideways on a beat and the channels pulling apart), **Kaleidoscope** (mirrored
  wedges), **Ripple** (rings running out from the middle), **Temperature** (white
  balance: warmer/cooler and green/magenta, brightness kept); and the transitions
  **Iris** (a circle opening from the middle), **Zoom** (the outgoing picture rushing
  in, the incoming one settling from close up), **Slide** (the incoming picture sliding
  over, easing to a stop) and **Blur Dissolve** (both defocusing to the middle). GPU
  tests cover each (the shadow and stroke on a small clip: the frame-filling test clip
  leaves them no room). The picker groups the stylize effects under "Stylize".
* **Tile** (`oa.warp.tile`) and **Scroll** (`oa.warp.scroll`), 2026-09-21: Tile puts the
  whole picture in each cell of a grid over the layer (`columns`, `rows`, a `gap` as a
  share of each cell, `mirror` to flip every other cell so neighbors meet edge to edge).
  Scroll slides the picture towards `direction` at `speed` px a second (keyframable, so
  it can speed up or stop) and, by default, wraps it round: its bilinear taps each wrap
  at the layer's own edges, so there's no seam where the picture meets itself (`edges:
  leave` lets it slide away instead). Because the wrap is the layer's size and a tiled
  picture repeats a whole number of times across it, **Tile then Scroll** is an endless
  scrolling pattern — scrolled by one cell it's pixel for pixel the same picture (GPU
  test) — and **Scroll then Tile** scrolls inside every cell.
* **Effect picker** (`picker.rs`): the Intro, Outro and Effects menus share one
  searchable grid — type to narrow by name or category, or click a chip. Built-ins are
  grouped by what they do (Color, Blur, Depth, Text…), a plugin's effects under the
  plugin's name.
* **Font menu** (`fontpick.rs` + `oa_text::fonts::sample_image`): every installed family,
  searchable, each name drawn in its own font. Samples are rasterized from the family's
  outlines, a few per frame and only for rows on screen, so a machine with a thousand
  fonts doesn't stall; a font that can't draw its own name falls back to plain text.
* **Auditioning sound** (`audition.rs`): audio cards in the bin — project media and
  assets alike — get a play button on hover, which opens a second output stream with
  just that file on it. It stops at the end of the file, on a second click, when another
  one starts, or the moment the sequence itself plays, since that's the thing being cut.
* **Volume is a clip property, keyframable like any other** (`schema::AUDIO_GAIN`, in
  dB): the ◇ next to it keyframes it, and it's the default line drawn on audio clips in
  the timeline, so a fade can be drawn by hand there. The mixer evaluates the curve every
  ~1.3 ms and ramps between, in playback and export alike. The volume in the panel below
  the inspector is *monitoring* — how loud it is here, not in the film.
* **Asset library** (`assets.rs`, `<config>/assets`, `OA_ASSETS_DIR`): a folder on the
  computer that every project can see — the whoosh you always use, a logo, a grain plate.
  The bin has **Media** and **Assets** tabs; the library is scanned from disk (with its
  own folders), "Save to assets" copies a clip's file in so it survives the project, and
  ＋ or a double-click imports it into the project the way dropping the file in would.
  Nothing about it touches the document.
* **Media-bin folders** (`MediaRef::folder` + `Project::bin_folders`,
  `Op::SetMediaFolder` / `Op::SetBinFolders`): a path per file ("b-roll/day 2"), and the
  project's own list so a folder can exist while it's still empty. **New folder** makes a
  card you type the name into — not a box inside a menu, which vanished when clicked.
  Breadcrumb, drop a card on a folder to move that file in, "Move to folder" in the
  card's menu, Delete puts the contents back at the top. Searching looks in every folder;
  compound clips stay at the top.
* **Scale mode** (`MediaRef::scaling`, `Op::SetMediaScaling`; right-click a bin card or a
  picture clip → Scale mode): *Pixel* (nearest neighbor), *Smooth*, or *Automatic* — pixel
  for pictures up to 32×32. The planner sets `LayerInfo::pixelated` and the compositor
  samples that layer with a nearest sampler; pixel-art textures are enlarged the same way
  before tiling.
* **Drag from the bin**: drop a card on the timeline to place it at the pointer, on the
  track under it if that spot is free (else another free track of its kind, else a new
  one); a ghost shows where while dragging (`Editor::place_clip`).
* **Notifications** (`notify.rs`): anything that fails where the user can't be
  interrupted becomes a card in the bottom-right corner — errors stay 12 s, notes 5 s,
  hovering holds them, a click dismisses — and also goes to the Messages list, so nothing
  is lost. Typed names go through `check_name` (non-empty, sane length) and say what is
  wrong instead of being dropped.
* **Localization** (`i18n.rs`, `crates/app/locales/en.json`): `t("key")` / `args("key",
  ...)`, English embedded as the fallback, extra languages as JSON in `<config>/locales`
  (no rebuild needed), chosen by the settings file, `OA_LANG` or the system locale, with
  a picker on the start page. The start page, notifications and error messages are
  translated; the editor's labels are moving over key by key.
* **Menus that stay put** (`widgets::context_menu`, `widgets::sticky_menu`): menus close
  on a click *outside* them, so one can hold a search box, filter chips or a text field —
  picking a category in the effect browser no longer shuts the browser.
* **Icons** (`icons.rs`, `assets/fonts/`): Google Material Symbols, subset to the icons
  in use (28 KB rather than the 15 MB variable font; `assets/fonts/subset.py` regenerates
  it). egui doesn't shape ligatures, so each icon is drawn by **codepoint** with its
  Material name kept beside it; a `MaterialSymbols*.ttf` in `<config>/fonts/` overrides
  the bundled subset. Every icon also has a **drawn fallback** on the same 24-unit grid,
  so a missing font is never an empty box.
  Icon rows (`icons::menu_item`: icon, text, shortcut) fill the menus, and
  `icons::text_button` puts an icon before a button's words. Icons added after the
  bundled subset was made are drawn from their fallback until `subset.py` is re-run.
* **Keyboard focus** (`App::shortcuts`): only a text field keeps the keyboard. egui
  would move focus with the arrow keys (out of one-line fields) and Tab onto buttons,
  where Space then "clicks" them and the shortcuts stay off; a key press while a
  non-text widget has focus drops that focus, and arrow keys never move it. The caption
  list uses Up/Down itself, to go from caption to caption. On Windows, `winfocus::keep`
  also watches for the editor being the active window with *no* window holding keyboard
  focus: keys then arrive as "system" keystrokes that beep, and egui (told the window
  lost focus) hides the caret. It gives the focus back and logs what had taken it.
* **The mark** (`assets/logo.svg`, `logo.rs`): the "A" is read from the SVG's own path
  (flattened, ear-clipped into triangles) and drawn in white on an accent rounded square:
  on the start page, beside the app menu, and rasterized (4×4 samples a pixel) as the
  window and taskbar icon. `assets/logo-badge.svg` is the same badge for the README.
* **Start page** (`home.rs`): the mark and name, a segmented Projects/Plugins switch,
  three tiles (New, Open, Import), recoveries as warning cards, recent projects as
  picture cards, the language at the foot. Plugins are cards with an icon, version and
  "built in" pills, what they add and where from, and a switch (`widgets::toggle`).
* **One command list** (`command.rs`): every action — name, group, shortcut, whether it
  can run right now, and a plain function that does it — rebuilt each frame from the
  selection. The app menu, the menu bar's Edit/Clip/Timeline/View menus
  and the **action bar** all read from it, so a command written once appears
  wherever it makes sense and can't drift out of step. Ctrl+K works while typing.
* **Menu bar** (`menu.rs`): what's true of the session — the app menu (New, Open,
  Recent ▸ with project names, Save, Save as,
  Import, Export, Home), then Edit/Clip/Timeline/View menus holding every command, undo/redo naming what they'd
  undo, the project's name with a dot when unsaved, the format picker, and Export. The
  OS window title follows the project too. Editing actions are *not* here.
* **Action bar** (`menu.rs`, CapCut's idea): the verbs for whatever is selected — split,
  duplicate, delete, group, enable, extract audio — right above the timeline, with the
  timeline's own tools (title, +V/+A, snap, fit) on the row below.
* **Design tokens** (`style.rs`): spacing on a 4 px grid, text at 11/13/15/20, one accent
  color for selection and focus, gold for groups/keyframes/playhead, semantic warning and
  error colors, and 24 px icon buttons whose tooltip always names the shortcut. Panels
  are a shade darker than the content they hold.
* **Project cards** (`home.rs`, `thumbnail.rs`): saving a project writes
  `<project>.thumb.png` beside it — one frame, rendered the way the viewer renders — and
  the start page shows projects as cards with that picture, the project's **name** (taken
  from the file name the first time it's saved, e.g. `my_holiday_edit.oaproj.json` →
  "my holiday edit") and when it was last touched. Names and thumbnails are cached in
  settings.json, so the page never opens a project to draw itself.
* **Panels**: the media bin and inspector are clamped (220–520 and 260–560 px) so they
  can't squeeze the viewer shut, and the timeline panel is resizable with the tracks
  **scrolling inside it** — adding tracks no longer grows the panel until the viewer is
  gone. Track height has four steps (⇕ in the timeline's tools).
* **Start page** (`home.rs`): what opens with nothing to edit — recent projects (kept in
  `<config>/settings.json`), unsaved-session recovery, New / Open / Import, and a
  Plugins tab listing every plugin with a switch, its version, author, what it provides
  and any problems, plus Reload and Open plugins folder. "Home" in the top bar goes back
  without closing the project; files on the command line or dropped on the window go
  straight to the editor.
* **Layout**: the inspector runs the full height on the right; the timeline spans the
  rest of the bottom; the media bin (filling its column) and the viewer share the top.
* **Compound clips** ("As media" / "Nest into one clip" in the clip menu,
  `Editor::compound`): the selection is copied into a new sequence (same tracks and
  spacing, starting at 0, the open format). It appears in the media bin, can be added as
  a clip (`ItemKind::Nested`, see-through where empty) and chosen as any effect's
  picture (mask…): the planner's `media_source` renders a sequence id at the layer's
  size. Its sound plays through the clip (`oa_export::audio_clips` recurses, cut to the
  clip). Media and sequence ids share one allocator, so an id names one or the other.
* **Crop**: four keyframable edge fractions in Transform (`crop.left/right/top/bottom`),
  applied by the planner as the hidden built-in `oa.internal.crop` point op right after
  the layer's picture, before its effects. The layer keeps its place.
* **Media bin** (`bin.rs`): cards with a picture (hover skims a video), a waveform for
  sound, length badges and skeletons while loading; search, sort by added/name/type/length
  (either way); double-click or ＋ adds to the timeline; right-click relinks or deletes an
  unused compound.
* **Inside compound clips** (`compound.rs`): double-click one (or its menu → *Open compound
  clip*) and its own timeline becomes the open one; a breadcrumb bar above the tracks
  (and Esc with nothing selected) leads back, landing on the same moment with the
  compound selected. Edits change every use of it. The bin then offers only compounds
  that can't contain the one being edited. Export always writes the project's main
  timeline; undoing the compound away steps out of it.
* **Curve editor** (`curves.rs`): right-click a number property → *Edit curve…* opens a
  window with its animation over the clip: drag keys in time and value (between their
  neighbors), drag bezier handles either side of the selected key, easing presets for
  the segment after it (hold, linear, ease in/out/in-out, overshoot), double-click to
  add a key, Delete to remove one (Delete over the window never deletes the clip).
* **Effect order by drag**: each effect card has a grip (⋮⋮); drag it and a line shows where it
  lands among the other effects (intros/outros and sound effects keep their places in
  the same list — `move_target`); one undo step per move. The ↑/↓ buttons are gone.
* **Track picking, reordering and dividers** ✅ (2026-09-21, `oa_edit::sections`,
  `timeline.rs`): double-click a track's header (or empty space on it) to pick it: Ctrl+V
  and Ctrl+D then place the clips in its **first free space at or after the playhead**
  (`free_spot` / `paste_into_track` — keeping their spacing, or one after another if they
  came from several tracks), and Alt+↑/↓ moves it; any header can be dragged up or down
  to reorder. **Dividers** (`Sequence::dividers`, `Op::SetDividers`) run across
  **every track** and cut the timeline into **sections**: from each divider to the next,
  a clip belonging to the one its start is in; a divider placed across clips splits them.
  Each section is tinted its divider's color over all tracks; the flag sits in the
  ruler. Drag the divider's line (in the ruler, or a track's upper half) to move it
  alone, or its flag to move the whole section — its clips on every track — among the
  others (`move_section`: sections laid end to end in the new order, clips taken out and
  put back so none collide midway, the first stretch gets a divider if it moves off the
  start). Right-click: name, color, select its clips, move earlier/later, clear its
  clips, delete it (`delete_section`: everything after, on every track, moves back to
  close the gap), remove the divider. (They were per track for one day.)
* **Layouts and remembered views** ✅ (2026-09-21, `layout.rs`): *Standard* (viewer in
  the middle, properties in the tall right column) or *Vertical* for 9:16 work, which
  swaps them so the tall column holds the viewer; *Auto* picks Vertical while the format
  is taller than wide. Settings has the default; each project can switch with the
  viewer's "Vertical layout" button. Each saved project remembers its panel sizes, its
  layout and the format it was showing (`Settings::project_views`, by canonical path),
  restored on open: panels are keyed by an epoch bumped on restore (and different every
  launch), so egui takes the saved sizes as fresh defaults. The main window's size and
  maximized state are kept too (`Settings::window`, written once it holds still for
  half a second — an OS resize drag doesn't show as a pointer press).
* **Batch effects** ✅ (2026-09-21, `inspector::effect_list`, `App::apply_effect_to`):
  picture and sound effects share one card. Drag the grip *out of the list* and a label
  follows the pointer; let go over a clip in the timeline or the viewer (outlined in the
  accent color, red if it can't take it) and a copy goes onto it (`EffectDrag`, an egui
  DnD payload; `App::drop_effect`). With several clips selected, each card has **Apply to
  All Selected** beside ✕ — the effect with its settings and role onto the others (a clip
  that already has that effect in that role gets the settings instead of a second copy;
  one undo step) — and "+ Add effect" becomes "+ Add effect to N clips". `effect_fits`
  decides what goes where: sound effects on clips with sound, picture ones on clips with a
  picture, text effects on titles, and no motion or text effects on the background.
  Intros and outros use the same cards (reorder, drag to copy, Apply to All Selected; the
  Outro list keeps its "Reverse" switch). The **drop is decided by the inspector**, which
  owns the drag (`App::effect_drag`): on release outside its list it looks up the clip
  under the pointer in what the timeline and viewer drew last frame
  (`App::drop_targets`, `App::viewer_canvas` → `scene::hit_test`; `App::clip_on_screen`)
  — two earlier versions relied on the other panel noticing the release through egui's
  DnD payload and failed in manual testing. A drop that changes nothing says so.
  **Editing a selection together** (2026-09-21): with several clips selected, a
  property changed in the inspector changes on each of them (`Editor::linked`, set only
  while the inspector is drawn). `set_param`, `set_value_at` and `toggle_keyframing` fan
  out (`Editor::fan_out`): the same parameter on clips of a kind that has it (a title's
  `text.*` only on titles), an effect's parameter on the matching effect (same kind,
  same how-many-th); each clip keeps its own keyframes (a key where it's keyframed, a
  new value where it isn't), and keyframing turns on or off for all at once. Transform
  rows do the same, except the position, which moves every clip by the same amount.
  One undo step. The inspector says so in its header.
* **Crop on the canvas** (`crop.rs`): double-click any other clip in the viewer to crop
  it: the cut-away part is dimmed, rounded squares sit on the corners and pills on the
  edges; drag them to crop, or drag inside to slide the crop window over the picture
  (the clip doesn't move — while cropping, the pointer belongs to the crop). Same crop
  properties, keyframes and format scope as the Transform rows; Enter, Esc or clicking
  away finishes. A layer's bounds are its **visible** part (`Placement::corners` is the
  cropped box; `full_corners` the whole picture), so selection, handles, snapping and
  clicks all shrink with the crop.
* **Settings** (`prefs.rs`, OpenAtelier menu → Settings…, Ctrl+,; kept in
  settings.json): *Advanced transformations* (squash and crop rows — shown anyway once
  they hold a value), *Advanced color* (source color, output tone map), the **default
  curve** for new keyframes (shape + power; 1.5-power ease in-out by default — an
  `Interp::Power` set process-wide with `oa_params::set_default_interp`, which only
  affects keys being created), how long pictures and titles last, interface size, the
  Performance panel (off by default), save as you go, GPU memory. A keyframe diamond
  shows a small curve in its corner when the property has shaped (non-default,
  non-linear) easing. Playback volume moved from the inspector to the transport row.
* **On-canvas text**: double-click a title in the viewer to type into it in place, at
  about its size on screen; it renders as you type, one undo step per session; Esc,
  Ctrl+Enter or clicking away finishes.
* ⏳ Curve editor for vector properties (position/scale per axis); several curves at once.

## 12. Audio 🟡

**Recording** ✅ (`oa_audio::Recorder`, `app/record.rs`, 2026-09-21): the microphone button
in the media bin opens a recorder — pick the input, record (pause/resume), with a level
meter and the waveform drawn as it comes in. The device callback appends to a pending
buffer the UI drains each frame (no whole-take copies). Then chop the take: click the
waveform to cut, drag cuts, right-click to remove, or **split at pauses** (RMS in 10 ms
windows below a threshold for at least a given gap; `Take::pauses`); name each part,
listen to it, untick what to drop, and add the rest to the bin — 16-bit WAVs in a
`Recordings` folder beside the project (bin folder "Recordings"), imported to the bin only.

**Sound effects, the same system as picture effects** ✅ (2026-09-21): a sound effect is
an ordinary effect — an `EffectDescriptor` of `EffectKind::Sound` in the same registry,
from the same plugins (Atelier Core included), stored in the clip's one effect list with
the same **roles**: over the whole clip, or as its intro or outro, where it gets the same
clock picture effects get (`visibility`, `progress`, `seconds` — `EffectRole::clock`,
shared through `oa-doc`; the mixer glides it across each block). Every one is a **sound
shader** (`oa_audio::shader`, 2026-09-24: Atelier Core's included — there are no native
processors left but the mixer's own pitch shifter for sped-up clips): OA script run per
sample per channel — `let`/`state`, math, `delay`/`delay_out`, named delay **lines**
(`line name = seconds;`, `write`, `read`, `line_max`/`line_min`) up to 4 s, **filters**
with per-call state (`lowpass`, `highpass`, `bandpass`, `peak`, `lowshelf`, `highshelf`:
RBJ biquads, coefficients cached per call site), `left`/`right` for stereo and linked
dynamics, and a `reduction` output for the card's meter. After `spectrum N;` the rest runs
per frequency bin of an STFT (sqrt-Hann, 75% overlap, `N` frames of latency) on `mag` and
`phase`, with `state` per bin and `pass;`-separated passes that read earlier passes'
bins (`at`, `mean`) — Denoise and Pitch Shift's formants are written that way. Sandboxed
like WGSL (no loops, files or calls out; output kept finite and within ±4, a channel that
blows up starts over). The manifest adds what isn't per-sample: `latency` (a lookahead,
in seconds), `tail` (a script: how long it rings on), `meter`, `editor: equalizer`.
Plugins declare `"kind": "sound"` with a shader file (`plugins/README.md`;
`example-looks/telephone.oasound`); `Plugins::load` compiles them (errors listed on the
Plugins page with the line), `Plugins::registry` installs the enabled ones in the mixer
(`fx::install`); Atelier Core's come from `plugin::core()`. They run as machine code
(`oa_script::jit`, below). Cost, release build, stereo 48 kHz, share of one core: most
0.06–0.3%, Reverb 0.4%, Limiter 0.7%, Denoise 1.8%, Pitch Shift 2.4% — within 2–3× of
the native Rust they replaced, and Tone, Fade, Tremolo, Drive and Width, interpreted
before, several times faster (`fx::tests::cost_of_each_core_effect`, ignored). The Sound tab uses the picture effect
cards (drag to reorder or copy, Apply to All Selected) plus a Whole clip / Intro / Outro
switch with a duration.

**Sound effects** ✅ (`plugins/atelier-core/sound`, 2026-09-18; sound shaders since
2026-09-24): **Bass Boost** (RBJ low shelf),
**Pitch Shift** (two crossfaded taps sweeping a delay line; ±12 semitones, same speed),
**Echo** (feedback delay), **Reverb** (Freeverb: 8 damped combs + 4 allpasses per
channel), **Threshold** (a noise gate: envelope follower, 2 ms attack, set release),
**Denoise** (spectral subtraction over a 1024-point STFT at 75% overlap against a
per-bin noise floor tracked by minimum statistics, with gain smoothing across bins and
time). They live on clips as effect instances (the planner skips sound effects),
params keyframable on the clip's clocks, and run in the mixer on each clip's
decoded samples before its gain — so playback and export sound the same. Each clip's
decoder keeps its chain of processors; edits that keep an effect keep its state, adding
or removing one rebuilds the chain and re-seeks. Look-ahead effects report a latency
(Denoise: exactly 1024 frames, whatever the block size) and the mixer **primes** them
after every seek, so sound stays sample-aligned with the picture (tested against the dry
mix before and after a seek). ✅ **Tails** (2026-09-24): a clip keeps feeding
 its effect chain silence for as long as its effects ring (each effect's `tail` script, `fx::tail_seconds`: echo
 delay × repeats, reverb pre-delay plus decay, pitch and dynamics briefly), so echoes and
 reverbs sound on past the clip's end at the gain it ended with.

**Speed** ✅: a clip's `TimeMap::speed` now plays its sound too (it used to be silent at
speed ≠ 1). The mixer resamples (linear) per decoder, tracking the exact source
position, and — with `audio.keep_pitch`, on by default — runs the pitch shifter by
−12·log₂(speed) semitones so voices stay natural; off, the pitch follows the speed like
tape. The inspector's Sound section has a speed field (0.25–4×, presets ½/1/2×) that
keeps the same part of the file and changes the clip's length on the timeline (an
error if the next clip is in the way). Freeze frames and reverse play stay silent.

**Effect tracks** ✅ (2026-09-24): thinner tracks (`Track::effects`) that hold only
**effect containers** (`ItemKind::Adjustment` items; `kind_fits` keeps everything else
off them and containers off ordinary tracks). A container's effects apply to everything
below its track while it lasts — a master effect. Picture: the planner flattens the
layers below (and the background, at the top level) into one composite, runs the
container's effect chain on it and carries on with that as the bottom layer (GPU test:
red below turns cyan, blue above stays). Sound: each audio clip has a **layer** (the
sound of picture tracks is 0; audio tracks count up from the one drawn lowest), and an
audio effect track's containers become **buses** (`AudioBus`) that the mixer runs in
place over the sum of every lower layer, with their own tails — at the top of the
audio tracks, a master bus. Created from a track's menu ("Add picture/sound effect track
above", VFX/AFX), by dropping an effect onto an effect track, or pasted (a VFX/AFX track
is made if none fits). The inspector shows a container's mix, then its effects. Its
**opacity and blend** set how its result lies over the untouched picture, and fade-only
motion intros and outros (Fade) scale that opacity — so a Fade intro fades the effect in.

**Dynamics and tone** ✅ (2026-09-24, `oa_audio::dynamics`): **Equalizer** (low shelf,
three peaks, high shelf; RBJ biquads; its card draws the response, `eq_response_db`,
with a point per band to drag — scroll a peak to change its width, double-click to
flatten), **Compressor** (soft knee, attack/release, makeup), **Limiter** (3 ms
look-ahead, reported as latency), **De-esser** (a high-passed detector driving a dynamic
high shelf), **Pitch Shift** with **formant** control (an STFT spectral-envelope warp, so
voices can change character without changing pitch, or keep it when shifted), and
**Reverb** with a **room picker** (small room, studio, hall, cathedral, plate, cave) and
pre-delay. Processors report gain reduction, and the mixer publishes a **meter** per
effect (`fx::meter`: in/out RMS, reduction, stereo correlation; stale after 500 ms):
the Compressor, Limiter, De-esser and Stereo Width cards show it live while playing.

**Implemented** (`oa-audio`):
* **Lock-free SPSC ring** between a producer thread and the device callback. The callback
  only pops, applies gain and counts samples — no locks, no allocation, silence on
  underrun. Tested for wrap-around, odd capacities, full/empty edges and two-thread
  ordering.
* **Decoding** behind an `AudioSource` trait; today an `ffmpeg` process decodes to
  interleaved f32 at the device's exact rate and channel count (audio is small and lives
  on the CPU anyway). Seeking restarts it at the requested time. Tested for format,
  end-of-file and seek accuracy against a tone whose loudness encodes its position.
* **The clock**: position comes from the samples the device has actually consumed, not
  from wall time, so video follows audio. Seeks bump an epoch; the producer re-seeks and
  the consumer flushes what was queued, then a **50 ms prefill** runs before the clock
  restarts, so play and seek never start with a glitch.
* **Measured** in the UI: a clip with a beep and a matching flash every second plays with
  the displayed frame equal to the file's burned-in timecode, 0 underruns, ~220 ms
  buffered, video offset 0 ms.

**The timeline mixer** ✅ (`TimelineAudio`):
* Sums every audible clip at its place, silence in the gaps, out to the sequence end so
  the clock keeps time where there's no sound.
* Positions are counted in **sample frames**, so boundaries land on exact samples and
  nothing drifts.
* One decoder **per clip** (keyed by item id), so overlapping clips of one file don't fight
  over a read position; decoders for clips starting within 0.5 s open early, and ones
  whose clips are behind the playhead close.
* **Clip gain** (`audio.gain`, dB, keyframable, −60 dB = silence) evaluated every 64
  frames and ramped linearly between.
* The clip list is swapped **live** through a `MixHandle`: the UI publishes after every
  edit, decoders of clips that survive carry on, and only a real jump in source position
  costs a re-seek. Playback and export share `oa_export::audio_clips`, so they can't
  disagree about what you hear.
* The output device's own format decides the mix format (a 44.1 kHz device no longer plays
  a 48 kHz mix at the wrong pitch).
* Tested with synthetic sources whose sample values encode which file and which moment they
  came from.

**Planned** ⏳:
* Per-track gain/pan, effects, buses; real time-stretch for speed changes (speed works
  today: resampled, with a simple keep-pitch option).
* Gain/mute applied in a late stage after the ring, so slider moves are heard immediately
  instead of after the buffered audio.
* Clock abstraction for export (frame-count clock) and a user sync offset for devices that
  misreport latency (Bluetooth).
* Stateful audio effects with pre-roll, plugin latency compensation, time-stretch
  (signalsmith-stretch, MIT) for speed changes, and the offline analysis pass that feeds
  audio-reactive video parameters.
* A platform decoder (Media Foundation / CoreAudio / libav) to replace the ffmpeg process.

## 13. Threading & scheduling 🟡

Today: UI/render thread, one decode thread per video file (lookahead), the audio producer
thread and the device callback. ⏳ The rest:

UI · coordinator · single GPU submit thread · decode pool · audio mixer + RT callback ·
background pool. Newest-request-wins with generation counters; priority: current frame →
playback lookahead → scrub neighbors → background. Pipelining (plan N+1 while N renders).
Progressive refinement after ~150 ms idle.

## 13b. Captions ✅ (engine download not yet tried end to end)

**Speech to captions** (`oa-captions`, `app/captions.rs`, 2026-09-21), with
[faster-whisper](https://github.com/SYSTRAN/faster-whisper) — but **not bundled**: a
Python runtime and a model of hundreds of megabytes would weigh on every install. The
**caption engine** is downloaded on request, into one folder
(`%LOCALAPPDATA%/OpenAtelier/captions`, `OA_CAPTIONS_DIR` to move it), and removed from
the same window:

* Setup is a list of `engine::Step`s run on a worker thread, streaming `Msg`s (step,
  log line, percentage, event, failure): download **uv** (Astral's single-file Python
  installer) with `curl`, unpack it with `tar` (both ship with Windows 10+), `uv venv`
  a private **Python 3.12** (`UV_PYTHON_PREFERENCE=only-managed`, installs and cache
  confined to the folder), `uv pip install faster-whisper`, clean uv's cache, then
  download the chosen model (tiny … large-v3, turbo) with faster-whisper's
  `download_model` into `models/<name>`. `READY` is written last, so an interrupted
  setup doesn't count.
* The bridge, `oa_captions.py` (embedded in the binary, written into the folder), speaks
  **JSON lines** on stdout — `info`, `segment` (with word times), `done`, `error` — and
  leaves stderr to the libraries' progress bars, which the window shows (split on `\r`,
  percentages picked out). It runs on the **CPU with int8** weights: works everywhere
  (a GPU would need NVIDIA's CUDA libraries, which it doesn't fetch).
* **Listening**: the timeline's sound — all of it, or the selected clips' stretch — is
  mixed down through the same `TimelineAudio` playback and export use, at 16 kHz mono,
  to a temporary WAV (`oa_export::write_wav`); word times are then timeline times plus
  that stretch's start.
  With **Enhance voices** (on by default) each clip gets a cleanup chain first
  (`fx::voice_cleanup`: rumble cut, Denoise, a presence lift, compressor, limiter),
  for the transcriber only — the timeline's sound is untouched.
* **Grouping** (`oa_captions::group`, pure and tested): *at phrases* (commas and
  sentence ends), *at sentences*, *by count*, or *one word*; plus max words, max length,
  the pause that breaks a caption (shorter gaps between captions are closed, so text
  doesn't flicker off), a shortest duration, UPPERCASE and dropping commas/periods.
  Regrouping is live. Colors are stored **per word**, so a caption colored for a
  speaker keeps its color when the grouping changes; hand-typed text belongs to one
  grouping and is cleared by a change.
* **Adding**: one undo step inserts a new **"Captions"** video track above the others
  with the captions as title clips on frame boundaries. With a title picked as the
  **style**, each caption is a copy of it (font, size, colors, outline, position,
  keyframes, effects with fresh ids) with its own words and, if set, its color; the
  default look is bold, outlined, in the lower third.

**The window** (2026-09-21, second pass): two columns. Left: listen (what, language,
model), then group and the caption list (click a row's time to preview it; tick rows and
pick a color per speaker). Right: the **style** — a live preview rendered by the real
renderer (`App::render_scaled`) over the footage at that caption's time, played through
when something follows the spoken word; "Copy a title's look" (any title on the
timeline, effects and keyframes included); font, bold/italic, size, color, outline,
height; a rounded **Background** (whole text / each line / each word); **Highlight the
spoken word** (its color, and a box behind it). The style is an `Item` template kept in
`CaptionPrefs::style`. Progress is an overall bar (steps, counting a step's own
percentage) over a per-step bar that sweeps when a step can't say how far along it is;
curl's `--progress-bar` and Hugging Face's tqdm lines feed it. The timeline button has
Material's "subtitles" icon, drawn by hand when the installed icon subset predates it
(`icons::in_font`).

**Editing the list** (2026-09-21, third pass): split a caption at the text cursor (the
✂ button, or Enter while typing) or join it with the next (the link button). These are
kept per word (`oa_captions::Manual`: forced breaks and joins, `group_with`), so they
hold when the grouping rules change; typed text is keyed by a caption's first word. The
preview renders the format being viewed, and **Play** under it plays the timeline with
its sound from the chosen caption while the preview follows the playhead caption by
caption (stopping after the last). An earlier version laid a click area over each row,
which took clicks from its text box and tick box.

**More editing, and undo** (2026-09-21, fourth pass): Enter or the scissors split at the caret (typed text
is cut there too, and the caret moves to the new caption); Backspace at a caption's
start joins it to the one above, Delete at its end joins the next one to it (typed text
is kept, the caret stays at the seam); Up/Down go from caption to caption. A third
button on each row **removes** that line (2026-09-23): its words go into
`CaptionsUi::removed` and are filtered out of every later grouping, so the line stays
out when the rest is split, joined or regrouped; the heading counts what's gone and
**Restore** brings it all back. **Undo**: while the window is open, Ctrl+Z /
Ctrl+Shift+Z / Ctrl+Y undo and redo the window's own changes — text, splits, joins,
removals, colors, grouping and style — in one history
(`Snapshot`s taken as things change; a run of typing into one caption is one step, a
slider one step when let go). The timeline's undo is back when the window closes.

**Arrow keys and undoing the add** (2026-09-24): with the window open, Left and Right
go to the caption before or after (not typing: shown in the preview, the sound moved to
it, the list scrolled to it; typing: Left at a line's very start and Right at its very
end carry on into the next line) — the timeline's frame stepping leaves them alone
while the window is open. When "Add captions" closes the window, the window is kept
(`App::captions_added`) rather than dropped: undoing that step brings it back exactly as
it was (transcript, edits, timings, style, history), and redoing it puts it away again.
A "… when spoken" row's own right-click menu has **Remove highlight when spoken** (it
was only on the property it belongs to), for every selected clip that has it.

**Timing and scrubbing** (2026-09-24): right-click a line's time to set it by hand —
start and end dragged or nudged a frame at a time, "By the words" to go back, "Close
the gap" to meet the line above. Moving an end past the next line takes that line's
start with it (and a start past the line above, its end), so lines stay joined at the
seam that splits and joins leave crooked. Hand-set times (`CaptionsUi::times`, by the
line's first word, gold in the list) are part of the undo history and go with the
grouping. The style column is wider (420 px) and the preview with it; **dragging across
the preview scrubs** the whole stretch listened to, with the sound's position on a bar
underneath that shows every line as a block (drag that too).

**Word times on clips**: caption clips carry `Item::word_times` (clip-local), from the
transcript. `Item::spoken_word(t, words)` is the last word started by `t` (it stays lit
through the pause after it); titles without word times spread their words evenly over the
clip.

⏳ Speaker detection (diarization) — colors are per caption, by hand, for now; GPU
transcription; captions for a compound clip's own timeline use the open one.

## 13c. Tracks ✅ (`oa-track`, `app/tracks.rs`, 2026-09-24; the bridge is tested with a stand-in model, the real online model not yet run)

A position can follow something in the picture. Right-click a position — a clip's
**position** (the anchor lands at `pivot + position × canvas`), or a point given as a
fraction of the clip (anchor, reframe focus, an effect's point; not a Surface's offsets)
— → Animate → **Edit track…**. The viewer is taken over — the track drawn over the picture
(its line, its points, where it is now) — and a panel floats over it: which track (a new
one, or any the project has: pick one to follow it; rename it; stop following; delete it,
which lets anything following it go where it is), and:

* **Follow** — click the thing to follow; **CoTracker** (Meta's AI point tracker, v3
  **online**: overlapping 16-frame windows with a support grid of helper points, so memory
  stays flat for long stretches — up to 900 frames — and it reports after every window)
  follows that point for as long **before and after** it as asked, looking at
  every frame, every 2nd or every 4th (**detail**). The bridge runs the forward part and,
  only when there is one, the backward part, each over just its own frames (half the work
  of tracking both ways over everything), on an **NVIDIA GPU** in half precision when the
  CUDA build of PyTorch is installed (Settings offers it when `nvidia-smi` finds a card;
  ~3.2 GB), on Apple's GPU, or on every CPU core. The panel shows where it runs and real progress —
  ffmpeg's frame count while reading the footage, then a percentage per window — while the
  path grows over the picture (`at` lines) and the playhead follows the newest point. The
  result is kept on the run when it arrives (it can come a UI frame before the job ends).
  The footage is the clip's own for a point on a video clip, else the top video under the
  point; ffmpeg hands over the stretch where both clips play as ≤ 900 evenly spaced RGB
  frames at ≤ 512 px, and the result comes back in footage px per frame → the footage clip's placement at each moment → the
  track, thinned to within 0.75 canvas px. The engine is a separate download in its own
  folder (`%LOCALAPPDATA%/OpenAtelier/tracker`, `OA_TRACKER_DIR`), set up through the
  caption engine's step runner (uv, Python, PyTorch for the CPU, CoTracker via torch.hub);
  CoTracker is **CC BY-NC 4.0** — non-commercial — which the panel says before setup.
  Settings → AI tracker shows where it is and what it runs on, switches CPU ↔ GPU, and
  removes it (with a confirmation).
* **By hand** — click where it is: a point at the playhead, which then steps on N frames;
  click again for the next, and so on.
* **Record** — the playhead runs (no sound) at a chosen speed from the playhead to the clip
  end once armed and started with a click in the picture, the pointer's path taken down
  until a click or Space stops it (or the clip ends); it's then smoothed
  (`oa_track::smooth`, Gaussian in time, up to 0.25 s) and thinned (`simplify`: RDP with
  the error measured at each sample's own time, so pauses stay).

**Tracks are the project's** (`Project::tracks`: `oa_params::PointTrack`, points in
position units — fractions of the canvas from its center — at timeline times;
`Op::SetPointTrack`, undoable). A new run replaces the points inside its stretch. A clip's
**position** follows its track live through `Modulator::Track`, which carries the track
(evaluation stays self-contained) and adds it at timeline time = clip time + its clock
(moved along by splits like the other clocks): the property's own value becomes an
**offset** from the track — labeled so in the inspector, and set when attaching so the
anchor sits right on the tracked point. Editing a track updates every property following
it in the same undo step; **Stop following** folds the track's position at the playhead
into the offset, so nothing jumps. A point on a clip (anchor, focus, an effect's point)
is keyed to the track instead (where a canvas point lands on a clip depends on the clip),
keeping motion (wiggle, wave, sound) layered on top.

Two menu entries (Animate): **Edit track…** (following) and **Edit stabilization…**
(video clips), the same editor and tools with only their own settings.

**On the diamond**: a property using a track shows a small crosshair in its keyframe
diamond's top corner (gold following, green stabilized), and one connected to the sound
shows little sound bars in the other, beside the curve and wave marks; the tooltip names
the track. The AI tool tab is **Auto** (its old name, Follow, now means the mode).

**Follow or stabilize** ✅ (2026-09-24, `oa_params::TrackUse`): a position uses its track
one of two ways. *Follow* adds the track (a title riding a car). *Stabilize* adds a
correction that moves the clip against it, so what was tracked holds still — steadying
shaky footage by tracking something in the clip itself. How firmly is the user's:
**Locked** holds the point still — where it is at the playhead, at the center of the
frame, or at any point picked on the frame (no reference point needed: that's the
default); **Smooth** only
takes out movement quicker than a smoothing time (Gaussian over the track's points,
0.05–5 s), so pans and walks stay; **Strength** (0–100%) takes out that share. The
correction is worked out once, when the settings or the track change (`Stabilize::new`,
kept on the modulator), so playing back costs one lookup. Switching between following
and stabilizing shifts the offset so the clip doesn't jump at the playhead
(`with_track_use`); between two stabilizations the offset (the clip's resting place)
stays and only the correction changes (`replace_track_use`), so a picked point is really
reached, and "Stop following"
folds in whatever the track adds there (`track_contribution`). The overlay also draws
where the tracked point ends up (green); **Zoom in to hide the edges** scales the clip by
1 + 2 × the largest correction inside it (idempotent: never below what's already there).
Stabilizing tracks the clip itself: the AI's start point must be on the clip (it says so
if not), and its own footage is followed rather than whatever is under it. Tracking a
clip that's already stabilized by that track (by hand, recorded, or by the AI) takes the stabilization's move back out, so the track stays in its
own terms. Tested: locked holds exactly; smooth leaves < 0.2% of a canvas of an 8 Hz shake
while keeping a slow pan; half strength halves; switching keeps the value.
⏳ Several points at once (for scale/rotation), GPU tracking.

## 14. Export ✅ (`oa-export`)

Export runs **the same planner, optimizer and executor as the preview**, at full quality
with proxies off — that is what makes what you saw what you get.

* It **waits** for what the preview may skip (2026-09-21): the app's renderer doesn't
  wait for shaders, glyphs or stills being prepared in the background (it keeps the last
  frame up instead), so `Exporter::step` turns `RenderOptions::wait` on for its frames
  and restores it. Exports used to fail with "rendering failed: still preparing" when a
  title or a new effect came up mid-export.

* Frames are converted to **NV12 on the GPU** (BT.709, limited range), so 1.5 bytes per
  pixel cross to the CPU instead of the 8 a float RGBA readback would cost.
* [`VideoSink`] is the encoder boundary, with two implementations:
  * **Media Foundation sink writer** (Windows, the default for H.264/HEVC): hardware
    transforms enabled, so it runs the GPU vendor's encoder that ships with the driver
    (NVIDIA's H.264 MFT, for example — which works on drivers where ffmpeg's NVENC
    doesn't). It muxes the sound itself (PCM in, AAC out), interleaved ~100 ms ahead of
    the picture, and writes BT.709 limited-range tags. Quality maps x264's CRF scale to a
    bitrate (crf 18 ≈ 0.12 bits/pixel, halving every 6 steps).
  * An **`ffmpeg` process** (libx264/libx265/ProRes), for ProRes, other platforms, and as
    the fallback when the platform encoder refuses a codec or size.
  Both take CPU NV12 today; giving the sink writer GPU surfaces through the DXGI device
  manager is the remaining step to frames never leaving the GPU.
* **Sound** is rendered offline through the same timeline audio used for playback, written
  to a temporary WAV and muxed by the encoder. Silence pads any stretch without audio, so
  the two streams stay the same length.
* Color metadata (BT.709, limited range) is written into the file rather than left for
  players to guess.
* [`Exporter`] runs in steps, so the UI renders a few frames per repaint and shows
  progress instead of freezing; the CLI loops the same API.
* **Verified**: exporting a clip whose frames carry their own number, decoding the result
  back through our own pipeline, and checking each sampled frame is the one the timeline
  showed (`crates/export/tests/roundtrip.rs`).
* **Verified**: both encoders pass the round trip (frames, BT.709 tags, 48 kHz AAC, length).
* **Pipelined**: frame N's NV12 readback is waited for only after frame N+1 has been
  rendered and submitted (`readback::start_read` → `PendingRead::wait` on that submission
  alone), so the GPU draws the next picture while this one goes to the encoder.
* **Measured** (RTX 3060, release, 8 s 1080p60 clip with sound): Media Foundation / NVIDIA
  **305 fps** (204 before pipelining); ffmpeg libx264 153 fps.
* **How long would it take?** (`oa stress`, `oa_edit::stress`, 2026-09-24):
  `oa stress <minutes> --intensity 0..1 --media <files…>` builds a stand-in project of
  that length from the files given (or test-pattern stand-ins): cuts end to end on V1
  with transitions between them, pictures and titles over it on up to three more
  tracks, sound on up to three tracks with gain moves and sound effects, and effects
  (from the registry, plugins' included), intros and outros, keyframed moves, wiggles
  and waves throughout — none of it at intensity 0 (plain cuts), all of it at 1. One
  seed builds the same project every time. It exports a slice for real — 20 s, or a
  tenth of a long project up to a minute, from a quarter of the way in — and projects
  the whole: the start (file opened, shaders built, decoders opened) once, the sound
  mixdown in proportion to the length, and every other frame at the rate measured
  from the warm-up to the file being **closed** (a hardware encoder queues frames and
  does much of its work at the end; timing only to the last frame handed over read
  twice as fast as it is). Checked against `--full`: 1m 12s predicted, 1m 12s taken
  (6 min, intensity 0.6, a video, a picture and music); within ~10% without media.
  `--out` saves the project to open in the editor, `--export` keeps the file.
* **Faster, lighter on the CPU** (2026-09-22):
  * **Its own thread in the app** (`app/src/export_worker.rs`): an export used to be
    rendered inside the UI loop, eight frames per UI frame, so it waited on vsync and on
    the whole editor redrawing, and shared the viewer's renderer and decoders (thrashing
    their caches and seeks). Now it runs on a thread with its own renderer (fused
    pipelines built before use) and its own hardware decoders, from a snapshot of the
    project taken at the start (edits made meanwhile don't leak into the file), sound
    mixdown included. The UI polls its progress, picks up a display-ready picture four
    times a second and redraws at 10 Hz while it runs; dropping the handle cancels it.
  * **Three frames in flight** (`IN_FLIGHT`), not one, so the GPU renders ahead while
    older frames cross to the CPU and into the encoder.
  * **One copy per frame**: staging buffers are reused (`start_read_reusing`), and
    `PendingRead::wait_packed` writes both NV12 planes, unpadded, one after the other into
    one reused buffer — NV12's own layout, which the Media Foundation sink hands over
    as is (it used to unpad, join and copy each frame).
  * **Where the time goes**: `ExportSummary::timings` (plan · render · readback · encode,
    on the export's thread), printed by `oa export` and in the app's messages.
  * **Debug builds** (`cargo run`) build every dependency at `opt-level = 3` and our
    crates at 1 (`[profile.dev]` in the workspace `Cargo.toml`): unoptimized wgpu made
    each GPU pass's bookkeeping many times slower, which multi-pass effects (Glow) felt
    most. App-only rebuilds stay ~10 s.
  * Measured (RTX 3060, 8 s 1080p30 test pattern with keyframed blur, a scaled clip and
    Glow on two layers, Media Foundation): release plain 285 → **317 fps**, with Glow
    249 → **286 fps**; debug build plain 174 → **307 fps**, with Glow 130 → **244 fps**.
    Output bit-identical to before.
* ✅ **Transparency** (2026-09-21): formats with alpha — **MOV ProRes 4444**, **WebM VP9**
  (`yuva420p`) and **GIF** (reserved transparent palette entry) — are fed straight-alpha
  sRGB RGBA (`GpuServices::to_rgba8`) instead of NV12; a transparent background stays
  see-through (tested: corners alpha 0, clip 255, all three). The Export window warns
  when the background is transparent but the chosen type can't keep it.
* ✅ **Export window** (`app/export_dialog.rs`): file type as cards (MP4 H.264 / HEVC, MOV
  ProRes, **GIF**), format or every format, resolution (full, 480p–4K), **part of the
  timeline** (all, the selected clips, or from–to with "here" buttons), quality or GIF
  frame rate, encoder, sound; a preview and a summary (sizes, length, estimated file
  size) beside them. Exports run from a queue; choices are remembered.
* ✅ **Exporting window**: the frame being written, live (`Exporter::last_frame`, drawn
  through the same display pass as the viewer), frame N of M and its timeline time,
  progress, speed and time left, hide/cancel; the top bar's progress bar reopens it.
* ✅ **Ranges** (`ExportOptions::range`): rendering starts on the range's first frame and
  the sound is rendered for the same span. **GIF** goes through ffmpeg: its own frame
  rate, one palette for the whole clip (`palettegen`/`paletteuse`, ordered dither),
  looping, no sound. Tested end to end (`gif_of_a_range`).
* ⏳ Gaps: frames still cross to the CPU (NV12 readback), no image-sequence or audio-only
  output, no resuming a canceled export.

## 16. Transitions ✅

**Model**: a transition lives on one end of a clip (`Item::transition_in/out`: type,
duration, keyframable params), so it moves, splits, undoes and deletes with its clip.
A clip's **head** transition covers the cut from the clip ending exactly where it starts,
centered on the cut, both clips shown past their edit points; with nothing directly before
it fades in from transparent. A **tail** transition fades out to transparent, only when
nothing follows directly (that cut belongs to the next clip's head). Durations clamp to
the clips (`oa_plan::transitions`, shared by rendering, the timeline UI and audio).

**Rendering**: each side is its clip composited alone onto a transparent canvas (its
transform and effects apply as usual), then a `Transition` node mixes the two with the
type's shader at `progress`. Transition shaders are ordinary registry entries
(`EffectKind::Transition`, `fn(pos, progress, base)` reading `sample_a`/`sample_b`), so
plugins can add their own. Built in: cross dissolve, dip to color, wipe (angle,
softness), push (direction). An unknown type cuts at the midpoint and is reported.

**Sound** crossfades over the same windows: both clips extend into their handles and
ramp against each other; fades ramp to or from silence.

**Tested**: each shader on the GPU (dissolve averages, dip reaches the color, wipe
splits, push moves), a planned red→blue dissolve renders half of each at the cut and a
fade-in half-way over black, timing rules (centering, fades, a tail yielding to the next
head, clamping), commands (clamping, settings kept on duration changes, a split moving the
tail transition to the back half), and the mixer's crossfade ramps.

## 15. Editing commands & direct manipulation ✅ (`oa-edit`)

Commands are **pure functions from a project snapshot to ops**: one command is one undo
step, commands compose, and every one is tested without a UI.

**Timeline** (`oa_edit::timeline`): trim (head or tail), split, split across tracks, ripple
delete, close gap, move (to the nearest free spot, optionally onto another track), slip,
snapping to edit points, frame snapping. Interactive commands **clamp** instead of failing:
a trim stops at the media's end, a neighbor or one frame. **Split changes no frame**:
the back half continues the footage, clip-anchored keyframes shift, clip-anchored
procedural motion carries a clock offset, variant overrides are copied, effects get fresh
ids — tested by comparing render-graph cache keys for every frame of both formats before
and after.

**Viewer** (`oa_edit::transform` over `oa_plan::scene`): `scene` is the planner's own
placement math (reframe, anchor, scale × squash, rotation, position) exposed as
`Placement`s, so hit testing and handles use exactly what the renderer draws. A `Gesture`
keeps its start state and recomputes the edit from it on every pointer move, so nothing
accumulates rounding. Tested properties: a dragged corner stays under the pointer (with
rotation and offset), scaling pivots on the anchor, uniform scaling keeps the aspect ratio,
moving the anchor leaves every corner where it was, rotation steps, center/edge snapping.

**Keyframe-aware writes** (`ParamSource::set_at`): a static value is replaced; a keyframed
one gets a key at the playhead on its own clock (keeping or borrowing easing); a modulated
one has its base adjusted so the displayed value is what was asked for, with the wiggle
still on top. Edits in a secondary format go to its override (seeded from the base, so the
animation carries over) and to wherever the displayed value already comes from.

## 17. Text ✅ (`oa-text` + the text pass in `oa-gpu`)

A text clip (`ItemKind::Text`) is a layer like any other — transform, motion, effects,
intros/outros, hit testing — whose picture is drawn on the GPU from its params
(`schema::text()`: content, font, bold, italic, size in canvas px, color (a directional
gradient), alignment, letter spacing and line height in ems, outline width and
color (a gradient too). Numbers, colors and gradients are keyframable. Projects from before gradients keep their top-to-bottom two-color setting.

**Fonts** (`oa_text::fonts`): an index of every installed family, built once by
memory-mapping font files and reading only their name tables (~540 files here); faces
load on first use and live for the process. Missing characters come from a fallback
chain (Segoe UI, Segoe UI Symbol/Emoji, CJK and Indic UI fonts…), and a bundled font
(Ubuntu Light, from `epaint_default_fonts`) guarantees text renders anywhere.

**Layout** (`oa_text::layout`): each line is split into runs by face and shaped with
`harfrust` (a Rust port of HarfBuzz: kerning, ligatures, marks, complex scripts); letter
spacing goes between clusters. The box holds every line and all ink (italic overhang
included) and is the layer's native size, placed 1:1 and centered (text ignores the fit
mode — its size is its font size). Drawn glyphs are numbered by index, line and word
for per-letter effects. Layouts are pure functions of `TextSpec` and cached, so the
planner (box size, hit testing) and the renderer share one.

**Glyphs** (`oa_text::sdf`): each glyph is rasterized once per size bucket (32/64/128 px
per em — the smallest at least as big as it's drawn) into an 8-bit **signed distance
field** (exact Euclidean distance transform, sub-pixel edges from coverage), spread
0.2 em each side of the outline. They're packed into a 4096² R8 atlas on first use.

**The text pass** (`oa_gpu::text`, `NodeOp::Text`): one instanced quad per glyph; the
vertex shader applies the per-letter effect chain to each glyph (offset, scale,
rotation, color), the fragment shader turns distance into coverage with screen-space
anti-aliasing (correct at any scale, including animated per-letter scale), composites
fill over outline (each a directional gradient over the box), then runs the per-pixel effect chain.
Output is a premultiplied layer image in the working format. Uniform block of 256
floats (style at 16, derived values at 30, effect params from 40); effect params include
their clock, so per-letter intros stagger off `visibility()`. The planner grows the
node's bounds by the outline and by how far letter effects may move glyphs (`Glyph {
expand }` names a param in ems), so letters flying in aren't clipped. The cache key
covers the spec, raster scale, style and chain.

⏳ Not yet: on-canvas text editing, paragraph wrapping to a box width, per-range styling
(rich text), color emoji (outlines only), text effects as plugins (built-in WGSL only),
atlas eviction beyond "start over when full".

---

* **Bounded text effects** ✅ (2026-09-24): right-click a per-letter or per-pixel text
  effect's name → **Bounded**: it applies only to a range of the letters — a start and
  an end in percent of the text or in letter numbers (from 1, both ends included; spaces
  aren't letters), with a blend over which it fades in and out at both ends. Stored with
  the effect's own params (`schema::bounds()`, `bounded.*`), so they keyframe: a
  highlight can sweep across a title. The planner hands the text node the range
  (`TextEffect::bounds`: percent?, start, end, blend, in letter edges); the text pass puts
  it after the effect's params and their spoken copy and blends the effect's result with
  the letter as it was by `oa_bounded(index, slot)` — per letter, the glyph's offset,
  scale, rotation and color (`oa_bounded_glyph`); per pixel, the color. Switching units in
  the card keeps the range where it was. **Any other picture effect** on a title (color,
  blur, glow, warp… — not motion, text backgrounds or transitions) bounds the same way,
  after the text pass: the planner adds a `NodeOp::TextMask` over the effect's area —
  each letter's cell (halfway to its neighbors on its line, lines halfway to each other,
  the ends reaching out) filled with its weight, blending across the cell when there's a
  blend — and puts the result together in three host passes with a second input:
  effect × mask (`MASK_KEEP`), picture before it × (1 − mask) (`MASK_DROP`), the two
  added (`ADD`). A blur or glow so spreads within its letters' share.
* **Highlight when spoken** ✅ (2026-09-21): right-click a title's color, outline width
  or outline color, or any text effect's parameter → "Highlight when spoken": a second
  value, stored beside the parameter as `<id>#spoken` (`schema::spoken`) and shown just
  under it, keyframable like any other. The planner evaluates both (`plan::spoken`) and
  hands the text node a spoken style, a spoken copy of each text effect's params
  (`oa_graph::TextEffect::spoken`) and the word being spoken — only when some parameter
  has a spoken value, so other titles' cache keys don't change per word. In the text
  pass (512-float uniform block; spoken style at 96, spoken word at 91, each effect's
  params followed by their spoken copy from 164) every letter picks its block by
  comparing its word with the spoken one: per-letter effects get
  `select(base, spoken_base, g.word == spoken_word())`, the fragment stage picks fill,
  outline and pixel-effect params the same way (the word rides along in `VOut.info.z`).
* **Boxes behind text** ✅ (`EffectKind::TextBox`, built-in **Background**,
  `oa.text.background`): the text pass draws instanced quads for the whole text, each
  line and each word (ink across, a fixed band around the baseline down, so boxes on a
  line match), grown 1.5 em for padding, before the glyphs; a box effect's shader
  (`fn <entry>(b: TextBox, base) -> vec4f`) colors their pixels — the Background draws a
  padded rounded rectangle for the shape it's set to. Word boxes take the spoken params
  too: a transparent box whose spoken color is solid is a karaoke highlight. Tested on
  the GPU (rounded corners; the highlight following word times; the box behind only the
  spoken word).

## 18. Releases and updates ✅ (2026-09-24; the first release not yet made)

**Releases** (`.github/workflows/release.yml`): pushing a tag `v<version>` (it must match
the workspace version; `0.1.0-beta.1` now) builds `oa-app` and `oa` in release mode
(thin LTO, no debug info) on Windows and Ubuntu 22.04 (for older glibc), packages each
as `OpenAtelier-<version>-<platform>.zip|.tar.gz` — one folder: `OpenAtelier.exe` /
`openatelier`, `oa`, LICENSE, README, `plugins/` — writes `SHA256SUMS.txt`, and publishes
a GitHub Release, a pre-release when the version has a pre-release part.

**Windows installer and a self-contained package** (2026-09-24): the release also builds
`OpenAtelier-<version>-windows-x64-setup.exe` with Inno Setup (`installer/openatelier.iss`)
from the packaged folder: per-user into `%LOCALAPPDATA%\Programs\OpenAtelier` (no admin
prompt, and writable, so the in-app updater still works; installing for everyone is
offered), Start menu entry, optional desktop shortcut, license page, uninstaller (which
also clears what the updater set aside), a fixed AppId so new versions upgrade in place.
Windows packages carry **ffmpeg and ffprobe** next to the program — Windows (and Rust's
`Command`) look in the program's folder before `PATH` — from a pinned gyan.dev build
checked against its SHA-256 and for the encoders export needs, with its GPL license and a
notice. The programs carry the **icon** and version details (`crates/app/build.rs`,
`winresource`, `assets/logo.ico` written from the same drawing as the window icon by an
ignored test). At start the app checks it can run ffmpeg and ffprobe (`app/deps.rs`) and
says how to get them if not, instead of imports failing with a puzzling error.

**Updates** (`app/update.rs`): the app reads the repository's releases (GitHub API via
`curl`, once a day at start, or Settings → Updates → Check now) and compares by semantic
versioning (`0.1.0-beta.2` > `beta.1`; a release above its pre-releases). The **Beta**
channel (the default for beta builds) also offers pre-releases; **Stable** only full
releases; only releases with a package for this platform count. A bar at the top offers
**Update**, **What's new** (the release page) and **Later**. Update downloads the package
and the checksums (the engines' step runner, with progress), checks the SHA-256, unpacks
with `tar` (zips too), and puts the files next to the running program — a file in the way
is renamed aside (`*.old`; Windows lets a running program be renamed, not overwritten),
cleared at the next start. **Restart now** closes the normal way (unsaved work is asked
about) and `on_exit` starts the new version. Builds run from a `target` folder, or
installs in a folder the app can't write, get the download page instead. `OA_UPDATE_REPO`
points it at another repository. Tested: version ordering, channels, GitHub's answers
(and its error messages), checksums, an end-to-end install from a real package (and a
damaged one refused), files set aside and cleared.

## Roadmap

1. ✅ **Core model** — time, params, document, variants, graph, planner, CLI.
2. ✅ **GPU executor** — Source/Solid/Effect/Fused/Composite on wgpu, pool, cache, async
   fusion, reference-vs-optimized tests, PNG render.
3. ✅ **Media, GPU-first** — probe/index, fingerprints, Media Foundation hardware decode
   into wgpu textures, frame-accuracy tests on H.264/HEVC/VFR/NTSC clips.
4. ✅ **Playback & editing** — preview window, media pool, import/conform, projects with
   relinking, multi-clip timeline, audio output with the audio clock driving video,
   export to file, decode lookahead threads, the audio mixer, trimming and razor.
5. ✅ **Editing UI** — timeline editing with snapping and zoom, tracks on/off, add,
   remove and reorder, direct manipulation in the viewer, inspector with keyframes,
   transitions, curve editor, editing inside compound clips, on-canvas text, the format
   picker.
6. 🟡 ✅ Text, shader plugins (Atelier Core), color management; ⏳ masks, the WASM plugin
   host, 10-bit decode.
7. ⏳ **Background backend track** (TODO.md): threading, parameter drivers, GPU device
   rebuild, media breadth, audio buses — a step at a time alongside front-end work.

# Plugins

A plugin is a folder with a `plugin.json` and the files it names: GPU shaders (WGSL)
for pictures, sound shaders for sound, and small scripts for what isn't a shader (how
an intro moves a layer, where an effect may draw). Effects from every enabled plugin
show up in the editor's pickers exactly like the built-in ones — because the built-ins
*are* a plugin. **Atelier Core** is the folder [`atelier-core`](atelier-core): every
effect OpenAtelier ships with, picture and sound, written the way this page describes.
The program carries a copy of it inside and loads it through the same loader as yours;
all it has that yours doesn't is its `oa.*` ids. Read it to see how anything is done.

Folders here are loaded when you run the app from a checkout. Yours belong in the
plugins folder the app opens from **Home → Plugins → Open plugins folder**
(`%LOCALAPPDATA%/OpenAtelier/plugins`). [`example-looks`](example-looks) is a small
working example to copy.

```jsonc
{
  "id": "com.example.looks",          // your own prefix; "oa.*" is Atelier Core's
  "name": "Example Looks",
  "version": "1.0.0",
  "author": "you",
  "description": "Shown on the Plugins page.",
  "api_version": 1,
  "effects": [{
    "id": "com.example.vignette",
    "name": "Vignette",
    "kind": "point",
    "category": "Looks",              // the picker's group; the plugin's name when missing
    "description": "Darkens the corners.",
    "preview": {"amount": 1},         // settings its picker thumbnail uses
    "params": [
      {"id": "amount", "type": "float", "default": 0.6, "min": 0, "max": 1},
      {"id": "size",   "type": "float", "default": 0.7, "min": 0.1, "max": 1.5, "unit": "layer_pixels"},
      {"id": "edges",  "type": "enum",  "options": ["soft", "hard"], "static_only": true}
    ],
    "shader": {"entry": "example_vignette", "file": "vignette.wgsl"}
  }]
}
```

Anything wrong — a bad kind, a missing file, a shader without its entry point, a script
that doesn't compile, an `oa.*` id — is listed on the Plugins page and that one effect is
skipped. Turning a plugin off removes its effects from the menus, from rendering and
from the mixer; projects keep the settings of clips that used them and say which plugin
is missing.

## Effects

| Field | |
|---|---|
| `kind` | What it works on — see [Kinds](#kinds). |
| `usage` | `passive` (default: the Effects list) · `in_out` (a clip's intro/outro) · `cut` (a transition; the default for `transition`). |
| `space` | `linear` (default: premultiplied scene-linear light) · `display` (sRGB-like 0–1 values, for classic "looks"). |
| `expand` | `spatial`/`glyph`: the parameter (in layer px, or ems for glyphs) by which it may draw outside its input. |
| `time_varying` | It animates on its own, reading `progress()` / `clip_seconds()` even as a passive effect. |
| `preserves_opacity` | An opaque input stays opaque (lets the renderer skip what's hidden below). Default: true for `point`. |
| `fusible` | May be run in one pass with its neighbors. Default: true for `point` and `uv_warp`. |
| `category`, `description`, `preview` | The picker's group, a tooltip, and settings for its thumbnail (by parameter id) when its defaults show little. |
| `params` | Its settings — see [Parameters](#parameters). |
| `shader` | `{"entry": "fn_name", "file": "x.wgsl"}` (or `"source": "…"` inline); `"passes": 2` runs it twice (`pass_index()` tells which). |
| `bounds` | A [bounds script](#bounds): where it may draw, when that isn't just `expand`. |
| `pass_count`, `pass_divisor` | [Pass scripts](#passes): how many passes, and how coarse each one runs. |
| `second_input` | `media` (default: its `media` parameter's picture) · `original` (the picture as it came in). |
| `motion` | `motion` kind: a [motion script](#motion). |
| `editor` | A host editor it uses beyond sliders: `surface` · `equalizer` — see [Editors](#editors). |
| `meter`, `latency`, `tail` | Sound: see [Sound effects](#sound-effects). |

Scripts are written in place (`"bounds": "left = x0 - 10;"`) or in a file beside the
manifest (`"bounds": {"file": "shape.bounds"}`).

### Parameters

`{"id": "radius", "type": "float", "default": 8, "min": 0, "max": 100, "unit": "layer_pixels"}`

* **Types**: `float`, `int`, `bool`, `vec2`, `vec3`, `color` (straight RGBA), `enum`
  (with `"options"`; the default is the first unless given), `gradient` (a color, or a
  whole gradient as the editor saves one), `media` (a picture from the project — the
  effect's second input), `text`.
* Every one is keyframable (and can wiggle) unless `"static_only": true`.
* **Units**: `none`, `layer_pixels` (scaled with the render resolution, so a radius
  looks the same in a ¼ preview), `canvas_fraction`, `source_fraction`, `degrees`,
  `direction` (edited with a dial: 0 = right, 90 = down), `seconds`, `decibels`.
* A **point on the clip** — a `vec2` in `source_fraction` (0–1 across the clip; `[0.5,
  0.5]` its middle), like a Swirl's `center` — gets a crosshair in the viewer to drag,
  and can follow a track (right-click its values → Animate → Edit track…).

### Kinds

WGSL effects read their parameters with `u(base + i)`, in the order declared (a `vec2`
takes 2 slots, a `color` 4, a `gradient` 32; `oa_gradient(base, pos, lo, size)` gives a
gradient's color), and their clock with `visibility()` (0 → 1 over an intro, 1 → 0 over
an outro, 1 otherwise), `progress()` and `clip_seconds()`.

* **`point`**: `fn name(c: vec4f, base: u32) -> vec4f` — one pixel's straight (not
  premultiplied) color; `layer_pos()`, `in_origin()`, `in_size()` say where it is.
* **`uv_warp`**, **`spatial`**: `fn name(pos: vec2f, base: u32) -> vec4f` — `pos` in
  layer px; read the picture with `sample_input(pos)` / `sample_input_clamped(pos)`
  (premultiplied) and the second input with `sample_media(pos)`. `pass_index()`,
  `out_origin()`, `out_size()` for multi-pass effects.
* **`transition`**: `fn name(pos: vec2f, progress: f32, base: u32) -> vec4f` — mixes
  `sample_a(pos)` (outgoing) and `sample_b(pos)` (incoming), in canvas px.
* **`glyph`** (text, per letter): `fn name(g: Glyph, base: u32) -> Glyph` — change
  `g.offset` (px), `g.scale`, `g.rotation` (degrees), `g.color` (a multiplier; alpha is
  the letter's opacity); read `g.index`/`g.count`, `g.line`, `g.word`, `g.center`,
  `g.em`. `letter_progress(g, stagger)` staggers `visibility()` across the letters.
* **`glyph_pixel`** (text, per pixel): `fn name(c: vec4f, p: TextPixel, base: u32) ->
  vec4f` — `p.pos`, `p.box_size`, `p.dist` (px to the outline, positive inside), `p.index`.
* **`text_box`** (text, behind the letters): `fn name(b: TextBox, base: u32) -> vec4f`
  — run for a box around the whole text, each line and each word (`b.kind` 0, 1, 2);
  `b.pos`, `b.center`, `b.size` in px, `b.em` the text size.
* **`motion`**: no shader — a [motion script](#motion) moves the whole layer, so it can
  travel anywhere on the canvas.
* **`sound`**: a [sound shader](#sound-effects).

## Scripts

Everything that isn't a GPU shader is written in **OA script**, a small language of
statements ending in `;`:

```text
// Comments start with // or #.
let k = 1 - exp(-TAU * cutoff / sample_rate);   // a value
state low = 0;                                   // a value kept from run to run, starting at 0
low += (in - low) * k;                           // = += -= *= /=
out = mix(in, low, amount);                      // one of the script's outputs
```

* **Math**: `+ - * / %` (`%` is never negative), `^` (power), comparisons and `&& || !`
  (true is 1, false 0); `sin cos tan asin acos atan atan2 abs sign floor ceil round
  fract sqrt exp log log2 pow min max clamp mix smoothstep step tanh`, `db(x)` (decibels
  → gain), `to_db(g)`, `select(c, a, b)` (`a` when `c`, else `b`), `choose(i, a, b, …)`
  (the `i`th value after `i` — handy with a choice parameter). `PI`, `TAU`.
* **Parameters** by id: numbers, switches (0/1), choices (the option's index); a point's
  parts as `id_x`, `id_y` (`id_z`), a color's as `id_r`, `id_g`, `id_b`, `id_a`. A
  parameter whose id is one of the script's own names isn't readable.
* No loops, no branches, nothing to reach but its own values: a script always finishes,
  quickly. `let`s that only read parameters are worked out once per block, not per run.
  Mistakes are reported with their line.

### Motion

Runs each frame for a `motion` effect. Reads `visibility`, `progress`, `seconds`,
`canvas_w`, `canvas_h` (px) and `leaving` (1 for an outro); writes `move_x`, `move_y`
(canvas px), `zoom` (× size), `turn` (degrees, clockwise) and `opacity` (×).
`noise(x, stream)` is smooth random wandering in −1 … 1 and `jitter(x, stream)` a
rougher one; each stream is its own curve, and each copy of the effect has its own.

```text
// Fly: arrives from the left over the intro, leaves to the right over the outro.
let eased = 1 - (1 - visibility) ^ 3;
move_x = (1 - eased) * canvas_w * select(leaving, 1, -1);
```

### Bounds

Where the effect may draw, given its input's box: reads `x0`, `y0`, `x1`, `y1` (px) and
`raster_scale` (multiply `layer_pixels` values by it); writes `left`, `top`, `right`,
`bottom` (starting as the input's box); `grow(x, y);` takes a point in. (Atelier Core's
Drop Shadow, Surface and Depth have one.)

### Passes

`pass_count` gives how many passes the shader runs (1–32), `pass_divisor` by how much
each pass's target is divided (1–64: a pass on a coarser grid). They read the
parameters as the shader gets them (layer pixels already scaled), and the divisor also
`pass` (from 0) and `passes`; they write `out`. (Atelier Core's Glow has both.)

## Sound effects

A sound effect is declared like any other effect, with `"kind": "sound"` and a **sound
shader** (`example-looks/telephone.oasound` is one to copy). It shows up on the Sound
tab, keyframes the same way, and can play over the whole clip or as its intro or outro
(`"usage": "in_out"`).

```jsonc
{
  "id": "com.example.telephone", "name": "Telephone", "kind": "sound",
  "params": [{"id": "amount", "type": "float", "default": 1, "min": 0, "max": 1}],
  "meter": "reduction",          // its card shows a meter: reduction · correlation
  "latency": 0.003,              // seconds it looks ahead (the mixer keeps it in sync)
  "tail": "out = 0.5;",          // seconds it keeps sounding after the clip (a script)
  "shader": {"file": "telephone.oasound"}
}
```

A sound shader is an OA script run once per sample, per channel — compiled to machine
code when it loads, so it runs about as fast as the same effect written in Rust:

```text
// Tremolo: the level swings with a sine.
let swing = 0.5 + 0.5 * sin(TAU * speed * time);
out = in * (1 - depth * swing);
```

* **Reads**: `in` (this sample), `left` and `right` (both channels at this moment, for
  stereo effects and for dynamics that treat both alike), `channel` (0 left, 1 right),
  `channels`, `sample_rate`, and the clock: `time` (seconds), `progress` (0 → 1),
  `visibility` (0 → 1 over an intro, 1 → 0 over an outro, 1 otherwise).
* **Writes**: `out` (it starts as `in`), and `reduction` — how many dB it's turning the
  sound down, which its card's meter shows.
* **Memory**: `state` values persist per channel. `delay(s)` is the input `s` seconds
  ago, `delay_out(s)` the output (feedback). For more, declare delay lines: `line echo =
  1.5;` (up to 4 seconds), then `write(echo, x);` stores this sample, `read(echo, s)`
  reads `s` seconds back (blending between samples), `line_max(echo, s)` /
  `line_min(echo, s)` give the most and least of the last `s` seconds.
* **Filters**, each call with its own memory: `lowpass(x, hz, q)`, `highpass(x, hz, q)`,
  `bandpass(x, hz, q)`, `peak(x, hz, q, db)`, `lowshelf(x, hz, db)`, `highshelf(x, hz,
  db)`. `noise()` is white noise, −1 … 1.
* **Spectrum**: after a line `spectrum 1024;` the rest of the shader works on
  frequencies. The sound (as the lines above left it) is cut into overlapping windows
  of that many samples (a power of two, 64 to 8192); for each window the lines run once
  per frequency bin, reading and writing `mag` and `phase`, reading `bin`, `bins`,
  `freq` (Hz) and `size` — then the bins become sound again, `size` samples later.
  `state` keeps a value per bin from window to window. `pass;` starts another pass over
  the bins, which can read what earlier passes left in other bins: `at(name, bin)`
  (blending between bins) and `mean(name, from, to)`. Atelier Core's Denoise and Pitch
  Shift's formants work this way.
* Output is kept within ±4 and never NaN: a shader whose math blows up goes quiet and
  starts over rather than getting stuck or deafening anyone.

## Editors

Some effects need more than sliders. A plugin opts into one of the host's editors with
`"editor"`, and names its parameters as that editor expects:

* **`surface`**: the viewer shows the effect's points, in place of the layer's
  handles, for dragging. Parameters: `grid` (a choice of `Corners`, `3 × 3`, `4 × 4`),
  then a `vec2` per point of a 4 × 4 grid, `p00` … `p33` (row, column), each an offset
  from where it rests, in fractions of the layer.
* **`equalizer`**: the sound card draws the response curve with draggable bands:
  `low_freq`/`low_gain` (a low shelf), `p1_freq`/`p1_gain`/`p1_q` … `p3_*` (peaks) and
  `high_freq`/`high_gain` (a high shelf).

## What plugins can't do (yet)

Effects that need earlier frames (feedback trails, frame blending) and effects that
run on the CPU aren't possible: every effect is a pure function of its parameters,
its clock and its input, which is what lets the renderer cache and share frames.

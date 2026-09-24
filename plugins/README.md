# Plugins

A plugin is a folder with a `plugin.json` and the WGSL it names. Effects from every
enabled plugin show up in the editor's pickers exactly like the built-in ones — the
built-ins *are* a plugin, **Atelier Core**, which ships inside the app.

Folders here are loaded when you run the app from a checkout. Yours belong in the
plugins folder the app opens from **Home → Plugins → Open plugins folder**
(`%LOCALAPPDATA%/OpenAtelier/plugins`). `example-looks` is a working example to copy.

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
    "kind": "point",                  // point · uv_warp · spatial · transition · glyph · glyph_pixel · sound
    "usage": "passive",               // passive (default) · in_out (clip intro/outro) · cut (transition)
    "space": "linear",                // linear (default, premultiplied scene-linear) · display
    "expand": "radius",               // spatial/glyph: the param the output grows by
    "time_varying": false,            // true if the shader reads progress() / clip_seconds()
    "preserves_opacity": true,
    "params": [
      {"id": "amount", "type": "float", "default": 0.6, "min": 0, "max": 1},
      {"id": "size",   "type": "float", "default": 0.7, "min": 0.1, "max": 1.5, "unit": "layer_pixels"},
      {"id": "edges",  "type": "enum",  "options": ["soft", "hard"], "static_only": true}
    ],
    "shader": {"entry": "example_vignette", "file": "vignette.wgsl"}
  }]
}
```

* **Parameter types**: `float`, `int`, `bool`, `vec2`, `vec3`, `color`, `enum`,
  `gradient`, `media`, `text`. Every one is keyframable (and wiggle-able) unless you set
  `"static_only": true`. **Units**: `none`, `layer_pixels` (scaled with the render
  resolution, so a radius looks the same in a ¼ preview), `canvas_fraction`,
  `source_fraction`, `degrees`, `direction` (edited with the dial), `seconds`,
  `decibels`.
* **Shaders** read parameters with `u(base + i)` in the order you declared them (a
  gradient takes 32 slots), and `visibility()`, `progress()`, `clip_seconds()` for the
  clock. Signatures:
  * `point`: `fn name(c: vec4f, base: u32) -> vec4f` — straight (not premultiplied)
    color; `layer_pos()`, `in_origin()`, `in_size()` place the pixel.
  * `spatial` / `uv_warp`: `fn name(pos: vec2f, base: u32) -> vec4f` — read the input
    with `sample_input(pos)` / `sample_input_clamped(pos)`; `pass_index()` for
    multi-pass (`"passes": 2`).
  * `transition`: `fn name(pos: vec2f, progress: f32, base: u32) -> vec4f` with
    `sample_a` / `sample_b`.
* `"source": "fn …"` inline instead of `"file"` works too. Anything wrong — a bad kind,
  a missing file, an entry point the shader doesn't define, an `oa.*` id — is listed on
  the Plugins page and that one effect is skipped.
* Turning a plugin off removes its effects from the menus and from rendering; projects
  keep the settings of clips that used them and say which plugin is missing.

## Sound effects

A sound effect is declared like any other effect, with `"kind": "sound"` and a **sound
shader** instead of WGSL (`example-looks/telephone.oasound` is one to copy). It shows up
on the Sound tab with Atelier Core's sound effects, keyframes the same way, and can play
over the whole clip or as its intro or outro (`"usage": "in_out"` makes it start out as
an intro, like a fade).

```jsonc
{
  "id": "com.example.telephone", "name": "Telephone", "kind": "sound",
  "params": [{"id": "amount", "type": "float", "default": 1, "min": 0, "max": 1}],
  "shader": {"file": "telephone.oasound"}      // or "source": "out = in * amount;"
}
```

A sound shader runs once per sample, per channel. It's a list of lines ending in `;`:

```text
// Tremolo: the level swings with a sine.
let swing = 0.5 + 0.5 * sin(TAU * speed * time);
out = in * (1 - depth * swing);
```

* **Reads**: `in` (this sample), `left` and `right` (both channels of this moment, for
  stereo effects), `channel` (0 left, 1 right), `channels`, `sample_rate`, the clock
  (`time` in seconds, `progress` 0 → 1, `visibility` 0 → 1 over an intro and 1 → 0 over
  an outro, 1 otherwise), `PI`, `TAU`, and every `float`, `int`, `bool` and `enum`
  parameter by its id (an enum reads as its option's index).
* **Writes**: `out`. It starts as `in`, so a shader that never sets it changes nothing.
* `let x = …;` is a value for this sample. `state x = …;` keeps its value from one sample
  to the next (per channel), starting at `…`: that's how filters, envelopes and
  oscillator phases remember. `x = …;`, `x += …;` (and `-=`, `*=`, `/=`) change either.
* **Math**: `+ - * / %`, `^` (power), comparisons and `&& || !` (true is 1, false is 0),
  `sin cos tan asin acos atan atan2 abs sign floor ceil round fract sqrt exp log log2
  pow min max clamp mix smoothstep step tanh`, `db(x)` (decibels to gain), `to_db(g)`,
  `select(c, a, b)` (`a` when `c` is true, else `b`), `noise()` (white noise, −1 … 1).
* **Memory**: `delay(s)` is the input `s` seconds ago and `delay_out(s)` the output `s`
  seconds ago (for echoes and feedback), up to 4 seconds back.
* Comments start with `//` or `#`. A mistake is reported on the Plugins page with its
  line number, and that one effect is skipped.
* Like WGSL effects, a sound shader can only reach its own samples: no files, no loops,
  no calls out. Its output is kept within ±4 and never NaN — a shader whose math blows
  up goes quiet and starts over rather than getting stuck or deafening anyone.

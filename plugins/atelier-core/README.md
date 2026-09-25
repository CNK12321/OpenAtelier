# Atelier Core

Every effect OpenAtelier ships with — color, blur, stylize, warp, light, keying,
masking, depth, motion, intros and outros, transitions, text animation and sound —
written as an ordinary plugin (see [`../README.md`](../README.md)).

The program carries a copy of this folder inside it (`crates/graph/build.rs`) and loads
it with the same loader as any other plugin, so it's always there and can't get out of
step with the program; a copy of it in a plugins folder is skipped. What it has that
other plugins don't is its ids: `oa.*` belongs to it.

To start a plugin from one of these effects, copy its entry from `plugin.json` and the
files it names into your own plugin, give it your own id (`com.you.…`), and rename its
shader's entry function to something of your own too (function names must be unique
across all effects).

| Folder | |
|---|---|
| `color`, `blur`, `stylize`, `warp`, `light`, `key`, `mask`, `depth` | Picture effects (WGSL) |
| `anim`, `motion` | Intros, outros and movement: shaders, and motion scripts (`.oamotion`) |
| `transition` | Cut transitions |
| `text` | Per-letter, per-pixel and behind-the-text effects |
| `sound` | Sound shaders (`.oasound`) and how long each rings on (`.tail`) |

Scripts beside the shaders: `light/glow.passes` and `light/glow.divisor` (how many
passes Glow runs and how coarse), `warp/surface.bounds` and `depth/slab.bounds` (where
Surface and Depth may draw).

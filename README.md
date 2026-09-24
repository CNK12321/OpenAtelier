<p align="center"><img src="assets/logo-badge.svg" width="96" alt="OpenAtelier"></p>

# OpenAtelier

A GPU-first video editor written in Rust, built for short-form and multi-format work:
one timeline, many aspect ratios (16:9, 9:16, 1:1…), with effects, titles, sound and
export all running through the same render engine the preview uses.

> **Status: early and moving fast.** It edits, plays and exports real projects, but file
> formats and APIs still change. It's developed on **Windows**; **Linux** support is new.

## What it does

- **Timeline editing**: tracks, trim, split, ripple, slip, snapping, groups, compound
  clips, track reordering, and dividers that cut the timeline into colored sections you
  can move, empty or delete.
- **Formats**: one sequence, several canvas sizes; each clip can be reframed per format.
  A vertical layout puts the viewer in a tall column for 9:16 work.
- **Direct manipulation**: move, scale, rotate and crop clips in the viewer, with
  keyframes, easing curves, and procedural waves (LFOs) on any property.
- **Effects**: color, blur, stylize, warp, keying, masks, depth, motion, clip
  intros/outros and cut transitions, per-letter and per-pixel text animation. They can
  be dragged between clips, applied to many clips at once, and put on the background.
- **Sound**: a sample-accurate mixer with keyframable gain, speed with pitch
  correction, and sound effects (EQ, echo, reverb, denoise, tone…) on the same effect
  system as the picture. A built-in recorder cuts takes into clips.
- **Captions**: speech to timed captions on their own track, with
  [faster-whisper](https://github.com/SYSTRAN/faster-whisper) running on your computer.
  It isn't bundled: the Captions window downloads it (and a model) when you first ask,
  into one folder you can remove from the same place. Group by phrase, sentence, count
  or single words, set limits and pauses, color captions per speaker, copy a title's
  style.
- **Color**: a scene-linear working space, per-file input transforms (log and HDR
  curves, gamuts) and tone mapping to SDR.
- **Plugins**: effects are plugins, even the built-in ones. A plugin is a folder with a
  `plugin.json` and WGSL shaders — and, for sound, small *sound shaders*. See
  [`plugins/README.md`](plugins/README.md) and the example in `plugins/example-looks`.
- **Export**: H.264/HEVC (hardware encoders through Media Foundation on Windows),
  ProRes, ProRes 4444 and WebM with transparency, and GIF; a time range; a live
  preview while it runs.

## Requirements

- **Windows 10/11 or Linux** (x86-64). macOS builds but hasn't been tried.
- **A GPU with DirectX 12, Vulkan or OpenGL 4.3** — most from the last ten years,
  dedicated or integrated. The best one is picked automatically (dedicated over
  integrated, DirectX 12 on Windows, Vulkan on Linux), and with no usable GPU driver it
  falls back to a software renderer (slow, but it works). Settings → Graphics picks
  another API or card; `oa gpu` lists what the computer has and what each can do.
- **ffmpeg and ffprobe on `PATH`**, for probing media, decoding video and audio, and
  some export formats. Tests that need them skip themselves when they're missing.
- **Rust**, recent stable (developed on 1.97), via [rustup](https://rustup.rs).

Video is decoded by Media Foundation's hardware decoder on Windows (with a DirectX 12
GPU), and by ffmpeg everywhere else — including on Windows for files Media Foundation
can't take — which uses the system's hardware decoder too when there is one (VA-API,
NVDEC, D3D11VA).

### Linux

Linux support is new: CI builds it and runs its tests on Ubuntu 24.04 (on a software
GPU), but it hasn't had much use on real desktops yet — reports are welcome. You'll need
the ALSA development files, ffmpeg and a Vulkan driver (Mesa's, or your GPU vendor's):

```bash
sudo apt install build-essential pkg-config libasound2-dev ffmpeg mesa-vulkan-drivers fontconfig   # Debian/Ubuntu
sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel ffmpeg mesa-vulkan-drivers fontconfig        # Fedora
```

Settings, autosaves and downloaded engines live in `~/.local/share/OpenAtelier`
(`$XDG_DATA_HOME`). If the window won't open, try OpenGL: `OA_GPU_BACKEND=gl`.

## Download

Ready-to-run builds are on the [Releases page](https://github.com/CNK12321/OpenAtelier/releases):
`OpenAtelier-<version>-windows-x64.zip` and `-linux-x64.tar.gz`. Unpack anywhere and run
`OpenAtelier.exe` (Windows) or `./openatelier` (Linux). The current builds are **betas**.

The app checks the releases once a day and offers newer versions in a bar at the top:
**Update** downloads the package, checks it against the release's `SHA256SUMS.txt`,
installs it in place and restarts. Settings → Updates picks the channel (Stable, or Beta
for pre-releases too), turns the check off, or checks now.

### Making a release

The version lives in the workspace `Cargo.toml`. Tagging it publishes the release:

```bash
git tag v0.1.0-beta.1
```

```bash
git push origin v0.1.0-beta.1
```

`.github/workflows/release.yml` builds Windows and Linux packages, writes the checksums
and publishes a GitHub Release (a pre-release when the version has a `-beta`/`-rc` part).

## Getting started

```bash
git clone <this repository>
cd OpenAtelier
cargo run --release -p oa-app                        # the editor
cargo run --release -p oa-app -- my_clip.mp4 photo.png   # open with media (or drop files on the window)
```

In the editor: drop media on the window or use the import button, drag clips onto the
timeline, click a clip to edit it in the inspector, and press Space to play. Ctrl+Z
undoes, S splits, Delete removes (Shift+Delete ripples). Settings are under the
**OpenAtelier** menu.

There's also a command-line tool, `oa`:

```bash
cargo run -p oa-cli -- gpu                                        # the GPUs this computer has, and the one used
cargo run -p oa-cli -- presets                                    # the canvas presets
cargo run -p oa-cli -- demo demo.oaproj.json --media my_clip.mp4  # a small project
cargo run -p oa-cli -- plan demo.oaproj.json 2.5 --variant "Vertical 9:16"   # the render graph for one frame
cargo run -p oa-cli -- render demo.oaproj.json 2.5 frame.png
cargo run --release -p oa-cli -- export demo.oaproj.json out.mp4
cargo run --release -p oa-cli -- bench demo.oaproj.json 6         # frame times
```

## How it's built

A snapshot of the document is turned into a render graph for each frame, optimized
(fusing effects, culling what's hidden, choosing decode resolutions) and run on the GPU
with wgpu; preview and export share that path, so what you see is what you get.
[DESIGN.md](DESIGN.md) explains the architecture and the reasoning section by section;
[TODO.md](TODO.md) is the working list of what's next and what's known to be missing.

| Crate | What it is |
|---|---|
| `oa-time` | Exact time (flicks) and frame rates |
| `oa-params` | Parameters, keyframes, easing, waves |
| `oa-doc` | The project document, edits (ops) and undo |
| `oa-graph` | The render graph, cache keys, the effect registry and plugins |
| `oa-plan` | Document snapshot + time → render graph |
| `oa-gpu` | The wgpu renderer |
| `oa-text` | Fonts, shaping and glyph distance fields |
| `oa-media` | Probing, importing and hardware decoding |
| `oa-audio` | Mixing, sound effects and sound shaders, playback, recording |
| `oa-edit` | Editing commands (trim, split, sections, transforms…) |
| `oa-captions` | Captions: the downloadable faster-whisper engine and caption grouping |
| `oa-track` | Point tracks: smoothing and thinning drawn tracks, the downloadable CoTracker engine |
| `oa-export` | Rendering to video files |
| `oa-cli` | The `oa` command-line tool |
| `oa-app` | The editor (egui) |

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Please read the
[code of conduct](CODE_OF_CONDUCT.md) first, and report security issues as described
in [SECURITY.md](SECURITY.md).

## License

OpenAtelier is free software, licensed under the **GNU Affero General Public License,
version 3 or later** ([LICENSE](LICENSE), `AGPL-3.0-or-later`).

You may use, study, copy, modify and share it. If you distribute it — or a modified
version — or let people use a modified version over a network (a hosted editor, a
rendering service), you must offer them its complete source under the same license.
Contributions are accepted under the same terms.

The bundled icon font is a subset of Google's Material Symbols (Apache License 2.0);
see [`assets/fonts/README.md`](assets/fonts/README.md).

# Contributing to OpenAtelier

Thanks for wanting to help. Bug reports, ideas, plugins, docs and code are all welcome.

## Before you start

- **Bugs**: open an issue with what you did, what you expected and what happened. The
  app's Messages list (bottom of the inspector) and any error in the corner are useful;
  so are your OS, GPU and a small project file if you can share one.
- **Features**: open an issue to talk it through first if it's bigger than a small fix,
  so work isn't duplicated or pulled in a direction that doesn't fit.
- **Effects**: most new effects don't need to touch the engine — they can be a plugin
  (WGSL for pictures, a sound shader for sound). See [`plugins/README.md`](plugins/README.md).

[DESIGN.md](DESIGN.md) explains how the pieces fit and why; [TODO.md](TODO.md) lists
what's planned and what's known to be missing — a good place to find something to do.

## Building and testing

You'll need what the [README](README.md#requirements) lists. Then:

```bash
cargo build
cargo test -- --test-threads=4   # GPU and decoder tests skip without a DX12 GPU or ffmpeg
cargo clippy --workspace --all-targets
```

Please make sure tests pass and clippy is clean before opening a pull request, and add
tests for what you change — the engine crates are well covered, and new behavior should
be too. UI code in `oa-app` is harder to test automatically; describe what you checked
by hand in the pull request.

## How the code is written

- **The document changes only through ops** (`oa_doc::Op`), so every edit can be undone.
  Editing commands (`oa-edit`) read a snapshot and return ops; they never mutate.
- **GPU first**: pictures stay as GPU textures. Avoid CPU round trips in the render path.
- **Plugins describe effects; they don't see engine internals.** Keep the
  `EffectDescriptor` boundary stable (`PLUGIN_API_VERSION`).
- **Everything animatable**: effect parameters should be keyframable and work with
  waves, unless they genuinely can't be.
- **American English** in code, comments, docs and UI text ("color", "center", "gray").
- Comments explain *why*, in plain words; match the density of the code around you.
- Keep changes focused: one topic per pull request is much easier to review.

## Pull requests

1. Fork, and branch from `main`.
2. Make your change with tests, and update DESIGN.md / TODO.md if it changes how
   something works or ticks something off.
3. Run the tests and clippy.
4. Open the pull request with a short description of what and why.

By contributing, you agree that your contribution is licensed under the project's
terms: the GNU Affero General Public License, version 3 or later (see
[LICENSE](LICENSE) and the [README](README.md#license)).

Please follow the [code of conduct](CODE_OF_CONDUCT.md) in all project spaces.

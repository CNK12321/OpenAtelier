# Icons

`MaterialSymbolsRounded.ttf` is **Google Material Symbols** (Apache License 2.0), cut
down to the icons the editor uses — 28 KB instead of the 15 MB variable font.

egui lays text out itself and doesn't do ligature shaping, so icons are drawn by
**codepoint** (the fallback Google documents for that case), with the ligature name kept
beside it in `crates/app/src/icons.rs` as the source of truth.

## Adding an icon

1. Find it on [fonts.google.com/icons](https://fonts.google.com/icons) — note its name
   and codepoint.
2. Add a line to the `icons!` list in `crates/app/src/icons.rs`, including a drawn
   fallback shape.
3. `python assets/fonts/subset.py` — it downloads the full font if needed, cuts it to
   whatever `icons.rs` asks for, and deletes the download again (`--keep` to keep it).

A `MaterialSymbols*.ttf` in `%LOCALAPPDATA%/OpenAtelier/fonts/` is used in preference to
this one, so the full font can be dropped in without a rebuild. With no font at all, the
icons are drawn as shapes instead — the app never shows empty boxes.

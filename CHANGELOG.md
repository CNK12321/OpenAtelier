# Changelog

Each release's section becomes its description on GitHub, and the app's update dialog
lists its points. Name a section after the version as it's tagged (`0.1-beta.6`, the
tag without its `v`) before tagging; until then it's **Unreleased**.

## Unreleased

### New
- **Characters** (text intro and outro): letters that change into the text, or out of
  it. Each starts as another character — random ones (a decode) or none — and settles
  into its own, one after another (**order**: left to right, right to left,
  random, center out, by word; **stagger**). **Cut** swaps characters in place, **roll**
  turns them like a slot machine's reels, **flip** folds them like a departure board,
  **fade** crossfades; **scramble** sets how many random characters pass on the way,
  from letters, digits, symbols or binary, matching each letter's case. The letters keep
  their places throughout, and other letter effects still move them.
- **Counter** (text effect): the title shows a number — keyframe **value** to count up
  or down — with **decimals**, a thousands **separator** (comma, period, space, none),
  a **prefix** and a **suffix** ($, %, pts). **Roll** turns the digits like an
  odometer's instead of jumping.
- **Bend Along Path** (Effects → Compositing): lays the picture along a curve you draw
  in the viewer, as a band. Drag the curve itself to bend it, click it to add a point
  (the curve keeps its shape), drag a point or, once it's picked, its two handles;
  drag the ticks either side of a point to widen or narrow the band there; double-click
  a point for a sharp or smooth corner, Delete to take it away. Start from a shape
  (straight, arc, S-curve, wave, circle). **Stretch** lays the picture along the path
  once; **repeat** lays it end to end at its own shape (**mirror** flips every other
  copy, for no seams); **alignment** turns which way of the picture runs along; each
  point's **offset** pushes the band sideways there; **slide** moves the picture along
  the path; **start** and **end** show part of it (keyframe **end** to draw it in).
- **Disintegrate** (Stylize): the picture breaking up and blowing away. It's cut into
  **blocks** that, one after another as the **amount** rises, slide off (up to
  **spread**, towards **direction**), split into red and blue (**fringe**) and crumble
  into specks (**grain**) until nothing is left. As an outro the clip falls apart; as
  an intro it comes together. **Speed** makes the pieces jitter; **seed** picks another
  break-up.
- **Distress** (Stylize): noise eating into the picture, like worn print, chipped paint
  or old film. It eats most near the picture's edges (**reach**), so titles and
  cut-outs crumble at their outlines, in clumps of **grain** size, with thin broken
  **scratches** at an **angle**. Holes go see-through or turn a color; **speed** makes
  it flicker and **seed** picks another pattern.
- **Dither** (Stylize): the picture in a few **levels**, its in-between shades made of
  a pattern: **ordered** (the grid of old computers and games), **noise** or **lines**,
  in chunkier cells with **size**. In color, black and white, or between two colors
  of your own (a handheld-console green to start). The pattern can drift: **direction**
  (a dial) and **speed** in pixels a second.
- **Path properties:** a curve is now a property like any other, keyframed as a whole —
  keys blend point by point, matched so that adding a point mid-animation never
  scrambles the in-between frames, and handles turn smoothly instead of collapsing.
  Plugins can use them too (`"type": "path"`).
- **Multi-cam** (turn on in Settings → Features): edit footage from several cameras as
  one clip. Right-click a compound clip → **Make multicam clip** (or select the clips
  from each camera, lined up, and press the new button beside Captions): each of its
  picture tracks becomes a cam, shown one at a time. Press **1–9** at the playhead —
  while playing, too — or right-click → **Show cam** to cut to another; a white line
  marks each cut on the clip, with the cam each stretch shows, and its thumbnails show
  that cam. While it's selected, all its cams line the right of the viewer as they are
  at the playhead (smaller when there are many) — click one to cut to it; a small
  number in each corner is its key, the one showing outlined. Under them, **previous
  cut**, **delete cut** (the cam before it carries on) and **next cut**; the panel
  folds away to a tab. Its audio tracks play
  throughout, and each camera's own sound plays while that camera shows (with a short
  fade at each cut so it doesn't click). **Extract cam** takes any cam out for the
  stretch between two cuts, as ordinary clips on a layer above — for a
  picture-in-picture or a split screen. **Make compound clip** turns it back.

### Changed
- **Settings from the start page:** a Settings button in its top right corner (Ctrl+,
  works there too), so themes, features and the rest can be set before opening a
  project.
- **The start page has a living backdrop:** a ghost of a timeline plays behind Home and
  Projects — faint picture and sound tracks full of clips (keyframe lines on some,
  waveforms on the sound), scrolling slowly by under a ruler of running timecodes, with
  a slow drifting light bringing some stretches up and letting others fade. The pointer
  is the playhead: it glides after the pointer and fades in, showing the moment it's
  at, and what's near it brightens gently. It's kept very faint, with a soft backing
  under the page's panels so they read clearly; it follows the theme, and switches off
  with **UI effects** (Settings → Interface).
- **A Features page in Settings:** the tools most people never need — advanced
  transformations, advanced color, masks and now multi-cam — are switched on there,
  each off until you want it. Turning one off only hides it; what you made with it
  stays as it is.
- **Scanlines' "roll" is now "speed"** (lines a second; below 0 they run down), named
  like the other effects' speeds. Projects that used it open with it carried over,
  keyframes and all.

### Fixed
- **Compound clips are made at the format you're editing:** nesting while looking at
  the vertical format makes a vertical compound (with the clips placed as they are in
  that format), not one at whichever format the timeline was saved on.
- **Inside a compound clip, the format you pick is the compound's:** switching or
  adding a format there changes the shape the compound clip is drawn at outside, and
  opening it again shows that format.

## 0.1-beta.9 — Beta 9

### New
- **Effect groups:** right-click an effect → **Group**, then drag more effects between
  its effects or onto its head (or right-click → Add to…). A group has:
  - **a mix slider**, keyframable: the group's result faded over the picture that went
    in (a true blend, so it works with any effect), or dry/wet for sound groups;
  - **an off switch** apart from the mix, so turning it back on returns to the same mix;
  - **a collapsed view**: tick the circle beside any of its effects' properties, and the
    collapsed group shows just those, with its mix;
  - **a lock** (right-click its head): it stays collapsed and its effects can't be
    changed, only its mix, switch and ticked properties.

  Drag a group's grip to move it in the list, or onto another clip to copy it there.
  Copy all effects / Paste effects bring groups along. A group is one panel around its
  header and effects, with a ⋮ menu; collapsed, it lists what's in it.
- **Themes** (Settings → Interface): Automatic (follows your system), Dark, Light or
  Black (dark with true-black panels, for OLED screens and dim rooms).
  Beside it, **UI effects** can be turned off: no glow around the viewer, and project
  cards stay flat instead of lifting and tilting.
- **Shortcuts list** (Settings → Shortcuts): every key and mouse shortcut, drawn as
  keycaps (arrows and mouse buttons as pictures), by panel — General, Playback,
  Timeline, Viewer, Media bin, Effects, Curve editor, Quick cut, Captions — with a
  search box.
- **Echo** (Effects → Motion): ghost copies of the clip from moments before, after or
  both — the frame-blended "slow shutter" look of vlog edits. Set how many copies, how
  many frames apart, how fast they fade, and whether they go over the clip (ghosts on
  full-frame footage), under it (trails behind a cut-out or title), average out like a
  slow shutter, or add up into a glow. It echoes the footage's own motion as well as
  keyframed moves; speeding the clip up spreads the copies further apart.
- **Bloom** (Effects → Light): light spilling out of the bright parts of the picture —
  what's over the threshold, blurred and added back, with a bright core round each
  highlight and a wide spread around it. It goes by how bright things really are, so an
  overexposed light blooms more than a white wall. Threshold (0.1–1), softness (how
  gently it cuts in), radius, intensity and tint, all keyframable; wide blooms are
  worked out on a coarser grid, so they stay fast.
- **Stepped sliders:** properties that only make sense in whole steps — Liquify's
  detail, Kaleidoscope's segments, Echo's copies, Motion Blur's samples — snap to them,
  with a dot on the slider at each step, and keyframed they move from step to step.
  Plugins get the same with `"step"` on a parameter.
- **Pick several effects:** click anywhere on an effect's card, Ctrl+click for more,
  Shift+click for a run (click a group's head to pick the whole group). A click on the
  list's empty space picks none (so does one on the media bin's, for its files). Tools
  stand down the side of the Effects, Sound and Transitions tabs, staying put as the
  list scrolls: group, ungroup,
  turn on/off, move up/down, copy, paste, put on the other selected clips, save to
  assets, fold every card, show only the picked ones, remove. Keys over the list:
  Ctrl+A, Del, Ctrl+C/V, Ctrl+G, Alt+↑/↓, Esc. Right-click a picked card for all of
  them. Dragging one drags them all: to reorder, onto a clip to copy, onto the media
  bin to keep them as one preset.
- **Effect cards fold** to their header (the arrow beside the grip).
- **Use with blend…** (an effect's right-click menu): its result mixes with the picture
  that went into it by a blend mode — add, screen, multiply, overlay and the rest, as
  the Blend effect has — and a mix, instead of replacing it. Keyframable mix.
- **Effects ↔ transitions:** drag an effect over the Transitions tab (it opens) and onto
  the intro or outro to add a copy there — if it can come in from an off state — or
  drag an intro or outro onto the Effects tab to have it for the whole clip.
- **Effect presets in Assets:** drag an effect or a group onto the media bin (it turns
  to Assets) or use **Save to assets…** in its right-click menu. Choose whether to keep
  its keyframes (they stretch to the clip it goes on) and its masks. Presets show as
  cards previewed on a sample picture; drag one onto a clip or the effect list, or
  double-click it, to put it on the selected clips. An effect whose plugin isn't
  installed comes along with its settings kept and is named, not silently dropped.
- **Smart relink** (Settings → Media, on by default): when a project opens, files that
  have moved are looked for on this computer — around where they were, by the project,
  in your Videos, Desktop, Downloads and other folders, then the drives. A file counts
  as found with the same name and the same content, or (renamed) the same content alone,
  told by a quick fingerprint of each file taken when it was imported; a different file
  that only shares the name is passed over. Projects older than fingerprints get theirs
  as they open. The bin's **Find missing files on this computer** does the same any
  time, and **Find missing files in a folder** now matches by content too, in the
  background.
- **A Duplicate's own effects are full effect cards:** under the Duplicate, the effects
  on its copy alone now fold, reorder by drag, get picked, grouped, masked, blended and
  saved as presets like the clip's own. Drag a card from the clip's list onto the copy's
  to move it there, or back out; a count shows on the Duplicate while its copy's list is
  folded away, and a rail marks which card the list belongs to. **own effects** says
  whether they run after the effects the copy takes from the clip or before them, and the
  eye button shows the copy alone in the viewer while you adjust it.

### Faster
- **Sped-up clips scrub and step back smoothly:** going backwards through a clip at
  2× or 4× decoded far more than it needed (stepping back at 4× ran at 32 fps; now 80).
- **Transparent video plays, scrubs and speeds up smoothly:** transparent footage bigger
  than 720p (ProRes 4444, VP9 with alpha…) now gets a proxy of its own, made in the
  background: small H.264 with the picture on top and its transparency below, which the
  hardware decoder plays and the viewer puts back together, soft edges and all. A 4×
  ProRes 4444 clip went from 20 fps to 260, a jump while scrubbing from 130 ms to 15.
  Exports still use the original. Until the proxy is ready, sped-up every-frame-a-keyframe
  footage only hands over the frames shown, and stepping back no longer restarts ffmpeg
  on every frame.
- **Footage with keyframes far apart** (game and screen recordings, 60 fps phone video)
  gets a proxy, made in the background: scrubbing a 4× clip of it jumps in 9 ms instead
  of 79. Exports still use the original.

### Changed
- **Liquify's amount slider runs 50–400**, where it's most useful (typing a value still
  goes past either end).
- **Mirror, Kaleidoscope, Tile, Scroll and Surface** are listed under Compositing instead
  of Warp: they arrange the picture rather than bend it.
- **The example plugin no longer ships:** releases carry only the plugin guide
  (`plugins/README.md`) and never load the `plugins` folder beside the program;
  `example-looks` stays in the repository for anyone writing a plugin, and a debug build
  run from a checkout still loads it.

### Fixed
- **Files and browser pictures can be dropped on a track's header:** they go on that
  track, right against the header (at the left edge of what the timeline shows), with
  the drop line showing where; before, a drop there went only into the media bin.
- **Pictures in the media bin keep their shape and scale mode:** assets that aren't
  16:9 were squashed into a 16:9 preview; every preview (bin cards, folders, assets,
  the timeline's clips, Quick cut) now keeps its picture's own shape, read from the file
  when it isn't known, and is never stretched to fit. Pixel art (or anything set to
  **Pixel scale**) previews pixel-sharp instead of blurred; changing a file's scale mode
  remakes its preview.
- **Dragging an effect across the inspector's tabs no longer flickers** between two of
  them: the tools column keeps its room on every tab while an effect is dragged, a tab
  opens once the pointer rests on it a moment, and none opens again until the pointer
  moves on.
- **Effect groups can be picked:** a click anywhere on a group's head picks the whole
  group (Ctrl or Shift: with what's picked), and a picked group is outlined, collapsed or
  not. Locked groups can be picked too, and removed — from their menu, the side strip or
  Delete; locking stops changes, not removal.
- **Projects get their pictures again:** a project's picture for the start page was only
  written on Ctrl+S, so projects that save themselves showed just their initial. Now
  it's written as you work (at most once a minute), when a project without one opens,
  and as you leave a project.
- **Mirror turns to any angle, with a handle in the viewer:** its line, like Slice's cut,
  through a point you drag and turned by its handle (it replaces the vertical/horizontal
  choice; older projects are converted).
- **Effects work on cropped clips:** a cropped clip's effects take what the crop keeps as
  the whole picture. Kaleidoscope folds round the middle of what you see, Mirror, Tile,
  Fisheye, Swirl and the rest work on the kept picture instead of reaching into the cut
  away (transparent) part, and their points in the viewer — a center, a mirror line —
  are on the kept picture too (a tracked point as well). Surface's points rest on the
  corners of what the crop leaves.
- **Alt+← / Alt+→ go clip to clip:** to the start of the previous or next clip on the
  track (the selected clip's, or the top one under the playhead), selecting it if a clip
  was selected; back from inside a clip goes to its own start first. Over the viewer,
  Alt+← / Alt+→ still nudge the picture.
- **Ctrl+← / Ctrl+→ step through keyframes:** the selected clip's start, each of its
  keyframes and its last frame, then on to the next clip; with Shift, its start and
  last frame only. Counted in whole frames, so it no longer lands a frame off on clips
  that don't end on a frame boundary.
- **The arrange buttons work on real footage:** Close gaps, Space evenly and Move to
  playhead refused with "item would overlap another item" whenever a clip's length
  wasn't a whole number of frames (most footage), or when moving clips later.
- **Adding a clip from the bin (＋ or double-click) or an asset puts it at the playhead**,
  on the first track with room there, instead of at the end of the bottom track.
- **Renaming a project from the top bar renames it everywhere:** its file and picture
  (what the start page and recent list show) and its folder, unless its media live in
  there. It used to change only the name stored inside the project.
- **Ctrl+S, Ctrl+Shift+S and Ctrl+I work:** the menus showed them, but nothing took
  the keys.
- **Clips that find no room go on a temp layer** everywhere, as pastes already did:
  dropping from the bin, Add Title, nesting, Extract audio, breaking a compound clip
  apart and appending to a timeline with no track of the kind made real tracks. A temp
  layer goes away once it's empty (keep it with its +), and Add Title over other clips
  is now one undo step, not two.
- **Fullscreen playback takes on the first press:** a request Windows dropped (it can,
  while the window is maximized or settling) is sent again, and a window taken out of
  fullscreen another way no longer needs a press to "leave" first. Leaving puts a
  maximized window back maximized.
- **The vertical layout opens with roomier proportions:** the viewer's column a little
  under half the width, the bin a quarter, the timeline over a third of the height, as
  shares of the window (sizes you've dragged are kept).

## 0.1-beta.8 — Beta 8

### New
- **A new Projects page:**
  - **Folders on the left**, like a file explorer: Recent, your own folders (+ New
    folder; drag cards onto one to move them in; right-click to rename or remove),
    Archived and Trash. They're real folders in the projects folder, so they travel
    with it.
  - **Every project in the projects folder** is listed, not just recently opened ones.
  - **Search, sort** (last edited, name, date created) and **a list view**.
  - **A ⋮ menu on each project** (and right-click): rename (its folder and file too),
    duplicate, show in folder, move to a folder, archive, move to trash; in the trash,
    restore or delete for good (and Empty trash), after asking.
  - **Livelier cards:** they tip toward the pointer and their edge glows; vertical
    projects sit on a blurred copy of their picture instead of black bars; projects
    without a picture get their own color and initial; "Edited 2 h ago".
  - **New project and Open or import** (one button for projects and media alike),
    centered at the top.
  - **Unsaved work shows on its project's card**, with Recover right there.
- **A better media bin:**
  - **Search** with Material icons and **smart filters** (Video, Audio, Pictures, Unused,
    and Missing when there are any), each with how many pass it.
  - **A list view** with details: what each file is, its length, and whether it's used.
  - **Pick several** files (Ctrl-click, Shift-click a run, Ctrl+A) to add, drag onto the
    timeline, move to a folder or remove together.
  - **Remove from project** (with the clips that use a file; Ctrl+Z brings it all
    back), from the right-click menu or with Delete.
  - **Missing files** get a red badge and a Relink button, and **Find missing files in a
    folder** relinks them all by name.
  - **Folders** show a mosaic of what's in them, and skim through it under the pointer.
- **Quick cut:** the ＋ with scissors on a video or sound file opens it with a preview. Play
  it, drag on its strip (or press I and O) to keep the parts you want, and add them to
  the timeline one after another.
- **Text wraps to a box:** turn on wrapping (the wrap icon) in a title's Text section and
  its lines break between words to fit the width you set (keyframable; a word too long for
  the box is split). Alignment works within the box.
- **Favorite and recent fonts:** star a font in the font menu to keep it at the top;
  the fonts you used lately come next.
- **Hue Anchor** (Color): pick an anchor color; every hue's distance from its hue is
  scaled. Above 1 the hues spread out and repeat round the wheel; below 1 they gather
  toward the anchor; at 0 everything takes its hue. Keyframable, and an intro/outro.
- **Invert can flip brightness, hue and saturation** too (new choices beside red, green,
  blue…): light and dark turned over with the colors kept, each color turned to its
  opposite with the light kept, or vivid and muted swapped.
- **Blurred content from one layer:** the background's "blur from" picks a layer to make
  the backdrop of, instead of everything on screen.

### Faster
- **Importing many files at once** no longer bogs the editor down: files are probed and
  converted a few at a time (as many as the computer has cores for, up to four) in the
  order they were added, filmstrips and waveforms are read two at a time, and a whole
  batch is one undo step ("Import media").
- **Thumbnails load much faster:** a file's skimming frames are read in one pass that
  decodes only its keyframes (one ffmpeg run per file, not one per frame); every card
  gets its first picture before any fills in; and finished filmstrips and waveforms are
  kept on disk — reopening a project shows them at once.
- **Projects open much faster:** their files are read in several at once, each file's
  probe (and whether it plays directly) is kept on disk so an unchanged file isn't probed
  again, and the first probe of a long video is quicker. The loading screen counts the
  files as they come in.
- **Playback stays smooth while media loads:** thumbnails, waveforms, probing, proxies
  and conversions run below normal priority (proxies on half the cores at most), so
  they no longer take the CPU from the preview.

### Changed
- **Keys stretch with a new speed:** speeding a clip up or slowing it down (ramps too)
  stretches its animation to match, so it stays with the footage. Trimming either end
  leaves the keys where they are, and cutting a clip leaves the curve exactly where it
  was on both halves.
- **Starting keyframes away from a clip's start** also puts a key at its start, holding
  the value shown there, so a change at the playhead animates from how the clip begins.
- **A neater Text section:** bold/italic, alignment and wrapping as icon buttons, with
  the font menu beside them.
- **A tidier top bar**, in three parts:
  - **Left:** the menus, now plain until you point at them (moving across them while
    one is open opens the next) and folded into one ☰ menu when the window is narrow,
    then undo and redo.
  - **Middle:** the project's name (click to rename it; it undoes), whether it's saved
    ("Saved 2 min ago", "Edited", "Saving…", or "Not saved" when it won't save itself)
    and the format, which now says what it is ("Landscape 16:9 · 1080p · 30 fps"); "all
    formats" moved into its menu.
  - **Right:** Export, the one button in the accent color, and Home.
  - It's a little taller, with a hairline under it, and the menu text is all translated.
- **Transform's reset, copy and paste are icon buttons**, and its "all formats" / "this
  format only" note is gone (the format menu has the switch).
- **Length in seconds or frames:** the Length section's heading switches between them
  (remembered), and a reset button there puts the length back (footage as long as its
  file allows, titles and solids the default length). The ±1 frame and "To the end"
  buttons are gone.
- **The layout follows the format:** a tall format gets the vertical layout and a wide
  one the standard, with no button to press. Settings can still pin one.
- **Proxies work out of sight:** heavy footage still gets a small copy the viewer plays
  (exports always use the original), but there's nothing to see or set about them — no
  tags in the bin, menu entries or settings.

### Fixed
- **The workspace waits while a project opens:** the timeline, bin, properties, menus,
  shortcuts and file drops are off until it's in (they used to edit the project being
  left), and playback stops.
- **Blurred content with a turned or moved clip** no longer leaves empty corners: the
  backdrop is enlarged by what's surely inside both the clip's picture and the frame.
- **Relinking a missing file that's on the timeline** works (it was refused with "media
  in use"), runs in the background, and projects that find moved files on opening now
  point at their new place.
- **Bin cards dropped on the viewer** go in at the playhead.
- **Picking in the bin** happens on release, so dragging a card doesn't pick it, and
  clicking the only picked card again unpicks it.
- **Dropping a bin card on a track's header** no longer cancels the drop: the clip lands
  on that track, against the header (at the left of what's in view).

## 0.1-beta.7 — Beta 7

### New
- **Duplicate** (Compositing): a second copy of the clip, drawn in front or behind,
  moved, scaled, turned and faded on its own. Its effects: the clip's (**inherit
  effects**), only those listed above the Duplicate (**copy previous effects**), or
  **no effects** — plus effects added to the duplicate alone, with the button on its
  card.
- **Slice** (Stylize): cuts the picture along a line you drag and turn in the viewer
  (Shift turns it in 15° steps), pushing the sides apart (separation) or sliding them
  along the cut.
- **Liquify** (Warp): melts the picture into swirling, marbled ribbons — amount, size,
  detail, swirl, and flow to set them moving.
- **New project** asks for a name, a format (with each shape drawn) and a frame rate,
  and saves the project straight away in a folder of its own under Documents →
  OpenAtelier Projects — no picking where it goes. Saving a project that was never
  saved does the same; Save as… still puts it anywhere. Change the folder under
  Settings → Editing.
- **Opening a project** shows a loading screen over the workspace until it's ready.
- **Temp layers:** a paste with no room on any track (or a clip whose new speed runs it
  into the next one) goes onto a temp layer, outlined in gray dots. It goes away by
  itself once it's empty; the + in its header keeps it as a real track.
- **Arrange buttons:** with several clips picked on one track, buttons in the bar under
  the timeline close the gaps between them, space them evenly, or move them to the
  playhead.
- **Open or close time:** Ctrl+drag on an empty stretch of a track. Right opens a gap
  there on every track (cutting clips under that point), pushing everything after it
  later; left takes the stretch dragged over out of every track and closes it up. Both
  ends snap to clip edges and the playhead, and the playhead takes the color while you
  drag.
- **Every effect with an off state is also an intro and an outro:** Blur, Glow, Invert,
  Pixelate, Glitch, Chromatic Aberration, Liquify, Shake and the rest (Echo, Reverb, Bass
  and other sound effects too) can be picked in the Transitions tab's intros and outros
  (or set to Intro/Outro on a sound effect's card). As an intro the effect starts at
  full and eases to off as the clip arrives; as an outro it eases from off to full as
  the clip leaves. Plugins mark an effect's off state with `off` in their manifest.
- **Masks, clearer:** the mask tools stand down the left of the viewer while the Masks
  tab is open — select, edit, rectangle, ellipse, pen, brush, eraser, magic select, fill
  and rotoscope, as icons (picking one with no mask yet starts one). Each mask is a card
  with a preview of what it covers, on/off and invert switches, and the one being drawn
  lit. An effect's card has a mask button after its name: choose the mask it runs on
  from their previews (or outside it); the same previews when choosing what properties
  “on mask” use.
- **Rotoscoping in the viewer:** a bar over the picture says what to click, counts the
  points and holds the Rotoscope button (and the progress while SAM 2 works); the points
  are clearer dots with + and −. Ctrl+Z takes points back one at a time (Ctrl+Shift+Z puts
  them back), and they stay after a run — undo the result, adjust, and run again. Its
  settings sit in a tidy grid in the Masks tab, which follows compact mode now too.
- **Select all after / before the playhead**, in the timeline's right-click menus.
- **Google Material icons** for the tools and buttons added lately (the arrange buttons,
  Fullscreen, the mask tools), drawn from the built-in icon font.
- **Click the empty space under the tracks** to let go of the selection, as on an empty
  stretch of a track.
- **Intro and outro pickers** list the effects made for intros and outros first, then
  those that ease from their off state.
- **Icons on the properties tabs**; when they don't fit in one row they stack, the open
  tab's row nearest the page.
- **With nothing selected**, the properties panel offers **Import Media**, **Add Title**
  and **Edit Background**.
- **The viewer's backdrop:** a faint grid of dots that moves with the picture as you
  pan and zoom, and around the frame a soft glow and a thin edge, each side in the colors
  near that edge of the picture as shown, slowly taking them in the longer it stays. The
  viewer's toolbar floats over it with no band of its own, and the playback buttons
  sit at the bottom of the viewer.

### Changed
- **The Messages panel** is hidden unless switched on (Settings → Interface).
- **◀ key / key ▶** show only when the clip has keyframes.
- **Speed ramps change the clip's length:** slowing down makes it longer and speeding
  up shorter, so it plays the same part of its file; one that runs into the next clip
  moves up a track. A plain speed change does the same instead of failing.
- **Negative speeds play a clip backwards**, in place of the Reverse switch; the speed
  presets are gone.

### Fixed
- **Pasting** puts clips on another track with room (the nearest, above first) when
  their own track is taken, instead of always adding a new track.
- **Changing a keyframed property adds a key** at the playhead from every control:
  a title's text, bold, italic, alignment and font, text and picture inputs of effects
  — these used to replace the keys with a single value.
- **Keyframing position, scale and the rest of a transform** in a format with its own
  values (a vertical version, say): the diamond keyframed the clip's main value while
  edits went to that format's own, so changes made later didn't animate. Keys now go
  where the edits go.
- **The keyframe diamond** no longer throws a property's keys away when clicked where
  there's no key: it adds one there. On a key it removes that key (the last one leaves
  the value as it was); right-click → **Stop keyframing** removes them all.
- **Keyframe diamonds on the timeline** and the properties panel's ◀ key / key ▶ show
  the keys of the format on screen (a vertical version's own keys too), only those
  inside the clip — key ▶ no longer jumps off the clip to a key a split left past its
  end. The diamonds are bigger, outlined over pictures and waveforms.
- **The media bin's search** is a proper search box (a magnifying glass, ✕ or Esc to
  clear): every word typed has to be somewhere in a file's name or what it is
  ("beach 4k", "wav"), and it searches the Assets tab too.
- **Keyframes on split clips** stick: a split leaves each half's keys where the other
  half carries on, out of sight past its ends, and those kept pulling new keys back
  (a value set late in a clip drifted back by its end). Editing a keyframed property
  now first turns keys outside the clip into keys at its ends — only where the motion
  there changes what's seen.
- **Titles bigger than the frame** (a wall of symbols, say) no longer stop an export with
  "texture … exceeds GPU limits": a title is drawn only where it can be seen, plus room
  for its shadow or blur, and one that can't be trimmed (under a scroll or a warp) is
  drawn at a size the GPU can make and enlarged to its place.

### Faster
- **Frames that are only cut clips** render in far fewer steps: a clip that covers the
  frame hides the blurred-content background under it (it's no longer made), and it's
  copied once instead of once per layer of nesting — 2 GPU passes instead of 7.

## 0.1-beta.6 — Beta 6

### New
- **A clearer update dialog:** "Beta 6 is available!", what's new, and two choices:
  **Restart and Update Now** or **Later**.

- **10-bit video stays 10-bit:** HEVC Main 10, ProRes, AV1 10-bit and other deep footage
  is decoded at its full depth, so gradients and log footage no longer band when graded.
- **Proxies:** 4K and 10-bit footage gets a small copy in the background that the viewer
  plays smoothly (a "P" on its card in the media bin). Switch them off in the viewer's
  toolbar to judge fine detail; exports always use the originals. Right-click a file
  to make or remove one.
- **Export a timeline for another editor** as OpenTimelineIO (`.otio`), for finishing in
  DaVinci Resolve or Kdenlive: clips, cuts, gaps, speed changes, dissolves and compound
  clips carry over.
- **Back after a graphics driver reset:** OpenAtelier restarts by itself and reopens your
  work where you were.
- **Speed ramps:** keyframe a clip's speed (the diamond in its speed row), anywhere from
  0.01× to 100×. The clip keeps its length and plays faster or slower through its file;
  the sound follows the ramp too.
- **New effects:** Motion Blur (smears whatever moves along its path, with a shutter
  angle and samples), Luma Key (key out the dark or bright parts), and Shatter (breaks the
  picture into shards: seed, parts, size, center point, travel distance, shards rotate).
- **Blend is an effect** (Compositing → Blend) instead of a property: add it to a clip
  or an effect container to pick how it mixes with what's under it, keyframe its mode,
  or switch it off. Older projects open with their blend modes turned into the effect.
  New modes that read the picture underneath: **Invert** (text that inverts the video
  behind it), Difference, Exclusion, Overlay, Soft Light, Hard Light, Color Dodge and
  Color Burn. Effects listed after Blend run on what it blended into (inverted text
  that's then blurred or glows, say); those before it, on the clip.
- **Eyedropper:** the pipette beside a color property takes the color from the viewer,
  showing the color under the pointer as you move (picked from the frame without that
  effect or clip, so a key color comes from the footage).
- **Drag pictures in from a browser:** a GIF, picture or video dragged out of a web page
  (or a link to one) comes in like a file; it's saved with the editor's files so the
  project keeps it.
- **Drop files onto the timeline:** they land where you let go, on the track under the
  pointer (a line shows where while you drag); dropped anywhere else, they go into the
  media bin as before.
- **Ctrl+← / Ctrl+→** jump to the start or the end of the selected clip (or the one under
  the playhead); again, to the clip before or after. Nudging a layer in the viewer is now
  Alt+arrows.
- **Pick several tracks:** click a track's header to pick it, Ctrl+click for more,
  Shift+click for a run; Alt+↑/↓ moves them together.
- **A tidier workspace:** the media bin's tabs run down its side; New folder and Import a
  folder share one menu, and Add title is there too; playback sits in the middle under
  the viewer; under the tracks, a bar that stays put holds the buttons that add tracks,
  Captions (when the selection has sound) and Fit. In the
  timeline the wheel scrolls through time, Shift+wheel up and down, Ctrl+wheel zooms.
- **Settings:** a Media page (Images, Videos with proxies, Audio), and snapping and track
  height under Editing → Timeline.
- **Viewer and top bar:** the viewer's Fit is now **Center**; thirds, center lines and
  safe areas are ticked in a **Guides** menu; Fullscreen is an icon on the right; the
  50/100/200% buttons and the Proxies toggle are gone (Ctrl+1 still shows 100%).
  **Vertical layout** sits beside the format menu in a tidier top bar.
- **Pickers:** an effect's picture input opens a searchable grid of the project's
  videos, pictures and compound clips (videos skim under the pointer); sound effects
  are added from cards grouped by what they do (Dynamics, EQ & Tone, Space…); long
  option lists open as a grid.
- **Español:** the whole editor is translated into Spanish. Pick the language on the start
  page or in Settings → Interface. Other languages can be added as a file, no rebuild
  needed.
- **Automatic rotoscoping (SAM 2):** in the Masks tab, pick Rotoscope, click what to
  cut out (Alt-click what to leave out), and it's followed through the clip frame by
  frame. Choose the model (Tiny to Large) and the softness; each model downloads when
  you first use it.
- **Invert** picks its channels: red, green and blue by default, or any mix of red,
  green, blue, magenta, cyan, yellow and alpha.
- **Color To** can include the alpha channel: match on transparency too, and make a
  color see-through.
- **Plugin safety:** a plugin effect that stalls the GPU is turned off before it can
  reset the driver, and plugin shaders have size and pass limits.

### Removed
- **Timeline dividers and sections.** Projects that have them still open; the dividers
  are left out.
- The **Import samples** button.

### Fixed
- **The effect menu** resizes to each tab instead of keeping the last tab's size.
- **Updating on Linux** from the `.deb` installs the new version (your system asks for
  your password) instead of opening the download page.

## 0.1-beta.5 — Beta 5

### New
- **Masks** (turn on under Settings → Masking): rectangle, ellipse, brush and eraser,
  fill, magic select, and bezier paths with per-point keyframes; imported black-and-white
  or see-through pictures as masks; keyframable center, scale, rotation, softness and
  harshness; per-shape expand and feather; add, subtract, intersect and difference
  between masks; masks that follow point tracks; "… on mask" values for opacity,
  position, scale, rotation and squash; "Use with mask" for effects; copy masks between
  clips. Automatic rotoscoping is coming soon.
- **Transitions between clips:** where a clip touches another, the Transitions tab
  switches its Intro/Outro to a transition from or into that clip.
- **X and Y connect to the sound separately** (Position and Scale → Animate).
- **0× speed** holds a clip's first frame as a silent still.
- **Media bin:** real tabs, a **Compounds** tab, and **folder import** (its folders
  become bin folders).
- **Glow:** a softness setting (no more streaks where edge colors meet) and solid color.

### Fixed
- **Transparent video** (MKV, ProRes 4444, VP8/VP9 with alpha) no longer flickers black
  in playback or export, and the layers under it are no longer skipped.
- **MKV** frames no longer repeat.
- **Clicking in the viewer** selects reliably; clicking the same spot again selects the
  next clip down.
- **Compound clips** resize with what's inside them, and their volume, fades and sound
  effects reach the sound inside; sound-only compounds go on audio tracks and show a
  waveform instead of a checkerboard.
- **Icons** in installed builds look as they should.
- **Panel sizes** fit the window when it opens.
- **Update checks** run at every start and every few hours.

### Faster
- Export skips mask work for clips without masks.

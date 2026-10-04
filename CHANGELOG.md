# Changelog

Each release's section becomes its description on GitHub, and the app's update dialog
lists its points. Name a section after the version as it's tagged (`0.1-beta.6`, the
tag without its `v`) before tagging; until then it's **Unreleased**.

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

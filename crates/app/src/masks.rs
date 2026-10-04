//! The Masks tab (Settings → Masking): masks drawn on a clip — rectangles, ellipses,
//! brush strokes, magic selections, fills and imported black-and-white or see-through
//! pictures — that its properties ("Opacity on mask") and effects ("Use with mask") can
//! be limited to. The model is `oa_doc::mask`; rendering is the planner's.
//!
//! While a drawing tool is picked, the viewer's pointer belongs to the mask: the
//! selected clip shows its mask tinted over it, and drags draw into it. A mask's
//! position, scale, turn, softness and harshness are ordinary keyframable properties.
//! A mask copied to a clip of another shape asks whether to fit or crop it.

use crate::i18n::{tr, trf};
use crate::preview_worker::FramePixels;
use crate::App;
use eframe::egui;
use oa_doc::mask::{self, Bitmap, Mask, MaskMode, MaskShape, MaskValues};
use oa_doc::{schema, ItemId, ItemKind, Op, ParamTarget};
use oa_params::{ParamId, ParamSource, Value};
use oa_plan::scene::Placement;
use oa_time::Time;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

/// The longest side of a magic selection, fill or imported picture, in pixels: the
/// clip's own resolution, up to the largest texture every GPU takes.
const BITMAP_SIDE: f64 = 8192.0;

#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum Tool {
    /// The viewer works as usual (the mask still shows).
    #[default]
    Select,
    /// Move and reshape what's drawn: a rectangle's or ellipse's corners, a path's
    /// points and handles.
    Edit,
    Rect,
    Ellipse,
    /// Bezier paths, point by point.
    Pen,
    Brush,
    Eraser,
    Magic,
    Fill,
    /// Click the thing: SAM 2 finds its outline on every frame (`roto.rs`).
    Rotoscope,
}

impl Tool {
    const ALL: [(Tool, &'static str, &'static str); 10] = [
        (Tool::Select, "Select", "The viewer works as usual: select and move clips. The mask stays shown."),
        (Tool::Edit, "Edit", "Drag a rectangle, ellipse or path to move it; drag its corners or points to reshape it. With “Keyframe points” on, a moved path point gets a key at the playhead."),
        (Tool::Rect, "Rectangle", "Drag a rectangle on the clip"),
        (Tool::Ellipse, "Ellipse", "Drag an ellipse on the clip"),
        (Tool::Pen, "Pen", "Click to place points of a path, drag to pull out curve handles; click the first point (or press Enter) to close it"),
        (Tool::Brush, "Brush", "Paint the mask on"),
        (Tool::Eraser, "Eraser", "Paint the mask away"),
        (Tool::Magic, "Magic select", "Click a color in the clip: everything of about that color is selected"),
        (Tool::Fill, "Fill", "Click inside an outline drawn in the mask to fill it (or click the mask to empty that part, with Erase on)"),
        (Tool::Rotoscope, "Rotoscope", "Click what to cut out on one frame (Alt-click what to leave out): SAM 2 finds its outline on every frame"),
    ];
}

/// How an imported picture becomes a mask.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum ImportMode {
    /// White is in, black is out.
    #[default]
    Brightness,
    /// Opaque is in, see-through is out.
    Opacity,
}

/// The tab's state: which mask, which tool, the tools' settings, and work in progress.
pub struct Masks {
    pub selected: Option<u64>,
    pub tool: Tool,
    /// Rectangles, ellipses, magic selections and fills take away instead of adding.
    pub erase: bool,
    /// Brush radius, as a share of the clip's height.
    pub brush: f64,
    /// The share of the brush's radius that fades out.
    pub brush_softness: f64,
    /// Magic select: how different a color may be (0..1).
    pub tolerance: f64,
    /// Magic select: only what touches the clicked pixel (or every such color).
    pub contiguous: bool,
    pub import_mode: ImportMode,
    /// The shape picked in the Edit tool (or the path the Pen is drawing), by index.
    pub shape: Option<usize>,
    /// The picked point of a path.
    pub point: Option<usize>,
    /// Moving a path point keys it at the playhead (rotoscoping), rather than moving it
    /// for the whole clip.
    pub animate_points: bool,
    /// The two point tracks a mask's scale and rotation are linked to (their ids).
    pub pair: [Option<u64>; 2],
    pub(crate) drag: Option<Drag>,
    /// The tinted picture of the mask shown over the clip, and what it was made from.
    pub(crate) overlay: Option<(u64, egui::TextureHandle)>,
    clipboard: Option<Copied>,
    /// A paste waiting for "fit or crop?".
    paste: Option<Vec<ItemId>>,
    pub(crate) job: Option<Job>,
    /// The Rotoscope tool (`roto.rs`).
    pub roto: crate::roto::RotoState,
}

impl Default for Masks {
    fn default() -> Self {
        Masks {
            selected: None,
            tool: Tool::Select,
            erase: false,
            brush: 0.04,
            brush_softness: 0.3,
            tolerance: 0.12,
            contiguous: true,
            import_mode: ImportMode::Brightness,
            shape: None,
            point: None,
            animate_points: false,
            pair: [None, None],
            drag: None,
            overlay: None,
            clipboard: None,
            paste: None,
            job: None,
            roto: Default::default(),
        }
    }
}

/// A drag drawing into a mask: where it started (drawing fractions), which shape it's
/// making or changing (once there is one), and — reshaping — what it grabbed and the
/// shape as it was.
pub(crate) struct Drag {
    pub item: ItemId,
    pub mask: u64,
    pub start: [f64; 2],
    pub shape: Option<usize>,
    pub grab: crate::mask_draw::Grab,
    pub before: Option<MaskShape>,
}

/// A copied mask: the drawing, its values (keyframes too) and the shape of the clip it
/// was drawn on (width ÷ height).
struct Copied {
    mask: Mask,
    params: Vec<(String, ParamSource)>,
    aspect: f64,
}

/// A magic selection: at `click` (drawing fractions) in `mask` on `item`, placed at
/// `place` with `values` at the time, the drawing stretched by `frame`.
pub(crate) struct Magic {
    pub item: ItemId,
    pub mask: u64,
    pub click: [f64; 2],
    pub erase: bool,
    pub place: Placement,
    pub values: MaskValues,
    pub frame: [f64; 2],
}

/// A picture being rendered for a tool.
pub(crate) enum Job {
    /// The clip on its own, in its own pixels, for a magic selection.
    Magic { rx: Receiver<Option<FramePixels>>, magic: Box<Magic> },
    /// A picture stretched over the clip, for its brightness or opacity.
    Import { rx: Receiver<Option<FramePixels>>, item: ItemId, mask: u64, mode: ImportMode },
}

/// The size a pixel mask is made at for a layer of `native` px: its own.
pub(crate) fn bitmap_size(native: [f64; 2]) -> [u32; 2] {
    let k = (BITMAP_SIDE / native[0].max(native[1]).max(1.0)).min(1.0);
    [(native[0] * k).round().max(1.0) as u32, (native[1] * k).round().max(1.0) as u32]
}

/// Drawing fractions → the layer's fractions before the mask's own move (the frame's
/// stretch applied), and back.
pub(crate) fn framed(d: [f64; 2], frame: [f64; 2]) -> [f64; 2] {
    [(d[0] - 0.5) * frame[0] + 0.5, (d[1] - 0.5) * frame[1] + 0.5]
}

pub(crate) fn unframed(f: [f64; 2], frame: [f64; 2]) -> [f64; 2] {
    [(f[0] - 0.5) / frame[0] + 0.5, (f[1] - 0.5) / frame[1] + 0.5]
}

/// Where canvas point `c` is on `mask`'s drawing (fractions), on a layer placed at `p`.
pub(crate) fn drawing_point(p: &Placement, v: &MaskValues, frame: [f64; 2], c: [f64; 2]) -> Option<[f64; 2]> {
    let l = p.to_layer_fraction(c)?;
    Some(unframed(v.unplace(l, p.native), frame))
}

/// Layer-space coverage (as the mask rasterizes, `size` px) resampled into drawing
/// space, where bitmaps are stored (the same unless the mask was refitted).
pub(crate) fn to_drawing(coverage: &[u8], size: [u32; 2], frame: [f64; 2]) -> Vec<u8> {
    if frame == [1.0, 1.0] {
        return coverage.to_vec();
    }
    let (w, h) = (size[0] as usize, size[1] as usize);
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let f = framed([(x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64], frame);
            let (sx, sy) = ((f[0] * w as f64).floor(), (f[1] * h as f64).floor());
            if sx >= 0.0 && sy >= 0.0 && (sx as usize) < w && (sy as usize) < h {
                out[y * w + x] = coverage[sy as usize * w + sx as usize];
            }
        }
    }
    out
}

impl App {
    fn masks_of(&self, item: ItemId) -> Vec<Mask> {
        self.editor.item(item).map(|i| i.masks.clone()).unwrap_or_default()
    }

    /// The clip's place at the playhead (its drawing's pixels are its own).
    pub(crate) fn mask_placement(&self, item: ItemId, t: Time) -> Option<Placement> {
        oa_edit::transform::placement_of(self.editor.doc.project(), self.editor.seq, self.variant_id(), item, t).ok()
    }

    /// Changes one mask (as one undo step, or a coalesced one while dragging).
    pub(crate) fn edit_mask(&mut self, item: ItemId, id: u64, label: &str, drag: Option<&str>, change: impl FnOnce(&mut Mask)) {
        let mut masks = self.masks_of(item);
        let Some(m) = masks.iter_mut().find(|m| m.id == id) else { return };
        change(m);
        let ops = vec![Op::SetMasks { seq: self.editor.seq, item, masks }];
        let result = match drag {
            Some(key) => self.editor.apply_drag(label, key, ops),
            None => self.editor.apply(label, ops),
        };
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
    }

    pub(crate) fn add_shape(&mut self, item: ItemId, id: u64, shape: MaskShape, label: &str) {
        self.edit_mask(item, id, label, None, |m| m.shapes.push(shape));
    }

    /// Whether masks can go on this clip: something with a picture.
    pub(crate) fn maskable(&self, item: ItemId) -> bool {
        self.editor.item(item).is_some_and(|i| match i.kind {
            ItemKind::Media { .. } => self.is_visual(item),
            ItemKind::Solid | ItemKind::Text | ItemKind::Nested { .. } => true,
            _ => false,
        })
    }

    // ---- the tab ----

    pub(crate) fn masks_tab(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        self.poll_mask_job();
        let Some(it) = self.editor.item(item).cloned() else { return };
        ui.label(
            egui::RichText::new(tr("Masks are drawn on the clip and move with it. Right-click a property (Opacity…) → With mask, or an effect's name → Use with mask. Bounded text effects can't be masked."))
                .small()
                .weak(),
        );
        ui.add_space(crate::style::GAP_S);
        if self.masks.selected.is_none_or(|s| !it.masks.iter().any(|m| m.id == s)) {
            self.masks.selected = it.masks.first().map(|m| m.id);
        }
        ui.horizontal(|ui| {
            if ui.button(tr("+ New mask")).on_hover_text(tr("An empty mask; draw it with the tools below")).clicked() {
                let id = self.editor.doc.alloc_id();
                let n = it.masks.len() + 1;
                let mut masks = it.masks.clone();
                masks.push(Mask { id, name: format!("Mask {n}"), enabled: true, invert: false, mode: MaskMode::Add, shapes: Vec::new(), frame: [1.0, 1.0] });
                self.apply_or_report("New mask", vec![Op::SetMasks { seq: self.editor.seq, item, masks }]);
                self.masks.selected = Some(id);
                if self.masks.tool == Tool::Select {
                    self.masks.tool = Tool::Rect;
                }
            }
            let pastable = self.masks.clipboard.is_some();
            if ui.add_enabled(pastable, egui::Button::new(tr("Paste mask"))).on_hover_text(tr("The copied mask, onto the selected clips")).clicked() {
                let targets = self.selected_clips();
                self.paste_mask(targets);
            }
        });
        let mut remove = None;
        let mut copy = None;
        for m in &it.masks {
            ui.horizontal(|ui| {
                let mut on = m.enabled;
                if ui.checkbox(&mut on, tr("")).on_hover_text(if on { "Turn off" } else { "Turn on" }).changed() {
                    self.edit_mask(item, m.id, "Toggle mask", None, |m| m.enabled = on);
                }
                let chosen = self.masks.selected == Some(m.id);
                let mut name = m.name.clone();
                if chosen {
                    let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(120.0));
                    if r.changed() {
                        self.edit_mask(item, m.id, "Rename mask", Some("mask-rename"), |m| m.name = name);
                    }
                    if r.lost_focus() {
                        self.editor.doc.seal();
                    }
                } else if ui.selectable_label(false, &m.name).clicked() {
                    self.masks.selected = Some(m.id);
                }
                let mut invert = m.invert;
                if ui.checkbox(&mut invert, tr("invert")).on_hover_text(tr("Everything outside what's drawn instead")).changed() {
                    self.edit_mask(item, m.id, "Invert mask", None, |m| m.invert = invert);
                }
                // How it joins the masks above it (the first has none above).
                if it.masks.first().map(|f| f.id) != Some(m.id) || m.mode != MaskMode::Add {
                    let mut mode = m.mode;
                    egui::ComboBox::from_id_salt(("mask-mode", m.id)).width(84.0).selected_text(tr(mode.name())).show_ui(ui, |ui| {
                        for option in MaskMode::ALL {
                            ui.selectable_value(&mut mode, option, tr(option.name()));
                        }
                    })
                    .response
                    .on_hover_text(tr("How it joins the masks above it where several are used together: added, taken away, only where both are, or where just one is"));
                    if mode != m.mode {
                        self.edit_mask(item, m.id, "Mask mode", None, |m| m.mode = mode);
                    }
                }
                ui.label(egui::RichText::new(trf("{0} shapes", &[("0", &(m.shapes.len()).to_string())])).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("✕")).on_hover_text(tr("Delete this mask")).clicked() {
                        remove = Some(m.id);
                    }
                    if ui.small_button(tr("Copy")).on_hover_text(tr("Copy this mask (and its keyframes) to paste on other clips")).clicked() {
                        copy = Some(m.id);
                    }
                });
            });
        }
        if let Some(id) = copy {
            self.copy_mask(item, id);
        }
        if let Some(id) = remove {
            self.delete_mask(item, id);
        }
        let Some(id) = self.masks.selected.filter(|s| it.masks.iter().any(|m| m.id == *s)) else {
            if it.masks.is_empty() {
                ui.label(egui::RichText::new(tr("No masks yet.")).weak());
            }
            return;
        };
        let native = self.mask_placement(item, t).map_or([1920.0, 1080.0], |p| p.native);

        crate::inspector::section(ui, tr("Draw"), tr("Pick a tool, then draw on the clip in the viewer."));
        ui.horizontal_wrapped(|ui| {
            for (tool, name, tip) in Tool::ALL {
                if ui.selectable_label(self.masks.tool == tool, tr(name)).on_hover_text(tr(tip)).clicked() {
                    self.masks.tool = tool;
                }
            }
        });
        match self.masks.tool {
            Tool::Rect | Tool::Ellipse | Tool::Magic | Tool::Fill => {
                ui.checkbox(&mut self.masks.erase, tr("Erase")).on_hover_text(tr("Take this away from the mask instead of adding it (or hold Alt while drawing)"));
            }
            Tool::Rotoscope => self.roto_panel(ui, item, id, native),
            _ => {}
        }
        if matches!(self.masks.tool, Tool::Brush | Tool::Eraser) {
            let mut px = self.masks.brush * native[1];
            ui.add(egui::Slider::new(&mut px, 1.0..=(native[1] * 0.5).max(2.0)).logarithmic(true).text(tr("brush size")).suffix(" px"));
            self.masks.brush = (px / native[1].max(1.0)).max(1e-4);
            ui.add(egui::Slider::new(&mut self.masks.brush_softness, 0.0..=1.0).text(tr("brush softness")));
        }
        if self.masks.tool == Tool::Magic {
            ui.add(egui::Slider::new(&mut self.masks.tolerance, 0.0..=1.0).text(tr("tolerance")));
            ui.checkbox(&mut self.masks.contiguous, tr("Only what touches the clicked color")).on_hover_text(tr("Off: that color everywhere in the clip"));
            if matches!(self.masks.job, Some(Job::Magic { .. })) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Selecting…"));
                });
            }
        }
        ui.horizontal(|ui| {
            let options: Vec<(oa_doc::MediaId, String)> =
                self.editor.pool.iter().filter(|m| !m.missing && m.probe.video.is_some()).map(|m| (m.id, m.name.clone())).collect();
            ui.menu_button(tr("Import picture…"), |ui| {
                ui.label(egui::RichText::new(tr("Stretched over the clip, added to the mask:")).small().weak());
                ui.radio_value(&mut self.masks.import_mode, ImportMode::Brightness, tr("Brightness (white in, black out)"));
                ui.radio_value(&mut self.masks.import_mode, ImportMode::Opacity, tr("Opacity (opaque in, see-through out)"));
                ui.separator();
                if options.is_empty() {
                    ui.label(egui::RichText::new(tr("Import a picture into the media bin first.")).weak());
                }
                for (media, name) in &options {
                    if ui.button(name).clicked() {
                        self.import_mask_picture(item, id, *media);
                        ui.close();
                    }
                }
            });
            if matches!(self.masks.job, Some(Job::Import { .. })) {
                ui.spinner();
            }
            if ui.button(tr("Clear drawing")).on_hover_text(tr("Everything drawn in this mask (its settings stay)")).clicked() {
                self.edit_mask(item, id, "Clear mask", None, |m| m.shapes.clear());
            }
            if ui.button(tr("Undo last shape")).clicked() {
                self.edit_mask(item, id, "Remove shape", None, |m| {
                    m.shapes.pop();
                });
            }
        });

        self.mask_shapes_section(ui, item, id, native, t);

        crate::inspector::section(ui, tr("Mask"), tr("Where it sits, how big, turned, how soft its edge and how strongly it applies — all keyframable."));
        for s in mask::params(id) {
            self.param_widget(ui, item, &ParamTarget::Item, &s, t, "mask");
        }
        self.mask_tracking_section(ui, item, id, native, t);

        crate::inspector::section(ui, tr("Used by"), tr("What this clip's masks change."));
        let ctx = it.eval_context(t);
        let props = it.params.get(mask::PROPS_USE).and_then(|s| s.eval(&ctx).as_float()).unwrap_or(mask::ALL);
        let name_of = |choice: f64| -> String {
            if choice == mask::ALL {
                return "every mask".into();
            }
            it.masks.iter().find(|m| m.id as f64 == choice).map_or("every mask".into(), |m| m.name.clone())
        };
        ui.horizontal(|ui| {
            ui.label(tr("Properties “on mask” use"));
            let mut chosen = props;
            egui::ComboBox::from_id_salt(("mask-props", item.0)).selected_text(name_of(props)).show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, mask::ALL, tr("every mask"));
                for m in &it.masks {
                    ui.selectable_value(&mut chosen, m.id as f64, &m.name);
                }
            });
            if chosen != props {
                self.editor.set_param(item, ParamTarget::Item, mask::PROPS_USE, ParamSource::Static(Value::Float(chosen)), mask::PROPS_USE);
                self.editor.doc.seal();
            }
        });
        let on_mask: Vec<&str> = mask::MASKABLE.into_iter().filter(|p| it.params.get(&mask::on_mask(p)).is_some()).collect();
        let effects: Vec<String> = it
            .effects
            .iter()
            .filter(|e| e.params.get(mask::EFFECT_USE).and_then(|s| s.eval(&ctx).as_float()).is_some_and(|v| v == mask::ALL || v > 0.0))
            .map(|e| self.registry.effect(&e.type_id).map_or(e.type_id.clone(), |d| d.name.clone()))
            .collect();
        let pretty = |p: &str| p.rsplit('.').next().unwrap_or(p).to_string();
        let list = on_mask.iter().map(|p| format!("{} on mask", pretty(p))).chain(effects).collect::<Vec<_>>();
        ui.label(egui::RichText::new(if list.is_empty() { "Nothing yet.".to_string() } else { list.join(" · ") }).small().weak());
    }

    /// Removes a mask, its values, and whatever used it (those go back to the whole clip).
    fn delete_mask(&mut self, item: ItemId, id: u64) {
        let Some(it) = self.editor.item(item).cloned() else { return };
        let seq = self.editor.seq;
        let mut ops = vec![Op::SetMasks { seq, item, masks: it.masks.iter().filter(|m| m.id != id).cloned().collect() }];
        let prefix = format!("mask.{id}.");
        for p in it.params.0.keys().filter(|p| p.as_str().starts_with(&prefix)) {
            ops.push(Op::SetParam { seq, item, target: ParamTarget::Item, param: p.clone(), source: None });
        }
        let names = |params: &oa_params::ParamSet, key: &str| params.get(key).and_then(|s| s.eval(&it.eval_context(it.range.start)).as_float()) == Some(id as f64);
        if names(&it.params, mask::PROPS_USE) {
            ops.push(Op::SetParam { seq, item, target: ParamTarget::Item, param: ParamId::new(mask::PROPS_USE), source: None });
        }
        for fx in it.effects.iter().filter(|fx| names(&fx.params, mask::EFFECT_USE)) {
            ops.push(Op::SetParam { seq, item, target: ParamTarget::Effect(fx.id), param: ParamId::new(mask::EFFECT_USE), source: None });
        }
        self.apply_or_report("Delete mask", ops);
    }

    fn copy_mask(&mut self, item: ItemId, id: u64) {
        let Some(it) = self.editor.item(item) else { return };
        let Some(m) = it.masks.iter().find(|m| m.id == id).cloned() else { return };
        let prefix = format!("mask.{id}.");
        let params = it.params.0.iter().filter_map(|(k, v)| Some((k.as_str().strip_prefix(&prefix)?.to_string(), v.clone()))).collect();
        let aspect = self.mask_placement(item, it.range.start).map_or(16.0 / 9.0, |p| p.native[0] / p.native[1].max(1.0));
        self.masks.clipboard = Some(Copied { mask: m, params, aspect });
        self.notify(tr("Mask copied — select other clips and press Paste mask"));
    }

    /// Pastes the copied mask onto `targets`, asking first when any is another shape.
    fn paste_mask(&mut self, targets: Vec<ItemId>) {
        let Some(c) = &self.masks.clipboard else { return };
        let aspect = c.aspect;
        let targets: Vec<ItemId> = targets.into_iter().filter(|t| self.maskable(*t)).collect();
        let differs = targets.iter().any(|t| {
            let it = self.editor.item(*t);
            let a = it.and_then(|i| self.mask_placement(*t, i.range.start)).map_or(aspect, |p| p.native[0] / p.native[1].max(1.0));
            (a / aspect - 1.0).abs() > 0.005
        });
        if differs {
            self.masks.paste = Some(targets);
        } else {
            self.paste_mask_as(&targets, true);
        }
    }

    fn paste_mask_as(&mut self, targets: &[ItemId], fit: bool) {
        let Some(c) = self.masks.clipboard.as_ref().map(|c| (c.mask.clone(), c.params.clone(), c.aspect)) else { return };
        let (copied, params, aspect) = c;
        let seq = self.editor.seq;
        let mut ops = Vec::new();
        let mut last = None;
        for &target in targets {
            let Some(it) = self.editor.item(target).cloned() else { continue };
            let to = self.mask_placement(target, it.range.start).map_or(aspect, |p| p.native[0] / p.native[1].max(1.0));
            let id = self.editor.doc.alloc_id();
            let mut m = copied.clone();
            m.id = id;
            m.refit(aspect, to, fit);
            let mut masks = it.masks.clone();
            masks.push(m);
            ops.push(Op::SetMasks { seq, item: target, masks });
            for (name, source) in &params {
                ops.push(Op::SetParam { seq, item: target, target: ParamTarget::Item, param: ParamId::new(&mask::param_id(id, name)), source: Some(source.clone()) });
            }
            if Some(target) == self.selection {
                last = Some(id);
            }
        }
        self.apply_or_report("Paste mask", ops);
        if let Some(id) = last {
            self.masks.selected = Some(id);
        }
    }

    /// "Fit or crop?" for a mask pasted onto a clip of another shape.
    pub(crate) fn mask_paste_modal(&mut self, ctx: &egui::Context) {
        let Some(targets) = self.masks.paste.clone() else { return };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("mask-paste")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(tr("A different shape"));
            ui.label(tr("The mask was drawn on a clip of another shape. How should it go on?"));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Fit")).on_hover_text(tr("All of the mask shows, its shapes kept; the clip's extra room stays empty")).clicked() {
                    choice = Some(Some(true));
                }
                if ui.button(tr("Crop")).on_hover_text(tr("The mask covers the whole clip, its shapes kept; what doesn't fit is cut off")).clicked() {
                    choice = Some(Some(false));
                }
                if ui.button(tr("Cancel")).clicked() {
                    choice = Some(None);
                }
            });
        });
        if let Some(choice) = choice {
            self.masks.paste = None;
            if let Some(fit) = choice {
                self.paste_mask_as(&targets, fit);
            }
        }
    }

    // ---- tools that need the picture ----

    /// Renders the clip on its own, unmoved and at its own resolution (a canvas its
    /// size, the clip stretched over it, its masks, moves and fades off), so a magic
    /// selection reads its colors pixel for pixel — wherever it sits on the canvas.
    pub(crate) fn start_magic(&mut self, magic: Magic) {
        let Some(it) = self.editor.item(magic.item).cloned() else { return };
        let size = bitmap_size(magic.place.native);
        let mut project = (*self.editor.doc.snapshot()).clone();
        let (seq_id, vid) = (oa_doc::SeqId(u64::MAX - 19), oa_doc::VariantId(u64::MAX - 19));
        let v = oa_doc::FormatVariant { id: vid, name: "clip".into(), size: oa_doc::CanvasSize::new(size[0], size[1]), overrides: Default::default() };
        let mut s = oa_doc::Sequence::new(seq_id, "clip", self.editor.sequence().rate, v);
        let mut track = oa_doc::Track::new(oa_doc::TrackId(u64::MAX - 19), "V", oa_doc::TrackKind::Video);
        let mut clip = it.clone();
        clip.range.start = Time::ZERO;
        clip.masks.clear();
        (clip.transition_in, clip.transition_out) = (None, None);
        // Its own picture: nothing that moves, turns or fades it, nothing inside masks.
        for p in [schema::POSITION, schema::SCALE, schema::SQUASH, schema::ROTATION, schema::ANCHOR, schema::OPACITY] {
            clip.params.0.remove(&ParamId::new(p));
            clip.params.0.remove(&ParamId::new(&mask::on_mask(p)));
        }
        clip.params.set(schema::FIT, ParamSource::Static(Value::Enum(oa_doc::FitMode::Stretch.as_str().into())));
        clip.effects.retain(|e| e.type_id != oa_graph::registry::BLEND && self.registry.effect(&e.type_id).is_none_or(|d| d.kind != oa_graph::EffectKind::Motion));
        track.items.push(clip);
        s.tracks.push(Arc::new(track));
        project.sequences.insert(seq_id, Arc::new(s));
        let (tx, rx) = std::sync::mpsc::channel();
        self.preview_worker.pixels(
            crate::preview_worker::Request {
                slot: 0,
                tag: 0,
                project: Arc::new(project),
                registry: self.registry.clone(),
                seq: seq_id,
                variant: vid,
                at: self.playhead - it.range.start,
                scale: 1.0,
                wanted: None,
                png: None,
                see_through: true,
            },
            tx,
        );
        self.masks.job = Some(Job::Magic { rx, magic: Box::new(magic) });
    }

    /// Renders a picture from the bin stretched to the clip's shape, to add to the mask.
    fn import_mask_picture(&mut self, item: ItemId, id: u64, media: oa_doc::MediaId) {
        let Some(place) = self.mask_placement(item, self.playhead) else { return };
        let size = bitmap_size(place.native);
        let mut project = (*self.editor.doc.snapshot()).clone();
        let (seq_id, vid) = (oa_doc::SeqId(u64::MAX - 17), oa_doc::VariantId(u64::MAX - 17));
        let v = oa_doc::FormatVariant { id: vid, name: "mask".into(), size: oa_doc::CanvasSize::new(size[0], size[1]), overrides: Default::default() };
        let mut s = oa_doc::Sequence::new(seq_id, "mask", oa_time::FrameRate::FPS_30, v);
        let mut track = oa_doc::Track::new(oa_doc::TrackId(u64::MAX - 17), "V", oa_doc::TrackKind::Video);
        let mut clip = oa_doc::Item::new(ItemId(u64::MAX - 17), "picture", ItemKind::Media { media }, oa_time::TimeRange::new(Time::ZERO, Time::from_seconds(1)));
        clip.params.set(schema::FIT, ParamSource::Static(Value::Enum(oa_doc::FitMode::Stretch.as_str().into())));
        track.items.push(clip);
        s.tracks.push(Arc::new(track));
        project.sequences.insert(seq_id, Arc::new(s));
        let (tx, rx) = std::sync::mpsc::channel();
        self.preview_worker.pixels(
            crate::preview_worker::Request {
                slot: 0,
                tag: 0,
                project: Arc::new(project),
                registry: self.registry.clone(),
                seq: seq_id,
                variant: vid,
                at: Time::ZERO,
                scale: 1.0,
                wanted: None,
                png: None,
                see_through: true,
            },
            tx,
        );
        self.masks.job = Some(Job::Import { rx, item, mask: id, mode: self.masks.import_mode });
    }

    /// A finished picture for a tool: the selection or the imported picture goes into
    /// the mask.
    pub(crate) fn poll_mask_job(&mut self) {
        let done = match &self.masks.job {
            Some(Job::Magic { rx, .. } | Job::Import { rx, .. }) => match rx.try_recv() {
                Ok(frame) => Some(frame),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(_) => Some(None),
            },
            None => None,
        };
        let Some(frame) = done else { return };
        let job = self.masks.job.take().expect("checked");
        let Some(frame) = frame else {
            self.error = Some(tr("Couldn't read the picture for the mask").into());
            return;
        };
        match job {
            Job::Magic { magic, .. } => {
                let Magic { item, mask: id, click, erase, place, values, frame: stretch } = *magic;
                let size = bitmap_size(place.native);
                let (w, h) = (size[0] as usize, size[1] as usize);
                let [fw, fh] = frame.size;
                // The clip's picture (already in its own pixels), pixel by pixel of the
                // drawing — the same pixels unless the mask was moved or refitted.
                let mut rgba = vec![0u8; w * h * 4];
                for y in 0..h {
                    for x in 0..w {
                        let f = framed([(x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64], stretch);
                        let l = values.place(f, place.native);
                        let (sx, sy) = ((l[0] * fw as f64).floor(), (l[1] * fh as f64).floor());
                        if sx >= 0.0 && sy >= 0.0 && (sx as usize) < fw && (sy as usize) < fh {
                            let s = (sy as usize * fw + sx as usize) * 4;
                            rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&frame.pixels[s..s + 4]);
                        }
                    }
                }
                let at = [((click[0] * w as f64) as usize).min(w - 1), ((click[1] * h as f64) as usize).min(h - 1)];
                let selected = mask::magic_select(&rgba, [w, h], at, self.masks.tolerance, self.masks.contiguous);
                if selected.iter().any(|c| *c > 0) {
                    self.add_shape(item, id, MaskShape::Bitmap { bitmap: Bitmap::encode(size, &selected), expand: 0.0, feather: 0.0, erase }, "Magic select");
                }
            }
            Job::Import { item, mask: id, mode, .. } => {
                let [w, h] = frame.size;
                let coverage: Vec<u8> = frame
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| match mode {
                        ImportMode::Opacity => p[3],
                        ImportMode::Brightness => ((0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64) * p[3] as f64 / 255.0).round() as u8,
                    })
                    .collect();
                if coverage.len() == w * h {
                    self.add_shape(item, id, MaskShape::Bitmap { bitmap: Bitmap::encode([w as u32, h as u32], &coverage), expand: 0.0, feather: 0.0, erase: false }, tr("Import mask picture"));
                }
            }
        }
    }

    // ---- tracking ----

    /// The mask following the point tracker: its center on one tracked point, its scale
    /// and rotation on two (how far apart they are, which way the line between them
    /// points).
    fn mask_tracking_section(&mut self, ui: &mut egui::Ui, item: ItemId, id: u64, native: [f64; 2], t: Time) {
        crate::inspector::section(ui, tr("Tracking"), tr("Make the mask follow something in the picture: its center one tracked point, its scale and rotation two."));
        let center = mask::param_id(id, mask::CENTER);
        let following = self.editor.param_source(item, &ParamTarget::Item, &center).and_then(|s| s.find_track().map(|(tr, _)| tr.name.clone()));
        ui.horizontal(|ui| {
            let label = if following.is_some() { tr("Edit center track…") } else { "Track center…" };
            if ui.button(label).on_hover_text(tr("The mask's center follows a point in the picture: have the tracker follow it, click it in by hand, or record it with the mouse")).clicked() {
                self.edit_track(item, ParamTarget::Item, &center, crate::tracks::Purpose::Track);
            }
            if let Some(name) = following {
                ui.label(egui::RichText::new(trf("follows “{name}”", &[("name", &name.to_string())])).small().color(crate::style::GOLD));
            }
        });
        let tracks: Vec<(u64, String)> = self.editor.doc.project().tracks.values().map(|tr| (tr.id, tr.name.clone())).collect();
        if tracks.len() < 2 {
            ui.label(egui::RichText::new(tr("Scale and rotation follow two tracked points on the clip: track two points first (Track center… makes one).")).small().weak());
            return;
        }
        let name = |id: Option<u64>| id.and_then(|id| tracks.iter().find(|t| t.0 == id)).map_or("pick a track".to_string(), |t| t.1.clone());
        ui.horizontal(|ui| {
            ui.label(tr("Points"));
            for (k, salt) in [(0, "pair-a"), (1, "pair-b")] {
                let mut chosen = self.masks.pair[k];
                egui::ComboBox::from_id_salt((salt, id)).width(110.0).selected_text(name(chosen)).show_ui(ui, |ui| {
                    for (tid, tname) in &tracks {
                        ui.selectable_value(&mut chosen, Some(*tid), tname);
                    }
                });
                self.masks.pair[k] = chosen;
            }
        });
        let Some(it) = self.editor.item(item).cloned() else { return };
        for (param, measure, what, default) in [(mask::SCALE, oa_params::PairMeasure::Scale, "scale", 1.0), (mask::ROTATION, oa_params::PairMeasure::Rotation, "rotation", 0.0)] {
            let pid = mask::param_id(id, param);
            let source = self.editor.param_source(item, &ParamTarget::Item, &pid);
            let linked = source.as_ref().and_then(|s| s.track_pair()).map(|(a, b)| format!("{} – {}", a.name, b.name));
            ui.horizontal(|ui| match (&linked, &source) {
                (Some(names), Some(source)) => {
                    ui.label(egui::RichText::new(trf("{what} follows {names}", &[("what", what), ("names", &names.to_string())])).small().color(crate::style::GOLD));
                    if ui.small_button(tr("Unlink")).on_hover_text(tr("Back to its own value (and keyframes)")).clicked() {
                        self.editor.set_param(item, ParamTarget::Item, &pid, source.clone().without_track_pair(), &pid);
                        self.editor.doc.seal();
                    }
                }
                _ => {
                    let [a, b] = self.masks.pair;
                    let ready = a.is_some() && b.is_some() && a != b;
                    let tip = trf("From here on the {what} changes as the two points move apart or turn, from its value now", &[("what", (what))]);
                    if ui.add_enabled(ready, egui::Button::new(trf("Link {what}", &[("what", what)]))).on_hover_text(tip).on_disabled_hover_text(tr("Pick two different tracks")).clicked() {
                        let project = self.editor.doc.project();
                        let (Some(a), Some(b)) = (a.and_then(|a| project.tracks.get(&a).cloned()), b.and_then(|b| project.tracks.get(&b).cloned())) else { return };
                        let aspect = native[0] / native[1].max(1.0);
                        match oa_params::Modulator::track_pair(a, b, it.range.start, measure, aspect, t) {
                            Some(pair) => {
                                let base = source.clone().unwrap_or(ParamSource::Static(Value::Float(default)));
                                self.editor.set_param(item, ParamTarget::Item, &pid, base.with_track_pair(pair), &pid);
                                self.editor.doc.seal();
                            }
                            None => self.error = Some(tr("The two tracks need points at the playhead, apart from each other").into()),
                        }
                    }
                }
            });
        }
    }

    // ---- menus elsewhere ----

    /// The "With mask" entry of a clip property's menu (Opacity, Position, Scale,
    /// Rotation, Squash): a second value used inside the clip's masks, shown under it.
    pub(crate) fn mask_entry(&mut self, ui: &mut egui::Ui, item: ItemId, target: &ParamTarget, param: &str, default: &Value, t: Time) {
        if !self.settings.masking || *target != ParamTarget::Item {
            return;
        }
        if let Some(base) = param.strip_suffix(mask::ON_MASK_SUFFIX) {
            if mask::MASKABLE.contains(&base) {
                ui.separator();
                if ui.button(tr("Remove with mask")).on_hover_text(tr("The same value inside the mask as outside")).clicked() {
                    self.remove_on_mask(item, param);
                    ui.close();
                }
            }
            return;
        }
        if !mask::MASKABLE.contains(&param) || !self.maskable(item) {
            return;
        }
        ui.separator();
        let key = mask::on_mask(param);
        let has_masks = self.editor.item(item).is_some_and(|i| !i.masks.is_empty());
        if self.editor.param_source(item, target, &key).is_none() {
            let r = ui.add_enabled(has_masks, egui::Button::new(tr("With mask")));
            let r = r.on_hover_text(tr("Give it its own value inside the clip's masks, set just below (keyframable)")).on_disabled_hover_text(tr("Draw a mask in the Masks tab first"));
            if r.clicked() {
                let now = self.editor.param_value(item, target, param, t).unwrap_or_else(|| default.clone());
                self.editor.set_param(item, ParamTarget::Item, &key, ParamSource::Static(now), &key);
                self.editor.doc.seal();
                ui.close();
            }
        } else if ui.button(tr("Remove with mask")).clicked() {
            self.remove_on_mask(item, &key);
            ui.close();
        }
    }

    /// Under a Transform property with a value inside the mask: that value, keyframable
    /// like the property (a row of the Transform grid).
    pub(crate) fn on_mask_row(&mut self, ui: &mut egui::Ui, item: ItemId, param: &str, t: Time) {
        let key = mask::on_mask(param);
        if self.editor.param_source(item, &ParamTarget::Item, &key).is_none() {
            return;
        }
        let Some(mut s) = schema::visual().iter().find(|s| s.id.as_str() == param).cloned() else { return };
        s.id = ParamId::new(&key);
        ui.label(tr(""));
        ui.label(egui::RichText::new(tr("↳ mask")).color(crate::style::ACCENT)).on_hover_text(tr("Its value inside the clip's masks — right-click the value to remove it"));
        ui.vertical(|ui| self.param_widget(ui, item, &ParamTarget::Item, &s, t, "on-mask"));
        ui.end_row();
    }

    fn remove_on_mask(&mut self, item: ItemId, key: &str) {
        let seq = self.editor.seq;
        let ops = std::iter::once(item)
            .chain(self.editor.linked.iter().copied().filter(|o| *o != item))
            .filter(|i| self.editor.param_source(*i, &ParamTarget::Item, key).is_some())
            .map(|item| Op::SetParam { seq, item, target: ParamTarget::Item, param: ParamId::new(key), source: None })
            .collect();
        self.apply_or_report(tr("Remove with mask"), ops);
    }

    /// "Use with mask" in an effect's menu: run it inside one of the clip's masks (or all
    /// of them), or outside. Not for bounded or per-letter text effects.
    pub(crate) fn effect_mask_menu(&mut self, ui: &mut egui::Ui, item: ItemId, fx: &oa_doc::EffectInstance) {
        if !self.settings.masking || !self.maskable(item) {
            return;
        }
        let Some(it) = self.editor.item(item).cloned() else { return };
        let at = oa_params::EvalContext::at(Time::ZERO, Time::ZERO);
        let bounded = matches!(fx.params.get(schema::BOUNDED).map(|s| s.eval(&at)), Some(Value::Bool(true)));
        let kind = self.registry.effect(&fx.type_id).map(|d| d.kind.clone());
        let text_only = kind.as_ref().is_some_and(|k| k.text_only());
        // Motion moves the whole clip, and sound isn't a picture: nothing to mask.
        if self.registry.is_sound(&fx.type_id) || kind == Some(oa_graph::EffectKind::Motion) {
            return;
        }
        ui.separator();
        if bounded || text_only {
            let why = if bounded { tr("Bounded text effects can't be masked (turn Bounded off first)") } else { tr("Per-letter text effects can't be masked") };
            ui.add_enabled(false, egui::Button::new(tr("Use with mask"))).on_disabled_hover_text(why);
            return;
        }
        let current = fx.params.get(mask::EFFECT_USE).and_then(|s| s.eval(&at).as_float()).unwrap_or(0.0);
        let invert = matches!(fx.params.get(mask::EFFECT_INVERT).map(|s| s.eval(&at)), Some(Value::Bool(true)));
        ui.menu_button(tr("Use with mask"), |ui| {
            if it.masks.is_empty() {
                ui.label(egui::RichText::new(tr("Draw a mask in the Masks tab first.")).weak());
                return;
            }
            let mut chosen = current;
            ui.radio_value(&mut chosen, 0.0, tr("The whole clip (no mask)"));
            ui.radio_value(&mut chosen, mask::ALL, tr("Every mask"));
            for m in &it.masks {
                ui.radio_value(&mut chosen, m.id as f64, &m.name);
            }
            if chosen != current {
                self.editor.set_param(item, ParamTarget::Effect(fx.id), mask::EFFECT_USE, ParamSource::Static(Value::Float(chosen)), mask::EFFECT_USE);
                self.editor.doc.seal();
            }
            ui.separator();
            let mut outside = invert;
            if ui.add_enabled(current != 0.0 || chosen != 0.0, egui::Checkbox::new(&mut outside, tr("Outside the mask instead"))).changed() {
                self.editor.set_param(item, ParamTarget::Effect(fx.id), mask::EFFECT_INVERT, ParamSource::Static(Value::Bool(outside)), mask::EFFECT_INVERT);
                self.editor.doc.seal();
            }
        });
    }
}

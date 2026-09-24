//! Color management controls (DESIGN.md §9): a file's input transform — in its
//! right-click menu and the clip's Properties tab — and the sequence's output transform
//! in the scene panel.

use crate::App;
use eframe::egui;
use oa_doc::color::{Gamut, InputColor, Matrix, Range, Transfer};
use oa_doc::{schema, MediaId, Op};
use oa_params::Value;

impl App {
    /// The file's settings, what they resolve to, and what `Automatic` would pick.
    fn media_color(&self, media: MediaId) -> Option<(InputColor, Transfer, Gamut, Transfer)> {
        let m = self.editor.doc.project().media(media)?;
        let (t, g) = m.input_color();
        let tags = m.info.as_ref().map(|i| i.color.clone()).unwrap_or_default();
        let auto = InputColor::default().resolve(&tags).0;
        Some((m.color.clone(), t, g, auto))
    }

    fn set_media_color(&mut self, media: MediaId, color: InputColor, drag: bool) {
        let op = vec![Op::SetMediaColor { media, color }];
        let result = if drag { self.editor.apply_drag("Source color", "source-color", op) } else { self.editor.apply("Source color", op) };
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
    }

    /// Menu entries for how a file's values are read as light.
    pub(crate) fn color_menu(&mut self, ui: &mut egui::Ui, media: MediaId) {
        let Some((color, transfer, gamut, auto)) = self.media_color(media) else { return };
        let mut picked: Option<InputColor> = None;
        ui.menu_button("Source color", |ui| {
            ui.menu_button(format!("Curve: {}", transfer.label()), |ui| {
                for t in Transfer::ALL {
                    let label = if t == Transfer::Auto { format!("Automatic ({})", auto.label()) } else { t.label().into() };
                    if ui.radio(color.transfer == t, label).clicked() {
                        picked = Some(InputColor { transfer: t, ..color.clone() });
                        ui.close();
                    }
                }
            });
            ui.menu_button(format!("Gamut: {}", gamut.label()), |ui| {
                for g in Gamut::ALL {
                    if ui.radio(color.gamut == g, g.label()).clicked() {
                        picked = Some(InputColor { gamut: g, ..color.clone() });
                        ui.close();
                    }
                }
            });
            ui.menu_button("Levels", |ui| {
                for (r, label) in [(Range::Auto, "Automatic (from the file)"), (Range::Limited, "Video (16–235)"), (Range::Full, "Full (0–255)")] {
                    if ui.radio(color.range == r, label).clicked() {
                        picked = Some(InputColor { range: r, ..color.clone() });
                        ui.close();
                    }
                }
            });
            ui.menu_button("YCbCr matrix", |ui| {
                for (m, label) in [(Matrix::Auto, "Automatic (from the file)"), (Matrix::Bt601, "BT.601 (SD)"), (Matrix::Bt709, "BT.709 (HD)"), (Matrix::Bt2020, "BT.2020 (UHD / HDR)")] {
                    if ui.radio(color.matrix == m, label).clicked() {
                        picked = Some(InputColor { matrix: m, ..color.clone() });
                        ui.close();
                    }
                }
            });
            if !color.is_auto() && ui.button("Reset to the file's own tags").clicked() {
                picked = Some(InputColor::default());
                ui.close();
            }
        });
        if let Some(c) = picked {
            self.set_media_color(media, c, false);
        }
    }

    /// The Properties tab's "Source color" section for a clip of `media`.
    pub(crate) fn source_color_section(&mut self, ui: &mut egui::Ui, media: MediaId) {
        let Some((color, transfer, gamut, auto)) = self.media_color(media) else { return };
        let mut next = color.clone();
        egui::Grid::new("source-color").num_columns(2).spacing([6.0, 4.0]).show(ui, |ui| {
            ui.label("curve");
            let auto = format!("Automatic ({})", auto.label());
            let text = if color.transfer == Transfer::Auto { auto.clone() } else { color.transfer.label().into() };
            egui::ComboBox::from_id_salt("source-transfer").selected_text(text).width(180.0).show_ui(ui, |ui| {
                for t in Transfer::ALL {
                    ui.selectable_value(&mut next.transfer, t, if t == Transfer::Auto { auto.as_str() } else { t.label() });
                }
            });
            ui.end_row();
            ui.label("gamut");
            let text = if color.gamut == Gamut::Auto { format!("Automatic ({})", gamut.label()) } else { gamut.label().into() };
            egui::ComboBox::from_id_salt("source-gamut").selected_text(text).width(180.0).show_ui(ui, |ui| {
                for g in Gamut::ALL {
                    ui.selectable_value(&mut next.gamut, g, g.label());
                }
            });
            ui.end_row();
            ui.label("exposure");
            let r = ui.add(egui::Slider::new(&mut next.exposure, -6.0..=6.0).suffix(" stops").step_by(0.05).clamping(egui::SliderClamping::Edits));
            ui.end_row();
            if next != color {
                let dragging = r.dragged() || r.has_focus();
                self.set_media_color(media, next.clone(), dragging);
            }
            if r.drag_stopped() || r.lost_focus() {
                self.editor.doc.seal();
            }
        });
        let hint = if transfer.is_high_range() {
            "HDR / log: its highlights go past white, so the output tone maps them (Output, with nothing selected)."
        } else {
            "Applies to every clip of this file. Levels and matrix: right-click the file."
        };
        ui.label(egui::RichText::new(hint).small().weak());
    }

    /// The scene panel's "Output" section: tone mapping and exposure for the whole
    /// picture, on screen and in the export.
    pub(crate) fn output_section(&mut self, ui: &mut egui::Ui) {
        let t = self.playhead;
        let seq = self.editor.sequence();
        let v = seq.params.eval(schema::output(), None, &oa_params::EvalContext::at(t, t));
        let current = match v.get(schema::OUT_TONE_MAP) {
            Some(Value::Enum(m)) => m.clone(),
            _ => "auto".into(),
        };
        let resolved = oa_plan::tone_map(self.editor.doc.project(), self.editor.seq, "auto");
        let label = |m: &str| match m {
            "off" => "Off (clip at white)".to_string(),
            "soft" => "Soft highlights".to_string(),
            "filmic" => "Filmic".to_string(),
            _ => format!("Automatic ({})", if resolved == oa_doc::color::ToneMap::Off { "off" } else { "soft" }),
        };
        let mut chosen = current.clone();
        ui.horizontal(|ui| {
            ui.label("tone map");
            egui::ComboBox::from_id_salt("output-tone-map").selected_text(label(&current)).show_ui(ui, |ui| {
                for m in ["auto", "off", "soft", "filmic"] {
                    ui.selectable_value(&mut chosen, m.to_string(), label(m));
                }
            });
        });
        if chosen != current {
            self.editor.set_sequence_value(schema::OUT_TONE_MAP, Value::Enum(chosen), t, "output-tone-map");
            self.editor.doc.seal();
        }
        let mut stops = v.float(schema::OUT_EXPOSURE);
        let r = ui.add(egui::Slider::new(&mut stops, -4.0..=4.0).text("exposure").suffix(" stops").step_by(0.05).clamping(egui::SliderClamping::Edits));
        if r.changed() {
            self.editor.set_sequence_value(schema::OUT_EXPOSURE, Value::Float(stops), t, "output-exposure");
        }
        if r.drag_stopped() || r.lost_focus() {
            self.editor.doc.seal();
        }
    }
}

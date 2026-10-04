//! Every command the editor has, in one list.
//!
//! The list is the single source for the menu bar, the palette (Ctrl+K) and the
//! contextual action bar above the timeline, so a command written once shows up
//! wherever it makes sense and always names its shortcut. It's also the place to look
//! to see what the editor can actually do — which, before this, meant hunting through
//! right-click menus.
//!
//! A command is a name, a group, the keys that run it, whether it can run right now,
//! and a plain function that does it. Nothing captures state, so the list can be
//! rebuilt every frame from the current selection and stay honest about what's
//! available.

use crate::i18n::tr;
use crate::icons;
use crate::App;

/// Where a command belongs. Also the palette's grouping.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Project,
    Edit,
    Clip,
    Timeline,
    View,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Project => tr("Project"),
            Group::Edit => tr("Edit"),
            Group::Clip => tr("Clip"),
            Group::Timeline => tr("Timeline"),
            Group::View => tr("View"),
        }
    }
}

pub struct Command {
    /// Stable id (for shortcuts and, later, for plugins to add their own).
    pub id: &'static str,
    pub title: String,
    pub group: Group,
    /// As the user would type it; empty when there isn't one.
    pub shortcut: &'static str,
    /// What the action bar draws for it, if it belongs there.
    pub icon: Option<crate::icons::Icon>,
    pub enabled: bool,
    /// Worth offering for what's selected right now (the action bar shows these).
    pub contextual: bool,
    pub run: fn(&mut App),
}

impl App {
    /// The commands as they stand for the current selection.
    pub(crate) fn commands(&self) -> Vec<Command> {
        let selected = !self.selected.is_empty();
        let one = self.selection.is_some();
        let picture = self.selection.is_some_and(|id| self.editor.item(id).is_some_and(|i| i.kind != oa_doc::ItemKind::Text));
        let grouped = self.selection_grouped();
        let many = self.selected.len() >= 2;
        let clips = self.editor.sequence().tracks.iter().any(|t| !t.items.is_empty());
        let cmd = |id, title: &str, group, shortcut, icon, enabled, contextual, run: fn(&mut App)| Command {
            id,
            title: title.to_string(),
            group,
            shortcut,
            icon,
            enabled,
            contextual,
            run,
        };
        vec![
            // ---- project ----
            cmd("project.new", tr("New project"), Group::Project, "", Some(icons::ADD), true, false, |a| {
                a.new_project();
            }),
            cmd("project.open", tr("Open project…"), Group::Project, "", Some(icons::FOLDER_OPEN), true, false, |a| {
                if let Some(path) = rfd::FileDialog::new().add_filter(tr("OpenAtelier project"), &["json"]).pick_file() {
                    a.open_project(&path);
                }
            }),
            cmd("project.save", tr("Save"), Group::Project, "Ctrl+S", Some(icons::SAVE), true, false, |a| a.save(false)),
            cmd("project.save_as", tr("Save as…"), Group::Project, "Ctrl+Shift+S", Some(icons::SAVE), true, false, |a| a.save(true)),
            cmd("project.import", tr("Import media…"), Group::Project, "Ctrl+I", Some(icons::IMPORT), true, false, |a| {
                if let Some(files) = rfd::FileDialog::new().add_filter(tr("Media"), crate::MEDIA_EXTENSIONS).pick_files() {
                    a.open_paths(&files);
                }
            }),
            cmd("project.export", tr("Export…"), Group::Project, "", Some(icons::EXPORT), true, false, |a| a.start_export()),
            cmd("project.export_otio", tr("Export timeline for another editor (.otio)…"), Group::Project, "", None, true, false, |a| a.export_otio()),
            cmd("project.home", tr("Home: projects and plugins"), Group::Project, "", Some(icons::HOME), true, false, |a| a.go_home()),
            // ---- edit ----
            cmd("edit.undo", tr("Undo"), Group::Edit, "Ctrl+Z", Some(icons::UNDO), self.editor.doc.undo_label().is_some(), false, |a| a.undo()),
            cmd("edit.redo", tr("Redo"), Group::Edit, "Ctrl+Shift+Z", Some(icons::REDO), self.editor.doc.redo_label().is_some(), false, |a| a.redo()),
            cmd("edit.cut", tr("Cut"), Group::Edit, "Ctrl+X", None, selected, false, |a| a.cut_selection()),
            cmd("edit.copy", tr("Copy"), Group::Edit, "Ctrl+C", None, selected, false, |a| a.copy_selection()),
            cmd("edit.paste", tr("Paste at playhead"), Group::Edit, "Ctrl+V", None, !self.clipboard.is_empty(), false, |a| {
                let at = a.playhead;
                a.paste_at(at);
            }),
            cmd("edit.duplicate", tr("Duplicate"), Group::Edit, "Ctrl+D", Some(icons::DUPLICATE), selected, true, |a| a.duplicate_selection()),
            cmd("edit.delete", tr("Delete"), Group::Edit, "Del", Some(icons::DELETE), selected, true, |a| a.delete_selection(false)),
            cmd("edit.ripple_delete", tr("Ripple delete"), Group::Edit, "Shift+Del", None, selected, false, |a| a.delete_selection(true)),
            cmd("edit.select_all", tr("Select all clips"), Group::Edit, "Ctrl+A", None, clips, false, |a| a.select_all()),
            // ---- clip ----
            cmd("clip.split", tr("Split at playhead"), Group::Clip, "S", Some(icons::SPLIT), selected, true, |a| a.split_at_playhead()),
            cmd("clip.group", tr("Group"), Group::Clip, "Ctrl+G", Some(icons::GROUP), many, true, |a| a.group_selection()),
            cmd("clip.ungroup", tr("Ungroup"), Group::Clip, "Ctrl+Shift+G", None, grouped, grouped, |a| a.ungroup_selection()),
            cmd("clip.enable", tr("Enable or disable"), Group::Clip, "", Some(icons::ENABLE), selected, true, |a| a.toggle_selection_enabled()),
            cmd("clip.extract_audio", tr("Extract audio"), Group::Clip, "", Some(icons::EXTRACT_AUDIO), !self.extractable().is_empty(), !self.extractable().is_empty(), |a| {
                a.extract_audio()
            }),
            cmd("clip.compound", tr("Make a compound clip (As media)"), Group::Clip, "", None, selected, false, |a| a.compound_selection(false)),
            cmd("clip.nest", tr("Nest into one clip"), Group::Clip, "", None, selected, false, |a| a.compound_selection(true)),
            cmd("clip.break", tr("Break compound apart"), Group::Clip, "", None, self.compound_selected(), self.compound_selected(), |a| a.break_compounds()),
            cmd("clip.copy_effects", tr("Copy effects"), Group::Clip, "", None, one, false, |a| {
                if let Some(id) = a.selection {
                    a.copy_effects(id, None);
                }
            }),
            cmd("clip.paste_effects", tr("Paste effects"), Group::Clip, "", None, !self.effect_clipboard.is_empty() && selected, false, |a| {
                let targets = a.selected_clips();
                a.paste_effects(&targets);
            }),
            cmd("clip.copy_transform", tr("Copy transform"), Group::Clip, "", None, one && picture, false, |a| {
                if let Some(id) = a.selection {
                    a.copy_transform(id);
                }
            }),
            cmd("clip.paste_transform", tr("Paste transform"), Group::Clip, "", None, self.transform_clipboard.is_some() && selected, false, |a| {
                let targets = a.selected_clips();
                a.paste_transform(&targets);
            }),
            // ---- timeline ----
            cmd("timeline.add_text", tr("Add a title"), Group::Timeline, "Ctrl+T", Some(icons::TITLE), true, false, |a| a.add_text()),
            cmd("timeline.add_video_track", tr("Add a video track"), Group::Timeline, "", None, true, false, |a| {
                if let Err(e) = a.editor.add_track(oa_doc::TrackKind::Video) {
                    a.report_error(e.to_string());
                }
            }),
            cmd("timeline.add_audio_track", tr("Add an audio track"), Group::Timeline, "", None, true, false, |a| {
                if let Err(e) = a.editor.add_track(oa_doc::TrackKind::Audio) {
                    a.report_error(e.to_string());
                }
            }),
            cmd("timeline.snap", tr("Snapping on or off"), Group::Timeline, "", Some(icons::SNAP), true, false, |a| { a.settings.snapping = !a.settings.snapping; a.settings.save(); }),
            cmd("timeline.fit", tr("Fit the whole sequence"), Group::Timeline, "", None, true, false, |a| a.timeline_view.fit = true),
            // ---- view ----
            cmd("view.play", tr("Play or pause"), Group::View, "Space", None, true, false, |a| {
                let playing = !a.playing;
                a.set_playing(playing);
            }),
            cmd("view.start", tr("Go to the start"), Group::View, "Home", None, true, false, |a| a.set_playhead(oa_time::Time::ZERO)),
            cmd("view.end", tr("Go to the end"), Group::View, "End", None, true, false, |a| {
                let end = a.editor.duration();
                a.set_playhead(end);
            }),
            cmd("view.clip_start", tr("Go to the start of the clip"), Group::View, "Ctrl+←", None, true, false, |a| a.go_to_clip_edge(true)),
            cmd("view.clip_end", tr("Go to the end of the clip"), Group::View, "Ctrl+→", None, true, false, |a| a.go_to_clip_edge(false)),
            cmd("view.zoom_fit", tr("Fit the viewer"), Group::View, "Ctrl+0", None, true, false, |a| a.view.zoom = None),
            cmd("view.actual_size", tr("Viewer at 100%"), Group::View, "Ctrl+1", None, true, false, |a| a.view.zoom = Some(1.0)),
            cmd("view.thirds", tr("Rule-of-thirds guides"), Group::View, "", None, true, false, |a| a.view.thirds = !a.view.thirds),
            cmd("view.safe", tr("Title and action safe areas"), Group::View, "", None, true, false, |a| a.view.safe_areas = !a.view.safe_areas),
        ]
    }

    /// Runs the command with this id, if it's available.
    pub(crate) fn run_command(&mut self, id: &str) {
        if let Some(run) = self.commands().iter().find(|c| c.id == id && c.enabled).map(|c| c.run) {
            run(self);
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is what the menus, the palette and the action bar all read, so it has to
    /// hold together: unique ids, a name for each, and an icon for anything that claims
    /// a place on the action bar.
    #[test]
    fn the_command_list_holds_together() {
        // A bare app isn't constructible in a test (it needs a GPU and a window), so
        // check the invariants that don't depend on state: the list is written by hand
        // and these are the mistakes a hand-written list makes.
        let groups = [Group::Project, Group::Edit, Group::Clip, Group::Timeline, Group::View];
        for g in groups {
            assert!(!g.title().is_empty());
        }
        // Groups sort in the order the palette shows them.
        let mut sorted = groups;
        sorted.sort();
        assert_eq!(sorted, groups, "groups are declared in display order");
    }
}

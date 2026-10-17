//! The edit session: a project, its media pool, and the operations the UI drives.

use crate::i18n::tr;
use oa_doc::*;
use oa_media::{Imported, MediaKind, MediaProbe};
use oa_params::{KeyframeAnchor, ParamId, ParamSource, Value};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Runtime knowledge about one pool item: what the project stores plus what decoding
/// needs (the conformed copy, the frame index, whether the file is still there).
pub struct PoolItem {
    pub id: MediaId,
    pub name: String,
    pub kind: MediaKind,
    /// The file to decode: the original, or a conformed copy.
    pub decode_path: PathBuf,
    pub probe: MediaProbe,
    pub conformed: Option<String>,
    pub missing: bool,
}

impl PoolItem {
    pub fn summary(&self) -> String {
        if self.missing {
            return "missing — the file moved or was deleted".into();
        }
        self.probe.summary()
    }
}

/// Where each clip is (track, place), and what it was worked out for: the project's
/// revision, its snapshot and the timeline.
type ItemIndex = (Option<(u64, usize, SeqId)>, std::collections::HashMap<ItemId, (usize, usize)>);

pub struct Editor {
    pub doc: Document,
    pub seq: SeqId,
    pub video_track: TrackId,
    pub audio_track: TrackId,
    pub pool: Vec<PoolItem>,
    /// Where the project was last saved/opened from.
    pub path: Option<PathBuf>,
    saved: Option<Arc<Project>>,
    /// Clips that follow parameter edits made to one of them (the rest of a multiple
    /// selection, while the inspector is drawn): `set_param`, `set_value_at` and
    /// `toggle_keyframing` write the same change to each.
    pub linked: Vec<ItemId>,
    /// Where each clip of the open timeline is (track, place), and the project revision,
    /// snapshot and timeline it was worked out for (`item`).
    index: std::cell::RefCell<ItemIndex>,
}

fn variant(project: &mut Project, preset: &str, short_edge: u32) -> FormatVariant {
    let p = AspectPreset::by_id(preset).expect("known preset");
    FormatVariant { id: VariantId(project.alloc_id()), name: p.name.into(), size: p.size(short_edge), overrides: BTreeMap::new() }
}

impl Editor {
    /// An empty project: one sequence, a video and an audio track, three format variants.
    pub fn new() -> Self {
        Self::with_setup("Untitled", "landscape-16x9", FrameRate::FPS_30)
    }

    /// An empty project as the New project window sets it up: its name, the format it
    /// opens in (`layout`, an `AspectPreset` id: first in the list, so it's the one
    /// shown) and its frame rate. Landscape, vertical and square are there too.
    pub fn with_setup(name: &str, layout: &str, rate: FrameRate) -> Self {
        // Built directly, not as an edit: the empty project is where undo stops. (As an
        // edit, undoing past the first change took the whole timeline away.)
        let mut project = Project::new(name);
        let (seq, video_track, audio_track) = (SeqId(project.alloc_id()), TrackId(project.alloc_id()), TrackId(project.alloc_id()));
        let layout = AspectPreset::by_id(layout).map_or("landscape-16x9", |p| p.id);
        let first = variant(&mut project, layout, 1080);
        let mut sequence = Sequence::new(seq, "Main", rate, first);
        for other in ["landscape-16x9", "vertical-9x16", "square-1x1"].into_iter().filter(|p| *p != layout) {
            let v = variant(&mut project, other, 1080);
            sequence.variants.push(v);
        }
        sequence.tracks.push(Arc::new(Track::new(video_track, "V1", TrackKind::Video)));
        sequence.tracks.push(Arc::new(Track::new(audio_track, "A1", TrackKind::Audio)));
        project.sequences.insert(seq, Arc::new(sequence));
        let doc = Document::new(project);
        let saved = Some(doc.snapshot());
        Editor { doc, seq, video_track, audio_track, pool: Vec::new(), path: None, saved, linked: Vec::new(), index: Default::default() }
    }

    pub fn sequence(&self) -> &Sequence {
        let p = self.doc.project();
        // An undo can remove the compound clip being edited mid-frame; the app steps out
        // of it next frame (`compound_still_there`) — until then, show the main timeline.
        p.sequence(self.seq).or_else(|| p.sequences.values().next().map(|s| &**s)).expect("a project has a sequence")
    }

    pub fn duration(&self) -> Time {
        self.sequence().duration()
    }

    pub fn dirty(&self) -> bool {
        match &self.saved {
            Some(saved) => !Arc::ptr_eq(saved, &self.doc.snapshot()),
            None => true,
        }
    }

    /// What the project is called on the start page.
    pub fn set_project_name(&mut self, name: &str) {
        self.doc.set_project_name(name);
    }

    pub fn title(&self) -> String {
        format!("{}{}", self.name(), if self.dirty() { " •" } else { "" })
    }

    /// What the project is called: its own name, or (for one without) its file's.
    pub fn name(&self) -> String {
        let named = self.doc.project().name.trim();
        if !named.is_empty() {
            return named.to_string();
        }
        let file = self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().trim_end_matches(".oaproj.json").to_string());
        file.unwrap_or_else(|| crate::i18n::tr("Untitled").to_string())
    }

    /// A clip of the open timeline by id. Found through an index of where each clip is,
    /// rebuilt only when the project changes: the timeline asks for every clip it draws,
    /// every frame, and searching the tracks each time cost ~9 ms a frame on an hour-long
    /// edit.
    pub fn item(&self, id: ItemId) -> Option<&Item> {
        let s = self.sequence();
        if id == oa_doc::BACKGROUND {
            return s.item(id);
        }
        let key = (self.doc.revision(), self.doc.project() as *const Project as usize, self.seq);
        let mut index = self.index.borrow_mut();
        if index.0 != Some(key) {
            index.1.clear();
            for (ti, track) in s.tracks.iter().enumerate() {
                for (ii, item) in track.items.iter().enumerate() {
                    index.1.insert(item.id, (ti, ii));
                }
            }
            index.0 = Some(key);
        }
        let (ti, ii) = *index.1.get(&id)?;
        // Checked, so a stale entry can never hand back the wrong clip.
        s.tracks.get(ti).and_then(|t| t.items.get(ii)).filter(|it| it.id == id).or_else(|| s.item(id))
    }

    pub fn pool_item(&self, id: MediaId) -> Option<&PoolItem> {
        self.pool.iter().find(|m| m.id == id)
    }

    /// Adds an imported file to the pool (deduplicating by fingerprint).
    #[cfg(test)]
    pub fn add_media(&mut self, imported: Imported) -> Result<MediaId, EditError> {
        self.add_media_into(imported, "", None)
    }

    /// Adds an imported file to the pool, into bin `folder` ("" the top). Edits with the
    /// same `batch` key merge into one undo step (a batch of imports: one "Import media").
    /// A file already in the project (the same fingerprint) isn't added twice.
    pub fn add_media_into(&mut self, imported: Imported, folder: &str, batch: Option<&str>) -> Result<MediaId, EditError> {
        if let Some(existing) = self
            .doc
            .project()
            .media
            .values()
            .find(|m| m.fingerprint.as_deref() == Some(imported.fingerprint.as_str()))
            .map(|m| m.id)
        {
            return Ok(existing);
        }
        let id = MediaId(self.doc.alloc_id());
        let video = imported.probe.video.as_ref();
        let info = MediaInfo {
            width: video.map(|v| v.width).unwrap_or(0),
            height: video.map(|v| v.height).unwrap_or(0),
            duration: imported.probe.duration,
            rate: video.and_then(|v| v.avg_rate),
            has_video: video.is_some(),
            has_audio: imported.probe.has_audio(),
            still: video.is_some_and(|v| v.still),
            color: oa_doc::color::ColorTags {
                transfer: video.and_then(|v| v.transfer_tag.clone()),
                primaries: video.and_then(|v| v.primaries_tag.clone()),
            },
            alpha: video.is_some_and(|v| v.has_alpha),
        };
        let media = MediaRef {
            id,
            path: imported.path.to_string_lossy().to_string(),
            fingerprint: Some(imported.fingerprint.clone()),
            info: Some(info),
            scaling: Default::default(),
            folder: folder.to_string(),
            color: Default::default(),
        };
        self.doc.edit_coalesced("Import media", batch, vec![Op::AddMedia(Arc::new(media))])?;
        self.pool.push(PoolItem {
            id,
            name: imported.name(),
            kind: imported.kind,
            decode_path: imported.decode_path,
            probe: imported.probe,
            conformed: imported.conformed,
            missing: false,
        });
        Ok(id)
    }

    /// Appends a clip playing `media` to the end of the matching track.
    pub fn append_clip(&mut self, media: MediaId, duration: Time) -> Result<ItemId, EditError> {
        let pool = self.pool_item(media).ok_or(EditError::NotFound("media", media.0))?;
        let (preferred, name) = (if pool.kind == MediaKind::Audio { self.audio_track } else { self.video_track }, pool.name.clone());
        let kind = if pool.kind == MediaKind::Audio { TrackKind::Audio } else { TrackKind::Video };
        // The default track may have been deleted: any track of the kind, or a temp layer.
        let found = self.sequence().track(preferred).map(|t| t.id).or_else(|| self.sequence().tracks.iter().find(|t| t.kind == kind && !t.effects).map(|t| t.id));
        let mut ops = Vec::new();
        let track = self.track_or_temp(found, kind, &mut ops);
        let start = self.sequence().track(track).map(|t| t.items.last().map_or(Time::ZERO, |i| i.range.end())).unwrap_or(Time::ZERO);
        let id = ItemId(self.doc.alloc_id());
        let item = Item::new(id, &name, ItemKind::Media { media }, TimeRange::new(start, duration.max(Time::from_seconds(1))));
        ops.push(Op::InsertItem { seq: self.seq, track, item });
        self.doc.edit("Add clip", ops)?;
        Ok(id)
    }

    /// Parts of a file, one after another at the end of its track (quick cut): each
    /// `(from, length)` in the file's own time becomes a clip playing that stretch. One
    /// undo step. Returns the new clips.
    pub fn append_parts(&mut self, media: MediaId, parts: &[(Time, Time)]) -> Result<Vec<ItemId>, EditError> {
        let pool = self.pool_item(media).ok_or(EditError::NotFound("media", media.0))?;
        let (preferred, name) = (if pool.kind == MediaKind::Audio { self.audio_track } else { self.video_track }, pool.name.clone());
        let kind = if pool.kind == MediaKind::Audio { TrackKind::Audio } else { TrackKind::Video };
        let found = self.sequence().track(preferred).map(|t| t.id).or_else(|| self.sequence().tracks.iter().find(|t| t.kind == kind && !t.effects).map(|t| t.id));
        let (mut ops, mut ids) = (Vec::new(), Vec::new());
        let track = self.track_or_temp(found, kind, &mut ops);
        let rate = self.sequence().rate;
        let mut at = self.sequence().track(track).map_or(Time::ZERO, |t| t.items.last().map_or(Time::ZERO, |i| i.range.end()));
        for &(from, length) in parts.iter().filter(|(_, l)| *l > Time::ZERO) {
            // Each on whole frames of the timeline, at least one long.
            let length = rate.frame_start(rate.frame_at(length).max(1));
            let id = ItemId(self.doc.alloc_id());
            let mut item = Item::new(id, &name, ItemKind::Media { media }, TimeRange::new(at, length));
            item.time_map = oa_doc::TimeMap::new(from.max(Time::ZERO), oa_time::Rational::new(1, 1));
            ops.push(Op::InsertItem { seq: self.seq, track, item });
            ids.push(id);
            at += length;
        }
        if !ids.is_empty() {
            self.doc.edit("Add parts", ops)?;
        }
        Ok(ids)
    }

    /// How many clips use each file, and each compound clip, on every timeline (inside
    /// compound clips too).
    pub fn media_uses(&self) -> (HashMap<MediaId, usize>, HashMap<SeqId, usize>) {
        let (mut files, mut compounds) = (HashMap::new(), HashMap::new());
        for s in self.doc.project().sequences.values() {
            for it in s.tracks.iter().flat_map(|t| t.items.iter()) {
                match it.kind {
                    ItemKind::Media { media } => *files.entry(media).or_insert(0) += 1,
                    ItemKind::Nested { sequence } => *compounds.entry(sequence).or_insert(0) += 1,
                    _ => {}
                }
            }
        }
        (files, compounds)
    }

    /// Takes files out of the project, with every clip that uses them (on any timeline):
    /// one undo step, which brings them all back.
    pub fn remove_media(&mut self, media: &[MediaId]) -> Result<(), EditError> {
        let mut ops = Vec::new();
        for (seq, s) in &self.doc.project().sequences {
            for it in s.tracks.iter().flat_map(|t| t.items.iter()) {
                if matches!(it.kind, ItemKind::Media { media: m } if media.contains(&m)) {
                    ops.push(Op::RemoveItem { seq: *seq, item: it.id });
                }
            }
        }
        ops.extend(media.iter().filter(|m| self.doc.project().media(**m).is_some()).map(|m| Op::RemoveMedia(*m)));
        let label = if media.len() == 1 { "Remove media" } else { "Remove media files" };
        self.doc.edit(label, ops)
    }

    /// Adds a text clip at `at` on top of everything: on the topmost video track if it's
    /// free there and something lies beneath, otherwise on a temp layer above.
    pub fn add_text(&mut self, at: Time, content: &str, length: Time) -> Result<ItemId, EditError> {
        let range = TimeRange::new(at, length.max(Time::from_seconds_f64(0.1)));
        let videos: Vec<&Track> = self.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.effects).map(|t| &**t).collect();
        let top = videos.last().filter(|t| videos.len() > 1 && !t.items.iter().any(|i| i.range.overlaps(range))).map(|t| t.id);
        let found = match top {
            Some(t) => Some(t),
            None if videos.iter().all(|t| t.items.is_empty()) && !videos.is_empty() => Some(videos[0].id),
            None => None,
        };
        let mut ops = Vec::new();
        let track = self.track_or_temp(found, TrackKind::Video, &mut ops);
        let id = ItemId(self.doc.alloc_id());
        let mut item = Item::new(id, "Text", ItemKind::Text, range);
        item.params.set(oa_doc::schema::TEXT_CONTENT, ParamSource::Static(Value::Text(content.into())));
        ops.push(Op::InsertItem { seq: self.seq, track, item });
        self.doc.edit("Add text", ops)?;
        Ok(id)
    }

    /// Adds a track: video tracks go on top of the other video tracks, audio after the
    /// other audio tracks.
    pub fn add_track(&mut self, kind: TrackKind) -> Result<TrackId, EditError> {
        let s = self.sequence();
        let count = s.tracks.iter().filter(|t| t.kind == kind && !t.effects).count();
        let index = s.tracks.iter().rposition(|t| t.kind == kind).map_or(s.tracks.len(), |i| i + 1);
        let name = format!("{}{}", if kind == TrackKind::Video { "V" } else { "A" }, count + 1);
        let id = TrackId(self.doc.alloc_id());
        let track = Arc::new(Track::new(id, &name, kind));
        self.doc.edit("Add track", vec![Op::InsertTrack { seq: self.seq, index, track }])?;
        Ok(id)
    }

    /// A temp layer of `kind`, where `add_track` would put a track, for a clip that found
    /// no room: outlined on the timeline, gone again once it's empty, kept with its +. The
    /// op goes in the same edit as the clip (an empty temp layer is pruned at once).
    pub fn temp_track(&mut self, kind: TrackKind) -> (TrackId, Op) {
        let s = self.sequence();
        let count = s.tracks.iter().filter(|t| t.kind == kind && !t.effects).count();
        let index = s.tracks.iter().rposition(|t| t.kind == kind).map_or(s.tracks.len(), |i| i + 1);
        let name = format!("{}{}", if kind == TrackKind::Video { "V" } else { "A" }, count + 1);
        let id = TrackId(self.doc.alloc_id());
        let track = Arc::new(Track { temp: true, ..Track::new(id, &name, kind) });
        (id, Op::InsertTrack { seq: self.seq, index, track })
    }

    /// The track to put a clip on: `found`, or a new temp layer (its op added to `ops`).
    fn track_or_temp(&mut self, found: Option<TrackId>, kind: TrackKind, ops: &mut Vec<Op>) -> TrackId {
        found.unwrap_or_else(|| {
            let (id, op) = self.temp_track(kind);
            ops.push(op);
            id
        })
    }

    /// Adds an effect track of `kind` just above `track` as the timeline shows it — for
    /// pictures, the next layer up; for sound, the row above — so it covers that track
    /// and everything below it.
    pub fn add_effect_track(&mut self, kind: TrackKind, track: TrackId) -> Result<TrackId, EditError> {
        let s = self.sequence();
        let at = s.tracks.iter().position(|t| t.id == track).ok_or(EditError::NotFound("track", track.0))?;
        // Picture tracks later in the list draw on top; audio rows run top down.
        let index = if kind == TrackKind::Video { at + 1 } else { at };
        let count = s.tracks.iter().filter(|t| t.kind == kind && t.effects).count();
        let name = format!("{}FX{}", if kind == TrackKind::Video { "V" } else { "A" }, count + 1);
        let id = TrackId(self.doc.alloc_id());
        self.doc.edit(tr("Add effect track"), vec![Op::InsertTrack { seq: self.seq, index, track: Arc::new(Track::effects(id, &name, kind)) }])?;
        Ok(id)
    }

    /// Adds an effect container on effect track `track` from `at`: five seconds, or up to
    /// the next container. Its effects are added in the inspector (or dropped on it).
    pub fn add_container(&mut self, track: TrackId, at: Time) -> Result<ItemId, EditError> {
        let s = self.sequence();
        let t = s.track(track).filter(|t| t.effects).ok_or(EditError::WrongTrackKind)?;
        if t.items.iter().any(|i| i.range.contains(at)) {
            return Err(EditError::Overlap);
        }
        let next = t.items.iter().map(|i| i.range.start).filter(|s| *s > at).min();
        let length = next.map_or(Time::from_seconds(5), |n| (n - at).min(Time::from_seconds(5)));
        let id = ItemId(self.doc.alloc_id());
        let item = Item::new(id, "Effects", ItemKind::Adjustment, TimeRange::new(at, length));
        self.apply(tr("Add effect container"), vec![Op::InsertItem { seq: self.seq, track, item }])?;
        Ok(id)
    }

    /// Compound clips (groups turned into media) usable in the open timeline: every
    /// other sequence except those containing it — the project's main timeline, or the
    /// compounds around the one being edited — since a clip can't contain itself.
    pub fn compounds(&self) -> Vec<&Sequence> {
        let p = self.doc.project();
        p.sequences.values().filter(|s| !p.sequence_reaches(s.id, self.seq)).map(|s| &**s).collect()
    }

    /// Copies `items` (clips of the open sequence) into a new sequence, a compound clip,
    /// keeping their tracks and spacing, moved to start at 0, at the open format. It's
    /// then media: it shows in the bin, can be added to the timeline and feeds effects
    /// that take a picture (a mask…). `nest`: the clips are also replaced by one clip
    /// playing it. One undo step. Returns the sequence and, when nesting, the new clip.
    pub fn compound(&mut self, items: &[ItemId], nest: bool) -> Result<(SeqId, Option<ItemId>), EditError> {
        let s = self.sequence().clone();
        let chosen: Vec<(usize, &Item)> =
            s.tracks.iter().enumerate().flat_map(|(t, tr)| tr.items.iter().filter(|i| items.contains(&i.id)).map(move |i| (t, i))).collect();
        let start = chosen.iter().map(|(_, i)| i.range.start).min().ok_or(EditError::NotFound("clips", 0))?;
        let end = chosen.iter().map(|(_, i)| i.range.end()).max().unwrap_or(start);
        let number = self.doc.project().sequences.len();
        let name = format!("Compound {number}");
        let id = SeqId(self.doc.alloc_id());
        let active = s.active();
        let mut variant = FormatVariant { id: VariantId(self.doc.alloc_id()), name: active.name.clone(), size: active.size, overrides: BTreeMap::new() };
        let mut inner = Sequence::new(id, &name, s.rate, variant.clone());
        for (t, track) in s.tracks.iter().enumerate() {
            let mine: Vec<&Item> = chosen.iter().filter(|(ct, _)| *ct == t).map(|(_, i)| *i).collect();
            if mine.is_empty() {
                continue;
            }
            let mut copy = Track::new(TrackId(self.doc.alloc_id()), &track.name, track.kind);
            for it in mine {
                let new_id = ItemId(self.doc.alloc_id());
                if let Some(o) = active.overrides.get(&it.id) {
                    variant.overrides.insert(new_id, o.clone());
                }
                let mut item = it.clone();
                item.id = new_id;
                item.group = None;
                item.range.start = it.range.start - start;
                copy.items.push(item);
            }
            inner.tracks.push(Arc::new(copy));
        }
        inner.variants = vec![variant];
        let mut ops = vec![Op::AddSequence(Arc::new(inner))];
        let mut clip = None;
        if nest {
            let range = TimeRange::new(start, end - start);
            ops.extend(chosen.iter().map(|(_, i)| Op::RemoveItem { seq: self.seq, item: i.id }));
            // On the first of their tracks (video first) that's free once they're gone,
            // or a temp video layer on top.
            let free = |t: &Track| !t.items.iter().any(|i| !items.contains(&i.id) && i.range.overlaps(range));
            let mut tracks: Vec<usize> = chosen.iter().map(|(t, _)| *t).collect();
            tracks.dedup();
            tracks.sort_by_key(|t| (s.tracks[*t].kind != TrackKind::Video, *t));
            let track = match tracks.into_iter().find(|t| free(&s.tracks[*t])) {
                Some(t) => s.tracks[t].id,
                None => {
                    let tid = TrackId(self.doc.alloc_id());
                    let count = s.tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.effects).count();
                    let index = s.tracks.iter().rposition(|t| t.kind == TrackKind::Video).map_or(0, |i| i + 1);
                    let layer = Track { temp: true, ..Track::new(tid, &format!("V{}", count + 1), TrackKind::Video) };
                    ops.push(Op::InsertTrack { seq: self.seq, index, track: Arc::new(layer) });
                    tid
                }
            };
            let item_id = ItemId(self.doc.alloc_id());
            ops.push(Op::InsertItem { seq: self.seq, track, item: Item::new(item_id, &name, ItemKind::Nested { sequence: id }, range) });
            clip = Some(item_id);
        }
        self.apply(if nest { "Nest clips" } else { tr("Make compound clip") }, ops)?;
        Ok((id, clip))
    }

    /// Whether compound `seq` shows anything: a clip on one of its picture tracks (a
    /// compound clip there counting only if it shows something itself). One made of
    /// sound alone doesn't — it goes on a sound track and draws as sound.
    pub fn sequence_has_picture(&self, seq: SeqId) -> bool {
        fn shows(p: &oa_doc::Project, seq: SeqId, depth: usize) -> bool {
            let Some(s) = p.sequence(seq).filter(|_| depth < 16) else { return false };
            s.tracks.iter().filter(|t| t.enabled && t.kind == TrackKind::Video && !t.effects).flat_map(|t| &t.items).any(|i| match i.kind {
                ItemKind::Nested { sequence } => shows(p, sequence, depth + 1),
                _ => i.enabled,
            })
        }
        shows(self.doc.project(), seq, 0)
    }

    /// The kind of track a clip of `kind` belongs on: sound files and compound clips of
    /// sound alone on a sound track, everything else on a picture track.
    pub fn track_kind_for(&self, kind: &ItemKind) -> TrackKind {
        match kind {
            ItemKind::Media { media } if self.pool_item(*media).is_some_and(|p| p.kind == MediaKind::Audio) => TrackKind::Audio,
            ItemKind::Nested { sequence } if !self.sequence_has_picture(*sequence) => TrackKind::Audio,
            _ => TrackKind::Video,
        }
    }

    /// Puts a clip of `kind` at `at` (on a frame boundary): on `preferred` if it's the
    /// right kind of track and free there, else the first free track of the kind, else a
    /// temp layer. One undo step.
    pub fn place_clip(&mut self, name: &str, kind: ItemKind, duration: Time, at: Time, preferred: Option<TrackId>) -> Result<ItemId, EditError> {
        let track_kind = self.track_kind_for(&kind);
        let rate = self.sequence().rate;
        let at = rate.frame_start(rate.frame_at(at.max(Time::ZERO)));
        let range = TimeRange::new(at, duration.max(Time::from_seconds(1)));
        let s = self.sequence();
        let free = |t: &Track| t.kind == track_kind && !t.effects && !t.items.iter().any(|i| i.range.overlaps(range));
        let found = preferred.and_then(|p| s.track(p)).filter(|t| free(t)).or_else(|| s.tracks.iter().map(|t| &**t).find(|t| free(t))).map(|t| t.id);
        let mut ops = Vec::new();
        let track = self.track_or_temp(found, track_kind, &mut ops);
        let id = ItemId(self.doc.alloc_id());
        ops.push(Op::InsertItem { seq: self.seq, track, item: Item::new(id, name, kind, range) });
        self.apply("Add clip", ops)?;
        Ok(id)
    }

    /// Deletes compound `seq` (refused while a clip uses it).
    pub fn remove_compound(&mut self, seq: SeqId) -> Result<(), EditError> {
        self.apply(tr("Delete compound clip"), vec![Op::RemoveSequence(seq)])
    }

    /// Applies a command's ops as one undo step (nothing happens for an empty list).
    pub fn apply(&mut self, label: &str, ops: Vec<Op>) -> Result<(), EditError> {
        if ops.is_empty() {
            return Ok(());
        }
        let ops = self.with_stretched_keys(ops, false);
        self.doc.edit(label, ops)
    }

    /// Like [`apply`](Self::apply) for a drag: consecutive updates with the same `key`
    /// merge into one undo step until [`Document::seal`].
    pub fn apply_drag(&mut self, label: &str, key: &str, ops: Vec<Op>) -> Result<(), EditError> {
        if ops.is_empty() {
            return Ok(());
        }
        let ops = self.with_stretched_keys(ops, false);
        self.doc.edit_coalesced(label, Some(key), ops)
    }

    /// Cuts `item` (or every clip under the playhead, if `None`) at `t`. Returns the new
    /// back-half item when a single clip was cut.
    pub fn split(&mut self, item: Option<ItemId>, t: Time) -> Result<Option<ItemId>, EditError> {
        let snapshot = self.doc.snapshot();
        let doc = &mut self.doc;
        let mut alloc = || doc.alloc_id();
        match item {
            Some(item) => {
                let (ops, back) = oa_edit::timeline::split(&snapshot, self.seq, item, t, &mut alloc)?;
                self.doc.edit("Split clip", ops)?;
                Ok(Some(back))
            }
            None => {
                let ops = oa_edit::timeline::split_all(&snapshot, self.seq, t, &[], &mut alloc)?;
                self.apply("Split", ops)?;
                Ok(None)
            }
        }
    }

    /// Cuts each of `items` at `t` (skipping any too close to an edge there), in one
    /// undo step. Returns the new back halves.
    pub fn split_many(&mut self, items: &[ItemId], t: Time) -> Result<Vec<ItemId>, EditError> {
        let snapshot = self.doc.snapshot();
        let doc = &mut self.doc;
        let mut alloc = || doc.alloc_id();
        let (mut ops, mut backs) = (Vec::new(), Vec::new());
        let mut last_err = None;
        for &item in items {
            match oa_edit::timeline::split(&snapshot, self.seq, item, t, &mut alloc) {
                Ok((more, back)) => {
                    ops.extend(more);
                    backs.push(back);
                }
                Err(e) => last_err = Some(e),
            }
        }
        if ops.is_empty() {
            return Err(last_err.unwrap_or(EditError::InvalidRange));
        }
        self.doc.edit(if items.len() == 1 { "Split clip" } else { "Split clips" }, ops)?;
        Ok(backs)
    }

    /// Where a clip's keyframes are on the timeline as format `variant` shows it: its
    /// own values (a value the format overrides counting from the override instead),
    /// its effects' and its transitions'. Only the ones inside the clip — a split leaves
    /// keys past each half's ends, where nothing can be seen of them.
    pub fn clip_key_times(seq: &oa_doc::Sequence, it: &Item, variant: oa_doc::VariantId) -> Vec<Time> {
        let overrides = seq.variant(variant).and_then(|v| v.overrides.get(&it.id));
        let own = it.params.0.iter().filter(|(id, _)| overrides.is_none_or(|o| o.get(id.as_str()).is_none())).map(|(_, s)| s);
        let mut keys: Vec<Time> = own
            .chain(overrides.into_iter().flat_map(|o| o.0.values()))
            .chain(it.effects.iter().flat_map(|e| e.params.0.values()))
            .chain([oa_doc::ClipEnd::Head, oa_doc::ClipEnd::Tail].into_iter().filter_map(|end| it.transition(end)).flat_map(|t| t.params.0.values()))
            .filter_map(|s| s.curve().filter(|c| c.anchor == KeyframeAnchor::ClipStart))
            .flat_map(|c| c.keys.iter().map(|k| k.t))
            .filter(|t| *t >= Time::ZERO && *t <= it.range.duration)
            .map(|t| it.range.start + t)
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Where Ctrl+←/→ stops on a clip, as frames of the timeline at `rate`, in order: its
    /// first frame (the first that shows it), each keyframe's frame (`keys`: timeline
    /// times) and its last frame (the one its end falls in, or just before).
    /// Which clip of a track Alt+← / Alt+→ goes to (`starts`: each clip's first frame, in
    /// order; `at`: the clip it goes from, if any; `now`: the playhead's frame): the next
    /// one, or the one before — back from inside a clip, that clip's own start first. On
    /// no clip: the first starting after the playhead, or the last before it.
    pub fn next_clip(starts: &[i64], at: Option<usize>, now: i64, forward: bool) -> Option<usize> {
        match (at, forward) {
            (Some(i), true) => Some(i + 1).filter(|n| *n < starts.len()),
            (Some(i), false) if starts[i] < now => Some(i),
            (Some(i), false) => i.checked_sub(1),
            (None, true) => starts.iter().position(|s| *s > now),
            (None, false) => starts.iter().rposition(|s| *s < now),
        }
    }

    pub fn clip_stops(it: &Item, keys: &[Time], rate: oa_time::FrameRate) -> Vec<i64> {
        let first = {
            let f = rate.frame_at(it.range.start);
            if rate.frame_start(f) < it.range.start { f + 1 } else { f }
        };
        let last = rate.frame_at(it.range.end() - Time(1)).max(first);
        let mut stops: Vec<i64> = std::iter::once(first).chain(keys.iter().map(|k| rate.frame_at(*k).clamp(first, last))).chain(std::iter::once(last)).collect();
        stops.sort();
        stops.dedup();
        stops
    }

    /// Where a clip or effect parameter's keyframes are, in clip-local seconds.
    pub fn keyframe_times(&self, item: ItemId, target: &ParamTarget, param: &str) -> Vec<Time> {
        let Some(it) = self.item(item) else { return Vec::new() };
        let Some(source) = self.source_of(it, target, param) else { return Vec::new() };
        let Some(curve) = source.curve() else { return Vec::new() };
        let offset = match curve.anchor {
            KeyframeAnchor::ClipStart => Time::ZERO,
            // Source-anchored keys: convert to clip time (speed 1 assumed for display).
            KeyframeAnchor::SourceMedia => Time::ZERO - it.time_map.source_in,
        };
        curve.keys.iter().map(|k| k.t + offset).collect()
    }

    /// Clips these ops play at a new speed — and so make longer or shorter — have their
    /// keys stretched with them, so the animation stays with the footage (unless the clip
    /// keeps its key times). A trim (either end, or a length typed) leaves them where they
    /// are: the clip shows more or less, at the same pace. `ramp`: the batch is an edit of
    /// speed ramps (`with_speed_lengths`), whose new lengths are speed changes too. Not in
    /// a batch that brings in a new clip: that's a cut (or a paste), and a cut's halves
    /// carry the curve on exactly where it was. A clip moved to another track on the way
    /// (re-inserted, same id) has its own keys stretched where it goes.
    fn with_stretched_keys(&self, mut ops: Vec<Op>, ramp: bool) -> Vec<Op> {
        let s = self.sequence();
        if ops.iter().any(|op| matches!(op, Op::InsertItem { seq, item, .. } if *seq == self.seq && s.item(item.id).is_none())) {
            return ops;
        }
        let factor = |old: &Item, new: Time, map: &oa_doc::TimeMap| {
            let new_speed = ramp || map.speed != old.time_map.speed;
            (new_speed && !old.keep_key_times && new != old.range.duration && old.range.duration > Time::ZERO && new > Time::ZERO).then(|| new.0 as f64 / old.range.duration.0 as f64)
        };
        let mut more = Vec::new();
        for op in &mut ops {
            match op {
                Op::SetItemTiming { seq, item, range, time_map } if *seq == self.seq => {
                    if let Some(old) = self.item(*item)
                        && let Some(k) = factor(old, range.duration, time_map)
                    {
                        more.extend(self.stretch_ops(old, k));
                    }
                }
                Op::InsertItem { seq, item, .. } if *seq == self.seq => {
                    if let Some(old) = self.item(item.id)
                        && let Some(k) = factor(old, item.range.duration, &item.time_map)
                    {
                        item.stretch_keys(k);
                        more.extend(self.stretch_ops(old, k).into_iter().filter(|op| matches!(op, Op::SetParam { target: ParamTarget::VariantOverride(_), .. })));
                    }
                }
                _ => {}
            }
        }
        ops.extend(more);
        ops
    }

    /// The edits stretching `old`'s keys by `factor` (`Item::stretch_keys`): its
    /// properties', its effects' and its per-format overrides'.
    fn stretch_ops(&self, old: &Item, factor: f64) -> Vec<Op> {
        let mut new = old.clone();
        new.stretch_keys(factor);
        let set = |target: ParamTarget, param: &ParamId, source: &ParamSource| Op::SetParam { seq: self.seq, item: old.id, target, param: param.clone(), source: Some(source.clone()) };
        let mut ops: Vec<Op> = new.params.0.iter().filter(|(id, src)| old.params.0.get(*id) != Some(*src)).map(|(id, src)| set(ParamTarget::Item, id, src)).collect();
        for (fx, was) in new.effects.iter().zip(&old.effects) {
            ops.extend(fx.params.0.iter().filter(|(id, src)| was.params.0.get(*id) != Some(*src)).map(|(id, src)| set(ParamTarget::Effect(fx.id), id, src)));
        }
        for v in &self.sequence().variants {
            for (id, src) in v.overrides.get(&old.id).into_iter().flat_map(|o| o.0.iter()) {
                let mut stretched = src.clone();
                stretched.scale_clip_clock(factor);
                if stretched != *src {
                    ops.push(set(ParamTarget::VariantOverride(v.id), id, &stretched));
                }
            }
        }
        ops
    }

    /// Edits of a clip's speed (its ramp) keep the clip playing the same part of its
    /// file: it grows as it slows and shrinks as it speeds up, moving up a track (onto a
    /// temp layer if need be) when it runs into the next clip. Other ops pass through.
    fn with_speed_lengths(&mut self, ops: Vec<Op>) -> Vec<Op> {
        if !ops.iter().any(|op| matches!(op, Op::SetParam { target: ParamTarget::Item, param, .. } if param.as_str() == oa_doc::schema::SPEED)) {
            return self.with_stretched_keys(ops, false);
        }
        let project = self.doc.snapshot();
        let mut out = Vec::new();
        for op in ops {
            match op {
                Op::SetParam { seq, item, target: ParamTarget::Item, param, source } if param.as_str() == oa_doc::schema::SPEED => {
                    let doc = &mut self.doc;
                    match oa_edit::timeline::set_speed_ramp(&project, seq, item, source.clone(), &mut || doc.alloc_id()) {
                        Ok(more) => out.extend(more),
                        Err(_) => out.push(Op::SetParam { seq, item, target: ParamTarget::Item, param, source }),
                    }
                }
                other => out.push(other),
            }
        }
        self.with_stretched_keys(out, true)
    }

    /// Every key of `param` goes; the value at `t` stays (procedural motion too).
    pub fn stop_keyframing(&mut self, item: ItemId, target: ParamTarget, param: &str, t: Time) -> Result<(), EditError> {
        let mut ops = Vec::new();
        for (item, target) in self.fan_out(item, &target, param) {
            let Some(it) = self.item(item) else { continue };
            let Some(src) = self.source_of(it, &target, param).filter(|s| s.curve().is_some()) else { continue };
            let value = base_value(&src, &it.eval_context(t));
            let source = match src {
                ParamSource::Modulated { modulator, .. } => ParamSource::Modulated { base: Box::new(ParamSource::Static(value)), modulator },
                _ => ParamSource::Static(value),
            };
            ops.push(Op::SetParam { seq: self.seq, item, target, param: ParamId::new(param), source: Some(source) });
        }
        let ops = self.with_speed_lengths(ops);
        self.doc.edit("Stop keyframing", ops)
    }

    /// The keyframe diamond at `t`: not keyframed yet, keyframing starts (the value
    /// there becomes the first key); keyframed with no key there, the value there
    /// becomes a key; on a key, that key goes — and with the last one, keyframing stops
    /// (the value there stays). `target` may be a format's override: it starts from the
    /// clip's own value, as an edit there does.
    pub fn toggle_keyframing(
        &mut self,
        item: ItemId,
        target: ParamTarget,
        param: &str,
        default: Value,
        anchor: KeyframeAnchor,
        t: Time,
    ) -> Result<(), EditError> {
        #[derive(PartialEq)]
        enum Diamond {
            Start,
            AddKey,
            RemoveKey,
        }
        let it = self.item(item).ok_or(EditError::NotFound("item", item.0))?;
        // Every linked clip goes the way the clicked one does.
        let ctx = it.eval_context(t);
        let action = match self.source_of(it, &target, param).as_ref().and_then(|s| s.curve()) {
            None => Diamond::Start,
            Some(c) if c.keys.iter().any(|k| k.t == c.clock(&ctx)) => Diamond::RemoveKey,
            Some(_) => Diamond::AddKey,
        };
        let mut ops = Vec::new();
        for (item, target) in self.fan_out(item, &target, param) {
            let Some(it) = self.item(item) else { continue };
            let ctx = it.eval_context(t);
            let current = self.source_of(it, &target, param);
            // A key added away from the clip's start gets one at the start too (if there's
            // none), holding the value shown there — so what's typed at the playhead
            // animates from how the clip begins, rather than holding all the way back.
            let start = it.eval_context(it.range.start);
            let keyed_at_start = |src: &mut ParamSource| {
                let missing = src.curve().is_some_and(|c| !c.keys.iter().any(|k| k.t == c.clock(&start)));
                if missing {
                    let value = src.eval(&start);
                    src.set_at(&start, value);
                }
            };
            let source = match (&action, current) {
                (Diamond::Start, Some(src)) if src.curve().is_some() => continue,
                (Diamond::Start, Some(src)) => {
                    let mut src = src.keyframed(&ctx, anchor);
                    keyed_at_start(&mut src);
                    src
                }
                (Diamond::Start, None) => {
                    let mut src = ParamSource::Static(default.clone()).keyframed(&ctx, anchor);
                    keyed_at_start(&mut src);
                    src
                }
                (Diamond::AddKey, Some(mut src)) if src.curve().is_some() => {
                    src.confine(it.range.duration);
                    let value = src.eval(&ctx);
                    keyed_at_start(&mut src);
                    src.set_at(&ctx, value);
                    src
                }
                (Diamond::RemoveKey, Some(mut src)) if src.curve().is_some() => {
                    src.confine(it.range.duration);
                    let value = base_value(&src, &ctx);
                    let curve = src.curve_mut().expect("checked");
                    let at = curve.clock(&ctx);
                    curve.remove_key(at);
                    if curve.keys.is_empty() {
                        match src {
                            // Keep procedural motion; only the keys go.
                            ParamSource::Modulated { modulator, .. } => ParamSource::Modulated { base: Box::new(ParamSource::Static(value)), modulator },
                            _ => ParamSource::Static(value),
                        }
                    } else {
                        src
                    }
                }
                _ => continue,
            };
            ops.push(Op::SetParam { seq: self.seq, item, target, param: ParamId::new(param), source: Some(source) });
        }
        let label = match action {
            Diamond::Start => "Keyframing",
            Diamond::AddKey => "Add keyframe",
            Diamond::RemoveKey => "Remove keyframe",
        };
        let ops = self.with_speed_lengths(ops);
        self.doc.edit(label, ops)
    }

    /// Value of a clip or effect parameter at `t`.
    pub fn param_value(&self, item: ItemId, target: &ParamTarget, param: &str, t: Time) -> Option<Value> {
        let it = self.item(item)?;
        Some(self.source_of(it, target, param)?.eval(&it.eval_context(t)))
    }

    /// How a parameter is stored (static, keyframed…), if it has been set.
    pub fn param_source(&self, item: ItemId, target: &ParamTarget, param: &str) -> Option<ParamSource> {
        self.source_of(self.item(item)?, target, param)
    }

    /// Where `param` of `it` at `target` comes from — a format's override included: the
    /// override, or (none yet) the clip's own value it would start from.
    fn source_of(&self, it: &Item, target: &ParamTarget, param: &str) -> Option<ParamSource> {
        match target {
            ParamTarget::VariantOverride(v) => {
                let own = self.sequence().variant(*v).and_then(|v| v.overrides.get(&it.id)).and_then(|o| o.get(param)).cloned();
                own.or_else(|| it.params.get(param).cloned())
            }
            _ => Self::param_set(it, target)?.get(param).cloned(),
        }
    }

    fn param_set<'a>(it: &'a Item, target: &ParamTarget) -> Option<&'a oa_params::ParamSet> {
        match target {
            ParamTarget::Item => Some(&it.params),
            ParamTarget::Effect(id) => it.effects.iter().find(|e| e.id == *id).map(|e| &e.params),
            ParamTarget::Transition(end) => it.transition(*end).map(|t| &t.params),
            ParamTarget::Group(g) => it.effect_group(*g).map(|g| &g.params),
            ParamTarget::VariantOverride(_) => None,
        }
    }

    /// The clip (`item`, at `target`) and the same parameter on each linked clip: the
    /// same place on a clip of a kind that has it (a title's text settings only on
    /// titles), the matching effect (the same kind, the same how-many-th) for an
    /// effect's parameter.
    fn fan_out(&self, item: ItemId, target: &ParamTarget, param: &str) -> Vec<(ItemId, ParamTarget)> {
        let mut out = vec![(item, target.clone())];
        let Some(it) = self.item(item) else { return out };
        for &other_id in self.linked.iter().filter(|l| **l != item) {
            let Some(other) = self.item(other_id) else { continue };
            let there = match target {
                ParamTarget::Item => {
                    let fits = match param.split('.').next() {
                        Some("text") => other.kind == ItemKind::Text,
                        Some("solid") => other.kind == ItemKind::Solid,
                        // One mask's values belong to that mask alone.
                        Some("mask") => oa_doc::mask::mask_of_param(param).is_none_or(|m| other.masks.iter().any(|x| x.id == m)),
                        _ => true,
                    };
                    fits.then_some(ParamTarget::Item)
                }
                ParamTarget::Effect(id) => it.effects.iter().find(|e| e.id == *id).and_then(|fx| {
                    let nth = it.effects.iter().take_while(|e| e.id != *id).filter(|e| e.type_id == fx.type_id).count();
                    other.effects.iter().filter(|e| e.type_id == fx.type_id).nth(nth).map(|e| ParamTarget::Effect(e.id))
                }),
                _ => None,
            };
            out.extend(there.map(|t| (other_id, t)));
        }
        out
    }

    /// Which of its kind an effect is on its clip: its type, and how many of that type
    /// come before it. Effects match across clips by this.
    pub fn effect_key(&self, item: ItemId, effect: oa_doc::EffectId) -> Option<(String, usize)> {
        let it = self.item(item)?;
        let fx = it.effects.iter().find(|e| e.id == effect)?;
        let nth = it.effects.iter().take_while(|e| e.id != effect).filter(|e| e.type_id == fx.type_id).count();
        Some((fx.type_id.clone(), nth))
    }

    /// The same effect on the linked clips (the rest of a multiple selection).
    pub fn effect_peers(&self, item: ItemId, effect: oa_doc::EffectId) -> Vec<(ItemId, oa_doc::EffectId)> {
        let Some((type_id, nth)) = self.effect_key(item, effect) else { return Vec::new() };
        self.linked
            .iter()
            .filter(|l| **l != item)
            .filter_map(|&other| self.item(other)?.effects.iter().filter(|e| e.type_id == type_id).nth(nth).map(|e| (other, e.id)))
            .collect()
    }

    /// The trims giving `item` and the rest of the selection (`Editor::linked`) the length
    /// `length(clip)` says — `None`: as long as each can be — and how many fell short of it
    /// (stopped by the file's end or the next clip). Each is worked out on the timeline as
    /// it is, so they never depend on one another.
    pub fn length_ops(&self, item: ItemId, length: &dyn Fn(&Item) -> Option<Time>) -> (Vec<oa_doc::Op>, usize) {
        let project = self.doc.project();
        let (seq, s) = (self.seq, self.sequence());
        let frame = s.rate.frame_start(1);
        let mut ops = Vec::new();
        let mut short = 0;
        let mut ids = vec![item];
        ids.extend(self.linked.iter().copied().filter(|l| *l != item));
        for id in ids {
            let Some(it) = self.item(id) else { continue };
            let asked = length(it).map(|l| oa_edit::timeline::snap_to_frame(s, it.range.start + l.max(frame)));
            // As far as it goes: the trim stops at the file's end and the next clip.
            let to = asked.unwrap_or(Time::from_seconds(1_000_000));
            match oa_edit::timeline::trim(project, seq, id, oa_edit::timeline::Edge::Tail, to) {
                Ok(o) => {
                    let got = o
                        .iter()
                        .find_map(|op| match op {
                            oa_doc::Op::SetItemTiming { range, .. } => Some(range.end()),
                            _ => None,
                        })
                        .unwrap_or(it.range.end());
                    if asked.is_some_and(|a| got != a) {
                        short += 1;
                    }
                    ops.extend(o);
                }
                Err(_) => short += 1,
            }
        }
        (ops, short)
    }

    /// `ops` made for one clip, done to the linked clips too: an effect switched on or
    /// off, removed or retimed happens to the same effect on each; an intro reversed as
    /// the outro, on each clip. (Moves within one clip's list stay its own.)
    pub fn fanned(&self, ops: Vec<Op>) -> Vec<Op> {
        let moved: Vec<oa_doc::EffectId> = ops.iter().filter_map(|op| if let Op::InsertEffect { effect, .. } = op { Some(effect.id) } else { None }).collect();
        let mut out = ops.clone();
        for op in &ops {
            match op {
                Op::SetEffectEnabled { seq, item, effect, enabled } => {
                    out.extend(self.effect_peers(*item, *effect).into_iter().map(|(item, effect)| Op::SetEffectEnabled { seq: *seq, item, effect, enabled: *enabled }));
                }
                Op::RemoveEffect { seq, item, effect } if !moved.contains(effect) => {
                    out.extend(self.effect_peers(*item, *effect).into_iter().map(|(item, effect)| Op::RemoveEffect { seq: *seq, item, effect }));
                }
                Op::SetEffectRole { seq, item, effect, role } => {
                    for (other, effect) in self.effect_peers(*item, *effect) {
                        // An intro or outro no longer than the clip.
                        let length = self.item(other).map_or(Time::MAX, |i| i.range.duration);
                        let role = match *role {
                            oa_doc::EffectRole::In { duration } => oa_doc::EffectRole::In { duration: duration.min(length) },
                            oa_doc::EffectRole::Out { duration } => oa_doc::EffectRole::Out { duration: duration.min(length) },
                            r => r,
                        };
                        out.push(Op::SetEffectRole { seq: *seq, item: other, effect, role });
                    }
                }
                Op::SetReverseIntro { seq, item, on } => {
                    out.extend(self.linked.iter().filter(|l| **l != *item).map(|&other| Op::SetReverseIntro { seq: *seq, item: other, on: *on }));
                }
                _ => {}
            }
        }
        out
    }

    /// The `type_id` effect on `item` (the first of that kind), added if it isn't there —
    /// first in its chain, so it works on the picture as it comes in (as a grade does) —
    /// on the linked clips too, so an edit made to it reaches each of them.
    pub fn ensure_effect(&mut self, item: ItemId, type_id: &str) -> Option<oa_doc::EffectId> {
        let mut ops = Vec::new();
        let mut mine = None;
        let ids: Vec<ItemId> = std::iter::once(item).chain(self.linked.iter().copied().filter(|l| *l != item)).collect();
        for id in ids {
            let Some(it) = self.item(id) else { continue };
            if let Some(fx) = it.effects.iter().find(|e| e.type_id == type_id) {
                if id == item {
                    mine = Some(fx.id);
                }
                continue;
            }
            let effect = oa_doc::EffectInstance::new(oa_doc::EffectId(self.doc.alloc_id()), type_id);
            if id == item {
                mine = Some(effect.id);
            }
            ops.push(Op::InsertEffect { seq: self.seq, item: id, index: 0, effect });
        }
        if !ops.is_empty()
            && let Err(e) = self.doc.edit_coalesced("Color", Some("color-add"), ops)
        {
            eprintln!("edit failed: {e}");
            return None;
        }
        mine
    }

    /// Replaces a parameter's source outright (coalescing drags under `drag_key`) — on
    /// the linked clips too.
    pub fn set_param(&mut self, item: ItemId, target: ParamTarget, param: &str, source: ParamSource, drag_key: &str) {
        let ops = self
            .fan_out(item, &target, param)
            .into_iter()
            .map(|(item, target)| Op::SetParam { seq: self.seq, item, target, param: ParamId::new(param), source: Some(source.clone()) })
            .collect();
        let ops = self.with_speed_lengths(ops);
        if let Err(e) = self.doc.edit_coalesced(drag_key, Some(drag_key), ops) {
            eprintln!("edit failed: {e}");
        }
    }

    /// Sets a parameter's value at timeline time `t` the way a slider should: a key at
    /// `t` if the parameter is keyframed, otherwise a new static value. Each linked clip
    /// gets the same value its own way (its own keys stay its own).
    pub fn set_value_at(&mut self, item: ItemId, target: ParamTarget, param: &str, value: Value, t: Time, drag_key: &str) {
        let mut ops = Vec::new();
        for (item, target) in self.fan_out(item, &target, param) {
            let Some(it) = self.item(item) else { continue };
            let ctx = it.eval_context(t);
            let source = match self.source_of(it, &target, param) {
                Some(existing) => {
                    let mut src = existing;
                    // Keys left outside the clip (by a split) don't pull on this one.
                    src.confine(it.range.duration);
                    src.set_at(&ctx, value.clone());
                    src
                }
                None => ParamSource::Static(value.clone()),
            };
            ops.push(Op::SetParam { seq: self.seq, item, target, param: ParamId::new(param), source: Some(source) });
        }
        let ops = self.with_speed_lengths(ops);
        if let Err(e) = self.doc.edit_coalesced(drag_key, Some(drag_key), ops) {
            eprintln!("edit failed: {e}");
        }
    }

    /// A sequence-wide setting (the background) at `t`: a key there if it's keyframed,
    /// otherwise a new value. Drags coalesce by `drag_key`.
    pub fn set_sequence_value(&mut self, param: &str, value: Value, t: Time, drag_key: &str) {
        let ctx = oa_params::EvalContext::at(t, t);
        let source = match self.sequence().params.get(param) {
            Some(existing) => {
                let mut src = existing.clone();
                src.set_at(&ctx, value);
                src
            }
            None => ParamSource::Static(value),
        };
        let op = Op::SetSequenceParam { seq: self.seq, param: ParamId::new(param), source: Some(source) };
        if let Err(e) = self.doc.edit_coalesced(drag_key, Some(drag_key), vec![op]) {
            eprintln!("edit failed: {e}");
        }
    }

    /// The sequence's settings at `t` (`schema::background()` and defaults).
    pub fn sequence_values(&self, t: Time) -> oa_params::Evaluated {
        self.sequence().params.eval(schema::background(), None, &oa_params::EvalContext::at(t, t))
    }

    pub fn add_variant(&mut self, preset: &str) -> Result<usize, EditError> {
        let p = AspectPreset::by_id(preset).expect("preset");
        let short = self.sequence().canvas().short_edge();
        let id = VariantId(self.doc.alloc_id());
        let variant = FormatVariant { id, name: p.name.into(), size: p.size(short), overrides: Default::default() };
        let index = self.sequence().variants.len();
        self.doc.edit("Add format", vec![Op::InsertVariant { seq: self.seq, index, variant, make_active: false }])?;
        Ok(index)
    }

    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        let json = ProjectFile::to_json(self.doc.project()).map_err(|e| e.to_string())?;
        // Atomic: a crash mid-save leaves the previous version intact, never half a file.
        crate::autosave::write_atomic(path, json.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.saved = Some(self.doc.snapshot());
        Ok(())
    }

    /// A save of `snapshot` to `path` made elsewhere (on a thread) has finished. Edits made
    /// since keep the project marked changed.
    pub fn mark_saved(&mut self, path: &Path, snapshot: Arc<Project>) {
        self.path = Some(path.to_path_buf());
        self.saved = Some(snapshot);
    }

    /// After opening an autosave: it belongs to `original` (if it was ever saved) and has
    /// changes that aren't in any real file yet.
    pub fn mark_recovered(&mut self, original: Option<PathBuf>) {
        self.path = original;
        self.saved = None;
    }

    /// Loads a project and re-establishes its media: relinking files that moved and
    /// re-importing each one (so conformed copies and frame indexes come back).
    pub fn open(path: &Path, import: impl Fn(&Path) -> Result<Imported, String> + Sync, progress: &(dyn Fn(usize, usize) + Sync), smart: bool) -> Result<(Self, Vec<String>), String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (mut project, report) = ProjectFile::load(&text).map_err(|e| e.to_string())?;
        let seq = *project.sequences.keys().next().ok_or("project has no sequences")?;
        let sequence = project.sequence(seq).expect("just found");
        let video_track = sequence.tracks.iter().find(|t| t.kind == TrackKind::Video && !t.effects).map(|t| t.id).ok_or("project has no video track")?;
        let audio_track = sequence.tracks.iter().find(|t| t.kind == TrackKind::Audio && !t.effects).map(|t| t.id).unwrap_or(video_track);

        let search_dirs: Vec<PathBuf> = [path.parent().map(Path::to_path_buf), path.parent().map(|p| p.join("samples"))]
            .into_iter()
            .flatten()
            .collect();

        let mut warnings: Vec<String> = report.repaired.iter().map(|m| format!("repaired: {m}")).collect();
        warnings.extend(report.warnings.iter().cloned());
        let mut pool = Vec::new();
        let mut relinks = Vec::new();
        // Where each file is (or was moved to)…
        let mut found: Vec<Option<PathBuf>> = Vec::with_capacity(project.media.len());
        for media in project.media.values() {
            let stored = PathBuf::from(&media.path);
            found.push(if stored.is_file() {
                Some(stored.clone())
            } else {
                let relinked = media
                    .fingerprint
                    .as_deref()
                    .and_then(|fp| oa_media::import::relink(&stored, fp, &search_dirs));
                if let Some(found) = &relinked {
                    warnings.push(format!("relinked {} → {}", media.path, found.display()));
                    relinks.push((media.id, found.clone()));
                }
                relinked
            });
        }
        // …anything still missing, looked for around this computer (Settings: smart
        // relink): the same name and content, or the same content renamed.
        if smart {
            let medias: Vec<&Arc<MediaRef>> = project.media.values().collect();
            let lost: Vec<usize> = (0..medias.len()).filter(|&i| found[i].is_none()).collect();
            if !lost.is_empty() {
                let wanted: Vec<oa_media::find::Missing> = lost.iter().map(|&i| oa_media::find::Missing { path: PathBuf::from(&medias[i].path), fingerprint: medias[i].fingerprint.clone() }).collect();
                let paths: Vec<PathBuf> = wanted.iter().map(|m| m.path.clone()).collect();
                let roots = crate::projects::relink_roots(path.parent(), &paths);
                let hits = oa_media::find::find_moved(&wanted, &roots, SMART_RELINK_BUDGET);
                for (&i, hit) in lost.iter().zip(hits) {
                    if let Some(p) = hit {
                        warnings.push(format!("relinked {} → {}", medias[i].path, p.display()));
                        relinks.push((medias[i].id, p.clone()));
                        found[i] = Some(p);
                    }
                }
            }
        }
        // …then all of them taken in, several at once: one at a time, a project with a
        // few hundred files took that many probes end to end.
        let results = import_all(&found, &import, progress);
        // Media imported before fingerprints were kept get one now, so a later search can
        // tell them by their content. (Not an edit: it's what the file is.)
        let mut prints = Vec::new();
        for (media, result) in project.media.values().zip(results) {
            if let (None, Some(Ok(imported))) = (&media.fingerprint, &result) {
                prints.push((media.id, imported.fingerprint.clone()));
            }
            match result {
                Some(Ok(imported)) => pool.push(PoolItem {
                    id: media.id,
                    name: imported.name(),
                    kind: imported.kind,
                    decode_path: imported.decode_path,
                    probe: imported.probe,
                    conformed: imported.conformed,
                    missing: false,
                }),
                Some(Err(e)) => {
                    warnings.push(format!("{}: {e}", media.path));
                    pool.push(missing_item(media));
                }
                None => {
                    warnings.push(format!("missing: {}", media.path));
                    pool.push(missing_item(media));
                }
            }
        }

        for (id, fp) in prints {
            if let Some(m) = project.media.get_mut(&id) {
                Arc::make_mut(m).fingerprint = Some(fp);
            }
        }
        // Projects saved before color tags were kept: take them from the fresh probe, so
        // an HDR file's automatic input transform finds it's HDR. (Not an edit — it's
        // what the file always said.)
        for item in &pool {
            let Some(v) = item.probe.video.as_ref() else { continue };
            let tags = oa_doc::color::ColorTags { transfer: v.transfer_tag.clone(), primaries: v.primaries_tag.clone() };
            if let Some(m) = project.media.get_mut(&item.id)
                && m.info.as_ref().is_some_and(|i| i.color.is_empty() && !tags.is_empty())
                && let Some(info) = Arc::make_mut(m).info.as_mut()
            {
                info.color = tags;
            }
            // Likewise transparency, which older projects didn't keep (and VP9's was missed).
            if let Some(m) = project.media.get_mut(&item.id)
                && m.info.as_ref().is_some_and(|i| i.alpha != v.has_alpha)
                && let Some(info) = Arc::make_mut(m).info.as_mut()
            {
                info.alpha = v.has_alpha;
            }
        }
        let mut doc = Document::new(project);
        for (id, found) in relinks {
            let media = doc.project().media(id).expect("in project").clone();
            let updated = MediaRef { path: found.to_string_lossy().to_string(), ..media };
            let _ = doc.edit("Relink media", vec![Op::ReplaceMedia(Arc::new(updated))]);
        }
        let saved = Some(doc.snapshot());
        Ok((Editor { doc, seq, video_track, audio_track, pool, path: Some(path.to_path_buf()), saved, linked: Vec::new(), index: Default::default() }, warnings))
    }
}

/// How long a smart relink may look for a project's moved media when it opens.
const SMART_RELINK_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);

/// Takes in each file found (`None`: not found, skipped), a few at a time on worker
/// threads — as many as there are cores, two to eight — telling `progress` (done, of how
/// many) as each finishes. The results are in the files' order.
fn import_all(found: &[Option<PathBuf>], import: &(impl Fn(&Path) -> Result<Imported, String> + Sync), progress: &(dyn Fn(usize, usize) + Sync)) -> Vec<Option<Result<Imported, String>>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let total = found.len();
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8).min(total.max(1));
    let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let results: Vec<std::sync::Mutex<Option<Result<Imported, String>>>> = (0..total).map(|_| std::sync::Mutex::new(None)).collect();
    progress(0, total);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= total {
                        break;
                    }
                    if let Some(path) = &found[i] {
                        *results[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(import(path));
                    }
                    progress(done.fetch_add(1, Ordering::SeqCst) + 1, total);
                }
            });
        }
    });
    results.into_iter().map(|r| r.into_inner().unwrap_or_else(|e| e.into_inner())).collect()
}

fn missing_item(media: &MediaRef) -> PoolItem {
    let info = media.info.clone();
    PoolItem {
        id: media.id,
        name: Path::new(&media.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| media.path.clone()),
        kind: match info.as_ref() {
            Some(i) if !i.has_video => MediaKind::Audio,
            _ => MediaKind::Video,
        },
        decode_path: PathBuf::from(&media.path),
        probe: MediaProbe { container: String::new(), duration: info.map(|i| i.duration).unwrap_or(Time::ZERO), video: None, audio: None },
        conformed: None,
        missing: true,
    }
}

/// What `src` is at `ctx` under any procedural motion on top (a wave, the sound): the
/// value its keys give.
fn base_value(src: &ParamSource, ctx: &oa_params::EvalContext) -> Value {
    match src {
        ParamSource::Modulated { base, .. } => base_value(base, ctx),
        other => other.eval(ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn importer(path: &Path) -> Result<Imported, String> {
        // `true`: pretend the platform decoder copes, so nothing is conformed in tests.
        oa_media::import(path, |_, _| true).map_err(|e| e.to_string())
    }

    fn have_ffprobe() -> bool {
        std::process::Command::new("ffprobe").arg("-version").output().is_ok_and(|o| o.status.success())
    }

    /// A small picture written for the test (in `dir`), so the tests need no sample
    /// media in the repository.
    fn test_picture(dir: &Path) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("E.png");
        let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let mut encoder = png::Encoder::new(file, 16, 8);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let pixels: Vec<u8> = (0..16 * 8).flat_map(|i| [(i * 2) as u8, 90, 200, 255]).collect();
        encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
        path
    }

    /// A project keeps its media and clips across save/open, finds files that moved, and
    /// still loads (flagging what's gone) when they vanish.
    /// With clips linked (a multiple selection in the inspector), a value set on one is
    /// set on each, the way each is stored: a keyframed clip gets a key, a plain one a
    /// new value; switching keyframing on turns it on for all of them.
    /// The clip index follows every change: clips added, moved along their track by an
    /// edit before them, removed, and brought back by undo.
    /// The keyframe diamond: grey starts keyframing; gold without a key here adds one
    /// (it used to throw the keys away); on a key, that key goes, and with the last
    /// one the value stays. Later changes key the value — in a format's own override
    /// too, where the keys start from the clip's value.
    #[test]
    fn changing_a_keyframed_toggle_or_text_adds_a_key() {
        let mut e = Editor::new();
        let clip = e.add_text(Time::ZERO, "A", Time::from_seconds(10)).unwrap();
        let (t1, t2) = (Time::from_seconds(1), Time::from_seconds(5));
        for (p, was, now) in [(schema::TEXT_BOLD, Value::Bool(false), Value::Bool(true)), (schema::TEXT_CONTENT, Value::Text("A".into()), Value::Text("B".into()))] {
            e.toggle_keyframing(clip, ParamTarget::Item, p, was.clone(), KeyframeAnchor::ClipStart, t1).unwrap();
            e.set_value_at(clip, ParamTarget::Item, p, now.clone(), t2, p);
            assert_eq!(e.keyframe_times(clip, &ParamTarget::Item, p).len(), 3, "{p}: the clip's start, where keying began, and where it changed");
            assert_eq!(e.param_value(clip, &ParamTarget::Item, p, t1), Some(was), "{p}: the first key kept");
            assert_eq!(e.param_value(clip, &ParamTarget::Item, p, t2), Some(now));
        }
    }

    #[test]
    fn a_new_project_opens_in_the_format_and_rate_picked() {
        let e = Editor::with_setup("Trip", "portrait-4x5", FrameRate::FPS_24);
        let s = e.sequence();
        assert_eq!(e.doc.project().name, "Trip");
        assert_eq!(s.rate, FrameRate::FPS_24);
        assert_eq!((s.variants[0].size.width, s.variants[0].size.height), (1080, 1350));
        assert_eq!(s.variants.len(), 4, "landscape, vertical and square are there too");
        let vertical = Editor::with_setup("V", "vertical-9x16", FrameRate::FPS_30);
        let names: Vec<&str> = vertical.sequence().variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Vertical 9:16", "Landscape 16:9", "Square 1:1"]);
    }

    #[test]
    fn the_keyframe_diamond_adds_and_removes_keys() {
        let mut e = Editor::new();
        let clip = e.add_text(Time::ZERO, "A", Time::from_seconds(10)).unwrap();
        let p = schema::POSITION;
        let (t1, t2, t3) = (Time::from_seconds(1), Time::from_seconds(4), Time::from_seconds(7));
        let at = |e: &Editor, t: Time| e.param_value(clip, &ParamTarget::Item, p, t).and_then(|v| v.as_vec2()).unwrap()[0];
        let keys = |e: &Editor, target: &ParamTarget| e.keyframe_times(clip, target, p).len();
        e.toggle_keyframing(clip, ParamTarget::Item, p, Value::Vec2([0.0; 2]), KeyframeAnchor::ClipStart, t1).unwrap();
        assert_eq!(keys(&e, &ParamTarget::Item), 2, "here, and at the clip's start with the same value");
        assert_eq!(e.keyframe_times(clip, &ParamTarget::Item, p)[0], Time::ZERO);
        // Changed later: another key, and the value moves between them.
        e.set_value_at(clip, ParamTarget::Item, p, Value::Vec2([0.4, 0.0]), t2, "x");
        assert_eq!(keys(&e, &ParamTarget::Item), 3);
        assert!((at(&e, t2) - 0.4).abs() < 1e-9 && at(&e, t1).abs() < 1e-9);
        // The diamond where there's no key: a key with the value there.
        let before = at(&e, t3);
        e.toggle_keyframing(clip, ParamTarget::Item, p, Value::Vec2([0.0; 2]), KeyframeAnchor::ClipStart, t3).unwrap();
        assert_eq!(keys(&e, &ParamTarget::Item), 4);
        assert!((at(&e, t3) - before).abs() < 1e-9);
        // On a key: that one goes; the last one leaves the value it had.
        for t in [t3, t1, Time::ZERO] {
            e.toggle_keyframing(clip, ParamTarget::Item, p, Value::Vec2([0.0; 2]), KeyframeAnchor::ClipStart, t).unwrap();
        }
        assert_eq!(keys(&e, &ParamTarget::Item), 1);
        e.toggle_keyframing(clip, ParamTarget::Item, p, Value::Vec2([0.0; 2]), KeyframeAnchor::ClipStart, t2).unwrap();
        assert_eq!(keys(&e, &ParamTarget::Item), 0);
        assert!((at(&e, t1) - 0.4).abs() < 1e-9, "still where it was");

        // A second format, edited on its own: its keys live in its override, starting
        // from the clip's value, and edits there animate.
        e.add_variant("vertical-9x16").unwrap();
        let v = e.sequence().variants[1].id;
        let here = ParamTarget::VariantOverride(v);
        e.toggle_keyframing(clip, here.clone(), p, Value::Vec2([0.0; 2]), KeyframeAnchor::ClipStart, t1).unwrap();
        assert_eq!(keys(&e, &here), 2);
        assert_eq!(keys(&e, &ParamTarget::Item), 0, "the clip's own value is untouched");
        e.set_value_at(clip, here.clone(), p, Value::Vec2([-0.2, 0.0]), t2, "y");
        let o = |t: Time| e.param_value(clip, &here, p, t);
        assert_eq!(keys(&e, &here), 3);
        assert!(o(t1).is_some_and(|v| (v.as_vec2().unwrap()[0] - 0.4).abs() < 1e-9), "{:?}", o(t1));
        assert!(o(t2).is_some_and(|v| (v.as_vec2().unwrap()[0] + 0.2).abs() < 1e-9), "{:?}", o(t2));
    }

    /// A clip split off a keyframed one has a key past its end (where the other half
    /// carries on). Keying it inside and changing the value later holds that value: the
    /// key out of sight no longer pulls it back.
    #[test]
    fn keys_left_past_a_split_dont_pull_on_new_ones() {
        let mut e = Editor::new();
        let clip = e.add_text(Time::ZERO, "A", Time::from_seconds(2)).unwrap();
        let p = schema::POSITION;
        let outside = ParamSource::Animated(oa_params::Curve::new(KeyframeAnchor::ClipStart, vec![oa_params::Keyframe::linear(Time::from_seconds(3), Value::Vec2([0.0; 2]))]));
        e.doc.edit("keys", vec![Op::SetParam { seq: e.seq, item: clip, target: ParamTarget::Item, param: ParamId::new(p), source: Some(outside) }]).unwrap();
        let s = |x: f64| Time::from_seconds_f64(x);
        e.toggle_keyframing(clip, ParamTarget::Item, p, Value::Vec2([0.0; 2]), KeyframeAnchor::ClipStart, s(0.2)).unwrap();
        e.set_value_at(clip, ParamTarget::Item, p, Value::Vec2([0.5, 0.0]), s(1.0), "x");
        let x = |t: f64| e.param_value(clip, &ParamTarget::Item, p, s(t)).and_then(|v| v.as_vec2()).unwrap()[0];
        assert!((x(1.0) - 0.5).abs() < 1e-9 && (x(1.8) - 0.5).abs() < 1e-9, "held: {} {}", x(1.0), x(1.8));
    }

    /// The keys a clip shows (on the timeline, and for ◀ key / key ▶): those of the
    /// format on screen — its own value where it has one — and only inside the clip.
    #[test]
    fn clips_show_the_keys_of_the_format_on_screen() {
        let mut e = Editor::new();
        let clip = e.add_text(Time::from_seconds(10), "A", Time::from_seconds(2)).unwrap();
        let curve = |ts: &[i64]| ParamSource::Animated(oa_params::Curve::new(KeyframeAnchor::ClipStart, ts.iter().map(|t| oa_params::Keyframe::linear(Time::from_seconds(*t), Value::Vec2([0.0; 2]))).collect()));
        e.add_variant("vertical-9x16").unwrap();
        let (wide, tall) = (e.sequence().variants[0].id, e.sequence().variants[1].id);
        let set = |target: ParamTarget, ts: &[i64]| Op::SetParam { seq: e.seq, item: clip, target, param: ParamId::new(schema::POSITION), source: Some(curve(ts)) };
        let ops = vec![set(ParamTarget::Item, &[-1, 1, 5]), set(ParamTarget::VariantOverride(tall), &[0, 2])];
        e.doc.edit("keys", ops).unwrap();
        let it = e.item(clip).unwrap().clone();
        let secs = |v: Vec<Time>| v.into_iter().map(|t| t.as_seconds_f64()).collect::<Vec<_>>();
        assert_eq!(secs(Editor::clip_key_times(e.sequence(), &it, wide)), [11.0], "the one key inside the clip");
        assert_eq!(secs(Editor::clip_key_times(e.sequence(), &it, tall)), [10.0, 12.0], "the format's own keys");
    }

    #[test]
    fn the_clip_index_is_never_stale() {
        let mut e = Editor::new();
        let a = e.add_text(Time::from_seconds(10), "A", Time::from_seconds(2)).unwrap();
        assert_eq!(e.item(a).map(|i| i.id), Some(a));
        // A clip before it: A moves one place along the track.
        let b = e.add_text(Time::ZERO, "B", Time::from_seconds(2)).unwrap();
        assert_eq!(e.item(a).map(|i| i.id), Some(a));
        assert_eq!(e.item(b).map(|i| i.id), Some(b));
        e.doc.edit("remove", vec![Op::RemoveItem { seq: e.seq, item: b }]).unwrap();
        assert!(e.item(b).is_none(), "gone");
        assert_eq!(e.item(a).map(|i| i.id), Some(a), "and the one after it found in its new place");
        e.doc.undo().unwrap();
        assert_eq!(e.item(b).map(|i| i.id), Some(b), "back after undo");
        e.doc.redo().unwrap();
        assert!(e.item(b).is_none(), "and gone again after redo");
    }

    /// Ctrl+←/→ stops on a clip's first frame, its keyframes and its last frame — whole
    /// frames, even when its end (or a key) falls between two.
    #[test]
    fn clips_stop_at_their_edges_and_keys() {
        let rate = oa_time::FrameRate::FPS_30;
        let frame = |n: i64| rate.frame_start(n);
        // From frame 30 to just past frame 89 (a length no whole number of frames long).
        let it = Item::new(ItemId(1), "c", ItemKind::Solid, TimeRange::new(frame(30), frame(90) - frame(30) + Time(7)));
        let keys = [frame(45), frame(60) + Time(3), frame(200)];
        assert_eq!(Editor::clip_stops(&it, &keys, rate), [30, 45, 60, 90], "its end falls in frame 90: that's its last");
        let even = Item::new(ItemId(2), "d", ItemKind::Solid, TimeRange::new(frame(30), frame(60) - frame(30)));
        assert_eq!(Editor::clip_stops(&even, &[], rate), [30, 59], "ending on a boundary: the frame before it");
    }

    /// Alt+←/→ goes clip to clip along a track.
    #[test]
    fn alt_arrows_go_clip_to_clip() {
        let starts = [0, 30, 90];
        let go = |at, now, forward| Editor::next_clip(&starts, at, now, forward);
        assert_eq!(go(Some(0), 10, true), Some(1));
        assert_eq!(go(Some(2), 95, true), None, "past the last: nowhere");
        assert_eq!(go(Some(1), 50, false), Some(1), "back from inside a clip: its own start");
        assert_eq!(go(Some(1), 30, false), Some(0), "back from its start: the one before");
        assert_eq!(go(Some(0), 0, false), None);
        // In a gap (on no clip): the next to start, or the last that started.
        assert_eq!(go(None, 60, true), Some(2));
        assert_eq!(go(None, 60, false), Some(1));
        assert_eq!(go(None, 100, true), None);
    }

    /// Added to the timeline from the bin, a clip goes at the playhead on the first track
    /// with room there — over the clip there, not at the end of the bottom track.
    #[test]
    fn added_clips_go_at_the_playhead_where_there_is_room() {
        let mut e = Editor::new();
        let a = e.add_text(Time::ZERO, "A", Time::from_seconds(10)).unwrap();
        let (track_a, _) = e.sequence().find_item(a).unwrap();
        let b = e.place_clip("B", ItemKind::Solid, Time::from_seconds(2), Time::from_seconds(3), None).unwrap();
        let (track_b, _) = e.sequence().find_item(b).unwrap();
        assert_eq!(e.item(b).unwrap().range.start, Time::from_seconds(3), "at the playhead");
        assert_ne!(track_a, track_b, "on a layer with room, not over A");
        let c = e.place_clip("C", ItemKind::Solid, Time::from_seconds(2), Time::from_seconds(12), None).unwrap();
        assert_eq!(e.sequence().find_item(c).unwrap().0, track_a, "after A ends, A's track has room again");
    }

    /// A clip that finds no room goes on a temp layer, made in the same undo step as the
    /// clip and gone again with it.
    #[test]
    fn clips_without_room_go_on_temp_layers() {
        let mut e = Editor::new();
        e.add_text(Time::ZERO, "A", Time::from_seconds(4)).unwrap();
        let tracks = e.sequence().tracks.len();
        let b = e.add_text(Time::from_seconds(1), "B", Time::from_seconds(2)).unwrap();
        let (t, _) = e.sequence().find_item(b).unwrap();
        assert!(e.sequence().tracks[t].temp, "the title's new layer is a temp one");
        let c = e.place_clip("C", ItemKind::Text, Time::from_seconds(2), Time::from_seconds(1), None).unwrap();
        let (t, _) = e.sequence().find_item(c).unwrap();
        assert!(e.sequence().tracks[t].temp, "so is a dropped clip's");
        e.doc.undo().unwrap();
        e.doc.undo().unwrap();
        assert_eq!(e.sequence().tracks.len(), tracks, "each undone with its clip, in one step");
    }

    /// With several clips selected, what's done to one clip's effect happens to the same
    /// effect on the others (matched by type and order), and "Reverse" reaches each; a
    /// split cuts every selected clip under the playhead.
    #[test]
    fn selections_act_as_one() {
        let mut e = Editor::new();
        let a = e.add_text(Time::ZERO, "A", Time::from_seconds(4)).unwrap();
        let b = e.add_text(Time::from_seconds(5), "B", Time::from_seconds(4)).unwrap();
        let seq = e.seq;
        let fx = |id: u64| EffectInstance::new(EffectId(id), "oa.color.tint");
        e.doc
            .edit("fx", vec![Op::InsertEffect { seq, item: a, index: 0, effect: fx(100) }, Op::InsertEffect { seq, item: b, index: 0, effect: fx(200) }])
            .unwrap();
        e.linked = vec![a, b];
        assert_eq!(e.effect_peers(a, EffectId(100)), vec![(b, EffectId(200))]);
        let ops = e.fanned(vec![Op::SetEffectEnabled { seq, item: a, effect: EffectId(100), enabled: false }, Op::SetReverseIntro { seq, item: a, on: true }]);
        e.doc.edit("toggle", ops).unwrap();
        for id in [a, b] {
            let it = e.item(id).unwrap();
            assert!(!it.effects[0].enabled && it.outro_reverses_intro, "{id:?}");
        }
        let ops = e.fanned(vec![Op::RemoveEffect { seq, item: a, effect: EffectId(100) }]);
        e.doc.edit("remove", ops).unwrap();
        assert!(e.item(a).unwrap().effects.is_empty() && e.item(b).unwrap().effects.is_empty());
        e.linked.clear();

        // Split: both clips, where the playhead crosses them; a clip it doesn't cross
        // is skipped, not an error.
        let c = e.add_text(Time::from_seconds(20), "C", Time::from_seconds(2)).unwrap();
        let backs = e.split_many(&[a, c], Time::from_seconds(2)).unwrap();
        assert_eq!(backs.len(), 1);
        let backs = e.split_many(&[b, c], Time::from_seconds(7)).unwrap();
        assert_eq!(backs.len(), 1);
        assert_eq!(e.item(b).unwrap().range.duration, Time::from_seconds(2));
    }

    #[test]
    fn linked_clips_follow_parameter_edits() {
        let mut e = Editor::new();
        let a = e.add_text(Time::ZERO, "A", Time::from_seconds(2)).unwrap();
        let b = e.add_text(Time::from_seconds(3), "B", Time::from_seconds(2)).unwrap();
        e.linked = vec![a, b];
        let at = Time::from_seconds_f64(0.5);
        e.toggle_keyframing(a, ParamTarget::Item, schema::OPACITY, Value::Float(1.0), KeyframeAnchor::ClipStart, at).unwrap();
        assert!(e.param_source(b, &ParamTarget::Item, schema::OPACITY).is_some_and(|s| s.curve().is_some()), "keyframing went on for both");
        e.set_value_at(a, ParamTarget::Item, schema::TEXT_SIZE, Value::Float(123.0), at, "size");
        for id in [a, b] {
            assert_eq!(e.param_value(id, &ParamTarget::Item, schema::TEXT_SIZE, at), Some(Value::Float(123.0)));
        }
        // Unlinked: only the one.
        e.linked.clear();
        e.set_value_at(a, ParamTarget::Item, schema::TEXT_SIZE, Value::Float(50.0), at, "size2");
        assert_eq!(e.param_value(b, &ParamTarget::Item, schema::TEXT_SIZE, at), Some(Value::Float(123.0)));
    }

    #[test]
    fn projects_round_trip_and_relink() {
        if !have_ffprobe() {
            eprintln!("skipping: ffprobe not on PATH");
            return;
        }
        let source = test_picture(&std::env::temp_dir().join("oa-project-test-source"));
        let root = std::env::temp_dir().join("oa-project-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("media")).unwrap();
        std::fs::create_dir_all(root.join("samples")).unwrap();
        let media_path = root.join("media").join("E.png");
        std::fs::copy(&source, &media_path).unwrap();

        let mut editor = Editor::new();
        assert!(!editor.dirty(), "a fresh project is clean");
        let imported = importer(&media_path).expect("import");
        let media = editor.add_media(imported).expect("add media");
        let clip = editor.append_clip(media, Time::from_seconds(3)).expect("append");
        assert!(editor.dirty());

        let project_path = root.join("project.oaproj.json");
        editor.save(&project_path).expect("save");
        assert!(!editor.dirty(), "saving marks the project clean");

        // Reopen: the pool and the timeline come back.
        let (reopened, warnings) = Editor::open(&project_path, importer, &|_, _| {}, false).expect("open");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reopened.pool.len(), 1);
        assert_eq!(reopened.pool[0].kind, MediaKind::Still);
        assert!(!reopened.pool[0].missing);
        assert_eq!(reopened.item(clip).map(|i| i.range.duration), Some(Time::from_seconds(3)));
        assert_eq!(reopened.duration(), Time::from_seconds(3));
        assert!(!reopened.dirty());

        // Move the file somewhere the project searches: it should be found by content.
        std::fs::rename(&media_path, root.join("samples").join("E.png")).unwrap();
        let (relinked, warnings) = Editor::open(&project_path, importer, &|_, _| {}, false).expect("open after move");
        assert!(warnings.iter().any(|w| w.starts_with("relinked")), "{warnings:?}");
        assert!(!relinked.pool[0].missing, "relinked media is usable again");
        assert!(relinked.item(clip).is_some());

        // Moved deeper and renamed: past the quick look beside the project — found by its
        // content only with smart relink on.
        let deep = root.join("deep").join("er");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::rename(root.join("samples").join("E.png"), deep.join("renamed.png")).unwrap();
        let (lost, _) = Editor::open(&project_path, importer, &|_, _| {}, false).expect("open, not looking");
        assert!(lost.pool[0].missing, "the quick look doesn't go that far");
        let (smart, warnings) = Editor::open(&project_path, importer, &|_, _| {}, true).expect("open, looking");
        assert!(!smart.pool[0].missing, "found by content: {warnings:?}");
        assert_eq!(smart.pool[0].decode_path.file_name().unwrap(), "renamed.png");
        std::fs::rename(deep.join("renamed.png"), root.join("samples").join("E.png")).unwrap();

        // Delete it: the project still opens, with the media flagged as missing.
        std::fs::remove_file(root.join("samples").join("E.png")).unwrap();
        let (broken, warnings) = Editor::open(&project_path, importer, &|_, _| {}, false).expect("open with missing media");
        assert!(warnings.iter().any(|w| w.starts_with("missing:")), "{warnings:?}");
        assert!(broken.pool[0].missing);
        assert!(broken.item(clip).is_some(), "the clip stays; only its media is missing");
    }

    /// A clip's keys stretch with it when its speed changes (and not when it keeps its
    /// key times); a trim leaves them where they are; a cut leaves the curve exactly where
    /// it was, on both halves.
    #[test]
    fn keys_stretch_with_the_speed_and_survive_a_cut() {
        let mut e = Editor::new();
        let clip = e.add_text(Time::ZERO, "A", Time::from_seconds(8)).unwrap();
        let p = schema::OPACITY;
        let s = Time::from_seconds;
        let keyed = ParamSource::Animated(oa_params::Curve::new(KeyframeAnchor::ClipStart, vec![oa_params::Keyframe::linear(s(2), Value::Float(0.0)), oa_params::Keyframe::linear(s(6), Value::Float(1.0))]));
        e.apply("keys", vec![Op::SetParam { seq: e.seq, item: clip, target: ParamTarget::Item, param: ParamId::new(p), source: Some(keyed) }]).unwrap();
        let keys = |e: &Editor| e.keyframe_times(clip, &ParamTarget::Item, p);

        // A trim: the clip shows less, at the same pace; the keys stay put.
        let (ops, _) = e.length_ops(clip, &|_| Some(s(7)));
        e.apply("Clip length", ops).unwrap();
        assert_eq!(keys(&e), [s(2), s(6)], "steady through a trim");
        let (ops, _) = e.length_ops(clip, &|_| Some(s(8)));
        e.apply("Clip length", ops).unwrap();
        // Twice as fast, half as long: the keys come in with it.
        let speed = |e: &Editor, num: i64| {
            let it = e.item(clip).unwrap();
            let range = TimeRange::new(it.range.start, Time(it.range.duration.0 * it.time_map.speed.num() / num));
            Op::SetItemTiming { seq: e.seq, item: clip, range, time_map: oa_doc::TimeMap::new(it.time_map.source_in, oa_time::Rational::new(num, 1)) }
        };
        let op = speed(&e, 2);
        e.apply("Clip speed", vec![op]).unwrap();
        assert_eq!(keys(&e), [s(1), s(3)], "stretched with the speed");
        // Kept: a new speed leaves them where they are.
        e.apply("keep", vec![Op::SetKeepKeyTimes { seq: e.seq, item: clip, keep: true }]).unwrap();
        let op = speed(&e, 1);
        e.apply("Clip speed", vec![op]).unwrap();
        assert_eq!(keys(&e), [s(1), s(3)], "kept");
        e.apply("stretch", vec![Op::SetKeepKeyTimes { seq: e.seq, item: clip, keep: false }]).unwrap();

        // Cut in the middle of the ramp: both halves show what the whole did.
        let before: Vec<f64> = [0.5, 1.5, 2.0, 2.5, 3.5].iter().map(|&t| e.param_value(clip, &ParamTarget::Item, p, Time::from_seconds_f64(t)).and_then(|v| v.as_float()).unwrap()).collect();
        let back = e.split(Some(clip), s(2)).unwrap().expect("a back half");
        let value = |e: &Editor, id: ItemId, t: f64| e.param_value(id, &ParamTarget::Item, p, Time::from_seconds_f64(t)).and_then(|v| v.as_float()).unwrap();
        let after: Vec<f64> = [0.5, 1.5].iter().map(|&t| value(&e, clip, t)).chain([2.0, 2.5, 3.5].iter().map(|&t| value(&e, back, t))).collect();
        for (a, b) in before.iter().zip(&after) {
            assert!((a - b).abs() < 1e-9, "the curve goes on as it was: {before:?} vs {after:?}");
        }
        assert_eq!(keys(&e), [s(1), s(3)], "the front half's keys untouched by the cut");
    }

    /// Opening takes the files in several at once, but the results come back in the
    /// files' order; a file not found is skipped; progress counts every file once.
    #[test]
    fn files_are_taken_in_together_in_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let found: Vec<Option<PathBuf>> = (0..40).map(|i| (i % 7 != 3).then(|| PathBuf::from(format!("clip{i}.mp4")))).collect();
        let calls = AtomicUsize::new(0);
        let import = |p: &Path| -> Result<Imported, String> {
            calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(2));
            Err(p.display().to_string())
        };
        let (last, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let results = super::import_all(&found, &import, &|done, total| {
            assert_eq!(total, 40);
            last.fetch_max(done, Ordering::SeqCst);
            most.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(calls.load(Ordering::SeqCst), found.iter().flatten().count(), "each found file once");
        for (f, r) in found.iter().zip(&results) {
            match (f, r) {
                (Some(p), Some(Err(e))) => assert_eq!(e, &p.display().to_string(), "in order"),
                (None, None) => {}
                _ => panic!("{f:?} → {r:?}"),
            }
        }
        assert_eq!(last.load(Ordering::SeqCst), 40, "progress reaches every file");
        assert_eq!(most.load(Ordering::SeqCst), 41, "once at the start, once per file");
    }

    /// Quick cut: parts of a file go one after another, each playing its own stretch;
    /// removing the file takes its clips too, and one undo brings both back.
    #[test]
    fn parts_append_in_order_and_media_removes_with_its_clips() {
        if !have_ffprobe() {
            return;
        }
        let picture = test_picture(&std::env::temp_dir().join("oa-parts-test"));
        let mut editor = Editor::new();
        let media = editor.add_media(importer(&picture).expect("import")).expect("add");
        let s = Time::from_seconds;
        let parts = editor.append_parts(media, &[(s(1), s(2)), (s(5), s(1)), (s(9), Time::ZERO)]).expect("parts");
        assert_eq!(parts.len(), 2, "an empty part is left out");
        let (a, b) = (editor.item(parts[0]).unwrap().clone(), editor.item(parts[1]).unwrap().clone());
        assert_eq!((a.range.start, a.range.duration, a.time_map.source_in), (Time::ZERO, s(2), s(1)));
        assert_eq!((b.range.start, b.time_map.source_in), (s(2), s(5)), "the next right after, from its own point");
        assert_eq!(editor.media_uses().0.get(&media), Some(&2));

        editor.remove_media(&[media]).expect("remove");
        assert!(editor.doc.project().media(media).is_none() && editor.item(parts[0]).is_none());
        editor.doc.undo().unwrap();
        assert!(editor.doc.project().media(media).is_some() && editor.item(parts[1]).is_some(), "one undo brings both back");
    }

    #[test]
    fn clips_append_one_after_another_on_the_right_track() {
        if !have_ffprobe() {
            return;
        }
        let picture = test_picture(&std::env::temp_dir().join("oa-append-test"));
        let mut editor = Editor::new();
        let imported = importer(&picture).expect("import");
        let media = editor.add_media(imported).expect("add");
        let first = editor.append_clip(media, Time::from_seconds(2)).expect("first");
        let second = editor.append_clip(media, Time::from_seconds(3)).expect("second");
        assert_eq!(editor.item(first).unwrap().range.start, Time::ZERO);
        assert_eq!(editor.item(second).unwrap().range.start, Time::from_seconds(2));
        assert_eq!(editor.duration(), Time::from_seconds(5));
        // Importing the same file twice reuses the pool entry.
        let again = importer(&picture).expect("import");
        assert_eq!(editor.add_media(again).expect("dedupe"), media);
        assert_eq!(editor.pool.len(), 1);
    }

    /// Length on a multiple selection: a typed length is every clip's; a frame more is
    /// each clip's own plus one; a clip stopped by its neighbor is counted, not dropped.
    #[test]
    fn length_applies_to_every_selected_clip() {
        let mut e = Editor::new();
        let a = e.add_text(Time::ZERO, "A", Time::from_seconds(2)).unwrap();
        let b = e.add_text(Time::from_seconds(3), "B", Time::from_seconds(4)).unwrap();
        // Not selected: C, right after A on A's track, at 3 s.
        let (ti, _) = e.sequence().find_item(a).unwrap();
        let track = e.sequence().tracks[ti].id;
        let c = Item::new(ItemId(e.doc.alloc_id()), "C", ItemKind::Text, TimeRange::new(Time::from_seconds(3), Time::from_seconds(1)));
        let seq = e.seq;
        e.doc.edit("c", vec![Op::InsertItem { seq, track, item: c }]).unwrap();
        e.linked = vec![a, b];
        let (ops, short) = e.length_ops(a, &|_| Some(Time::from_seconds(1)));
        assert_eq!(short, 0);
        e.doc.edit("length", ops).unwrap();
        assert_eq!([a, b].map(|id| e.item(id).unwrap().range.duration), [Time::from_seconds(1); 2]);
        // Each one frame longer, from its own length.
        let frame = e.sequence().rate.frame_start(1);
        let (ops, _) = e.length_ops(a, &|it| Some(it.range.duration + frame));
        e.doc.edit("frame", ops).unwrap();
        assert_eq!([a, b].map(|id| e.item(id).unwrap().range.duration), [Time::from_seconds(1) + frame; 2]);
        // 5 s each: B can, A stops at C (3 s in) — made as long as it can be, and counted.
        let (ops, short) = e.length_ops(a, &|_| Some(Time::from_seconds(5)));
        assert_eq!(short, 1);
        e.doc.edit("long", ops).unwrap();
        assert_eq!(e.item(a).unwrap().range.end(), Time::from_seconds(3));
        assert_eq!(e.item(b).unwrap().range.duration, Time::from_seconds(5));
    }
}

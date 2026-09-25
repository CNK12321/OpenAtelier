//! The edit session: a project, its media pool, and the operations the UI drives.

use oa_doc::*;
use oa_media::{Imported, MediaKind, MediaProbe};
use oa_params::{KeyframeAnchor, ParamId, ParamSource, Value};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
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
        // Built directly, not as an edit: the empty project is where undo stops. (As an
        // edit, undoing past the first change took the whole timeline away.)
        let mut project = Project::new("Untitled");
        let (seq, video_track, audio_track) = (SeqId(project.alloc_id()), TrackId(project.alloc_id()), TrackId(project.alloc_id()));
        let wide = variant(&mut project, "landscape-16x9", 1080);
        let tall = variant(&mut project, "vertical-9x16", 1080);
        let square = variant(&mut project, "square-1x1", 1080);
        let mut sequence = Sequence::new(seq, "Main", FrameRate::FPS_30, wide);
        sequence.variants.push(tall);
        sequence.variants.push(square);
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
        let name = self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string());
        format!("{}{}", name.unwrap_or_else(|| "Untitled".into()), if self.dirty() { " •" } else { "" })
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
    pub fn add_media(&mut self, imported: Imported) -> Result<MediaId, EditError> {
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
        };
        let media = MediaRef {
            id,
            path: imported.path.to_string_lossy().to_string(),
            fingerprint: Some(imported.fingerprint.clone()),
            info: Some(info),
            scaling: Default::default(),
            folder: String::new(),
            color: Default::default(),
        };
        self.doc.edit("Import media", vec![Op::AddMedia(Arc::new(media))])?;
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
        // The default track may have been deleted: any track of the kind, or a new one.
        let track = match self.sequence().track(preferred).map(|t| t.id).or_else(|| self.sequence().tracks.iter().find(|t| t.kind == kind && !t.effects).map(|t| t.id)) {
            Some(t) => t,
            None => self.add_track(kind)?,
        };
        let start = self.sequence().track(track).map(|t| t.items.last().map_or(Time::ZERO, |i| i.range.end())).unwrap_or(Time::ZERO);
        let id = ItemId(self.doc.alloc_id());
        let item = Item::new(id, &name, ItemKind::Media { media }, TimeRange::new(start, duration.max(Time::from_seconds(1))));
        self.doc.edit("Add clip", vec![Op::InsertItem { seq: self.seq, track, item }])?;
        Ok(id)
    }

    /// Adds a text clip at `at` on top of everything: on the topmost video track if it's
    /// free there and something lies beneath, otherwise on a new track above.
    pub fn add_text(&mut self, at: Time, content: &str, length: Time) -> Result<ItemId, EditError> {
        let range = TimeRange::new(at, length.max(Time::from_seconds_f64(0.1)));
        let videos: Vec<&Track> = self.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.effects).map(|t| &**t).collect();
        let top = videos.last().filter(|t| videos.len() > 1 && !t.items.iter().any(|i| i.range.overlaps(range))).map(|t| t.id);
        let track = match top {
            Some(t) => t,
            None if videos.iter().all(|t| t.items.is_empty()) && !videos.is_empty() => videos[0].id,
            None => self.add_track(TrackKind::Video)?,
        };
        let id = ItemId(self.doc.alloc_id());
        let mut item = Item::new(id, "Text", ItemKind::Text, range);
        item.params.set(oa_doc::schema::TEXT_CONTENT, ParamSource::Static(Value::Text(content.into())));
        self.doc.edit("Add text", vec![Op::InsertItem { seq: self.seq, track, item }])?;
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
        self.doc.edit("Add effect track", vec![Op::InsertTrack { seq: self.seq, index, track: Arc::new(Track::effects(id, &name, kind)) }])?;
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
        self.apply("Add effect container", vec![Op::InsertItem { seq: self.seq, track, item }])?;
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
            // or a new video track on top.
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
                    ops.push(Op::InsertTrack { seq: self.seq, index, track: Arc::new(Track::new(tid, &format!("V{}", count + 1), TrackKind::Video)) });
                    tid
                }
            };
            let item_id = ItemId(self.doc.alloc_id());
            ops.push(Op::InsertItem { seq: self.seq, track, item: Item::new(item_id, &name, ItemKind::Nested { sequence: id }, range) });
            clip = Some(item_id);
        }
        self.apply(if nest { "Nest clips" } else { "Make compound clip" }, ops)?;
        Ok((id, clip))
    }

    /// Adds a clip playing compound `seq` at the end of the video track.
    pub fn append_compound(&mut self, seq: SeqId) -> Result<ItemId, EditError> {
        let inner = self.doc.project().sequence(seq).ok_or(EditError::NotFound("sequence", seq.0))?;
        let (name, duration) = (inner.name.clone(), inner.duration());
        let track = match self.sequence().track(self.video_track).map(|t| t.id).or_else(|| self.sequence().tracks.iter().find(|t| t.kind == TrackKind::Video && !t.effects).map(|t| t.id)) {
            Some(t) => t,
            None => self.add_track(TrackKind::Video)?,
        };
        let start = self.sequence().track(track).map_or(Time::ZERO, |t| t.items.last().map_or(Time::ZERO, |i| i.range.end()));
        let id = ItemId(self.doc.alloc_id());
        let item = Item::new(id, &name, ItemKind::Nested { sequence: seq }, TimeRange::new(start, duration.max(Time::from_seconds(1))));
        self.doc.edit("Add clip", vec![Op::InsertItem { seq: self.seq, track, item }])?;
        Ok(id)
    }

    /// Puts a clip of `kind` at `at` (on a frame boundary): on `preferred` if it's the
    /// right kind of track and free there, else the first free track of the kind, else a
    /// new track. One undo step.
    pub fn place_clip(&mut self, name: &str, kind: ItemKind, duration: Time, at: Time, preferred: Option<TrackId>) -> Result<ItemId, EditError> {
        let track_kind = match &kind {
            ItemKind::Media { media } if self.pool_item(*media).is_some_and(|p| p.kind == MediaKind::Audio) => TrackKind::Audio,
            _ => TrackKind::Video,
        };
        let rate = self.sequence().rate;
        let at = rate.frame_start(rate.frame_at(at.max(Time::ZERO)));
        let range = TimeRange::new(at, duration.max(Time::from_seconds(1)));
        let s = self.sequence();
        let free = |t: &Track| t.kind == track_kind && !t.effects && !t.items.iter().any(|i| i.range.overlaps(range));
        let found = preferred.and_then(|p| s.track(p)).filter(|t| free(t)).or_else(|| s.tracks.iter().map(|t| &**t).find(|t| free(t))).map(|t| t.id);
        let mut ops = Vec::new();
        let track = match found {
            Some(t) => t,
            None => {
                let id = TrackId(self.doc.alloc_id());
                let s = self.sequence();
                let count = s.tracks.iter().filter(|t| t.kind == track_kind && !t.effects).count();
                let index = s.tracks.iter().rposition(|t| t.kind == track_kind).map_or(s.tracks.len(), |i| i + 1);
                let name = format!("{}{}", if track_kind == TrackKind::Video { "V" } else { "A" }, count + 1);
                ops.push(Op::InsertTrack { seq: self.seq, index, track: Arc::new(Track::new(id, &name, track_kind)) });
                id
            }
        };
        let id = ItemId(self.doc.alloc_id());
        ops.push(Op::InsertItem { seq: self.seq, track, item: Item::new(id, name, kind, range) });
        self.apply("Add clip", ops)?;
        Ok(id)
    }

    /// Deletes compound `seq` (refused while a clip uses it).
    pub fn remove_compound(&mut self, seq: SeqId) -> Result<(), EditError> {
        self.apply("Delete compound clip", vec![Op::RemoveSequence(seq)])
    }

    /// Applies a command's ops as one undo step (nothing happens for an empty list).
    pub fn apply(&mut self, label: &str, ops: Vec<Op>) -> Result<(), EditError> {
        if ops.is_empty() {
            return Ok(());
        }
        self.doc.edit(label, ops)
    }

    /// Like [`apply`](Self::apply) for a drag: consecutive updates with the same `key`
    /// merge into one undo step until [`Document::seal`].
    pub fn apply_drag(&mut self, label: &str, key: &str, ops: Vec<Op>) -> Result<(), EditError> {
        if ops.is_empty() {
            return Ok(());
        }
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

    /// Where a clip or effect parameter's keyframes are, in clip-local seconds.
    pub fn keyframe_times(&self, item: ItemId, target: &ParamTarget, param: &str) -> Vec<Time> {
        let Some(it) = self.item(item) else { return Vec::new() };
        let Some(curve) = Self::param_set(it, target).and_then(|set| set.get(param)).and_then(|s| s.curve()) else { return Vec::new() };
        let offset = match curve.anchor {
            KeyframeAnchor::ClipStart => Time::ZERO,
            // Source-anchored keys: convert to clip time (speed 1 assumed for display).
            KeyframeAnchor::SourceMedia => Time::ZERO - it.time_map.source_in,
        };
        curve.keys.iter().map(|k| k.t + offset).collect()
    }

    /// Turns keyframing of a clip parameter on (the current value becomes a key at `t`)
    /// or off (the value at `t` becomes the static value).
    pub fn toggle_keyframing(
        &mut self,
        item: ItemId,
        target: ParamTarget,
        param: &str,
        default: Value,
        anchor: KeyframeAnchor,
        t: Time,
    ) -> Result<(), EditError> {
        let it = self.item(item).ok_or(EditError::NotFound("item", item.0))?;
        // Every linked clip goes the way the clicked one does: all on, or all off.
        let turning_off = Self::param_set(it, &target).and_then(|set| set.get(param)).is_some_and(|s| s.curve().is_some());
        let mut ops = Vec::new();
        for (item, target) in self.fan_out(item, &target, param) {
            let Some(it) = self.item(item) else { continue };
            let ctx = it.eval_context(t);
            let current = Self::param_set(it, &target).and_then(|set| set.get(param)).cloned();
            let source = match current {
                Some(src) if turning_off => {
                    let value = src.eval(&ctx);
                    match src {
                        // Keep procedural motion; only the keys go.
                        ParamSource::Modulated { modulator, .. } => {
                            ParamSource::Modulated { base: Box::new(ParamSource::Static(value)), modulator }
                        }
                        _ => ParamSource::Static(value),
                    }
                }
                None if turning_off => continue,
                Some(src) if src.curve().is_some() => continue,
                Some(src) => src.keyframed(&ctx, anchor),
                None => ParamSource::Static(default.clone()).keyframed(&ctx, anchor),
            };
            ops.push(Op::SetParam { seq: self.seq, item, target, param: ParamId::new(param), source: Some(source) });
        }
        self.doc.edit("Keyframing", ops)
    }

    /// Value of a clip or effect parameter at `t`.
    pub fn param_value(&self, item: ItemId, target: &ParamTarget, param: &str, t: Time) -> Option<Value> {
        let it = self.item(item)?;
        Some(Self::param_set(it, target)?.get(param)?.eval(&it.eval_context(t)))
    }

    /// How a parameter is stored (static, keyframed…), if it has been set.
    pub fn param_source(&self, item: ItemId, target: &ParamTarget, param: &str) -> Option<ParamSource> {
        Self::param_set(self.item(item)?, target)?.get(param).cloned()
    }

    fn param_set<'a>(it: &'a Item, target: &ParamTarget) -> Option<&'a oa_params::ParamSet> {
        match target {
            ParamTarget::Item => Some(&it.params),
            ParamTarget::Effect(id) => it.effects.iter().find(|e| e.id == *id).map(|e| &e.params),
            ParamTarget::Transition(end) => it.transition(*end).map(|t| &t.params),
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

    /// Replaces a parameter's source outright (coalescing drags under `drag_key`) — on
    /// the linked clips too.
    pub fn set_param(&mut self, item: ItemId, target: ParamTarget, param: &str, source: ParamSource, drag_key: &str) {
        let ops = self
            .fan_out(item, &target, param)
            .into_iter()
            .map(|(item, target)| Op::SetParam { seq: self.seq, item, target, param: ParamId::new(param), source: Some(source.clone()) })
            .collect();
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
            let source = match Self::param_set(it, &target).and_then(|set| set.get(param)) {
                Some(existing) => {
                    let mut src = existing.clone();
                    src.set_at(&ctx, value.clone());
                    src
                }
                None => ParamSource::Static(value.clone()),
            };
            ops.push(Op::SetParam { seq: self.seq, item, target, param: ParamId::new(param), source: Some(source) });
        }
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
    pub fn open(path: &Path, mut import: impl FnMut(&Path) -> Result<Imported, String>) -> Result<(Self, Vec<String>), String> {
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
        for media in project.media.values() {
            let stored = PathBuf::from(&media.path);
            let found = if stored.is_file() {
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
            };
            match found.as_deref().map(&mut import) {
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
        }
        let mut doc = Document::new(project);
        for (id, found) in relinks {
            let media = doc.project().media(id).expect("in project").clone();
            let updated = MediaRef { path: found.to_string_lossy().to_string(), ..media };
            let _ = doc.edit("Relink media", vec![Op::RemoveMedia(id), Op::AddMedia(Arc::new(updated))]);
        }
        let saved = Some(doc.snapshot());
        Ok((Editor { doc, seq, video_track, audio_track, pool, path: Some(path.to_path_buf()), saved, linked: Vec::new(), index: Default::default() }, warnings))
    }
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
        let (reopened, warnings) = Editor::open(&project_path, importer).expect("open");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reopened.pool.len(), 1);
        assert_eq!(reopened.pool[0].kind, MediaKind::Still);
        assert!(!reopened.pool[0].missing);
        assert_eq!(reopened.item(clip).map(|i| i.range.duration), Some(Time::from_seconds(3)));
        assert_eq!(reopened.duration(), Time::from_seconds(3));
        assert!(!reopened.dirty());

        // Move the file somewhere the project searches: it should be found by content.
        std::fs::rename(&media_path, root.join("samples").join("E.png")).unwrap();
        let (relinked, warnings) = Editor::open(&project_path, importer).expect("open after move");
        assert!(warnings.iter().any(|w| w.starts_with("relinked")), "{warnings:?}");
        assert!(!relinked.pool[0].missing, "relinked media is usable again");
        assert!(relinked.item(clip).is_some());

        // Delete it: the project still opens, with the media flagged as missing.
        std::fs::remove_file(root.join("samples").join("E.png")).unwrap();
        let (broken, warnings) = Editor::open(&project_path, importer).expect("open with missing media");
        assert!(warnings.iter().any(|w| w.starts_with("missing:")), "{warnings:?}");
        assert!(broken.pool[0].missing);
        assert!(broken.item(clip).is_some(), "the clip stays; only its media is missing");
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
}

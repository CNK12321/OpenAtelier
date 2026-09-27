//! Frame-accurate video frames for the renderer, decoded on background threads.
//!
//! Each file gets a **decode thread** that owns its platform decoder (Media Foundation's
//! COM objects stay on the thread that made them). The renderer asks for a frame index
//! and blocks only until that frame is ready; meanwhile the thread decodes a few frames
//! **ahead** in the direction of playback, so steady playback usually finds its frame
//! already waiting. The preview never blocks for long: scrubbing waits for nothing
//! (`interactive`), and playback at most a moment (`wait_budget`) before showing a
//! stand-in or keeping the last picture.
//!
//! Frames cross threads as [`Surface`]s: decoded pictures in the decoder's native
//! layout (NV12) in textures the renderer can read. The renderer converts them to the
//! working format in the render graph. A surface's texture is reused by the decoder only
//! after the GPU has finished reading it — the source holds on to every surface it
//! converted until the queue reports that submission done (see
//! [`FrameSource::submitted`]).

use crate::{MediaError, VideoTrack};
use oa_gpu::{FrameSource, GpuImage, GpuServices, Nv12Frame, RenderError, SourceRequest, VideoColor};
use oa_time::Time;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// A platform decoder. It lives on its decode thread for its whole life.
pub trait VideoDecoder {
    type Frame;

    /// Positions the decoder at or before `t` (normally the keyframe at/before it).
    fn seek(&mut self, t: Time) -> Result<(), MediaError>;

    /// Next frame in presentation order with its presentation time, or `None` at the end.
    fn next(&mut self) -> Result<Option<(Time, Self::Frame)>, MediaError>;

    /// Makes a decoded frame readable by the renderer — e.g. a GPU copy into a texture
    /// shared with the render device. Called only for frames that will be shown.
    fn publish(&mut self, frame: &Self::Frame) -> Result<Surface, MediaError>;

    /// Asks for frames `divisor` times smaller than the file's (1, 2, 4 or 8), when the
    /// picture is only shown that small. True if the decoder changed and needs a
    /// [`seek`](VideoDecoder::seek) before its next frame; decoders that can't (or
    /// needn't: a hardware decoder already on the GPU) keep full size and say false.
    /// What a seek costs, in frames decoded: past a keyframe, decoding on from where the
    /// decoder is beats seeking unless it's more frames than the seek's own decode plus
    /// this. A decoder whose seek is a process restart (ffmpeg) says so here.
    fn seek_cost(&self) -> usize {
        FORWARD_DECODE_LIMIT
    }

    fn set_scale_divisor(&mut self, _divisor: u32) -> bool {
        false
    }
}

/// Marks a decoder texture busy while any clone of the [`Surface`] using it lives.
#[derive(Debug)]
pub struct Lease(Arc<AtomicBool>);

impl Lease {
    /// Takes `free` (sets it busy); it's released when the lease drops.
    pub fn take(free: &Arc<AtomicBool>) -> Lease {
        free.store(false, Ordering::Release);
        Lease(free.clone())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// A decoded picture on the GPU in the decoder's layout, ready for conversion.
#[derive(Clone, Debug)]
pub struct Surface {
    /// NV12 — or the luma plane (`R8Unorm`) when `chroma` is set.
    pub texture: Arc<wgpu::Texture>,
    /// The chroma plane (`Rg8Unorm`, half size) for decoders that upload planes apart.
    pub chroma: Option<Arc<wgpu::Texture>>,
    /// Transparency (`R8Unorm`, the luma plane's size), for video that has it.
    pub alpha: Option<Arc<wgpu::Texture>>,
    pub coded_size: [u32; 2],
    pub visible_size: [u32; 2],
    pub rotation_quarter_turns: u32,
    pub color: VideoColor,
    pub lease: Arc<Lease>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodeStats {
    pub seeks: u64,
    pub decoded: u64,
    pub uploads: u64,
    /// Frames served from the recently-shown cache instead of the decoder.
    pub frame_cache_hits: u64,
    /// Frames where the decoder couldn't land exactly on the requested frame.
    pub inexact: u64,
    /// Frames that were already decoded ahead when the renderer asked for them.
    pub lookahead_hits: u64,
    /// Frames decoded ahead but not uploaded: a steady stride (a sped-up clip) skips them.
    pub skipped_uploads: u64,
}

impl DecodeStats {
    fn add(&mut self, o: &DecodeStats) {
        self.seeks += o.seeks;
        self.decoded += o.decoded;
        self.uploads += o.uploads;
        self.frame_cache_hits += o.frame_cache_hits;
        self.inexact += o.inexact;
        self.lookahead_hits += o.lookahead_hits;
        self.skipped_uploads += o.skipped_uploads;
    }
}

/// Opens a decoder for a file; called on the file's decode thread.
pub type Opener<D> = Arc<dyn Fn(&Path, &VideoTrack) -> Result<D, MediaError> + Send + Sync>;

/// Decoder clocks are often rounded (Media Foundation uses 100 ns units), so a decoded
/// timestamp is nudged by this much before mapping it onto the exact frame index.
const TIMESTAMP_TOLERANCE: Time = Time(705_600); // 1 ms

/// Frames within this distance are reached by decoding forward instead of seeking.
const FORWARD_DECODE_LIMIT: usize = 12;

/// How far behind `target` a decoder can be and still get there by decoding on rather
/// than seeking (as the decode thread decides it: a seek decodes from the keyframe
/// anyway): for routing requests to the decoder that's cheapest.
fn forward_reach(video: &VideoTrack, target: usize) -> usize {
    (target - video.index.keyframe_before(target).min(target)) + FORWARD_DECODE_LIMIT
}

/// Frames decoded ahead of the last request during forward playback.
pub const LOOKAHEAD: usize = 4;

/// Playing backwards (a reversed clip): frames decoded forward in one go from their
/// keyframe and handed out last first — this many at a time, and the next stretch
/// (further back) is decoded once fewer than [`REVERSE_LOW`] are left.
pub const REVERSE_CHUNK: usize = 12;
const REVERSE_LOW: usize = 4;

/// How much GPU memory recently shown frames may hold. Stepping or playing backwards
/// revisits frames that were just decoded, which would otherwise mean a seek and a GOP of
/// decoding per frame — but at 4K a frame is ~66 MB, so the window is bounded by bytes.
const RECENT_FRAME_BYTES: u64 = 128 << 20;
/// ...and never more than this many frames, however small they are.
const RECENT_FRAMES: usize = 48;

enum Cmd {
    /// A frame the renderer is waiting for.
    Frame { target: usize, reply: mpsc::Sender<Result<(usize, Surface), MediaError>> },
    /// A frame wanted soon, while scrubbing: the answer goes to `Shared::latest`, and a
    /// newer `Want` (a higher generation) makes the thread drop this one mid-decode.
    Want { target: usize, generation: u64 },
    Stop,
}

/// State the render side and a decode thread share.
#[derive(Default)]
struct Shared {
    /// The newest `Want` generation; the thread abandons older work when it moves.
    wanted: AtomicU64,
    /// The most recent frame published for a `Want`: the exact frame once it's decoded,
    /// and meanwhile the keyframe the decoder landed on (a close stand-in).
    latest: Mutex<Option<(usize, Surface)>>,
    /// The size the render side last asked for (width << 32 | height; 0: unknown), so a
    /// decoder that can may decode smaller than the file.
    size: AtomicU64,
}

impl Shared {
    fn ask_size(&self, size: [u32; 2]) {
        self.size.store(((size[0] as u64) << 32) | size[1] as u64, Ordering::Release);
    }
}

/// The render thread's handle on one file's decode thread.
struct Worker {
    tx: mpsc::Sender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
    stats: Arc<Mutex<DecodeStats>>,
    idle: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Decoder state on the decode thread.
struct Decoding<D: VideoDecoder> {
    path: PathBuf,
    video: VideoTrack,
    open: Opener<D>,
    decoder: Option<D>,
    /// Decoder clock minus index clock. Decoders may ignore container edit lists (Media
    /// Foundation reports B-frame composition delay), so this is measured from the first
    /// frame when the decoder opens.
    offset: Time,
    /// Most recently decoded frame and its index.
    current: Option<(usize, D::Frame)>,
    /// Frames decoded ahead and published, in order.
    ready: VecDeque<(usize, Surface)>,
    /// The last frame handed out (answering repeats, e.g. the same frame at two sizes).
    last: Option<(usize, Surface)>,
    /// Whether to decode ahead: after any request that isn't a step backwards; cleared at
    /// the end of the file or on errors.
    ahead: bool,
    /// The last few steps between requested frames. When they agree (a clip at 2×, 4×…
    /// asks for every 2nd, 4th… frame), work-ahead only publishes the frames that will be
    /// asked for — the others are decoded (they must be) but never uploaded.
    strides: [usize; 3],
    stats: Arc<Mutex<DecodeStats>>,
    shared: Arc<Shared>,
    /// While serving a `Want`: its generation (to notice when it's superseded) and
    /// whether the first frame after a seek should be published as a stand-in.
    want: Option<u64>,
    stand_in_pending: bool,
    /// How many times smaller than the file the decoder is asked to decode (`follow_size`).
    divisor: u32,
    /// Playing backwards: frames below the last one handed out, decoded and published
    /// ahead (ascending), so each step back is ready rather than a seek and a GOP of
    /// decoding. Empty when playing forwards.
    behind: VecDeque<(usize, Surface)>,
    /// The last requests went backwards, one or a few frames at a time.
    reverse: bool,
}

impl<D: VideoDecoder> Decoding<D> {
    fn superseded(&self) -> bool {
        self.want.is_some_and(|g| self.shared.wanted.load(Ordering::Acquire) != g)
    }

    /// Serves a `Want`: decodes towards `target`, publishing the landing keyframe early
    /// as a stand-in, then the exact frame — unless a newer `Want` arrives first.
    fn serve_want(&mut self, target: usize, generation: u64) {
        self.want = Some(generation);
        self.stand_in_pending = true;
        // If a newer Want cut the decode short, this is the nearest frame reached — still a
        // better stand-in than nothing; the render side checks which frame it got.
        if let Ok(frame) = self.serve(target) {
            *self.shared.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        }
        self.want = None;
        self.stand_in_pending = false;
    }

    fn publish_stand_in(&mut self) {
        if let Ok(frame) = self.publish_current() {
            *self.shared.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        }
    }
    fn count(&self, f: impl FnOnce(&mut DecodeStats)) {
        f(&mut self.stats.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn index_of(&self, t: Time) -> usize {
        self.video.index.frame_at(t - self.offset + TIMESTAMP_TOLERANCE).unwrap_or(0)
    }

    /// Follows the size the render side asks for: the smallest of full, ½, ¼ or ⅛ size
    /// that's still at least as big (a 4K file shown in a 1080p viewer decodes at half
    /// size — a quarter of the bytes to move and upload). A decoder that changes size
    /// starts over from the next request's seek.
    fn follow_size(&mut self) {
        let packed = self.shared.size.load(Ordering::Acquire);
        if packed == 0 {
            return;
        }
        let asked = ((packed >> 32) as u32).max(packed as u32).max(1);
        let native = self.video.width.max(self.video.height);
        let mut divisor = 1;
        while divisor < 8 && native / (divisor * 2) >= asked {
            divisor *= 2;
        }
        if divisor == self.divisor {
            return;
        }
        self.divisor = divisor;
        if let Some(d) = self.decoder.as_mut()
            && d.set_scale_divisor(divisor)
        {
            self.current = None;
            self.ready.clear();
            self.behind.clear();
            self.last = None;
        }
    }

    fn ensure_open(&mut self) -> Result<(), MediaError> {
        if self.decoder.is_some() {
            return Ok(());
        }
        let mut decoder = (self.open)(&self.path, &self.video)?;
        if self.divisor > 1 && decoder.set_scale_divisor(self.divisor) {
            decoder.seek(Time::ZERO)?;
        }
        let (t0, first) = decoder.next()?.ok_or_else(|| MediaError::Decode("file has no decodable frames".into()))?;
        self.count(|s| s.decoded += 1);
        self.offset = t0 - self.video.index.time_of(0);
        self.current = Some((0, first));
        self.decoder = Some(decoder);
        Ok(())
    }

    /// Decodes until `current` is frame `target` (or the nearest frame at/after it).
    fn decode_to(&mut self, target: usize) -> Result<(), MediaError> {
        self.ensure_open()?;
        if self.current.as_ref().is_some_and(|(i, _)| *i == target) {
            return Ok(());
        }
        let mut keyframe = self.video.index.keyframe_before(target);
        // Decode on, or seek? Seeking decodes from the target's keyframe, plus the seek's
        // own cost; going on decodes everything in between. (A sped-up clip asks for
        // every k-th frame: at 16× this used to seek — for ffmpeg, restart — on every
        // frame past a keyframe.)
        let seek_cost = self.decoder.as_ref().map_or(FORWARD_DECODE_LIMIT, |d| d.seek_cost());
        let can_continue = self.current.as_ref().is_some_and(|(i, _)| *i < target && target - *i <= (target - keyframe) + seek_cost);
        let mut seek_to = (!can_continue).then_some(keyframe);
        loop {
            // Scrubbing moved on: stop here (the caller shows the nearest frame reached).
            if self.current.is_some() && seek_to.is_none() && self.superseded() {
                return Ok(());
            }
            if let Some(k) = seek_to.take() {
                let t = self.video.index.time_of(k) + self.offset;
                self.decoder.as_mut().expect("opened").seek(t)?;
                self.current = None;
                self.ready.clear();
                self.count(|s| s.seeks += 1);
            }
            let Some((t, frame)) = self.decoder.as_mut().expect("opened").next()? else {
                // Past the last decodable frame: keep showing the last one we have.
                if self.current.is_none() {
                    return Err(MediaError::Decode(format!("no frames decoded near frame {target}")));
                }
                self.count(|s| s.inexact += 1);
                return Ok(());
            };
            self.count(|s| s.decoded += 1);
            let i = self.index_of(t);
            if i > target && self.current.is_none() && keyframe > 0 {
                // The decoder landed after the target (imprecise seek): back up one GOP.
                // `keyframe` strictly decreases, so this terminates.
                keyframe = self.video.index.keyframe_before(keyframe - 1);
                seek_to = Some(keyframe);
                continue;
            }
            self.current = Some((i, frame));
            if i >= target {
                if i > target {
                    self.count(|s| s.inexact += 1);
                }
                return Ok(());
            }
            // While scrubbing, the keyframe the seek landed on shows immediately — a
            // frame or two from the target — while decoding continues to the exact one.
            if self.stand_in_pending {
                self.stand_in_pending = false;
                self.publish_stand_in();
            }
        }
    }

    fn publish_current(&mut self) -> Result<(usize, Surface), MediaError> {
        let (i, frame) = self.current.as_ref().ok_or_else(|| MediaError::Decode("decoder produced no frame".into()))?;
        let surface = self.decoder.as_mut().expect("opened").publish(frame)?;
        Ok((*i, surface))
    }

    fn serve(&mut self, target: usize) -> Result<(usize, Surface), MediaError> {
        self.follow_size();
        if let Some(last) = self.last.as_ref().filter(|(i, _)| *i == target) {
            return Ok(last.clone());
        }
        let backward = self.last.as_ref().is_some_and(|(i, _)| target < *i);
        // A step back (a clip playing in reverse, or stepping backwards): from the frames
        // decoded ahead going that way — a stretch decoded forward from its keyframe once,
        // handed out last first — rather than a seek and a GOP of decoding every frame.
        let step_back = self.last.as_ref().is_some_and(|(i, _)| target < *i && *i - target <= 2 * REVERSE_CHUNK);
        if step_back {
            while self.behind.back().is_some_and(|(i, _)| *i > target) {
                self.behind.pop_back();
            }
            if self.behind.back().is_none_or(|(i, _)| *i != target) {
                self.behind.clear();
                let got = self.decode_range(target.saturating_sub(REVERSE_CHUNK - 1), target)?;
                self.behind.extend(got);
            } else {
                self.count(|s| s.lookahead_hits += 1);
            }
            let served = self.behind.pop_back().ok_or_else(|| MediaError::Decode(format!("no frames decoded near frame {target}")))?;
            self.ready.clear();
            self.reverse = true;
            self.ahead = false;
            self.last = Some(served.clone());
            return Ok(served);
        }
        self.reverse = false;
        self.behind.clear();
        if let Some((prev, _)) = self.last.as_ref().filter(|(i, _)| *i < target) {
            self.strides = [self.strides[1], self.strides[2], target - prev];
        }
        // Frames decoded ahead that playback has already passed are no use.
        while self.ready.front().is_some_and(|(i, _)| *i < target) {
            self.ready.pop_front();
        }
        let served = match self.ready.front() {
            Some((i, _)) if *i == target => {
                self.count(|s| s.lookahead_hits += 1);
                self.ready.pop_front().expect("checked")
            }
            _ => {
                self.ready.clear();
                if self.current.as_ref().is_some_and(|(i, _)| *i > target) {
                    // The decoder is past the target (it ran ahead): only a seek goes back.
                    self.current = None;
                }
                self.decode_to(target)?;
                self.publish_current()?
            }
        };
        self.last = Some(served.clone());
        // Work ahead unless the viewer is going backwards (then the frames ahead are the
        // ones just shown, and the renderer's recent-frame cache has them).
        self.ahead = !backward;
        Ok(served)
    }

    /// Frames `from..=to` (no further back than `to`'s keyframe), decoded forward and
    /// published, ascending. The last is `to` — or the nearest frame the file has there.
    fn decode_range(&mut self, from: usize, to: usize) -> Result<Vec<(usize, Surface)>, MediaError> {
        let from = from.max(self.video.index.keyframe_before(to));
        // Past it (or nowhere yet): `decode_to` seeks back to the keyframe.
        if self.current.as_ref().is_some_and(|(i, _)| *i > from) {
            self.current = None;
        }
        self.decode_to(from)?;
        let mut out = Vec::new();
        loop {
            let at = self.current.as_ref().map(|(i, _)| *i);
            match at {
                Some(i) if i >= from && i <= to => out.push(self.publish_current()?),
                _ => {}
            }
            if at.is_none_or(|i| i >= to) {
                break;
            }
            let Some((t, frame)) = self.decoder.as_mut().expect("opened").next()? else { break };
            self.count(|s| s.decoded += 1);
            let i = self.index_of(t);
            self.current = Some((i, frame));
        }
        Ok(out)
    }

    /// Playing backwards: the stretch before the frames on hand, decoded while there are
    /// still a few of those left. False when there's nothing to do.
    fn prefetch_behind(&mut self) -> bool {
        if !self.reverse || self.behind.len() >= REVERSE_LOW || self.decoder.is_none() {
            return false;
        }
        let lowest = self.behind.front().or(self.last.as_ref()).map(|(i, _)| *i);
        let Some(below) = lowest.and_then(|i| i.checked_sub(1)) else { return false };
        match self.decode_range(below.saturating_sub(REVERSE_CHUNK - 1), below) {
            Ok(got) if !got.is_empty() => {
                for frame in got.into_iter().rev() {
                    self.behind.push_front(frame);
                }
                true
            }
            _ => {
                self.reverse = false;
                false
            }
        }
    }

    /// Decodes and publishes one more frame ahead. False when there's nothing to do.
    fn prefetch(&mut self) -> bool {
        if self.prefetch_behind() {
            return true;
        }
        if !self.ahead || self.ready.len() >= LOOKAHEAD || self.decoder.is_none() {
            return false;
        }
        let step = (|| -> Result<bool, MediaError> {
            let Some((t, frame)) = self.decoder.as_mut().expect("checked").next()? else { return Ok(false) };
            self.count(|s| s.decoded += 1);
            let i = self.index_of(t);
            self.current = Some((i, frame));
            // A steady stride (a sped-up clip, or a sped-up compound's clip): only the
            // frames it will land on are uploaded — at 4× that's a quarter of them.
            let stride = self.strides[0];
            let steady = stride > 1 && self.strides.iter().all(|s| *s == stride);
            let from = self.ready.back().or(self.last.as_ref()).map(|(j, _)| *j);
            if steady && from.is_some_and(|j| i > j && !(i - j).is_multiple_of(stride)) {
                self.count(|s| s.skipped_uploads += 1);
                return Ok(true);
            }
            let published = self.publish_current()?;
            self.ready.push_back(published);
            Ok(true)
        })();
        match step {
            Ok(more) => {
                self.ahead = more;
                more
            }
            Err(_) => {
                // Leave the error for the next real request to report.
                self.ahead = false;
                self.ready.clear();
                false
            }
        }
    }
}

fn spawn<D: VideoDecoder + 'static>(path: PathBuf, video: VideoTrack, open: Opener<D>) -> Worker {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let stats = Arc::new(Mutex::new(DecodeStats::default()));
    // Busy until the thread finds nothing to do: every worker starts with a request on
    // its way, and `wait_idle` mustn't see it idle before the thread has even begun.
    let idle = Arc::new(AtomicBool::new(false));
    let (thread_stats, thread_idle) = (stats.clone(), idle.clone());
    let shared = Arc::new(Shared::default());
    let thread_shared = shared.clone();
    let name = format!("oa-decode {}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    let thread = std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            let mut d = Decoding::<D> {
                path,
                video,
                open,
                decoder: None,
                offset: Time::ZERO,
                current: None,
                ready: VecDeque::new(),
                last: None,
                ahead: false,
                strides: [1; 3],
                stats: thread_stats,
                shared: thread_shared,
                want: None,
                stand_in_pending: false,
                divisor: 1,
                behind: VecDeque::new(),
                reverse: false,
            };
            loop {
                // Work ahead while nothing is asked of us; block when there's nothing to do.
                let cmd = match rx.try_recv() {
                    Ok(cmd) => cmd,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {
                        if d.prefetch() {
                            continue;
                        }
                        thread_idle.store(true, Ordering::Release);
                        match rx.recv() {
                            Ok(cmd) => cmd,
                            Err(_) => return,
                        }
                    }
                };
                thread_idle.store(false, Ordering::Release);
                match cmd {
                    Cmd::Stop => return,
                    Cmd::Frame { target, reply } => {
                        let _ = reply.send(d.serve(target));
                    }
                    Cmd::Want { target, generation } => {
                        // Scrubbing sends a stream of these; only the newest matters.
                        // Frames someone is waiting for are still answered, in order.
                        let mut newest = (target, generation);
                        let mut waiting = Vec::new();
                        while let Ok(next) = rx.try_recv() {
                            match next {
                                Cmd::Want { target, generation } => newest = (target, generation),
                                Cmd::Frame { target, reply } => waiting.push((target, reply)),
                                Cmd::Stop => return,
                            }
                        }
                        for (target, reply) in waiting {
                            let _ = reply.send(d.serve(target));
                        }
                        d.serve_want(newest.0, newest.1);
                    }
                }
            }
        })
        .expect("spawn decode thread");
    Worker { tx, thread: Some(thread), stats, idle, shared }
}

struct Entry {
    path: PathBuf,
    video: VideoTrack,
    /// Decode threads for this file, started on demand. More than one when the same file
    /// is shown at two places at once (both sides of a transition inside one clip, a
    /// clip and its copy on another track) — one decoder serving both would seek back and
    /// forth every frame.
    workers: Vec<(Worker, Slot)>,
}

/// What the render side knows about a worker's position, for routing requests.
struct Slot {
    last: Option<usize>,
    /// The render frame (count of submits) that last used it.
    used: u64,
    /// The target of the `Want` in flight, so a UI redrawing the same frame doesn't
    /// re-ask every repaint.
    wanted: Option<usize>,
    /// When it was warmed for a clip about to play (render frame), if it was: kept for
    /// that clip a while, then free again if the clip never came (playback jumped).
    warmed_at: Option<u64>,
    /// A frame asked for during playback that wasn't ready within the wait budget: the
    /// decoder is still on it, and its answer is collected before asking for another (so
    /// requests never pile up behind a slow seek).
    pending: Option<(usize, Reply)>,
}

/// Where a decode thread's answer to a `Cmd::Frame` arrives.
type Reply = mpsc::Receiver<Result<(usize, Surface), MediaError>>;

/// Decoders one file may use at once: the place playing, the other side of a
/// transition, and a couple warmed for the next cuts back into the same file.
const DECODERS_PER_FILE: usize = 4;

/// Render frames a decoder warmed for a clip is kept for it (a few seconds of playback);
/// after that, if the clip never came, it's free for another.
const WARM_HOLD_FRAMES: u64 = 180;

impl Slot {
    /// Warmed for a clip that hasn't come yet, recently enough to still be kept for it.
    fn reserved(&self, frame: u64) -> bool {
        self.warmed_at.is_some_and(|at| frame.saturating_sub(at) < WARM_HOLD_FRAMES)
    }
}

/// (media id, frame index, requested size, YCbCr override) — what identifies a
/// converted frame.
type FrameKey = (u64, usize, [u32; 2], [u8; 2]);

/// The file's YCbCr matrix and range, unless the user overrode them
/// ([`SourceRequest::yuv`]: 0 keeps the file's own).
fn with_yuv_override(mut color: VideoColor, yuv: [u8; 2]) -> VideoColor {
    match yuv[0] {
        1 => color.matrix = oa_gpu::YuvMatrix::Bt601,
        2 => color.matrix = oa_gpu::YuvMatrix::Bt709,
        3 => color.matrix = oa_gpu::YuvMatrix::Bt2020,
        _ => {}
    }
    match yuv[1] {
        1 => color.full_range = false,
        2 => color.full_range = true,
        _ => {}
    }
    color
}

/// Serves `Source` nodes from real media files, frame-accurately.
///
/// Playback decodes forward without seeking; jumps seek to the keyframe at or before the
/// target and decode forward from there. Unknown media ids go to `fallback`.
pub struct MediaFrameSource<D: VideoDecoder> {
    open: Opener<D>,
    media: HashMap<u64, Entry>,
    /// Recently shown frames, newest last: (media, frame index, size) → converted image.
    recent: VecDeque<(FrameKey, GpuImage)>,
    /// Surfaces converted since the last submit; released when the GPU is done with them.
    in_flight: Vec<Surface>,
    pub fallback: Option<Box<dyn FrameSource>>,
    /// Render-side counters (uploads, cache hits); decode counters live with the threads.
    local: DecodeStats,
    /// Render frames so far (bumped on every submit): decoders used within one frame are
    /// in use at the same time.
    frame: u64,
    /// Scrubbing mode: never wait for a decode. Frames not ready yet are asked for in the
    /// background and a stand-in (the nearest frame already on hand) is shown meanwhile;
    /// [`FrameSource::settled`] says when to render again. Playback and export leave it
    /// off and get exact frames every time.
    pub interactive: bool,
    /// Playback: how long a frame may wait for its decoder. A frame that takes longer (a
    /// clip starting whose decoder is still seeking, a jump) shows the nearest frame on
    /// hand meanwhile — or keeps the last picture ([`RenderError::NotReady`]) when there
    /// is none — so the preview never stalls. `None` (export): always wait.
    pub wait_budget: Option<std::time::Duration>,
    wants: u64,
    last_exact: bool,
    stand_ins: bool,
}

impl<D: VideoDecoder + 'static> MediaFrameSource<D> {
    pub fn new(open: impl Fn(&Path, &VideoTrack) -> Result<D, MediaError> + Send + Sync + 'static) -> Self {
        MediaFrameSource {
            open: Arc::new(open),
            media: HashMap::new(),
            recent: VecDeque::new(),
            in_flight: Vec::new(),
            fallback: None,
            local: DecodeStats::default(),
            frame: 1,
            interactive: false,
            wait_budget: None,
            wants: 0,
            last_exact: true,
            stand_ins: false,
        }
    }

    /// Registers a file's video track under `media_id`.
    pub fn add(&mut self, media_id: u64, path: impl Into<PathBuf>, video: VideoTrack) {
        self.media.insert(media_id, Entry { path: path.into(), video, workers: Vec::new() });
    }

    /// Drops cached frames (e.g. when the project or preview size changes).
    pub fn clear_recent(&mut self) {
        self.recent.clear();
    }

    /// Counters summed over every file.
    pub fn stats(&self) -> DecodeStats {
        let mut total = self.local.clone();
        for (w, _) in self.media.values().flat_map(|e| e.workers.iter()) {
            total.add(&w.stats.lock().unwrap_or_else(|e| e.into_inner()));
        }
        total
    }

    /// Waits (up to `timeout`) until every decode thread has finished working ahead.
    /// For tests that count decoder work.
    pub fn wait_idle(&self, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if self.media.values().flat_map(|e| e.workers.iter()).all(|(w, _)| w.idle.load(Ordering::Acquire)) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    /// Gets a decoder ready for `media` at `source_time` before it's needed: playback
    /// calls this for clips about to start, so a new file doesn't hold up the frame it
    /// first shows in — its decoder is open, sought and decoding ahead by then. Does
    /// nothing if a decoder is already there (or on its way). A file with as many decoders
    /// as it may (one long file cut into many clips) moves one that isn't on screen —
    /// otherwise every cut back into that file would wait on a seek.
    pub fn warm(&mut self, media: u64, source_time: Time) {
        let frame = self.frame;
        let Some(entry) = self.media.get_mut(&media) else { return };
        let Some(target) = entry.video.index.frame_at(source_time.max(Time::ZERO)) else { return };
        let reach = forward_reach(&entry.video, target);
        let near = |s: &Slot| s.wanted == Some(target) || s.last.is_some_and(|l| l <= target && target - l <= reach);
        if entry.workers.iter().any(|(_, s)| near(s)) {
            return;
        }
        self.wants += 1;
        let generation = self.wants;
        // Marked as last used a frame ago, so the clip playing now keeps its own decoder.
        let slot = Slot { last: Some(target), used: frame.saturating_sub(1), wanted: Some(target), pending: None, warmed_at: Some(frame) };
        if entry.workers.len() < DECODERS_PER_FILE {
            let worker = spawn::<D>(entry.path.clone(), entry.video.clone(), self.open.clone());
            worker.shared.wanted.store(generation, Ordering::Release);
            let _ = worker.tx.send(Cmd::Want { target, generation });
            entry.workers.push((worker, slot));
            return;
        }
        // Not shown this frame or the one before, not busy with a frame of playback, and
        // not warmed for another clip still to come (one warmed for a clip playback jumped
        // past is free again after a while).
        let spare = entry
            .workers
            .iter()
            .enumerate()
            .filter(|(_, (_, s))| s.used + 1 < frame && s.pending.is_none() && !s.reserved(frame))
            .min_by_key(|(_, (_, s))| s.used)
            .map(|(i, _)| i);
        if let Some(i) = spare {
            let (worker, old) = &mut entry.workers[i];
            worker.shared.wanted.store(generation, Ordering::Release);
            let _ = worker.tx.send(Cmd::Want { target, generation });
            *old = Slot { used: old.used, ..slot };
        }
    }

    fn fetch(&mut self, media: u64, target: usize, size: [u32; 2]) -> Result<(usize, Surface), MediaError> {
        let index = self.route(media, target);
        let (worker, slot) = &mut self.media.get_mut(&media).expect("checked by caller").workers[index];
        // Answered after it anyway (the thread takes requests in order).
        slot.pending = None;
        slot.wanted = None;
        worker.shared.ask_size(size);
        let (reply, answer) = mpsc::channel();
        worker.tx.send(Cmd::Frame { target, reply }).map_err(|_| MediaError::Decode("decode thread stopped".into()))?;
        answer.recv().map_err(|_| MediaError::Decode("decode thread stopped".into()))?
    }

    /// Playback within a wait budget: the exact frame if its decoder has it within
    /// `budget`, else `None` — the request stays with the decoder (`Slot::pending`) and
    /// its answer is picked up next time, converted and kept as a stand-in if playback
    /// has moved on by then.
    fn fetch_within(&mut self, gpu: &mut GpuServices<'_>, media: u64, target: usize, size: [u32; 2], yuv: [u8; 2], budget: std::time::Duration) -> Result<Option<GpuImage>, RenderError> {
        let until = std::time::Instant::now() + budget;
        let index = self.route(media, target);
        let (worker, slot) = &mut self.media.get_mut(&media).expect("checked by caller").workers[index];
        slot.wanted = None;
        worker.shared.ask_size(size);
        let tx = worker.tx.clone();
        let stopped = || RenderError::Source("decode thread stopped".into());
        let left = || until.saturating_duration_since(std::time::Instant::now());
        // The decoder is still on an earlier request: its answer comes first. (Like
        // `fetch`, an answer is kept under the frame asked for — past the file's end
        // that's the last frame there is.)
        if let Some((asked, rx)) = slot.pending.take() {
            match rx.recv_timeout(left()) {
                Ok(Ok((_, surface))) if asked == target => return self.convert(gpu, surface, (media, target, size, yuv)).map(Some),
                // Passed by now; still the nearest thing to show if this one is late.
                Ok(Ok((_, surface))) => drop(self.convert(gpu, surface, (media, asked, size, yuv))?),
                Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.slot(media, index).pending = Some((asked, rx));
                    return Ok(None);
                }
            }
        }
        let (reply, rx) = mpsc::channel();
        tx.send(Cmd::Frame { target, reply }).map_err(|_| stopped())?;
        match rx.recv_timeout(left()) {
            Ok(Ok((_, surface))) => self.convert(gpu, surface, (media, target, size, yuv)).map(Some),
            Ok(Err(e)) => Err(RenderError::Source(e.to_string())),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(stopped()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.slot(media, index).pending = Some((target, rx));
                Ok(None)
            }
        }
    }

    fn slot(&mut self, media: u64, index: usize) -> &mut Slot {
        &mut self.media.get_mut(&media).expect("checked by caller").workers[index].1
    }

    /// The frame of `media` on hand nearest to `target` (converted recently at this size),
    /// if one is close enough to stand in for it: a frame from elsewhere in the file (the
    /// clip before a cut) would flash the wrong picture.
    fn nearest_on_hand(&self, media: u64, target: usize, size: [u32; 2], yuv: [u8; 2]) -> Option<GpuImage> {
        self.recent
            .iter()
            .filter(|((m, i, s, y), _)| *m == media && *s == size && *y == yuv && i.abs_diff(target) <= 2 * FORWARD_DECODE_LIMIT)
            .min_by_key(|((_, i, _, _), _)| i.abs_diff(target))
            .map(|(_, image)| image.clone())
    }

    /// The decoder `target` should go to (starting one if needed).
    fn route(&mut self, media: u64, target: usize) -> usize {
        let frame = self.frame;
        let entry = self.media.get_mut(&media).expect("checked by caller");
        // Route to the decoder that gets there cheapest: one already at the frame or just
        // behind it (decoding forward). Otherwise reuse one this frame isn't using (a
        // plain seek), and only when every decoder is busy in this same frame — two
        // places in one file shown at once — start another.
        // A decoder another request already used this frame is taken (the other side of a
        // transition in the same file): sharing it would send it back and forth every
        // frame — unless it's at this very frame. Of the rest, the one closest behind.
        let reach = forward_reach(&entry.video, target);
        // Just behind it (decoding on) — or just ahead, a step back: a decoder playing a
        // clip in reverse has the frames before its last one ready.
        let near = |s: &Slot| {
            s.last == Some(target) || (s.used != frame && s.last.is_some_and(|l| (l <= target && target - l <= reach) || (l > target && l - target <= 2 * REVERSE_CHUNK)))
        };
        // Idle: not used this frame — and one warmed for a clip still to come only if
        // there's nothing else (sending it off to seek here would undo the warming).
        let idle = entry.workers.iter().enumerate().filter(|(_, (_, s))| s.used != frame).min_by_key(|(_, (_, s))| (s.reserved(frame), s.used)).map(|(i, _)| i);
        // Of those near, one warmed for another clip still to come is taken only when it's
        // this very frame (its clip has come).
        let behind = |s: &Slot| s.last.unwrap_or(target).abs_diff(target);
        let nearest = entry
            .workers
            .iter()
            .enumerate()
            .filter(|(_, (_, s))| near(s) && (behind(s) == 0 || !s.reserved(frame)))
            .min_by_key(|(_, (_, s))| behind(s))
            .map(|(i, _)| i);
        let index = match (nearest, idle) {
            (Some(i), _) | (None, Some(i)) => i,
            (None, None) if entry.workers.len() < DECODERS_PER_FILE || entry.workers.is_empty() => {
                let worker = spawn::<D>(entry.path.clone(), entry.video.clone(), self.open.clone());
                entry.workers.push((worker, Slot { last: None, used: 0, wanted: None, pending: None, warmed_at: None }));
                entry.workers.len() - 1
            }
            (None, None) => 0,
        };
        let slot = &mut entry.workers[index].1;
        slot.last = Some(target);
        slot.used = frame;
        // Its clip has come (or it's been put to other use): no longer held.
        slot.warmed_at = None;
        index
    }

    /// Converts a decoded surface to a working-format image of `size` and remembers it.
    fn convert(&mut self, gpu: &mut GpuServices<'_>, surface: Surface, key: FrameKey) -> Result<GpuImage, RenderError> {
        self.local.uploads += 1;
        let frame = Nv12Frame {
            texture: &surface.texture,
            chroma: surface.chroma.as_deref(),
            alpha: surface.alpha.as_deref(),
            coded_size: surface.coded_size,
            visible_size: surface.visible_size,
            rotation_quarter_turns: surface.rotation_quarter_turns,
            color: with_yuv_override(surface.color, key.3),
        };
        let image = gpu.convert_nv12(&frame, key.2).map_err(|e| RenderError::Source(e.to_string()))?;
        self.in_flight.push(surface);
        self.recent.push_back((key, image.clone()));
        let mut bytes: u64 = self.recent.iter().map(|(_, i)| i.tex.bytes()).sum();
        while self.recent.len() > RECENT_FRAMES || (bytes > RECENT_FRAME_BYTES && self.recent.len() > 1) {
            if let Some((_, dropped)) = self.recent.pop_front() {
                bytes -= dropped.tex.bytes();
            }
        }
        Ok(image)
    }

    fn recent_image(&mut self, key: FrameKey) -> Option<GpuImage> {
        let pos = self.recent.iter().position(|(k, _)| *k == key)?;
        let hit = self.recent.remove(pos).expect("found above");
        let image = hit.1.clone();
        self.recent.push_back(hit);
        Some(image)
    }

    /// Scrubbing: the exact frame if it's ready, else ask for it in the background and
    /// return the closest stand-in on hand. `None` only when nothing of this file has
    /// been shown yet (the caller then waits once).
    fn frame_now(&mut self, gpu: &mut GpuServices<'_>, media: u64, target: usize, size: [u32; 2], yuv: [u8; 2]) -> Result<Option<GpuImage>, RenderError> {
        let index = self.route(media, target);
        let (worker, slot) = &mut self.media.get_mut(&media).expect("checked by caller").workers[index];
        worker.shared.ask_size(size);
        let latest = worker.shared.latest.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some((i, surface)) = latest.clone().filter(|(i, _)| *i == target) {
            slot.wanted = None;
            return self.convert(gpu, surface, (media, i, size, yuv)).map(Some);
        }
        if slot.wanted != Some(target) {
            self.wants += 1;
            worker.shared.wanted.store(self.wants, Ordering::Release);
            let _ = worker.tx.send(Cmd::Want { target, generation: self.wants });
            slot.wanted = Some(target);
        }
        self.last_exact = false;
        self.stand_ins = true;
        // Stand-in: whichever is closer to the target — the decoder's latest frame (often
        // the keyframe it just landed on) or a frame of this file already converted.
        let nearest = self
            .recent
            .iter()
            .filter(|((m, _, s, y), _)| *m == media && *s == size && *y == yuv)
            .min_by_key(|((_, i, _, _), _)| i.abs_diff(target))
            .map(|((_, i, _, _), image)| (*i, image.clone()));
        match (latest, nearest) {
            (Some((i, surface)), near) if near.as_ref().is_none_or(|(n, _)| i.abs_diff(target) < n.abs_diff(target)) => {
                let key = (media, i, size, yuv);
                match self.recent_image(key) {
                    Some(image) => Ok(Some(image)),
                    None => self.convert(gpu, surface, key).map(Some),
                }
            }
            (_, near) => Ok(near.map(|(_, image)| image)),
        }
    }
}

impl<D: VideoDecoder + 'static> FrameSource for MediaFrameSource<D> {
    fn frame(&mut self, gpu: &mut GpuServices<'_>, req: &SourceRequest) -> Result<GpuImage, RenderError> {
        let Some(entry) = self.media.get(&req.media) else {
            return match self.fallback.as_mut() {
                Some(f) => f.frame(gpu, req),
                None => Err(RenderError::Source(format!("media {} is not loaded", req.media))),
            };
        };
        let target = entry
            .video
            .index
            .frame_at(req.source_time.max(Time::ZERO))
            .ok_or_else(|| RenderError::Source(format!("media {} has no frames", req.media)))?;
        let key = (req.media, target, req.size, req.yuv);
        self.last_exact = true;
        if let Some(image) = self.recent_image(key) {
            self.local.frame_cache_hits += 1;
            return Ok(image);
        }
        // Let finished GPU work release decoder textures before asking for more.
        let _ = gpu.ctx.device.poll(wgpu::PollType::Poll);
        if self.interactive {
            // Nothing of this file on hand yet (its decoder is still opening and seeking):
            // the last picture stays up until it is, rather than the app waiting.
            return self.frame_now(gpu, req.media, target, req.size, req.yuv)?.ok_or(RenderError::NotReady);
        }
        if let Some(budget) = self.wait_budget {
            if let Some(image) = self.fetch_within(gpu, req.media, target, req.size, req.yuv, budget)? {
                return Ok(image);
            }
            self.last_exact = false;
            self.stand_ins = true;
            return self.nearest_on_hand(req.media, target, req.size, req.yuv).ok_or(RenderError::NotReady);
        }
        self.last_exact = true;
        // A decoder past the file's end answers with its last frame; it's cached under the
        // key asked for, so the next request doesn't decode again.
        let (_, surface) = self.fetch(req.media, target, req.size).map_err(|e| RenderError::Source(e.to_string()))?;
        self.convert(gpu, surface, key)
    }

    fn exact(&self) -> bool {
        self.last_exact
    }

    fn settled(&mut self) -> bool {
        !std::mem::take(&mut self.stand_ins)
    }

    fn submitted(&mut self, queue: &wgpu::Queue) {
        self.frame += 1;
        if self.in_flight.is_empty() {
            return;
        }
        let done = std::mem::take(&mut self.in_flight);
        queue.on_submitted_work_done(move || drop(done));
    }

    fn status(&self) -> Option<String> {
        let s = self.stats();
        Some(format!(
            "decoded {}, shown {}, seeks {}, reused {}, ahead {}",
            s.decoded, s.uploads, s.seeks, s.frame_cache_hits, s.lookahead_hits
        ))
    }
}

impl<D: VideoDecoder> Drop for MediaFrameSource<D> {
    fn drop(&mut self) {
        // Surfaces still waiting on a submission that never happened: release them now.
        self.in_flight.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_time::Rational;

    /// Never asked to decode: routing is all these tests look at.
    struct Idle;

    impl VideoDecoder for Idle {
        type Frame = ();
        fn seek(&mut self, _t: Time) -> Result<(), MediaError> {
            Ok(())
        }
        fn next(&mut self) -> Result<Option<(Time, ())>, MediaError> {
            Ok(None)
        }
        fn publish(&mut self, _frame: &()) -> Result<Surface, MediaError> {
            Err(MediaError::Decode("not in this test".into()))
        }
    }

    /// 240 frames, keyframes every 120: two places 40 frames apart are each within
    /// "decode on" reach of the other's decoder.
    fn long_gop() -> VideoTrack {
        VideoTrack {
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            width: 64,
            height: 64,
            coded_width: 64,
            coded_height: 64,
            rotation_quarter_turns: 0,
            time_base: Rational::new(1, 30),
            avg_rate: None,
            color: VideoColor::guess(64),
            hdr: false,
            transfer_tag: None,
            primaries_tag: None,
            has_alpha: false,
            still: false,
            index: crate::FrameIndex { time_base: Rational::new(1, 30), pts: (0..240).collect(), keyframes: vec![0, 120] },
        }
    }

    /// Two places in one file shown in the same frame (both sides of a transition) keep
    /// a decoder each, whichever order they're asked in — one decoder given both would go
    /// back and forth every frame (the stress project's export ran at two-thirds speed).
    #[test]
    fn two_places_at_once_keep_their_own_decoders() {
        let mut s = MediaFrameSource::<Idle>::new(|_, _| Ok(Idle));
        s.add(1, "none", long_gop());
        let (a, b) = (s.route(1, 20), s.route(1, 60));
        assert_ne!(a, b, "the second place got a decoder of its own");
        for i in 1..20 {
            s.frame += 1;
            // Asked the other way round on odd frames.
            let (x, y) = if i % 2 == 1 { (s.route(1, 60 + i), s.route(1, 20 + i)) } else { (s.route(1, 20 + i), s.route(1, 60 + i)) };
            let (early, late) = if i % 2 == 1 { (y, x) } else { (x, y) };
            assert_eq!((early, late), (a, b), "frame {i}: each place stays with its decoder");
        }
        // The same frame asked twice (at two sizes) goes to the one decoder.
        s.frame += 1;
        assert_eq!(s.route(1, 40), s.route(1, 40));
    }

    /// A decoder warmed for the next cut into the same file is kept for it: the clip
    /// playing now doesn't take it for a seek of its own, and the cut finds it there. One
    /// warmed for a clip that never came (playback jumped) is free again later.
    #[test]
    fn warmed_decoders_wait_for_their_clip() {
        let mut s = MediaFrameSource::<Idle>::new(|_, _| Ok(Idle));
        s.add(1, "none", long_gop());
        // Playing frame 200 and (the other side of a transition) 150; the next clip cuts
        // to frame 10.
        let playing = s.route(1, 200);
        let other = s.route(1, 150);
        assert_ne!(playing, other);
        s.frame += 2;
        s.warm(1, long_gop().index.time_of(10));
        let warmed = s.media[&1].workers.iter().position(|(_, slot)| slot.reserved(s.frame)).expect("warmed one");
        // Playback jumps within the current clip (a seek): not onto the warmed decoder.
        s.frame += 1;
        assert_ne!(s.route(1, 20), warmed, "a seek of the clip playing took the warmed decoder");
        // The cut comes: frame 10 goes to the warmed decoder.
        s.frame += 1;
        assert_eq!(s.route(1, 10), warmed);
        assert!(!s.media[&1].workers[warmed].1.reserved(s.frame), "its clip came: no longer held");
        // A warm-up for a clip that never comes lets go after a while.
        s.warm(1, long_gop().index.time_of(230));
        s.frame += WARM_HOLD_FRAMES;
        assert!(s.media[&1].workers.iter().all(|(_, slot)| !slot.reserved(s.frame)));
    }
}

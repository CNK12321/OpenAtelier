//! Routing a frame request to whatever can serve it: the hardware video decoder, a
//! still image on the GPU, or the test pattern when a file is missing.

use oa_gpu::{FrameSource, GpuImage, GpuServices, RenderError, SourceRequest, TestPatternSource};
use oa_media::{MediaKind, StillSource};
use std::collections::HashMap;

/// Video files, each with the decoder that suits it (`oa_media::frame_source`).
type VideoSource = oa_media::MediaFrameSource<oa_media::AnyDecoder>;

pub struct Sources {
    video: Option<VideoSource>,
    stills: StillSource,
    pattern: TestPatternSource,
    kinds: HashMap<u64, MediaKind>,
    last_exact: bool,
}

impl Sources {
    pub fn new() -> Self {
        Sources {
            video: None,
            stills: StillSource::default(),
            pattern: TestPatternSource::default(),
            kinds: HashMap::new(),
            last_exact: true,
        }
    }

    pub fn set_video(&mut self, video: Option<VideoSource>) {
        self.video = video;
    }

    /// Scrubbing (true): never wait for the decoder — show the nearest frame on hand and
    /// fill in the exact one when it's decoded. Playback and export: exact frames always.
    pub fn set_interactive(&mut self, on: bool) {
        if let Some(video) = self.video.as_mut() {
            video.interactive = on;
        }
    }

    /// Opens a decoder for `media` at `source_time` ahead of time (a clip about to play).
    pub fn warm(&mut self, media: u64, source_time: oa_time::Time) {
        if let Some(video) = self.video.as_mut() {
            video.warm(media, source_time);
        }
    }

    pub fn stills_mut(&mut self) -> &mut StillSource {
        &mut self.stills
    }

    pub fn take_video(&mut self) -> Option<VideoSource> {
        self.video.take()
    }

    /// Whether `media_id` is already set up to be served.
    pub fn knows(&self, media_id: u64) -> bool {
        self.kinds.contains_key(&media_id)
    }

    pub fn register(&mut self, media_id: u64, kind: MediaKind) {
        self.kinds.insert(media_id, kind);
    }

    pub fn forget_all(&mut self) {
        self.kinds.clear();
        self.stills = StillSource::default();
        self.video = None;
    }
}

impl FrameSource for Sources {
    fn frame(&mut self, gpu: &mut GpuServices<'_>, req: &SourceRequest) -> Result<GpuImage, RenderError> {
        self.last_exact = true;
        if self.stills.contains(req.media) {
            return self.stills.frame(gpu, req);
        }
        if let Some(video) = self.video.as_mut() {
            match video.frame(gpu, req) {
                Ok(image) => {
                    self.last_exact = video.exact();
                    return Ok(image);
                }
                // A missing or undecodable file shows the test pattern rather than
                // failing the whole frame.
                Err(e) => {
                    let _ = e;
                }
            }
        }
        self.pattern.frame(gpu, req)
    }

    fn status(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(video) = &self.video {
            parts.extend(video.status());
        }
        parts.extend(self.stills.status());
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn exact(&self) -> bool {
        self.last_exact
    }

    fn settled(&mut self) -> bool {
        if let Some(video) = self.video.as_mut() {
            return video.settled();
        }
        true
    }

    fn submitted(&mut self, queue: &wgpu::Queue) {
        if let Some(video) = self.video.as_mut() {
            video.submitted(queue);
        }
    }
}

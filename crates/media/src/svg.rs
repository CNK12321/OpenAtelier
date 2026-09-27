//! SVG images, as stills: parsed and drawn in Rust (`resvg`), since ffmpeg only reads SVG
//! when it was built with librsvg.
//!
//! A drawing has no pixel size of its own, so it's drawn big enough to stay sharp when
//! scaled up on the canvas: its own size, or [`MIN_LONG_SIDE`] on the longer side if
//! that's more (never past [`MAX_LONG_SIDE`]). That becomes the still's "native" size.

use crate::{FrameIndex, MediaError, MediaProbe, VideoTrack};
use oa_time::{Rational, Time};
use std::path::Path;

/// Drawings are rasterized at least this large on their longer side.
pub const MIN_LONG_SIDE: f32 = 2048.0;
/// ...and no larger than this (a GPU texture limit, and memory).
pub const MAX_LONG_SIDE: f32 = 8192.0;

pub fn is_svg(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

fn tree(path: &Path) -> Result<resvg::usvg::Tree, MediaError> {
    let data = std::fs::read(path).map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
    let mut options = resvg::usvg::Options { resources_dir: path.parent().map(Path::to_path_buf), ..Default::default() };
    options.fontdb_mut().load_system_fonts();
    resvg::usvg::Tree::from_data(&data, &options).map_err(|e| MediaError::Probe(format!("{}: {e}", path.display())))
}

/// The size a drawing of `w`×`h` is rasterized at (see the module docs).
pub fn raster_size(w: f32, h: f32) -> [u32; 2] {
    let (w, h) = (w.max(1.0), h.max(1.0));
    let long = w.max(h);
    let k = (MIN_LONG_SIDE / long).max(1.0).min(MAX_LONG_SIDE / long);
    [(w * k).round().max(1.0) as u32, (h * k).round().max(1.0) as u32]
}

/// Probes an SVG as a still image with transparency.
pub fn probe(path: &Path) -> Result<MediaProbe, MediaError> {
    let size = tree(path)?.size();
    let [width, height] = raster_size(size.width(), size.height());
    let time_base = Rational::new(1, 1000);
    let mut color = oa_gpu::VideoColor::guess(height);
    color.full_range = true;
    let video = VideoTrack {
        codec: "svg".into(),
        pixel_format: "rgba".into(),
        width,
        height,
        coded_width: width,
        coded_height: height,
        rotation_quarter_turns: 0,
        time_base,
        avg_rate: None,
        color,
        hdr: false,
        transfer_tag: None,
        primaries_tag: None,
        has_alpha: true,
        still: true,
        index: FrameIndex { time_base, pts: vec![0], keyframes: vec![0] },
    };
    Ok(MediaProbe { container: "svg".into(), duration: Time::ZERO, video: Some(video), audio: None })
}

/// Draws the SVG at exactly `size` as straight-alpha sRGB RGBA8 (what stills upload).
pub fn rasterize(path: &Path, size: [u32; 2]) -> Result<Vec<u8>, MediaError> {
    let tree = tree(path)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size[0].max(1), size[1].max(1)).ok_or_else(|| MediaError::Decode(format!("can't draw an SVG at {}x{}", size[0], size[1])))?;
    let s = tree.size();
    let transform = resvg::tiny_skia::Transform::from_scale(size[0] as f32 / s.width().max(1e-3), size[1] as f32 / s.height().max(1e-3));
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // tiny-skia keeps premultiplied pixels; stills are straight alpha.
    let mut pixels = pixmap.take();
    for px in pixels.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            }
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CIRCLE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" viewBox="0 0 100 50">
        <rect x="0" y="0" width="50" height="50" fill="#ff0000"/>
        <circle cx="75" cy="25" r="20" fill="#0000ff" fill-opacity="0.5"/>
    </svg>"##;

    fn file(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("oa-svg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    /// Small drawings are rasterized big enough to scale up sharply; shape kept.
    #[test]
    fn drawings_get_a_generous_size() {
        assert_eq!(raster_size(100.0, 50.0), [2048, 1024]);
        assert_eq!(raster_size(4000.0, 1000.0), [4000, 1000], "already big: its own size");
        assert_eq!(raster_size(20000.0, 100.0), [8192, 41], "capped");
    }

    #[test]
    fn an_svg_probes_as_a_still_and_draws() {
        let p = file("shapes.svg", CIRCLE);
        assert!(is_svg(&p));
        let probe = probe(&p).unwrap();
        let v = probe.video.as_ref().unwrap();
        assert!(v.still && v.has_alpha);
        assert_eq!([v.width, v.height], [2048, 1024]);
        let small = [200, 100];
        let px = rasterize(&p, small).unwrap();
        assert_eq!(px.len(), 200 * 100 * 4);
        let at = |x: usize, y: usize| &px[(y * 200 + x) * 4..(y * 200 + x) * 4 + 4];
        assert_eq!(at(20, 50), [255, 0, 0, 255], "the red square");
        let blue = at(150, 50);
        assert_eq!((blue[2], blue[3]), (255, 128), "half-transparent blue, straight alpha: {blue:?}");
        assert_eq!(at(199, 0)[3], 0, "transparent around it");
    }
}

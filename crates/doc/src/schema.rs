//! Built-in parameters every visual item has.
//!
//! Coordinate conventions (see DESIGN.md §"Space and resolution"):
//! * `transform.position` is an offset from the reframed position, as a fraction of
//!   the canvas per axis, so layouts survive switching aspect ratio.
//! * `transform.scale` multiplies the reframe scale (1 = fitted/filled size).
//! * `transform.anchor` and `reframe.focus` are fractions of the source.
//! * `transform.squash` is volume-preserving squash & stretch: x *= 1+s, y /= 1+s.

use crate::format::FitMode;
use oa_params::{Gradient, ParamSchema, Unit, Value};
use std::sync::OnceLock;

pub const POSITION: &str = "transform.position";
pub const SCALE: &str = "transform.scale";
pub const SQUASH: &str = "transform.squash";
pub const ROTATION: &str = "transform.rotation";
pub const ANCHOR: &str = "transform.anchor";
pub const OPACITY: &str = "transform.opacity";
/// How the layer mixes with what's under it: `normal`, `add`, `multiply`, `screen`,
/// `darken`, `lighten`. On an effect container: how its result mixes over the picture it
/// took (with its opacity, so an intro fade fades the effect in).
pub const BLEND: &str = "transform.blend";
pub const BLEND_MODES: [&str; 6] = ["normal", "add", "multiply", "screen", "darken", "lighten"];
/// Crop: how much of each edge of the picture is cut away, as a fraction of its width or
/// height. The layer keeps its place; the cut parts are transparent.
pub const CROP_LEFT: &str = "crop.left";
pub const CROP_RIGHT: &str = "crop.right";
pub const CROP_TOP: &str = "crop.top";
pub const CROP_BOTTOM: &str = "crop.bottom";
pub const CROPS: [&str; 4] = [CROP_LEFT, CROP_RIGHT, CROP_TOP, CROP_BOTTOM];
pub const FIT: &str = "reframe.fit";
pub const FOCUS: &str = "reframe.focus";
pub const SOLID_COLOR: &str = "solid.color";
/// Clip gain in decibels; -60 and below is silence.
pub const AUDIO_GAIN: &str = "audio.gain";
/// With the clip's speed ≠ 1, keep the sound's pitch (on by default) rather than letting
/// it rise and fall with the speed like tape.
pub const AUDIO_KEEP_PITCH: &str = "audio.keep_pitch";
/// Off when the clip's sound has been extracted to its own clip on an audio track.
pub const AUDIO_ENABLED: &str = "audio.enabled";

pub fn visual() -> &'static [ParamSchema] {
    static S: OnceLock<Vec<ParamSchema>> = OnceLock::new();
    S.get_or_init(|| {
        vec![
            ParamSchema::new(POSITION, Value::Vec2([0.0, 0.0]), Unit::CanvasFraction),
            ParamSchema::new(SCALE, Value::Vec2([1.0, 1.0]), Unit::None),
            ParamSchema::new(SQUASH, Value::Float(0.0), Unit::None).range(-0.9, 2.0),
            ParamSchema::new(ROTATION, Value::Float(0.0), Unit::Degrees),
            ParamSchema::new(ANCHOR, Value::Vec2([0.5, 0.5]), Unit::SourceFraction),
            ParamSchema::new(OPACITY, Value::Float(1.0), Unit::None).range(0.0, 1.0),
            ParamSchema::new(BLEND, Value::Enum("normal".into()), Unit::None).options(&BLEND_MODES),
            ParamSchema::new(FIT, Value::Enum(FitMode::Fill.as_str().into()), Unit::None).static_only(),
            ParamSchema::new(FOCUS, Value::Vec2([0.5, 0.5]), Unit::SourceFraction).anchored_to_source(),
            ParamSchema::new(CROP_LEFT, Value::Float(0.0), Unit::None).range(0.0, 1.0),
            ParamSchema::new(CROP_RIGHT, Value::Float(0.0), Unit::None).range(0.0, 1.0),
            ParamSchema::new(CROP_TOP, Value::Float(0.0), Unit::None).range(0.0, 1.0),
            ParamSchema::new(CROP_BOTTOM, Value::Float(0.0), Unit::None).range(0.0, 1.0),
        ]
    })
}

/// What's behind the clips (`Sequence::params`): `solid` (a color or gradient),
/// `blur` (the picture on screen, enlarged to cover the canvas and blurred — fills the
/// sides when 16:9 footage sits in a 9:16 format) or `texture` (a picture tiled).
pub const BG_MODE: &str = "background.mode";
pub const BG_COLOR: &str = "background.color";
/// Blur radius in canvas px (at the canvas's size).
pub const BG_BLUR: &str = "background.blur";
/// How much darker the blurred picture is: 0 = as is, 1 = black.
pub const BG_DIM: &str = "background.dim";
pub const BG_TEXTURE: &str = "background.texture";
/// Tile height as a fraction of the canvas height.
pub const BG_TILE: &str = "background.tile";

pub fn background() -> &'static [ParamSchema] {
    static S: OnceLock<Vec<ParamSchema>> = OnceLock::new();
    S.get_or_init(|| {
        vec![
            ParamSchema::new(BG_MODE, Value::Enum("solid".into()), Unit::None).options(&["solid", "blur", "texture"]).static_only(),
            ParamSchema::new(BG_COLOR, Value::Gradient(Gradient::solid([0.0, 0.0, 0.0, 1.0])), Unit::None),
            ParamSchema::new(BG_BLUR, Value::Float(60.0), Unit::None).range(0.0, 300.0),
            ParamSchema::new(BG_DIM, Value::Float(0.2), Unit::None).range(0.0, 1.0),
            ParamSchema::new(BG_TEXTURE, Value::Media(None), Unit::None).static_only(),
            ParamSchema::new(BG_TILE, Value::Float(0.25), Unit::None).range(0.02, 2.0),
        ]
    })
}

/// The output transform (`Sequence::params`, DESIGN.md §9): how the working space goes
/// onto an SDR screen and into the exported file — the same step for both, so what you
/// see is what you get. `auto` tone maps only when the sequence uses HDR or log footage;
/// `off` clips at white; `soft` rolls highlights off; `filmic` is an S-curve.
pub const OUT_TONE_MAP: &str = "output.tone_map";
/// Exposure of the whole picture before tone mapping, in stops.
pub const OUT_EXPOSURE: &str = "output.exposure";

pub fn output() -> &'static [ParamSchema] {
    static S: OnceLock<Vec<ParamSchema>> = OnceLock::new();
    S.get_or_init(|| {
        vec![
            ParamSchema::new(OUT_TONE_MAP, Value::Enum("auto".into()), Unit::None).options(&["auto", "off", "soft", "filmic"]).static_only(),
            ParamSchema::new(OUT_EXPOSURE, Value::Float(0.0), Unit::None).range(-4.0, 4.0),
        ]
    })
}

pub fn solid() -> &'static [ParamSchema] {
    static S: OnceLock<Vec<ParamSchema>> = OnceLock::new();
    S.get_or_init(|| vec![ParamSchema::new(SOLID_COLOR, Value::Color([0.0, 0.0, 0.0, 1.0]), Unit::None)])
}

/// "Highlight when spoken": the value a parameter takes on the word being spoken is
/// stored beside it, under its id with this suffix (for a title's color and outline, and
/// any text effect's parameters).
pub const SPOKEN_SUFFIX: &str = "#spoken";

/// The id holding `param`'s value for the word being spoken.
pub fn spoken(param: &str) -> String {
    format!("{param}{SPOKEN_SUFFIX}")
}

/// A title's own parameters that can be highlighted when spoken.
pub const SPOKEN_TEXT_PARAMS: [&str; 3] = [TEXT_COLOR, TEXT_OUTLINE, TEXT_OUTLINE_COLOR];

pub const TEXT_CONTENT: &str = "text.content";
/// Family name; empty means the platform's default UI font.
pub const TEXT_FONT: &str = "text.font";
pub const TEXT_BOLD: &str = "text.bold";
pub const TEXT_ITALIC: &str = "text.italic";
/// Em size in canvas px.
pub const TEXT_SIZE: &str = "text.size";
/// The fill: a directional gradient across the text box (one stop = a plain color).
pub const TEXT_COLOR: &str = "text.color";
/// Legacy (before gradients): on/off for a top-to-bottom gradient from `text.color` to
/// `text.color2`. Still honored when set; no longer offered.
pub const TEXT_GRADIENT: &str = "text.gradient";
pub const TEXT_COLOR2: &str = "text.color2";
pub const TEXT_ALIGN: &str = "text.align";
/// Extra letter spacing, in ems.
pub const TEXT_TRACKING: &str = "text.tracking";
/// Baseline-to-baseline distance, in ems.
pub const TEXT_LINE_HEIGHT: &str = "text.line_height";
/// Outline thickness in canvas px (0 = none).
pub const TEXT_OUTLINE: &str = "text.outline";
pub const TEXT_OUTLINE_COLOR: &str = "text.outline_color";

/// Parameters of a text clip: what it says and how it's set. Numbers and colors are
/// keyframable like any other property.
pub fn text() -> &'static [ParamSchema] {
    static S: OnceLock<Vec<ParamSchema>> = OnceLock::new();
    S.get_or_init(|| {
        vec![
            ParamSchema::new(TEXT_CONTENT, Value::Text("Title".into()), Unit::None).static_only(),
            ParamSchema::new(TEXT_FONT, Value::Text(String::new()), Unit::None).static_only(),
            ParamSchema::new(TEXT_BOLD, Value::Bool(false), Unit::None).static_only(),
            ParamSchema::new(TEXT_ITALIC, Value::Bool(false), Unit::None).static_only(),
            ParamSchema::new(TEXT_SIZE, Value::Float(120.0), Unit::None).range(4.0, 1000.0),
            ParamSchema::new(TEXT_COLOR, Value::Gradient(Gradient::solid([1.0, 1.0, 1.0, 1.0])), Unit::None),
            ParamSchema::new(TEXT_ALIGN, Value::Enum("center".into()), Unit::None).options(&["left", "center", "right"]).static_only(),
            ParamSchema::new(TEXT_TRACKING, Value::Float(0.0), Unit::None).range(-0.2, 1.0),
            ParamSchema::new(TEXT_LINE_HEIGHT, Value::Float(1.2), Unit::None).range(0.6, 3.0),
            ParamSchema::new(TEXT_OUTLINE, Value::Float(0.0), Unit::None).range(0.0, 40.0),
            ParamSchema::new(TEXT_OUTLINE_COLOR, Value::Gradient(Gradient::solid([0.0, 0.0, 0.0, 1.0])), Unit::None),
        ]
    })
}

/// Built-in parameters of anything audible.
pub fn audio() -> &'static [ParamSchema] {
    static S: OnceLock<Vec<ParamSchema>> = OnceLock::new();
    S.get_or_init(|| {
        vec![
            ParamSchema::new(AUDIO_GAIN, Value::Float(0.0), Unit::Decibels).range(-60.0, 12.0),
            ParamSchema::new(AUDIO_KEEP_PITCH, Value::Bool(true), Unit::None).static_only(),
            ParamSchema::new(AUDIO_ENABLED, Value::Bool(true), Unit::None).static_only(),
        ]
    })
}

//! Text for text layers: system fonts, shaping and layout, and glyph distance fields.
//!
//! Rendering happens on the GPU (`oa-gpu`): glyph distance fields go into an atlas and
//! each glyph is one quad, so per-letter animation is a vertex-shader transform and
//! edges, outlines and fills are per-pixel work on the distance field.

pub mod fonts;
pub mod layout;
pub mod sdf;

pub use fonts::{default_family, families, FontId};
pub use layout::{layout, Align, Layout, PlacedGlyph, TextSpec};

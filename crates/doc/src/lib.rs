//! The project document.
//!
//! The document is plain data with structural sharing (`Arc` per sequence/track/media),
//! so cloning a [`Project`] for a render snapshot is cheap and never blocks the UI.
//! All changes go through [`Op`]s, which return their own inverse; [`Document`] builds
//! undo/redo on top of that.

pub mod color;
mod file;
mod format;
mod history;
mod model;
mod ops;
pub mod schema;
mod validate;

pub use file::{FileError, ProjectFile, FORMAT_VERSION};
pub use format::{AspectPreset, CanvasSize, FitMode, FormatVariant, Orientation, Reframe, PRESETS};
pub use history::{Document, MAX_UNDO};
pub use model::*;
pub use ops::{EditError, Op, ParamTarget};
pub use validate::{repair, Report};

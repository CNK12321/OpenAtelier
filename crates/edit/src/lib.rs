//! Editing commands — the layer between user gestures and document [`Op`]s.
//!
//! * [`timeline`]: trim, split, ripple delete, move, slip, snapping.
//! * [`sections`]: timeline dividers and their sections; pasting into a track's free space.
//! * [`compound`]: breaking a compound clip apart onto the timeline.
//! * [`swap`]: putting other media in a clip, keeping everything done to it.
//! * [`stress`]: a stand-in project for measuring export speed.
//! * [`transform`]: clicking and dragging layers in the viewer — hit testing, handles,
//!   move/scale/rotate/anchor gestures, written as keyframes when a property is animated.
//!
//! Commands read a project snapshot and return ops; they never mutate anything, so the
//! caller decides how edits become undo steps (one per gesture, coalesced while
//! dragging).
//!
//! [`Op`]: oa_doc::Op

pub mod compound;
pub mod sections;
pub mod stress;
pub mod swap;
pub mod timeline;
pub mod transform;

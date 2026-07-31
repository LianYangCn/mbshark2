//! Capture layer: RTU framer (pure) plus async serial engine behind `gui`.

pub mod framer;

#[cfg(feature = "gui")]
pub mod engine;
#[cfg(feature = "gui")]
pub mod serial;

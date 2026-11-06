//! Non-destructive project state. No GUI, file I/O, decoded buffers or GPU types.
#![forbid(unsafe_code)]

mod project;
mod timeline;

pub use project::*;
pub use timeline::*;

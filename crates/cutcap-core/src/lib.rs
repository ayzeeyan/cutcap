//! Non-destructive project state. No GUI, file I/O, decoded buffers or GPU types.
#![forbid(unsafe_code)]

pub mod edit;
mod export;
mod project;
mod timeline;

pub use edit::{ClipChange, EditError, SourceRange};
pub use export::*;
pub use project::*;
pub use timeline::*;

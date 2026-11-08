//! Bounded external media adapter. No UI handles or project mutation.
#![forbid(unsafe_code)]
mod audio;
mod backend;
mod cancel;
mod clock;
mod frame;
mod latest;
mod persistent;
mod probe;
mod process;
mod stream;
mod thumbnail;

pub use audio::*;
pub use backend::*;
pub use cancel::Cancellation;
pub use clock::*;
pub use frame::*;
pub use latest::*;
pub use persistent::*;
pub use probe::parse_probe;
pub use thumbnail::*;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("Operation cancelled")]
    Cancelled,
    #[error("{0} exceeded its time limit")]
    Timeout(&'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("Unsupported media: {0}")]
    Unsupported(String),
    #[error("Media resource limit: {0}")]
    Budget(&'static str),
    #[error("{stage} failed ({code:?}): {detail}")]
    Process {
        stage: &'static str,
        code: Option<i32>,
        detail: String,
    },
    #[error("{stage}: {source}")]
    Io {
        stage: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("No frame at or after this source timestamp (try an earlier position)")]
    NoFrame,
    #[error("Media worker stopped unexpectedly")]
    Worker,
}

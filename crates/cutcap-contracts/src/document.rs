//! Persistent source identity and small document/session status values.
use crate::{MediaId, MediaSummary, RequestId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFingerprint {
    pub bytes: u64,
    /// SHA-256 of the entire source; timestamps alone cannot detect replacement.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedMedia {
    pub media: MediaSummary,
    pub fingerprint: SourceFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Checking,
    Ready,
    Missing,
    Changed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAvailability {
    pub id: MediaId,
    pub path: PathBuf,
    pub state: Availability,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentStatus {
    pub path: Option<PathBuf>,
    pub revision: u64,
    pub saved_revision: Option<u64>,
    pub dirty: bool,
    pub operation_id: Option<RequestId>,
    /// idle, saving, opening, saved, opened, failed. Terminal state is retained.
    pub state: String,
    pub error: Option<String>,
}

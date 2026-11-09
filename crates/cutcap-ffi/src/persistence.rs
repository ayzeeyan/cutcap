//! Bounded document I/O. Runs only on a file/probe worker, never in an ABI call.
use cutcap_contracts::*;
use cutcap_core::Project;
use cutcap_media::{Cancellation, MediaBackend, MediaError, ProbeResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    thread::{self, JoinHandle},
};

pub const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_DOCUMENT_MEDIA: usize = 128;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSource {
    pub id: MediaId,
    pub metadata: Option<SourceMetadata>,
    pub fingerprint: Option<SourceFingerprint>,
}

/// Format version and domain schema are independent (see PROJECT_FORMAT.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    format: String,
    format_version: u32,
    pub project: Project,
    pub sources: Vec<SavedSource>,
}

impl Document {
    pub fn new(project: Project, sources: Vec<SavedSource>) -> Result<Self, EngineError> {
        let doc = Self {
            format: "CutCap".into(),
            format_version: 1,
            project,
            sources,
        };
        doc.validate()?;
        Ok(doc)
    }
    fn validate(&self) -> Result<(), EngineError> {
        if self.format != "CutCap" || self.format_version != 1 {
            return Err(invalid("Unsupported CutCap document format/version."));
        }
        self.project
            .validate()
            .map_err(|e| invalid(e.to_string()))?;
        if self.project.media().len() > MAX_DOCUMENT_MEDIA
            || self.sources.len() != self.project.media().len()
        {
            return Err(invalid(
                "Document source manifest must match the project (at most 128 assets).",
            ));
        }
        let mut seen = HashSet::new();
        for source in &self.sources {
            if !seen.insert(source.id) || !self.project.media().iter().any(|m| m.id == source.id) {
                return Err(invalid("Invalid or duplicate source manifest identity."));
            }
            if let Some(metadata) = &source.metadata {
                metadata.validate().map_err(|e| invalid(e.to_string()))?;
                let video_asset = self
                    .project
                    .media()
                    .iter()
                    .any(|m| m.id == source.id && m.kind == TrackKind::Video);
                if video_asset && metadata.is_audio_only() {
                    return Err(invalid("A video asset cannot have audio-only metadata."));
                }
            }
            if let Some(hash) = &source.fingerprint
                && (hash.bytes == 0
                    || hash.bytes > MAX_FILE_BYTES
                    || hash.sha256.len() != 64
                    || !hash
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
            {
                return Err(invalid("Invalid source fingerprint."));
            }
        }
        for asset in self.project.media() {
            validate_reference(&asset.path)?;
        }
        Ok(())
    }
}

pub enum FileOutput {
    Saved(PathBuf),
    Opened(Box<Document>, PathBuf),
}
pub struct FileJob {
    pub request_id: RequestId,
    pub revision: u64,
    pub saving: bool,
    cancel: Cancellation,
    worker: Option<JoinHandle<Result<FileOutput, EngineError>>>,
}
impl FileJob {
    pub fn save(
        request_id: RequestId,
        revision: u64,
        doc: Document,
        path: PathBuf,
    ) -> Result<Self, EngineError> {
        Self::spawn(request_id, revision, true, move |cancel| {
            save(&doc, &path, cancel).map(FileOutput::Saved)
        })
    }
    pub fn open(request_id: RequestId, path: PathBuf) -> Result<Self, EngineError> {
        Self::spawn(request_id, 0, false, move |cancel| {
            let path = document_path(&path)?;
            let doc = open(&path, cancel)?;
            Ok(FileOutput::Opened(Box::new(doc), path))
        })
    }
    fn spawn(
        request_id: RequestId,
        revision: u64,
        saving: bool,
        task: impl FnOnce(&Cancellation) -> Result<FileOutput, EngineError> + Send + 'static,
    ) -> Result<Self, EngineError> {
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let worker = thread::Builder::new()
            .name("cutcap-project-file".into())
            .spawn(move || task(&token))
            .map_err(|e| io_error("Start project worker", e))?;
        Ok(Self {
            request_id,
            revision,
            saving,
            cancel,
            worker: Some(worker),
        })
    }
    pub fn poll(&mut self) -> Option<Result<FileOutput, EngineError>> {
        if !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return None;
        }
        self.worker.take().map(|worker| {
            worker
                .join()
                .unwrap_or_else(|_| Err(invalid("Project file worker failed.")))
        })
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
impl Drop for FileJob {
    fn drop(&mut self) {
        self.cancel();
        // Never make session destruction wait for an uninterruptible OS file
        // call. A detached task owns only its snapshot/temp file and token; it
        // cannot adopt state. Checks before commit prevent a later cancelled save
        // from starting replacement. An already-entered atomic rename may finish.
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            let _result = worker.join();
        }
    }
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::new(ErrorCode::InvalidInput, message)
}
fn io_error(stage: &str, e: std::io::Error) -> EngineError {
    EngineError::new(ErrorCode::Internal, format!("{stage}: {e}"))
}
fn check(cancel: &Cancellation) -> Result<(), EngineError> {
    if cancel.is_cancelled() {
        Err(invalid("Project operation cancelled."))
    } else {
        Ok(())
    }
}

fn validate_reference(path: &Path) -> Result<(), EngineError> {
    let Some(text) = path.to_str() else {
        return Err(invalid("Project paths must be valid Unicode."));
    };
    if text.is_empty() || text.len() > 32_768 || text.chars().any(char::is_control) {
        return Err(invalid("Invalid project media path."));
    }
    // A relative reference is explicitly project-directory-relative. Disallow
    // drive-relative/root-relative syntax and '..', rather than reinterpreting it.
    if !path.is_absolute()
        && path
            .components()
            .any(|p| !matches!(p, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid(
            "Relative media references cannot escape the project directory.",
        ));
    }
    Ok(())
}
fn document_path(path: &Path) -> Result<PathBuf, EngineError> {
    validate_reference(path)?;
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cutcap"))
    {
        return Err(invalid("Choose an absolute path ending in .cutcap."));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Project directory is missing."))?;
    let parent = fs::canonicalize(parent).map_err(|e| io_error("Open project directory", e))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("Project filename is missing."))?;
    Ok(parent.join(name))
}

pub fn open(path: &Path, cancel: &Cancellation) -> Result<Document, EngineError> {
    check(cancel)?;
    let mut file = File::open(path).map_err(|e| io_error("Open project", e))?;
    let info = file
        .metadata()
        .map_err(|e| io_error("Read project size", e))?;
    if !info.is_file() || info.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid("Project exceeds the 16 MiB file budget."));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("Read project", e))?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("Project grew beyond the file budget."));
    }
    check(cancel)?;
    preflight(&bytes)?;
    let mut doc: Document = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(format!("Invalid CutCap project: {e}")))?;
    doc.validate()?;
    let directory = path
        .parent()
        .ok_or_else(|| invalid("Project directory is missing."))?;
    for asset in doc.project.media().to_vec() {
        let resolved = if asset.path.is_absolute() {
            asset.path
        } else {
            directory.join(asset.path)
        };
        doc.project
            .relink_media(asset.id, resolved)
            .map_err(|e| invalid(e.to_string()))?;
    }
    check(cancel)?;
    Ok(doc)
}

pub fn save(doc: &Document, path: &Path, cancel: &Cancellation) -> Result<PathBuf, EngineError> {
    save_with(doc, path, cancel, || Ok(()))
}
// Fault-injection boundary is immediately before replacement, after sync_all.
fn save_with(
    doc: &Document,
    path: &Path,
    cancel: &Cancellation,
    before_replace: impl FnOnce() -> Result<(), EngineError>,
) -> Result<PathBuf, EngineError> {
    check(cancel)?;
    doc.validate()?;
    let path = document_path(path)?;
    let directory = path
        .parent()
        .ok_or_else(|| invalid("Project directory is missing."))?;
    let mut portable = doc.clone();
    for asset in doc.project.media() {
        if !asset.path.is_absolute() {
            return Err(invalid("Runtime media references must be absolute."));
        }
        let target = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if asset.path == path || fs::canonicalize(&asset.path).is_ok_and(|p| p == target) {
            return Err(invalid(
                "A project must not overwrite referenced source media.",
            ));
        }
        // Relative only for descendants. Other paths remain explicitly absolute.
        let source = fs::canonicalize(&asset.path).unwrap_or_else(|_| asset.path.clone());
        let reference = source
            .strip_prefix(directory)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| source.clone(), Path::to_path_buf);
        portable
            .project
            .relink_media(asset.id, reference)
            .map_err(|e| invalid(e.to_string()))?;
    }
    let bytes = serde_json::to_vec_pretty(&portable)
        .map_err(|e| invalid(format!("Serialize project: {e}")))?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("Project exceeds the 16 MiB file budget."));
    }
    // No delete/truncate of the destination, and no cross-volume copy fallback.
    let mut temp = tempfile::Builder::new()
        .prefix(".cutcap-")
        .suffix(".tmp")
        .tempfile_in(directory)
        .map_err(|e| io_error("Create project temporary file", e))?;
    for chunk in bytes.chunks(64 * 1024) {
        check(cancel)?;
        temp.write_all(chunk)
            .map_err(|e| io_error("Write project temporary file", e))?;
    }
    temp.as_file()
        .sync_all()
        .map_err(|e| io_error("Flush project temporary file", e))?;
    before_replace()?;
    check(cancel)?;
    temp.persist(&path)
        .map_err(|e| io_error("Replace project (previous file retained)", e.error))?;
    Ok(path)
}

/// Open a shared-read-only handle while probing and hashing. On Windows this
/// denies concurrent writes/rename/delete for the inspected file's lifetime.
pub fn probe_verified(
    backend: &dyn MediaBackend,
    path: &Path,
    cancel: &Cancellation,
) -> Result<(ProbeResult, SourceFingerprint), MediaError> {
    let path = fs::canonicalize(path).map_err(|source| MediaError::Io {
        stage: "Locate source media",
        source,
    })?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ, safe standard-library API
    }
    let mut file = options.open(&path).map_err(|source| MediaError::Io {
        stage: "Inspect source media",
        source,
    })?;
    let before = file.metadata().map_err(|source| MediaError::Io {
        stage: "Read source size",
        source,
    })?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_FILE_BYTES {
        return Err(MediaError::Budget("source file size"));
    }
    let result = backend.probe(&path, cancel)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let count = file.read(&mut buffer).map_err(|source| MediaError::Io {
            stage: "Fingerprint source",
            source,
        })?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(MediaError::Budget("source fingerprint"))?;
        if total > MAX_FILE_BYTES {
            return Err(MediaError::Budget("source fingerprint"));
        }
        hash.update(&buffer[..count]);
    }
    let after = file.metadata().map_err(|source| MediaError::Io {
        stage: "Check source size",
        source,
    })?;
    if total != before.len()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(MediaError::Invalid(
            "Source changed during inspection. Try again.".into(),
        ));
    }
    let sha256 = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((
        result,
        SourceFingerprint {
            bytes: total,
            sha256,
        },
    ))
}

/// A byte limit precedes all parsing. This allocation-bounded structural pass
/// rejects deep/wide input before typed model construction and graph validation.
fn preflight(bytes: &[u8]) -> Result<(), EngineError> {
    use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
    struct Bounded {
        depth: usize,
    }
    impl<'de> DeserializeSeed<'de> for Bounded {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            if self.depth > 32 {
                return Err(D::Error::custom("project nesting limit"));
            }
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Bounded {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded project JSON")
        }
        fn visit_bool<E: Error>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E: Error>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E: Error>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E: Error>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E: Error>(self) -> Result<(), E> {
            Ok(())
        }
        fn visit_str<E: Error>(self, value: &str) -> Result<(), E> {
            if value.len() > 32_768 {
                Err(E::custom("project string limit"))
            } else {
                Ok(())
            }
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let mut count = 0;
            while seq
                .next_element_seed(Bounded {
                    depth: self.depth + 1,
                })?
                .is_some()
            {
                count += 1;
                if count > cutcap_core::MAX_CLIPS {
                    return Err(A::Error::custom("project array limit"));
                }
            }
            Ok(())
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut count = 0;
            while let Some(key) = map.next_key::<String>()? {
                count += 1;
                if count > 32 || key.len() > 128 {
                    return Err(A::Error::custom("project object limit"));
                }
                map.next_value_seed(Bounded {
                    depth: self.depth + 1,
                })?;
            }
            Ok(())
        }
    }
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    Bounded { depth: 0 }
        .deserialize(&mut parser)
        .map_err(|e| invalid(format!("Invalid project structure: {e}")))?;
    parser.end().map_err(|e| invalid(e.to_string()))
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod tests;

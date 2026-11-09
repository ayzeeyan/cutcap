//! Where an export writes, and how it reaches its final name safely
//! (PHASE_5A_EXPORT_CONTRACT.md §9).
use super::{
    Fail,
    plan::{ExportPlan, OutputRequest},
};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};
use tempfile::{Builder, NamedTempFile, TempPath};

/// Video size assumed by the disk reservation, bits per second.
const ESTIMATED_VIDEO_BPS: u64 = 8_000_000;
const RESERVE_MARGIN: u64 = 64 * 1024 * 1024;

const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A checked destination: a local `.mp4` in an existing folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub folder: PathBuf,
    pub stem: String,
    pub allow_overwrite: bool,
}

/// Checks a destination before anything is started.
pub fn check_target(request: &OutputRequest, plan: &ExportPlan) -> Result<Target, Fail> {
    let refuse = |message: &str| Err(Fail::Message(message.to_owned()));
    let path = &request.path;
    let raw = path.as_os_str().to_string_lossy();
    if !path.is_absolute() {
        return refuse("Choose a complete file path for the export.");
    }
    if raw.starts_with("\\\\") {
        return refuse("Export to a local drive, not a network or device path.");
    }
    if !path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    {
        return refuse("The export file name must end in .mp4.");
    }
    let (Some(stem), Some(parent)) = (path.file_stem().and_then(OsStr::to_str), path.parent())
    else {
        return refuse("Choose a valid file name for the export.");
    };
    if RESERVED_NAMES
        .iter()
        .any(|name| name.eq_ignore_ascii_case(stem.split('.').next().unwrap_or(stem)))
    {
        return refuse("That file name is reserved by Windows.");
    }
    // Canonical only to check it: the person's own path is what gets used
    // and shown, not Windows' verbatim `\\?\` form of it.
    let canonical = match fs::canonicalize(parent) {
        Ok(folder) if folder.is_dir() => folder,
        _ => return refuse("The destination folder does not exist."),
    };
    if canonical
        .as_os_str()
        .to_string_lossy()
        .starts_with(r"\\?\UNC\")
    {
        return refuse("Export to a local drive, not a network path.");
    }
    let folder = parent.to_path_buf();
    let final_path = path.clone();
    // Any file already there, by any casing Windows treats as the same name.
    match fs::metadata(&final_path) {
        Ok(existing) if existing.is_dir() => {
            return refuse("That name is already a folder.");
        }
        Ok(_) => {
            if !request.allow_overwrite {
                return refuse("That file already exists. Choose it again to replace it.");
            }
            let same_as_source = fs::canonicalize(&final_path).ok().is_some_and(|existing| {
                plan.sources
                    .values()
                    .any(|source| fs::canonicalize(&source.media.path).is_ok_and(|s| s == existing))
            });
            if same_as_source {
                return refuse("The export cannot replace one of its own source files.");
            }
        }
        Err(_) => {}
    }
    Ok(Target {
        path: final_path,
        folder,
        stem: stem.to_owned(),
        allow_overwrite: request.allow_overwrite,
    })
}

/// The job's temporary files, beside the destination. Dropping this removes
/// whatever still exists, on every exit path.
pub struct Scratch {
    pcm: Option<NamedTempFile>,
    pcm_path: Option<TempPath>,
    output: Option<TempPath>,
}

fn temporary(target: &Target, suffix: &str) -> Result<NamedTempFile, Fail> {
    // Keep generated names short: the user's stem may be near the path limit.
    let short: String = target.stem.chars().take(40).collect();
    Builder::new()
        .prefix(&format!(".{short}.cutcap-"))
        .suffix(suffix)
        .tempfile_in(&target.folder)
        .map_err(|e| Fail::io("Creating a temporary file in the destination folder", &e))
}

impl Scratch {
    /// Reserves disk space for the mix and an estimate of the video. Setting
    /// a file's length allocates its clusters, so a full disk fails here,
    /// and this also proves the folder is writable.
    pub fn reserve(target: &Target, plan: &ExportPlan) -> Result<Self, Fail> {
        let pcm_bytes = plan.grid.samples * plan.preset.sample_frame_bytes();
        let seconds = plan.grid.frames / u64::from(plan.preset.fps) + 1;
        let audio_bps = u64::from(plan.preset.audio_bitrate_kbps) * 1000;
        let estimate = seconds * (ESTIMATED_VIDEO_BPS + audio_bps) / 8 + RESERVE_MARGIN;
        let pcm = temporary(target, ".pcm")?;
        pcm.as_file()
            .set_len(pcm_bytes)
            .map_err(|e| Fail::io("Reserving space for the audio mix", &e))?;
        let output = temporary(target, ".mp4")?;
        output
            .as_file()
            .set_len(estimate)
            .map_err(|e| Fail::io("Reserving space for the video", &e))?;
        output
            .as_file()
            .set_len(0)
            .map_err(|e| Fail::io("Preparing the output file", &e))?;
        Ok(Self {
            pcm: Some(pcm),
            pcm_path: None,
            output: Some(output.into_temp_path()),
        })
    }

    pub fn pcm_file(&mut self) -> Result<&mut File, Fail> {
        self.pcm
            .as_mut()
            .map(NamedTempFile::as_file_mut)
            .ok_or_else(|| Fail::Message("The audio mix was already closed.".into()))
    }

    /// Closes the mix so the encoder can read it. Returns its path.
    pub fn close_pcm(&mut self) -> Result<PathBuf, Fail> {
        let pcm = self
            .pcm
            .take()
            .ok_or_else(|| Fail::Message("The audio mix was already closed.".into()))?;
        let path = pcm.into_temp_path();
        let owned = path.to_path_buf();
        self.pcm_path = Some(path);
        Ok(owned)
    }

    pub fn output_path(&self) -> Result<&Path, Fail> {
        self.output
            .as_deref()
            .ok_or_else(|| Fail::Message("The output was already published.".into()))
    }

    /// Flushes the finished file and gives it its final name. On failure the
    /// temporary file is removed and an existing destination is left alone.
    pub fn publish(&mut self, target: &Target) -> Result<u64, Fail> {
        let output = self
            .output
            .take()
            .ok_or_else(|| Fail::Message("The output was already published.".into()))?;
        let bytes = {
            let file = OpenOptions::new()
                .write(true)
                .open(&*output)
                .map_err(|e| Fail::io("Finishing the output file", &e))?;
            file.sync_all()
                .map_err(|e| Fail::io("Flushing the output file", &e))?;
            file.metadata()
                .map_err(|e| Fail::io("Finishing the output file", &e))?
                .len()
        };
        let moved = if target.allow_overwrite {
            output.persist(&target.path)
        } else {
            output.persist_noclobber(&target.path)
        };
        moved.map_err(|e| Fail::io("Moving the finished file into place", &e.error))?;
        Ok(bytes)
    }
}

//! Phase 5A export: one supervised job at a time, per session.
//!
//! The job owns an immutable plan and runs on its own thread; the session only
//! accepts commands and reads a coalesced status. Nothing here blocks the UI
//! thread, and a job cannot touch the live project
//! (PHASE_5A_EXPORT_CONTRACT.md).
pub(crate) mod audio;
pub(crate) mod output;
pub(crate) mod plan;
pub(crate) mod video;

pub use plan::*;

use crate::audio_mix::{layout, overlap_error};
use crate::persistence::probe_verified;
use crate::view;
use cutcap_contracts::*;
use cutcap_core::{ClipStream, map_clip_source, output_grid};
use cutcap_media::{Cancellation, EncodeSpec, MediaBackend, MediaError};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Share of the progress bar each stage of output work earns.
const AUDIO_SHARE: f64 = 0.10;
const VIDEO_SHARE: f64 = 0.88;
const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);
/// Session shutdown waits this long for a cancelled job to clean up.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

/// Why a job stopped early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    Cancelled,
    Message(String),
}

/// A plain reason for a failed file operation, by what actually went wrong.
fn plain_io(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(112 | 39) => "the disk is full".to_owned(),
        Some(32 | 33) => "the file is in use by another program".to_owned(),
        Some(3) => "the folder no longer exists".to_owned(),
        _ => match error.kind() {
            std::io::ErrorKind::PermissionDenied => "permission was denied".to_owned(),
            std::io::ErrorKind::NotFound => "the folder or file no longer exists".to_owned(),
            _ => error.to_string(),
        },
    }
}

impl Fail {
    pub fn io(what: &str, error: &std::io::Error) -> Self {
        Self::Message(format!("{what} failed: {}.", plain_io(error)))
    }

    pub fn media(what: &str, error: &MediaError) -> Self {
        match error {
            MediaError::Cancelled => Self::Cancelled,
            MediaError::Process { detail, .. } if detail.contains("No space left") => {
                Self::Message(format!("{what} failed: the disk is full."))
            }
            MediaError::Process { detail, .. } if detail.contains("Permission denied") => {
                Self::Message(format!("{what} failed: permission was denied."))
            }
            MediaError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                Self::Message(format!("{what} failed: FFmpeg could not be started."))
            }
            MediaError::Io { source, .. } => {
                Self::Message(format!("{what} failed: {}.", plain_io(source)))
            }
            other => {
                let text: String = other.to_string().chars().take(600).collect();
                Self::Message(format!("{what} failed: {text}"))
            }
        }
    }
}

/// One export's finished figures.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ExportResult {
    pub path: String,
    pub bytes: u64,
    pub frames: u64,
    pub duration_ms: u64,
    pub clipped_samples: u64,
    pub peak: f32,
    pub source_silence_ms: u64,
    pub gap_frames: u64,
    pub no_picture_frames: u64,
    pub head_frames: u64,
    pub decoded_frames: u64,
    pub reused_frames: u64,
    pub video_decoder_starts: u64,
    pub clip_changes: u64,
    pub cut_wait_max_us: u64,
    pub cut_wait_total_us: u64,
    pub audio_decoder_starts: u64,
    pub audio_decoders_peak: usize,
    pub mix_us: u64,
    pub encode_us: u64,
    pub total_us: u64,
}

/// A job's state as the frontend sees it. A small coalesced value, never pixels.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ExportJobView {
    pub id: String,
    /// validating, preparing, decoding, encoding, finalizing, cancelling,
    /// completed, cancelled, failed.
    pub state: &'static str,
    pub running: bool,
    pub progress: f64,
    pub detail: String,
    pub output_path: String,
    pub file_name: String,
    pub project_name: String,
    pub revision: u64,
    pub track_name: Option<String>,
    pub frames_total: u64,
    pub frames_submitted: u64,
    pub frames_encoded: u64,
    pub samples_total: u64,
    pub samples_mixed: u64,
    pub elapsed_ms: u64,
    pub elapsed_text: String,
    /// Encoded output seconds per second of encoding, once measured.
    pub speed: Option<f64>,
    pub error: Option<String>,
    pub result: Option<ExportResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ExportView {
    /// Whether this FFmpeg can encode the preset; null until it has been asked.
    pub available: Option<bool>,
    pub unavailable_reason: Option<String>,
    pub summary: ExportSummary,
    pub job: Option<ExportJobView>,
}

struct Shared {
    status: Mutex<Status>,
    changed: AtomicBool,
}

#[derive(Default)]
struct Status {
    available: Option<Result<(), String>>,
    job: Option<ExportJobView>,
    cancel: Option<Cancellation>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A job's reports, coalesced. Only a terminal state or a new state is
/// published at once; counts are published at most every 100 ms.
pub struct Progress<'a> {
    shared: &'a Shared,
    plan_frames: u64,
    plan_samples: u64,
    started: Instant,
    encoding_since: Option<Instant>,
    published: Instant,
    mixed: u64,
    submitted: u64,
    encoded: u64,
}

impl<'a> Progress<'a> {
    fn new(shared: &'a Shared) -> Self {
        let now = Instant::now();
        Self {
            shared,
            plan_frames: 0,
            plan_samples: 0,
            started: now,
            encoding_since: None,
            published: now,
            mixed: 0,
            submitted: 0,
            encoded: 0,
        }
    }

    fn fraction(&self) -> f64 {
        let share = |done: u64, total: u64, weight: f64| {
            if total == 0 {
                0.0
            } else {
                weight * (done as f64 / total as f64).min(1.0)
            }
        };
        share(self.mixed, self.plan_samples, AUDIO_SHARE)
            + share(self.encoded, self.plan_frames, VIDEO_SHARE)
    }

    fn publish(&mut self) {
        let fraction = self.fraction();
        let elapsed = self.started.elapsed();
        let speed = self.encoding_since.and_then(|since| {
            let seconds = since.elapsed().as_secs_f64();
            (seconds > 0.5 && self.encoded > 0 && self.plan_frames > 0)
                .then(|| self.encoded as f64 / f64::from(ExportPreset::STANDARD.fps) / seconds)
        });
        let mut status = self.shared.lock();
        if let Some(job) = status.job.as_mut() {
            job.progress = fraction;
            job.samples_mixed = self.mixed;
            job.frames_submitted = self.submitted;
            job.frames_encoded = self.encoded;
            job.elapsed_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
            job.elapsed_text = view::precise_duration(job.elapsed_ms as i64);
            job.speed = speed.or(job.speed);
        }
        drop(status);
        self.published = Instant::now();
        self.shared.changed.store(true, Ordering::Release);
    }

    fn maybe_publish(&mut self) {
        if self.published.elapsed() >= PUBLISH_INTERVAL {
            self.publish();
        }
    }

    /// Moves to a new state, unless the user has already asked to cancel.
    fn state(&mut self, state: &'static str, detail: &str) {
        {
            let mut status = self.shared.lock();
            if let Some(job) = status.job.as_mut()
                && job.state != "cancelling"
            {
                job.state = state;
                job.detail = detail.to_owned();
            }
        }
        self.publish();
    }

    /// `k` sample frames of the mix are written.
    pub fn mixed(&mut self, k: u64) {
        self.mixed = k;
        self.maybe_publish();
    }

    /// `frames` have gone to the encoder, which has finished `encoded`.
    pub fn submitted(&mut self, frames: u64, encoded: u64) {
        if self.submitted == 0 {
            self.encoding_since = Some(Instant::now());
            self.state("encoding", "Rendering and encoding the timeline.");
        }
        self.submitted = frames;
        self.encoded = encoded;
        self.maybe_publish();
    }
}

/// The session's single export owner.
pub struct ExportHost {
    backend: Arc<dyn MediaBackend>,
    shared: Arc<Shared>,
    probe_cancel: Cancellation,
    probe: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
}

impl ExportHost {
    /// Also asks FFmpeg, once and off-thread, what it can encode.
    pub fn start(backend: Arc<dyn MediaBackend>) -> Result<Self, std::io::Error> {
        let shared = Arc::new(Shared {
            status: Mutex::new(Status::default()),
            changed: AtomicBool::new(false),
        });
        let probe_cancel = Cancellation::default();
        let probe = {
            let (backend, shared, cancel) = (backend.clone(), shared.clone(), probe_cancel.clone());
            thread::Builder::new()
                .name("cutcap-export-probe".into())
                .spawn(move || {
                    let found = backend.check_export_encoders(&cancel);
                    if matches!(found, Err(MediaError::Cancelled)) {
                        return;
                    }
                    shared.lock().available = Some(found.map_err(|error| {
                        match error {
                            MediaError::Io { .. } => {
                                "FFmpeg could not be started. Install FFmpeg and add it to PATH."
                                    .to_owned()
                            }
                            MediaError::Unsupported(detail) => format!("Export needs: {detail}."),
                            other => other.to_string(),
                        }
                    }));
                    shared.changed.store(true, Ordering::Release);
                })?
        };
        Ok(Self {
            backend,
            shared,
            probe_cancel,
            probe: Some(probe),
            worker: None,
        })
    }

    pub fn running(&self) -> bool {
        self.shared
            .lock()
            .job
            .as_ref()
            .is_some_and(|job| job.running)
    }

    /// A start was accepted: the job exists from now on, in `validating`.
    pub fn announce(&mut self, output: &Path) -> String {
        let id = RequestId::new().to_string();
        let mut status = self.shared.lock();
        status.job = Some(ExportJobView {
            id: id.clone(),
            state: "validating",
            running: true,
            detail: "Capturing the project.".into(),
            output_path: output.display().to_string(),
            file_name: output
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            elapsed_text: "00:00.000".into(),
            ..Default::default()
        });
        status.cancel = Some(Cancellation::default());
        drop(status);
        self.shared.changed.store(true, Ordering::Release);
        id
    }

    /// The command owner refused to build a plan.
    pub fn refuse(&mut self, job: &str, message: String) {
        let mut status = self.shared.lock();
        if let Some(view) = status.job.as_mut()
            && view.id == job
        {
            view.state = "failed";
            view.running = false;
            view.detail = String::new();
            view.error = Some(message);
        }
        drop(status);
        self.shared.changed.store(true, Ordering::Release);
    }

    /// Hands a captured plan to a new worker thread.
    pub fn begin(
        &mut self,
        job: &str,
        plan: ExportPlan,
        request: OutputRequest,
    ) -> Result<(), std::io::Error> {
        let cancel = {
            let mut status = self.shared.lock();
            let Some(view) = status.job.as_mut().filter(|view| view.id == job) else {
                return Ok(());
            };
            view.project_name.clone_from(&plan.project_name);
            view.revision = plan.revision;
            view.track_name.clone_from(&plan.video_track_name);
            view.frames_total = plan.grid.frames;
            view.samples_total = plan.grid.samples;
            view.detail = "Checking sources.".into();
            status.cancel.clone().unwrap_or_default()
        };
        // A finished worker has nothing left to wait for.
        if let Some(previous) = self.worker.take() {
            let _ = previous.join();
        }
        let (backend, shared) = (self.backend.clone(), self.shared.clone());
        self.worker = Some(
            thread::Builder::new()
                .name("cutcap-export".into())
                .spawn(move || run_job(&backend, &shared, plan, request, cancel))?,
        );
        Ok(())
    }

    /// Asks job `id` to stop. A stale id is refused, so a late click cannot
    /// cancel the next export.
    pub fn cancel(&self, id: &str) -> Result<(), &'static str> {
        let mut status = self.shared.lock();
        let cancel = status.cancel.clone();
        let Some(job) = status.job.as_mut() else {
            return Err("No export is running.");
        };
        if job.id != id {
            return Err("That export is no longer current.");
        }
        if !job.running {
            return Err("That export has already finished.");
        }
        job.state = "cancelling";
        job.detail = "Stopping the export.".into();
        drop(status);
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.shared.changed.store(true, Ordering::Release);
        Ok(())
    }

    /// True once per change, so an idle view is not rebuilt.
    pub fn take_changed(&self) -> bool {
        self.shared.changed.swap(false, Ordering::AcqRel)
    }

    pub fn view(&self, summary: &ExportSummary) -> ExportView {
        let status = self.shared.lock();
        ExportView {
            available: status.available.as_ref().map(Result::is_ok),
            unavailable_reason: status
                .available
                .as_ref()
                .and_then(|found| found.as_ref().err().cloned()),
            summary: summary.clone(),
            job: status.job.clone(),
        }
    }

    /// Stops any job and waits for it, within a bound. A job that cannot stop
    /// in time is left to finish cleaning up; its children die with the host.
    pub fn shutdown(&mut self) {
        self.probe_cancel.cancel();
        if let Some(cancel) = self.shared.lock().cancel.clone() {
            cancel.cancel();
        }
        if let Some(probe) = self.probe.take() {
            let _ = probe.join();
        }
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                tracing::error!("an export did not stop within the shutdown limit");
            }
        }
    }
}

impl Drop for ExportHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_job(
    backend: &Arc<dyn MediaBackend>,
    shared: &Arc<Shared>,
    plan: ExportPlan,
    request: OutputRequest,
    cancel: Cancellation,
) {
    let mut progress = Progress::new(shared);
    progress.plan_frames = plan.grid.frames;
    progress.plan_samples = plan.grid.samples;
    let outcome = execute(backend, &plan, &request, &mut progress, &cancel);
    progress.publish();
    let mut status = shared.lock();
    if let Some(job) = status.job.as_mut() {
        job.running = false;
        job.detail = String::new();
        match outcome {
            Ok(result) => {
                job.state = "completed";
                job.progress = 1.0;
                job.frames_submitted = job.frames_total;
                job.frames_encoded = job.frames_total;
                job.result = Some(result);
            }
            Err(Fail::Cancelled) => job.state = "cancelled",
            Err(Fail::Message(message)) => {
                job.state = "failed";
                job.error = Some(message);
            }
        }
    }
    drop(status);
    shared.changed.store(true, Ordering::Release);
}

/// Everything the plan needs from the outside world, checked before any
/// decoder or encoder is started.
fn preflight(
    backend: &Arc<dyn MediaBackend>,
    plan: &ExportPlan,
    cancel: &Cancellation,
) -> Result<(), Fail> {
    backend
        .check_export_encoders(cancel)
        .map_err(|e| Fail::media("Checking the encoders", &e))?;
    for source in plan.sources.values() {
        if cancel.is_cancelled() {
            return Err(Fail::Cancelled);
        }
        let name = view::file_name(&source.media);
        let (probed, fingerprint) = probe_verified(backend.as_ref(), &source.media.path, cancel)
            .map_err(|e| match e {
                MediaError::Cancelled => Fail::Cancelled,
                _ => Fail::Message(format!(
                    "\u{201c}{name}\u{201d} could not be read. Relink it before exporting."
                )),
            })?;
        if source.fingerprint.as_ref() != Some(&fingerprint)
            || !source.media.metadata.matches_probe(&probed.metadata)
        {
            return Err(Fail::Message(format!(
                "\u{201c}{name}\u{201d} has changed since it was added. Relink it before exporting."
            )));
        }
    }
    if let Some(track) = plan.video_track {
        for clip in plan.index.clips(track) {
            let source = plan
                .sources
                .get(&clip.media_id)
                .ok_or_else(|| Fail::Message("A video clip's source is not in the plan.".into()))?;
            let video = source.media.metadata.video().ok_or_else(|| {
                Fail::Message("A clip on the video track has no picture stream.".into())
            })?;
            if clip.stream != ClipStream::Video {
                return Err(Fail::Message(
                    "The chosen track is not a video track.".into(),
                ));
            }
            video
                .fit_within(plan.preset.width, plan.preset.height)
                .map_err(|e| Fail::Message(e.to_string()))?;
            map_clip_source(clip, clip.start, plan.timebase, video.timebase).map_err(|e| {
                Fail::Message(format!("A clip cannot be mapped to its source: {e}"))
            })?;
        }
    }
    if let Some(reason) = plan.audio.clips.iter().find_map(|c| c.unavailable.as_ref()) {
        return Err(Fail::Message(
            reason.replace("; silence plays.", ". Fix it before exporting."),
        ));
    }
    let (spans, _, _) = layout(&plan.audio, TimelineTime::ZERO, plan.preset.sample_rate);
    overlap_error(
        &spans,
        |_| true,
        &plan.audio,
        TimelineTime::ZERO,
        plan.preset.sample_rate,
    )
    .map_err(Fail::Message)
}

fn execute(
    backend: &Arc<dyn MediaBackend>,
    plan: &ExportPlan,
    request: &OutputRequest,
    progress: &mut Progress<'_>,
    cancel: &Cancellation,
) -> Result<ExportResult, Fail> {
    let started = Instant::now();
    progress.state("validating", "Checking sources and the destination.");
    let target = output::check_target(request, plan)?;
    preflight(backend, plan, cancel)?;
    if cancel.is_cancelled() {
        return Err(Fail::Cancelled);
    }
    let mut scratch = output::Scratch::reserve(&target, plan)?;
    progress.state("preparing", "Mixing the audio tracks.");
    let mix = audio::mix_to_file(plan, backend, scratch.pcm_file()?, progress, cancel)?;
    let pcm = scratch.close_pcm()?;
    progress.state("decoding", "Starting the encoder and the first decoder.");
    let spec = EncodeSpec {
        output: scratch.output_path()?.to_path_buf(),
        audio_pcm: pcm,
        preset: plan.preset,
    };
    let encode_started = Instant::now();
    let mut encoder = backend
        .open_encoder(&spec, cancel)
        .map_err(|e| Fail::media("Starting the encoder", &e))?;
    let picture = video::render(plan, backend, encoder.as_mut(), progress, cancel)?;
    progress.state("finalizing", "Finishing the file.");
    encoder.finish().map_err(|e| Fail::media("Encoding", &e))?;
    let encode_us = encode_started
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    if cancel.is_cancelled() {
        return Err(Fail::Cancelled);
    }
    let report = backend
        .inspect_output(scratch.output_path()?, cancel)
        .map_err(|e| Fail::media("Checking the finished file", &e))?;
    if let Some(problem) = report.mismatch(plan.grid.frames, plan.preset) {
        return Err(Fail::Message(format!(
            "The finished file failed validation: {problem}."
        )));
    }
    let bytes = scratch.publish(&target)?;
    let fps = u64::from(plan.preset.fps);
    let rate = u64::from(plan.preset.sample_rate);
    Ok(ExportResult {
        path: target.path.display().to_string(),
        bytes,
        frames: plan.grid.frames,
        duration_ms: plan.grid.frames * 1000 / fps,
        clipped_samples: mix.clipped_samples,
        peak: mix.peak,
        source_silence_ms: mix.source_silence_frames * 1000 / rate,
        gap_frames: picture.gap_frames,
        no_picture_frames: picture.no_picture_frames,
        head_frames: picture.head_frames,
        decoded_frames: picture.decoded,
        reused_frames: picture.reused,
        video_decoder_starts: picture.decoder_starts,
        clip_changes: picture.clip_changes,
        cut_wait_max_us: picture.cut_wait_max_us,
        cut_wait_total_us: picture.cut_wait_total_us,
        audio_decoder_starts: mix.decoder_starts,
        audio_decoders_peak: mix.decoders_peak,
        mix_us: mix.mix_us,
        encode_us,
        total_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
    })
}

/// What Export would do with `snapshot`. Built once per snapshot, never per
/// playback frame.
pub fn summarize(snapshot: &ProjectSnapshot) -> ExportSummary {
    let preset = ExportPreset::STANDARD;
    let any_solo = snapshot
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Audio && t.controls.solo);
    let audible: BTreeSet<TrackId> = snapshot
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && t.controls.audible(any_solo))
        .map(|t| t.id)
        .collect();
    let end = |clip: &ClipSummary| i128::from(clip.start.0) + i128::from(clip.duration.ticks());
    let missing = |media: MediaId| {
        snapshot
            .media_state
            .iter()
            .find(|m| m.id == media && m.state != Availability::Ready)
            .map(|m| {
                let name = m
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                format!("\u{201c}{name}\u{201d} is missing or changed. Relink it to export.")
            })
    };
    let mut audio_end = 0i128;
    let mut audio_blocked = None;
    let mut with_clips = BTreeSet::new();
    let mut heard = BTreeSet::new();
    for clip in &snapshot.clips {
        if matches!(clip.stream, ClipStream::Audio { .. }) {
            with_clips.insert(clip.track_id);
            if audible.contains(&clip.track_id) {
                heard.insert(clip.track_id);
                audio_end = audio_end.max(end(clip));
                audio_blocked = audio_blocked.or_else(|| missing(clip.media_id));
            }
        }
    }
    let video_tracks: Vec<ExportTrackChoice> = snapshot
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .map(|track| {
            let clips: Vec<&ClipSummary> = snapshot
                .clips
                .iter()
                .filter(|c| c.track_id == track.id)
                .collect();
            let extent = clips
                .iter()
                .map(|c| end(c))
                .max()
                .unwrap_or(0)
                .max(audio_end);
            let extent = TimelineTime(i64::try_from(extent).unwrap_or(i64::MAX));
            let grid = output_grid(extent, snapshot.timebase, preset);
            let blocked = match &grid {
                Err(error) => Some(error.to_string()),
                Ok(_) => audio_blocked
                    .clone()
                    .or_else(|| clips.iter().find_map(|c| missing(c.media_id))),
            };
            let frames = grid.map_or(0, |g| g.frames);
            let duration_ms = i64::try_from(
                i128::from(extent.0) * i128::from(snapshot.timebase.numerator()) * 1000
                    / i128::from(snapshot.timebase.denominator()),
            )
            .unwrap_or(i64::MAX);
            ExportTrackChoice {
                id: track.id.to_string(),
                name: track.name.clone(),
                clips: clips.len(),
                duration_ms,
                duration_text: view::precise_duration(duration_ms),
                frames,
                blocked,
            }
        })
        .collect();
    let default_track = video_tracks
        .iter()
        .find(|t| t.clips > 0)
        .or(video_tracks.first())
        .map(|t| t.id.clone());
    ExportSummary {
        preset_name: preset.name.to_owned(),
        preset_text: preset.summary(),
        video_tracks,
        default_track,
        audio_tracks: heard.len(),
        audio_tracks_with_clips: with_clips.len(),
        revision: snapshot.document.revision,
    }
}

//! UI-owned coordination of native dialog, probe job and latest-frame worker.
use crate::{
    AppWindow,
    preview::{self, PresentationMeasurement, Presenter},
};
use cutcap_contracts::*;
use cutcap_media::*;
use slint::ComponentHandle;
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
    time::Instant,
};

struct ProbeJob {
    project_id: ProjectId,
    media_id: MediaId,
    token: Cancellation,
    worker: JoinHandle<Result<ProbeResult, MediaError>>,
    started: Instant,
}
pub struct Registration {
    pub project_id: ProjectId,
    pub media: MediaSummary,
}

pub struct MediaUi {
    backend: Arc<dyn MediaBackend>,
    decoder: LatestDecoder,
    presenter: Presenter,
    dialog: Option<Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>>,
    dialog_project: Option<ProjectId>,
    probe: Option<ProbeJob>,
    selected: Option<(ProjectId, MediaSummary)>,
    probe_times: HashMap<MediaId, (u64, Instant)>,
    first_frame: bool,
    pub measurements: Vec<PresentationMeasurement>,
    pub max_tick_gap_us: u64,
    last_tick: Instant,
    benchmark: Option<crate::benchmark::Run>,
}

impl MediaUi {
    pub fn new(window: &AppWindow) -> Result<Self, std::io::Error> {
        let backend: Arc<dyn MediaBackend> = Arc::new(FfmpegBackend::default());
        let decoder = LatestDecoder::start(backend.clone())?;
        let mut presenter = Presenter::default();
        presenter.reset(window);
        Ok(Self {
            backend,
            decoder,
            presenter,
            dialog: None,
            dialog_project: None,
            probe: None,
            selected: None,
            probe_times: HashMap::new(),
            first_frame: false,
            measurements: Vec::new(),
            max_tick_gap_us: 0,
            last_tick: Instant::now(),
            benchmark: None,
        })
    }
    pub fn enable_benchmark(&mut self, directory: PathBuf) {
        self.benchmark = Some(crate::benchmark::Run::new(directory));
        self.max_tick_gap_us = 0;
        self.measurements.clear();
    }
    pub fn benchmark_tick(&mut self, window: &AppWindow, project: Option<ProjectId>) {
        let Some(project) = project else {
            return;
        };
        let Some(run) = &mut self.benchmark else {
            return;
        };
        let action = run.advance(self.measurements.len());
        if run.complete
            && let Err(error) = slint::quit_event_loop()
        {
            tracing::error!("benchmark could not stop event loop: {error}");
        }
        match action {
            Some(crate::benchmark::Action::Import(path)) => self.start_probe(window, project, path),
            Some(crate::benchmark::Action::Seek(ms)) => self.seek(window, ms),
            None => {}
        }
    }
    pub fn benchmark_report(&self) -> crate::benchmark::Report<'_> {
        crate::benchmark::Report {
            method: "Real project registration, latest decoder and Slint software UI. Latency ends at image assignment on UI thread; excludes physical paint/scanout. 100ms settling between samples; first frame includes import/probe/registration.",
            app_version: env!("CARGO_PKG_VERSION"),
            max_ui_tick_gap_us: self.max_tick_gap_us,
            completed: self
                .benchmark
                .as_ref()
                .is_some_and(|run| run.complete && run.error.is_none()),
            error: self.benchmark.as_ref().and_then(|run| run.error.as_deref()),
            samples: &self.measurements,
        }
    }
    pub fn open_dialog(&mut self, window: &AppWindow, project_id: ProjectId) {
        if self.probe.is_some() || self.dialog.is_some() {
            return;
        }
        self.dialog_project = Some(project_id);
        self.dialog = Some(Box::pin(
            rfd::AsyncFileDialog::new()
                .set_parent(&window.window().window_handle())
                .set_title("Import source video")
                .add_filter("Video", &["mp4", "mov", "m4v", "mkv", "webm", "avi", "ts"])
                .add_filter("All files", &["*"])
                .pick_file(),
        ));
        window.set_import_busy(true);
        window.set_media_status("Choose a local video…".into());
        window.set_media_error(false);
    }
    pub fn start_probe(&mut self, window: &AppWindow, project_id: ProjectId, path: PathBuf) {
        if self.probe.is_some() {
            return;
        }
        let token = Cancellation::default();
        let cancel = token.clone();
        let backend = self.backend.clone();
        let started = Instant::now();
        match thread::Builder::new()
            .name("cutcap-media-probe".into())
            .spawn(move || backend.probe(&path, &cancel))
        {
            Ok(worker) => {
                self.probe = Some(ProbeJob {
                    project_id,
                    media_id: MediaId::new(),
                    token,
                    worker,
                    started,
                });
                window.set_import_busy(true);
                window.set_media_status("Reading video metadata…".into());
                window.set_media_error(false);
            }
            Err(error) => self.import_error(window, &error.to_string()),
        }
    }
    pub fn cancel_import(&mut self, window: &AppWindow) {
        if let Some(job) = &self.probe {
            job.token.cancel();
        }
        window.set_media_status("Import cancelled".into());
        window.set_media_error(false);
    }
    pub fn pending_selection(&mut self, window: &AppWindow) {
        if let Err(error) = self.decoder.cancel() {
            tracing::error!("{error}");
        }
        self.presenter.reset(window);
        self.selected = None;
        window.set_preview_status("Selecting source…".into());
    }
    pub fn reset(&mut self, window: &AppWindow) {
        self.cancel_import(window);
        self.dialog = None;
        self.dialog_project = None;
        self.pending_selection(window);
        self.presenter.reset(window);
        self.probe_times.clear();
        window.set_media_status("Import a local video.".into());
        window.set_media_error(false);
    }
    pub fn select(&mut self, window: &AppWindow, project_id: ProjectId, media: MediaSummary) {
        if self
            .selected
            .as_ref()
            .is_some_and(|(project, previous)| *project == project_id && previous.id == media.id)
        {
            return;
        }
        self.pending_selection(window);
        window.set_source_ready(true);
        window.set_source_title(
            media
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
                .into(),
        );
        let duration_ms = media
            .metadata
            .duration_ticks
            .and_then(|ticks| {
                SourceTime {
                    ticks,
                    timebase: media.metadata.timebase,
                }
                .rescale(
                    Timebase::new(1, 1000).expect("nonzero constant"),
                    Rounding::Ceil,
                )
                .ok()
            })
            .and_then(|t| i32::try_from(t.ticks).ok())
            .unwrap_or(0);
        window.set_source_duration_ms(duration_ms);
        window.set_source_position_ms(0);
        self.selected = Some((project_id, media));
        self.first_frame = true;
        self.seek(window, 0);
    }
    pub fn seek(&mut self, window: &AppWindow, milliseconds: i32) {
        let Some((project_id, media)) = &self.selected else {
            return;
        };
        if milliseconds < 0 || i64::from(milliseconds) > MAX_MEDIA_SECONDS * 1000 {
            self.presenter
                .fail(window, "Source time must be between 0 and 24 hours.");
            return;
        }
        let offset = SourceTime {
            ticks: i64::from(milliseconds),
            timebase: Timebase::new(1, 1000).expect("nonzero constant"),
        }
        .rescale(media.metadata.timebase, Rounding::Ceil);
        let Ok(offset) = offset else {
            self.presenter.fail(window, "Source time overflow.");
            return;
        };
        let Some(ticks) = media.metadata.start_pts.checked_add(offset.ticks) else {
            self.presenter.fail(window, "Source time overflow.");
            return;
        };
        let ticket = PreviewTicket {
            project_id: *project_id,
            media_id: media.id,
            request_id: RequestId::new(),
        };
        let request = DecodeRequest {
            ticket,
            media: media.clone(),
            position: SourceTime {
                ticks,
                timebase: media.metadata.timebase,
            },
        };
        match self.decoder.request(request) {
            Ok(()) => {
                self.presenter.begin(window, ticket);
                window.set_source_position_ms(milliseconds);
            }
            Err(error) => self.presenter.fail(window, &error.to_string()),
        }
    }
    pub fn cancel_preview(&mut self, window: &AppWindow) {
        if let Err(error) = self.decoder.cancel() {
            tracing::error!("{error}");
        }
        self.presenter.cancel(window);
    }
    pub fn import_error(&self, window: &AppWindow, message: &str) {
        tracing::warn!("import failed: {message}");
        window.set_import_busy(false);
        window.set_media_error(true);
        window.set_media_status(message.into());
        window.set_status_text(message.into());
        window.set_status_error(true);
    }
    pub fn poll(
        &mut self,
        window: &AppWindow,
        current_project: Option<ProjectId>,
    ) -> Option<Registration> {
        self.max_tick_gap_us = self.max_tick_gap_us.max(preview::micros(self.last_tick));
        self.last_tick = Instant::now();
        if let Some(mut dialog) = self.dialog.take() {
            let mut context = Context::from_waker(Waker::noop());
            match dialog.as_mut().poll(&mut context) {
                Poll::Pending => self.dialog = Some(dialog),
                Poll::Ready(selected) => {
                    window.set_import_busy(false);
                    if let (Some(file), Some(project)) = (selected, self.dialog_project.take()) {
                        if current_project == Some(project) {
                            self.start_probe(window, project, file.path().to_owned());
                        }
                    } else {
                        window.set_media_status("Import cancelled".into());
                    }
                }
            }
        }
        let mut registration = None;
        if self
            .probe
            .as_ref()
            .is_some_and(|job| job.worker.is_finished())
            && let Some(job) = self.probe.take()
        {
            window.set_import_busy(false);
            match job.worker.join().unwrap_or(Err(MediaError::Worker)) {
                Ok(result)
                    if !job.token.is_cancelled() && current_project == Some(job.project_id) =>
                {
                    tracing::info!(probe_us = result.probe_us, "media probed");
                    if self.probe_times.len() >= 128
                        && let Some(oldest) = self
                            .probe_times
                            .iter()
                            .min_by_key(|(_, (_, started))| *started)
                            .map(|(id, _)| *id)
                    {
                        self.probe_times.remove(&oldest);
                    }
                    self.probe_times
                        .insert(job.media_id, (result.probe_us, job.started));
                    registration = Some(Registration {
                        project_id: job.project_id,
                        media: MediaSummary {
                            id: job.media_id,
                            path: result.path,
                            metadata: result.metadata,
                        },
                    });
                    window.set_media_status("Metadata ready; registering asset…".into());
                }
                Ok(_) | Err(MediaError::Cancelled) => {
                    window.set_media_status("Import cancelled".into());
                }
                Err(error) => {
                    tracing::warn!("probe failed: {error}");
                    self.import_error(window, &user_message(&error));
                }
            }
        }
        match self.decoder.take() {
            Ok(Some(outcome)) if self.presenter.accepts(outcome.ticket) => match outcome.result {
                Ok(frame) => {
                    if let Some((_, media)) = &self.selected {
                        let timing = self.probe_times.get(&media.id).copied();
                        if let Some(measurement) = self.presenter.present(
                            window,
                            media,
                            frame,
                            timing.map(|(probe_us, _)| probe_us),
                            timing
                                .filter(|_| self.first_frame)
                                .map(|(_, started)| started),
                        ) {
                            self.first_frame = false;
                            if self.measurements.len() < 256 {
                                self.measurements.push(measurement);
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!("decode failed: {error}");
                    self.presenter.fail(window, &user_message(&error));
                }
            },
            Err(error) => self.presenter.fail(window, &error.to_string()),
            Ok(_) => {}
        }
        registration
    }
    pub fn shutdown(&mut self) -> Result<(), MediaError> {
        self.dialog = None;
        let probe_result = if let Some(job) = self.probe.take() {
            job.token.cancel();
            job.worker
                .join()
                .unwrap_or(Err(MediaError::Worker))
                .map(|_| ())
        } else {
            Ok(())
        };
        let decoder_result = self.decoder.shutdown();
        if let Err(error) = probe_result
            && !matches!(error, MediaError::Cancelled)
        {
            return Err(error);
        }
        decoder_result
    }
}

fn user_message(error: &MediaError) -> String {
    match error {
        MediaError::Process {
            stage: "Probe media",
            ..
        } => "FFprobe could not read this video. It may be damaged or unsupported.".into(),
        MediaError::Process { .. } => {
            "FFmpeg could not decode this frame. Try an earlier position or another video.".into()
        }
        MediaError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            "The file or media tool was not found. Check the source file and FFmpeg/FFprobe setup."
                .into()
        }
        _ => error.to_string(),
    }
}

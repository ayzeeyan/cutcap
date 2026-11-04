//! Frontend-independent coordination of the probe job and latest-frame decoder.
//!
//! Ported from the Slint `media_ui`/`preview` adapters with the window handle
//! replaced by plain view state. Semantics are unchanged: one cancellable probe,
//! one latest decoder (active/pending/completed), ticket-gated presentation and
//! the ceiling source-PTS policy.
use crate::view::{self, FrameView, ImportView, PreviewMetrics, SourceView, StatusView};
use cutcap_contracts::*;
use cutcap_media::*;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Instant,
};

struct ProbeJob {
    project_id: ProjectId,
    media_id: MediaId,
    token: Cancellation,
    worker: JoinHandle<Result<ProbeResult, MediaError>>,
}

/// A validated probe result waiting for the project owner to commit it.
pub struct Registration {
    pub project_id: ProjectId,
    pub media: MediaSummary,
}

/// The latest presented frame. Owned by Rust; the frontend copies it out.
pub struct PresentedFrame {
    pub generation: u64,
    pub image: RgbaFrame,
}

pub struct MediaHost {
    backend: Arc<dyn MediaBackend>,
    decoder: LatestDecoder,
    probe: Option<ProbeJob>,
    selected: Option<(ProjectId, MediaSummary)>,
    /// Presentation gate: only this ticket may replace the displayed frame.
    ticket: Option<(PreviewTicket, Instant)>,
    frame: Option<PresentedFrame>,
    generation: u64,
    probe_times: HashMap<MediaId, u64>,
    pub source: SourceView,
    pub import: ImportView,
    pub metrics: PreviewMetrics,
}

fn millis() -> Timebase {
    Timebase::new(1, 1000).expect("nonzero constant")
}

impl MediaHost {
    pub fn start(backend: Arc<dyn MediaBackend>) -> Result<Self, std::io::Error> {
        let decoder = LatestDecoder::start(backend.clone())?;
        Ok(Self {
            backend,
            decoder,
            probe: None,
            selected: None,
            ticket: None,
            frame: None,
            generation: 0,
            probe_times: HashMap::new(),
            source: SourceView::default(),
            import: ImportView::default(),
            metrics: PreviewMetrics::default(),
        })
    }

    pub fn frame(&self) -> Option<&PresentedFrame> {
        self.frame.as_ref()
    }

    pub fn frame_view(&self) -> Option<FrameView> {
        self.frame.as_ref().map(|frame| FrameView {
            generation: frame.generation,
            width: frame.image.width(),
            height: frame.image.height(),
        })
    }

    pub fn selected_media(&self) -> Option<MediaId> {
        self.selected.as_ref().map(|(_, media)| media.id)
    }

    pub fn import_busy(&self) -> bool {
        self.probe.is_some()
    }

    pub fn start_probe(&mut self, project_id: ProjectId, path: PathBuf, status: &mut StatusView) {
        if self.probe.is_some() {
            return;
        }
        let token = Cancellation::default();
        let cancel = token.clone();
        let backend = self.backend.clone();
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
                });
                self.import = ImportView {
                    busy: true,
                    error: false,
                    message: "Reading video metadata…".into(),
                };
            }
            Err(error) => self.import_error(&error.to_string(), status),
        }
    }

    pub fn cancel_import(&mut self) {
        if let Some(job) = &self.probe {
            job.token.cancel();
        }
        self.import.error = false;
        self.import.message = "Import cancelled".into();
    }

    fn clear_preview(&mut self) {
        self.ticket = None;
        self.frame = None;
        self.source = SourceView::default();
    }

    pub fn pending_selection(&mut self) {
        if let Err(error) = self.decoder.cancel() {
            tracing::error!("{error}");
        }
        self.clear_preview();
        self.selected = None;
        self.source.status = "Selecting source…".into();
    }

    /// Project replacement: cancel everything scoped to the previous project.
    pub fn reset(&mut self) {
        self.cancel_import();
        self.pending_selection();
        self.clear_preview();
        self.probe_times.clear();
        self.import = ImportView {
            busy: self.probe.is_some(),
            ..ImportView::default()
        };
    }

    pub fn select(&mut self, project_id: ProjectId, media: MediaSummary, status: &mut StatusView) {
        if self
            .selected
            .as_ref()
            .is_some_and(|(project, previous)| *project == project_id && previous.id == media.id)
        {
            return;
        }
        self.pending_selection();
        self.source.ready = true;
        self.source.title = view::file_name(&media);
        self.source.duration_ms = media
            .metadata
            .duration_ticks
            .and_then(|ticks| {
                SourceTime {
                    ticks,
                    timebase: media.metadata.timebase,
                }
                .rescale(millis(), Rounding::Ceil)
                .ok()
            })
            .map_or(0, |time| time.ticks.clamp(0, i64::from(i32::MAX)));
        self.source.duration_estimated = media.metadata.duration_is_estimate;
        self.source.position_ms = 0;
        self.metrics.probe_us = self.probe_times.get(&media.id).copied();
        self.selected = Some((project_id, media));
        self.seek(0, status);
    }

    pub fn seek(&mut self, milliseconds: i64, status: &mut StatusView) {
        let Some((project_id, media)) = &self.selected else {
            return;
        };
        if !(0..=MAX_MEDIA_SECONDS * 1000).contains(&milliseconds) {
            self.fail("Source time must be between 0 and 24 hours.", status);
            return;
        }
        let offset = SourceTime {
            ticks: milliseconds,
            timebase: millis(),
        }
        .rescale(media.metadata.timebase, Rounding::Ceil);
        let Some(ticks) = offset
            .ok()
            .and_then(|offset| media.metadata.start_pts.checked_add(offset.ticks))
        else {
            self.fail("Source time overflow.", status);
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
                self.ticket = Some((ticket, Instant::now()));
                self.source.busy = true;
                self.source.status = "Decoding source frame…".into();
                self.source.detail = "A newer seek replaces this request.".into();
                self.source.position_ms = milliseconds;
            }
            Err(error) => self.fail(&error.to_string(), status),
        }
    }

    pub fn cancel_preview(&mut self, status: &mut StatusView) {
        if let Err(error) = self.decoder.cancel() {
            tracing::error!("{error}");
        }
        self.ticket = None;
        self.source.busy = false;
        self.source.status = "Preview cancelled".into();
        self.source.detail = "Request another source position to continue.".into();
        *status = StatusView {
            text: "Frame request cancelled; the last displayed frame is retained.".into(),
            error: false,
        };
    }

    pub fn fail(&mut self, message: &str, status: &mut StatusView) {
        self.ticket = None;
        self.source.busy = false;
        self.source.status = "Preview unavailable at this position".into();
        self.source.detail = message.into();
        *status = StatusView {
            text: message.into(),
            error: true,
        };
    }

    pub fn import_error(&mut self, message: &str, status: &mut StatusView) {
        tracing::warn!("import failed: {message}");
        self.import = ImportView {
            busy: false,
            error: true,
            message: message.into(),
        };
        *status = StatusView {
            text: message.into(),
            error: true,
        };
    }

    pub fn import_finished(&mut self, name: Option<String>) {
        self.import = ImportView {
            busy: false,
            error: false,
            message: name.map_or_else(
                || "Video imported.".to_owned(),
                |n| format!("Imported {n}."),
            ),
        };
    }

    /// Bounded, non-blocking poll. Returns a probe result for the project owner
    /// and whether any view state changed.
    pub fn poll(
        &mut self,
        current_project: Option<ProjectId>,
        status: &mut StatusView,
    ) -> (Option<Registration>, bool) {
        let mut changed = false;
        let mut registration = None;
        if self
            .probe
            .as_ref()
            .is_some_and(|job| job.worker.is_finished())
            && let Some(job) = self.probe.take()
        {
            changed = true;
            self.import.busy = false;
            match job.worker.join().unwrap_or(Err(MediaError::Worker)) {
                Ok(result)
                    if !job.token.is_cancelled() && current_project == Some(job.project_id) =>
                {
                    tracing::info!(probe_us = result.probe_us, "media probed");
                    if self.probe_times.len() >= 128 {
                        self.probe_times.clear();
                    }
                    self.probe_times.insert(job.media_id, result.probe_us);
                    registration = Some(Registration {
                        project_id: job.project_id,
                        media: MediaSummary {
                            id: job.media_id,
                            path: result.path,
                            metadata: result.metadata,
                        },
                    });
                    self.import.busy = true;
                    self.import.message = "Metadata ready; registering asset…".into();
                }
                Ok(_) | Err(MediaError::Cancelled) => {
                    self.import.message = "Import cancelled".into();
                }
                Err(error) => {
                    tracing::warn!("probe failed: {error}");
                    self.import_error(&user_message(&error), status);
                }
            }
        }
        match self.decoder.take() {
            Ok(Some(outcome)) if self.accepts(outcome.ticket) => {
                changed = true;
                match outcome.result {
                    Ok(frame) => self.present(frame, status),
                    Err(error) => {
                        tracing::warn!("decode failed: {error}");
                        self.fail(&user_message(&error), status);
                    }
                }
            }
            Err(error) => {
                changed = true;
                self.fail(&error.to_string(), status);
            }
            Ok(_) => {}
        }
        (registration, changed)
    }

    fn accepts(&self, ticket: PreviewTicket) -> bool {
        self.ticket.is_some_and(|(current, _)| current == ticket)
    }

    fn present(&mut self, decoded: DecodedFrame, status: &mut StatusView) {
        // Independent gate: protects even a result taken just before a new selection.
        let Some((project_id, media)) = &self.selected else {
            return;
        };
        let Some((ticket, queued)) = self.ticket else {
            return;
        };
        if ticket != decoded.ticket
            || media.id != decoded.ticket.media_id
            || *project_id != decoded.ticket.project_id
        {
            return;
        }
        let relative = media
            .metadata
            .start()
            .rescale(decoded.actual.timebase, Rounding::Floor)
            .ok()
            .and_then(|origin| decoded.actual.ticks.checked_sub(origin.ticks))
            .map(|ticks| SourceTime {
                ticks,
                timebase: decoded.actual.timebase,
            });
        let Some(relative) = relative else {
            self.fail("Decoded source timestamp cannot be represented.", status);
            return;
        };
        let caption = format!(
            "Frame +{} · PTS {} @ {}/{}",
            view::format_time(relative),
            decoded.actual.ticks,
            decoded.actual.timebase.numerator(),
            decoded.actual.timebase.denominator()
        );
        let ready_us = micros(queued);
        tracing::info!(request = %decoded.ticket.request_id, decode_us = decoded.decode_us,
            request_to_ready_us = ready_us, bytes = decoded.image.pixels().len(),
            pts = decoded.actual.ticks, child_pid = decoded.process_id, "preview frame ready");
        self.generation += 1;
        // RgbaFrame has already validated dimensions, packed stride, length and opaque alpha.
        self.frame = Some(PresentedFrame {
            generation: self.generation,
            image: decoded.image,
        });
        self.ticket = None;
        self.source.has_frame = true;
        self.source.busy = false;
        self.source.status = "Source frame decoded".into();
        self.source.detail = String::new();
        self.source.caption = caption;
        self.metrics.decode_us = Some(decoded.decode_us);
        self.metrics.request_to_ready_us = Some(ready_us);
        self.metrics.frames_presented += 1;
    }

    pub fn shutdown(&mut self) -> Result<(), MediaError> {
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

fn micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

pub fn user_message(error: &MediaError) -> String {
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

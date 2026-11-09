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
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct ProbeJob {
    project_id: ProjectId,
    media_id: MediaId,
    token: Cancellation,
    worker: JoinHandle<Result<(ProbeResult, SourceFingerprint), MediaError>>,
    checking: bool,
    relink: bool,
    abandoned: bool,
    finished: mpsc::Receiver<()>,
}

/// A validated probe result waiting for the project owner to commit it.
pub struct Registration {
    pub project_id: ProjectId,
    pub media_id: MediaId,
    pub result: Result<Box<VerifiedMedia>, EngineError>,
    pub checking: bool,
    pub relink: bool,
}

/// The latest presented frame. Owned by Rust; the frontend copies it out.
pub struct PresentedFrame {
    pub generation: u64,
    pub image: RgbaFrame,
    pub ticket: PreviewTicket,
    pub actual: SourceTime,
}

pub struct MediaHost {
    backend: Arc<dyn MediaBackend>,
    decoder: LatestDecoder,
    pub(crate) playback: crate::playback::Playback,
    copy_ticket: Option<PreviewTicket>,
    probe: Option<ProbeJob>,
    selected: Option<(ProjectId, MediaSummary)>,
    /// The audio-only source shown in the Source monitor (nothing to decode).
    audio_only: Option<(ProjectId, MediaId)>,
    /// Presentation gate: only this ticket may replace the displayed frame.
    ticket: Option<(PreviewTicket, Instant)>,
    frame: Option<PresentedFrame>,
    generation: u64,
    probe_times: HashMap<MediaId, u64>,
    pub source: SourceView,
    pub import: ImportView,
    pub metrics: PreviewMetrics,
    source_visible: bool,
    source_needs_frame: bool,
    timeline_ticket: Option<PreviewTicket>,
    timeline_outcome: Option<DecodeOutcome>,
}

fn millis() -> Timebase {
    Timebase::new(1, 1000).expect("nonzero constant")
}

impl MediaHost {
    pub fn start(backend: Arc<dyn MediaBackend>) -> Result<Self, std::io::Error> {
        let decoder = LatestDecoder::start(backend.clone())?;
        let playback = crate::playback::Playback::start(backend.clone())?;
        Ok(Self {
            backend,
            decoder,
            playback,
            copy_ticket: None,
            probe: None,
            selected: None,
            audio_only: None,
            ticket: None,
            frame: None,
            generation: 0,
            probe_times: HashMap::new(),
            source: SourceView::default(),
            import: ImportView::default(),
            metrics: PreviewMetrics::default(),
            source_visible: true,
            source_needs_frame: false,
            timeline_ticket: None,
            timeline_outcome: None,
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
            actual: frame.actual,
            epoch: frame.ticket.request_id.to_string(),
        })
    }

    pub fn selected_media(&self) -> Option<MediaId> {
        self.selected.as_ref().map(|(_, media)| media.id)
    }

    /// One counter for both presentation modes; no duplicate frontend cache.
    pub(crate) fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
    pub(crate) fn request_timeline(&mut self, request: DecodeRequest) -> Result<(), MediaError> {
        self.decoder.request(request.clone())?;
        self.timeline_ticket = Some(request.ticket);
        self.timeline_outcome = None;
        self.ticket = None;
        Ok(())
    }
    pub(crate) fn take_timeline(&mut self) -> Option<DecodeOutcome> {
        self.timeline_outcome.take()
    }
    pub(crate) fn cancel_timeline(&mut self) -> Result<(), MediaError> {
        if self.timeline_ticket.take().is_some() {
            self.decoder.cancel()?;
        }
        self.timeline_outcome = None;
        Ok(())
    }
    pub(crate) fn show_source(
        &mut self,
        visible: bool,
        status: &mut StatusView,
    ) -> Result<(), MediaError> {
        if visible == self.source_visible {
            return Ok(());
        }
        self.source_visible = visible;
        if !visible {
            self.source_needs_frame |= self.source.busy && !self.playback.active();
            self.pause();
            self.decoder.cancel()?;
            self.ticket = None;
            self.source.busy = false;
        } else {
            self.cancel_timeline()?;
            if self.source_needs_frame || self.frame.is_none() {
                self.frame = None;
                self.source.has_frame = false;
                self.source_needs_frame = false;
                self.seek(self.source.position_ms, status);
            } else {
                let generation = self.next_generation();
                if let Some(frame) = &mut self.frame {
                    frame.generation = generation;
                    self.copy_ticket = Some(frame.ticket);
                    if self.playback.active() {
                        self.playback.offered(generation, frame.actual)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn import_busy(&self) -> bool {
        self.probe.is_some()
    }

    pub fn start_probe(
        &mut self,
        project_id: ProjectId,
        media_id: MediaId,
        path: PathBuf,
        checking: bool,
        relink: bool,
        status: &mut StatusView,
    ) -> Result<(), MediaError> {
        if self.probe.is_some() {
            return Err(MediaError::Invalid(
                "Another source inspection is still running.".into(),
            ));
        }
        let token = Cancellation::default();
        let cancel = token.clone();
        let backend = self.backend.clone();
        let (finished, completion) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("cutcap-media-probe".into())
            .spawn(move || {
                let result = crate::persistence::probe_verified(backend.as_ref(), &path, &cancel);
                let _notified = finished.send(());
                result
            }) {
            Ok(worker) => {
                self.probe = Some(ProbeJob {
                    project_id,
                    media_id,
                    token,
                    worker,
                    checking,
                    relink,
                    abandoned: false,
                    finished: completion,
                });
                self.import = ImportView {
                    busy: true,
                    error: false,
                    message: if checking {
                        "Checking source content and metadata…"
                    } else {
                        "Reading media metadata and source fingerprint…"
                    }
                    .into(),
                };
            }
            Err(error) => {
                self.import_error(&error.to_string(), status);
                return Err(MediaError::Worker);
            }
        }
        Ok(())
    }

    pub fn cancel_import(&mut self) {
        if let Some(job) = &self.probe {
            job.token.cancel();
            self.import.message = "Cancelling source inspection…".into();
        } else {
            self.import.message = "Inspection has completed; awaiting project update.".into();
        }
        self.import.error = false;
    }

    fn clear_preview(&mut self) {
        self.copy_ticket = None;
        self.ticket = None;
        self.frame = None;
        self.source = SourceView::default();
    }

    pub fn pending_selection(&mut self) {
        self.timeline_ticket = None;
        self.timeline_outcome = None;
        if let Err(error) = self.playback.reset() {
            tracing::error!("{error}");
        }
        if let Err(error) = self.decoder.cancel() {
            tracing::error!("{error}");
        }
        self.clear_preview();
        self.selected = None;
        self.audio_only = None;
        self.source.status = "Selecting source…".into();
    }

    /// Project replacement: cancel everything scoped to the previous project.
    pub fn reset(&mut self) {
        if let Some(job) = &mut self.probe {
            job.abandoned = true;
        }
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
            .is_some_and(|(project, previous)| *project == project_id && previous == &media)
        {
            return;
        }
        if self.audio_only == Some((project_id, media.id)) {
            return;
        }
        self.pending_selection();
        self.source.title = view::file_name(&media);
        // An audio-only source has no picture to preview: say so and stay idle.
        let Some(video) = media.metadata.video() else {
            self.audio_only = Some((project_id, media.id));
            self.source.ready = false;
            self.source.status = "Audio-only source".into();
            self.source.detail = "Place it on an audio track to hear it on the timeline.".into();
            return;
        };
        self.source.ready = true;
        self.source.duration_ms = video
            .duration_ticks
            .and_then(|ticks| {
                SourceTime {
                    ticks,
                    timebase: video.timebase,
                }
                .rescale(millis(), Rounding::Ceil)
                .ok()
            })
            .map_or(0, |time| time.ticks.clamp(0, i64::from(i32::MAX)));
        self.source.duration_estimated = video.duration_is_estimate;
        self.source.position_ms = 0;
        self.metrics.probe_us = self.probe_times.get(&media.id).copied();
        self.selected = Some((project_id, media));
        if self.source_visible {
            self.seek(0, status);
        } else {
            self.source_needs_frame = true;
        }
    }

    fn request_for(&self, milliseconds: i64) -> Result<DecodeRequest, MediaError> {
        let (project, media) = self
            .selected
            .as_ref()
            .ok_or_else(|| MediaError::Invalid("Select a source first.".into()))?;
        let video = media
            .metadata
            .video()
            .ok_or_else(|| MediaError::Unsupported("This source has no video stream.".into()))?;
        if !(0..=MAX_MEDIA_SECONDS * 1000).contains(&milliseconds) {
            return Err(MediaError::Invalid(
                "Source time must be between 0 and 24 hours.".into(),
            ));
        }
        let offset = SourceTime {
            ticks: milliseconds,
            timebase: millis(),
        }
        .rescale(video.timebase, Rounding::Ceil)
        .map_err(|_| MediaError::Invalid("Source time overflow.".into()))?;
        let ticks = video
            .start_pts
            .checked_add(offset.ticks)
            .ok_or_else(|| MediaError::Invalid("Source time overflow.".into()))?;
        Ok(DecodeRequest {
            ticket: PreviewTicket {
                project_id: *project,
                media_id: media.id,
                request_id: RequestId::new(),
            },
            media: media.clone(),
            position: SourceTime {
                ticks,
                timebase: video.timebase,
            },
        })
    }

    pub fn seek(&mut self, milliseconds: i64, status: &mut StatusView) {
        if self.selected.is_none() {
            return;
        }
        let result = self.request_for(milliseconds).and_then(|request| {
            let ticket = request.ticket;
            if self.playback.active() {
                self.decoder.cancel()?;
                self.ticket = None;
                self.playback.request(
                    request,
                    milliseconds,
                    self.playback.view.desired_playing,
                    true,
                )?;
            } else {
                self.decoder.request(request)?;
                self.ticket = Some((ticket, Instant::now()));
            }
            self.copy_ticket = Some(ticket);
            Ok(())
        });
        match result {
            Ok(()) => {
                self.source.position_ms = milliseconds;
                self.source.busy = true;
                self.source.status = "Decoding source frame…".into();
                self.source.detail = "A newer seek replaces this request.".into();
            }
            Err(error) => self.fail(&error.to_string(), status),
        }
    }

    pub fn play(&mut self) -> Result<(), MediaError> {
        if self.playback.active() && !matches!(self.playback.view.state, "ended" | "failed") {
            self.playback.resume(Instant::now());
        } else {
            let ms = if self.playback.view.state == "ended" {
                0
            } else {
                self.source.position_ms
            };
            let request = self.request_for(ms)?;
            self.decoder.cancel()?;
            self.ticket = None;
            self.copy_ticket = Some(request.ticket);
            self.playback.request(request, ms, true, false)?;
            self.source.position_ms = ms;
        }
        self.sync_transport();
        Ok(())
    }

    pub fn pause(&mut self) {
        self.playback.pause(Instant::now());
        self.sync_transport();
    }

    pub fn stop(&mut self, status: &mut StatusView) -> Result<(), MediaError> {
        self.playback.reset()?;
        if self.selected.is_some() {
            self.seek(0, status);
        }
        Ok(())
    }

    fn sync_transport(&mut self) {
        let state = self.playback.view.state;
        self.source.busy = matches!(state, "starting" | "seeking" | "buffering");
        self.source.status = match state {
            "starting" => "Starting source playback…",
            "seeking" => "Seeking source…",
            "buffering" => "Buffering source…",
            "playing" => "Playing source",
            "paused" => "Source paused",
            "ended" => "End of source",
            "failed" => "Source playback failed",
            _ => "Source stopped",
        }
        .into();
        if state != "failed" {
            self.source.detail = String::new();
        }
    }

    pub fn copy_allowed(&self, generation: u64) -> bool {
        self.frame.as_ref().is_some_and(|frame| {
            frame.generation == generation && Some(frame.ticket) == self.copy_ticket
        })
    }

    pub fn frame_copied(&mut self, generation: u64) -> Result<(), MediaError> {
        if let Some(frame) = &self.frame
            && self.playback.ticket() == Some(frame.ticket)
        {
            self.playback.copied(generation, frame.actual)?;
        }
        Ok(())
    }

    pub fn cancel_preview(&mut self, status: &mut StatusView) {
        if let Err(error) = self.playback.reset() {
            tracing::error!("{error}");
        }
        self.copy_ticket = self.frame.as_ref().map(|frame| frame.ticket);
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
                || "Media imported.".to_owned(),
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
            let result = job.worker.join().unwrap_or(Err(MediaError::Worker));
            if !job.abandoned && current_project == Some(job.project_id) && job.checking {
                let result = if job.token.is_cancelled() {
                    Err(MediaError::Cancelled)
                } else {
                    result
                };
                registration = Some(Registration {
                    project_id: job.project_id,
                    media_id: job.media_id,
                    checking: true,
                    relink: job.relink,
                    result: result
                        .map(|(result, fingerprint)| {
                            Box::new(VerifiedMedia {
                                media: MediaSummary {
                                    id: job.media_id,
                                    path: result.path,
                                    metadata: result.metadata,
                                },
                                fingerprint,
                            })
                        })
                        .map_err(inspection_error),
                });
            } else if !job.abandoned && current_project == Some(job.project_id) {
                match result {
                    Ok((result, fingerprint))
                        if !job.token.is_cancelled() && current_project == Some(job.project_id) =>
                    {
                        tracing::info!(probe_us = result.probe_us, "media probed");
                        if self.probe_times.len() >= 128 {
                            self.probe_times.clear();
                        }
                        self.probe_times.insert(job.media_id, result.probe_us);
                        registration = Some(Registration {
                            project_id: job.project_id,
                            media_id: job.media_id,
                            result: Ok(Box::new(VerifiedMedia {
                                media: MediaSummary {
                                    id: job.media_id,
                                    path: result.path,
                                    metadata: result.metadata,
                                },
                                fingerprint,
                            })),
                            checking: false,
                            relink: false,
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
        }
        match self.decoder.take() {
            Ok(Some(outcome)) if self.timeline_ticket == Some(outcome.ticket) => {
                self.timeline_outcome = Some(outcome);
                changed = true;
            }
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
        match self.playback.tick(Instant::now()) {
            Ok((frame, state_changed)) => {
                changed |= state_changed;
                if let Some(frame) = frame {
                    let actual = frame.actual;
                    self.ticket = Some((frame.ticket, self.playback.requested_at()));
                    self.present(frame, status);
                    if let Err(error) = self.playback.offered(self.generation, actual) {
                        self.playback.fail(&error, Instant::now());
                        self.fail(&error.to_string(), status);
                    }
                    changed = true;
                }
                if self.playback.active() {
                    self.sync_transport();
                }
            }
            Err(error) => {
                self.playback.fail(&error, Instant::now());
                self.fail(&user_message(&error), status);
                changed = true;
            }
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
            .video()
            .and_then(|video| {
                video
                    .start()
                    .rescale(decoded.actual.timebase, Rounding::Floor)
                    .ok()
            })
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
            ticket: decoded.ticket,
            actual: decoded.actual,
        });
        self.ticket = None;
        self.source.has_frame = true;
        self.source.busy = false;
        self.source.status = "Source frame decoded".into();
        self.source.detail = String::new();
        self.source.caption = caption;
        self.metrics.decode_us = Some(decoded.decode_us);
        if !self.playback.active() || self.playback.view.offered == 0 {
            self.metrics.request_to_ready_us = Some(ready_us);
        }
        self.metrics.frames_presented += 1;
    }

    pub fn shutdown(&mut self) -> Result<(), MediaError> {
        let probe_result = if let Some(job) = self.probe.take() {
            job.token.cancel();
            if matches!(
                job.finished.recv_timeout(Duration::from_millis(250)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                // OS path/read calls cannot be interrupted. No session/UI is
                // captured; the owned task will release its read handle on return.
                tracing::warn!(
                    "source inspection still inside OS/backend call; cancellation requested, detaching owned task"
                );
                Ok(())
            } else {
                job.worker
                    .join()
                    .unwrap_or(Err(MediaError::Worker))
                    .map(|_| ())
            }
        } else {
            Ok(())
        };
        let decoder_result = self.decoder.shutdown();
        let playback_result = self.playback.shutdown();
        if let Err(error) = probe_result
            && !matches!(error, MediaError::Cancelled)
        {
            return Err(error);
        }
        decoder_result?;
        playback_result
    }
}

fn micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn inspection_error(error: MediaError) -> EngineError {
    let code = match &error {
        MediaError::Io {
            stage: "Locate source media" | "Inspect source media",
            source,
        } if source.kind() == std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        _ => ErrorCode::Internal,
    };
    EngineError::new(code, user_message(&error))
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_probe_tool_is_not_reported_as_a_missing_source() {
        let missing = |stage| {
            inspection_error(MediaError::Io {
                stage,
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        };
        assert_eq!(missing("Locate source media").code, ErrorCode::NotFound);
        assert_eq!(missing("Inspect source media").code, ErrorCode::NotFound);
        assert_eq!(missing("Start media tool").code, ErrorCode::Internal);
    }
}

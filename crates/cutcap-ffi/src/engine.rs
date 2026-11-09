//! The lightweight command owner. Future media jobs must not run in this loop.
use crate::history::{Change, Edit, History, bump};
use crate::persistence::{Document, FileJob, FileOutput, SavedSource};
use cutcap_contracts::*;
use cutcap_core::{Clip, Project, ProjectError, TimelineIndex, Track, map_clip_source};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
pub struct Engine {
    project: Option<Project>,
    selection: SelectionState,
    media_details: BTreeMap<MediaId, MediaSummary>,
    source_media: Option<MediaId>,
    sources: BTreeMap<MediaId, SavedSource>,
    availability: BTreeMap<MediaId, MediaAvailability>,
    document: DocumentStatus,
    file_job: Option<FileJob>,
    /// Clip-edit undo/redo; reset by New and Open.
    history: History,
    /// Highest document revision ever issued in this session (revisions are unique).
    next_revision: u64,
    evaluation_generation: RequestId,
    /// Adjustment made by the last successful edit, for the status line.
    edit_notice: Option<String>,
    evaluation_index: Option<Arc<TimelineIndex>>,
    timeline_position: TimelineTime,
}

impl Engine {
    pub fn file_busy(&self) -> bool {
        self.file_job.is_some()
    }
    pub fn handle(&mut self, envelope: CommandEnvelope) -> EngineEvent {
        let request_id = envelope.request_id;
        if let Command::EvaluateTimeline { position, track_id } = envelope.command {
            return EngineEvent::TimelineEvaluated {
                request_id,
                result: self
                    .evaluate(envelope.project_id, position, track_id)
                    .map(Box::new),
            };
        }
        let geometry = matches!(
            envelope.command,
            Command::NewProject { .. }
                | Command::AddTrack { .. }
                | Command::InsertClip { .. }
                | Command::RemoveClip { .. }
                | Command::PlaceClip { .. }
                | Command::MoveClip { .. }
                | Command::TrimClip { .. }
                | Command::SplitClip { .. }
                | Command::PlaceAudioStream { .. }
                | Command::UnlinkClip { .. }
                | Command::LinkClips { .. }
                | Command::SetTrackGain { .. }
                | Command::SetTrackMute { .. }
                | Command::SetTrackSolo { .. }
                | Command::Undo
                | Command::Redo
        );
        let verifying = matches!(envelope.command, Command::VerifyMedia { .. });
        let result = self.begin_file(&envelope);
        if let Ok(true) = result {
            return match self.snapshot() {
                Ok(snapshot) => EngineEvent::Updated { snapshot },
                Err(error) => EngineEvent::Failed { request_id, error },
            };
        }
        match result.and_then(|_| self.execute(envelope)) {
            Ok(snapshot) => {
                if geometry {
                    self.evaluation_index = None;
                }
                EngineEvent::Completed {
                    request_id,
                    snapshot,
                }
            }
            Err(error) if verifying => {
                self.evaluation_generation = RequestId::new();
                EngineEvent::FileFailed {
                    request_id,
                    error,
                    snapshot: self.snapshot().ok(),
                }
            }
            Err(error) => EngineEvent::Failed { request_id, error },
        }
    }

    fn execute(&mut self, envelope: CommandEnvelope) -> Result<ProjectSnapshot, EngineError> {
        let old_monitor = self
            .evaluation_index
            .as_ref()
            .and_then(|i| i.monitor(self.selection))
            .map(|t| t.id);
        // Track controls change only the mix: the transport keeps running.
        let affects_evaluation = !matches!(
            envelope.command,
            Command::GetSnapshot
                | Command::SelectMedia { .. }
                | Command::Select { .. }
                | Command::SetTrackGain { .. }
                | Command::SetTrackMute { .. }
                | Command::SetTrackSolo { .. }
        ) && !match envelope.command {
            Command::Undo => self.history.next_is_track(true),
            Command::Redo => self.history.next_is_track(false),
            _ => false,
        };
        if self.file_job.as_ref().is_some_and(|job| !job.saving) {
            return Err(EngineError::new(
                ErrorCode::Busy,
                "Wait for project opening to finish.",
            ));
        }
        if let Command::NewProject { name } = envelope.command {
            if self.file_job.is_some() {
                return Err(EngineError::new(
                    ErrorCode::Busy,
                    "Wait for the project save to finish.",
                ));
            }
            let mut project =
                Project::new(name, Timebase::DEFAULT_TIMELINE).map_err(domain_error)?;
            for (name, kind) in [("Video 1", TrackKind::Video), ("Audio 1", TrackKind::Audio)] {
                project
                    .add_track(Track {
                        id: TrackId::new(),
                        name: name.into(),
                        kind,
                        controls: Default::default(),
                    })
                    .map_err(domain_error)?;
            }
            self.project = Some(project);
            self.selection = SelectionState::None;
            self.media_details.clear();
            self.source_media = None;
            self.sources.clear();
            self.availability.clear();
            self.history.clear();
            self.edit_notice = None;
            self.timeline_position = TimelineTime::ZERO;
            let mut revision = 0;
            bump(&mut self.next_revision, &mut revision);
            self.document = DocumentStatus {
                revision,
                state: "idle".into(),
                dirty: true,
                ..Default::default()
            };
        } else {
            let project = self
                .project
                .as_mut()
                .ok_or_else(|| EngineError::new(ErrorCode::NotFound, "Create a project first."))?;
            if envelope.project_id != Some(project.id()) {
                return Err(EngineError::new(
                    ErrorCode::StaleProject,
                    "This action belongs to a previous project. Try again.",
                ));
            }
            let mut notice = None;
            match envelope.command {
                Command::GetSnapshot => {}
                Command::RegisterMedia { media, fingerprint } => {
                    let media = *media;
                    media.metadata.validate().map_err(|error| {
                        EngineError::new(ErrorCode::InvalidInput, error.to_string())
                    })?;
                    if self.media_details.len() >= 128 {
                        return Err(EngineError::new(
                            ErrorCode::InvalidInput,
                            "Phase 1 supports at most 128 imported assets.",
                        ));
                    }
                    if project.media().iter().any(|asset| asset.path == media.path) {
                        return Err(EngineError::new(
                            ErrorCode::InvalidInput,
                            "This file is already in the library. Select its entry.",
                        ));
                    }
                    project
                        .add_media(cutcap_core::MediaAsset {
                            id: media.id,
                            path: media.path.clone(),
                            kind: if media.metadata.is_audio_only() {
                                TrackKind::Audio
                            } else {
                                TrackKind::Video
                            },
                        })
                        .map_err(domain_error)?;
                    self.source_media = Some(media.id);
                    self.sources.insert(
                        media.id,
                        SavedSource {
                            id: media.id,
                            metadata: Some(media.metadata.clone()),
                            fingerprint,
                        },
                    );
                    self.availability.insert(
                        media.id,
                        MediaAvailability {
                            id: media.id,
                            path: media.path.clone(),
                            state: Availability::Ready,
                            detail: "Source verified".into(),
                        },
                    );
                    self.media_details.insert(media.id, media);
                    bump(&mut self.next_revision, &mut self.document.revision);
                }
                Command::VerifyMedia {
                    media_id,
                    result,
                    relink,
                } => {
                    let asset = project
                        .media()
                        .iter()
                        .find(|m| m.id == media_id)
                        .ok_or_else(|| {
                            EngineError::new(
                                ErrorCode::NotFound,
                                "Media no longer belongs to this project.",
                            )
                        })?;
                    let path = asset.path.clone();
                    let source = self.sources.get(&media_id).ok_or_else(|| {
                        EngineError::new(ErrorCode::NotFound, "Source record is missing.")
                    })?;
                    let verification = result.and_then(|verified| {
                        if verified.media.id != media_id { return Err(EngineError::new(ErrorCode::InvalidInput, "Source identity mismatch.")); }
                        if source.fingerprint.is_none() || source.metadata.is_none() {
                            return Err(EngineError::new(ErrorCode::Unsupported, "This saved source has no verified video identity. Import supported video as a new asset."));
                        }
                        if source.fingerprint.as_ref() != Some(&verified.fingerprint)
                            || !source.metadata.as_ref().is_some_and(|m| m.matches_probe(&verified.media.metadata)) {
                            return Err(EngineError::new(ErrorCode::InvalidInput, "Source content or metadata changed. Relink the original file, or import the changed file as a new asset."));
                        }
                        Ok(verified)
                    });
                    match verification {
                        Ok(verified) => {
                            if relink && path != verified.media.path {
                                project
                                    .relink_media(media_id, verified.media.path.clone())
                                    .map_err(domain_error)?;
                                bump(&mut self.next_revision, &mut self.document.revision);
                            }
                            self.availability.insert(
                                media_id,
                                MediaAvailability {
                                    id: media_id,
                                    path: verified.media.path.clone(),
                                    state: Availability::Ready,
                                    detail: "Source verified".into(),
                                },
                            );
                            // Metadata saved before Phase 4B gains its audio details.
                            if let Some(saved) = self.sources.get_mut(&media_id) {
                                saved.metadata = Some(verified.media.metadata.clone());
                            }
                            self.media_details.insert(media_id, verified.media);
                        }
                        Err(error) => {
                            // A failed relink must not invalidate a previously ready source.
                            if !relink
                                || self
                                    .availability
                                    .get(&media_id)
                                    .is_none_or(|s| s.state != Availability::Ready)
                            {
                                let state = match error.code {
                                    ErrorCode::NotFound => Availability::Missing,
                                    ErrorCode::InvalidInput => Availability::Changed,
                                    _ => Availability::Unavailable,
                                };
                                self.availability.insert(
                                    media_id,
                                    MediaAvailability {
                                        id: media_id,
                                        path,
                                        state,
                                        detail: error.to_string(),
                                    },
                                );
                            }
                            return Err(error);
                        }
                    }
                }
                Command::SelectMedia { media_id } => {
                    if !project.media().iter().any(|asset| asset.id == media_id) {
                        return Err(EngineError::new(
                            ErrorCode::NotFound,
                            "This media asset is no longer in this project.",
                        ));
                    }
                    self.source_media = Some(media_id);
                }
                Command::AddTrack { kind } => {
                    let number = project
                        .tracks()
                        .iter()
                        .filter(|track| track.kind == kind)
                        .count()
                        + 1;
                    let label = match kind {
                        TrackKind::Video => "Video",
                        TrackKind::Audio => "Audio",
                    };
                    let id = TrackId::new();
                    project
                        .add_track(Track {
                            id,
                            name: format!("{label} {number}"),
                            kind,
                            controls: Default::default(),
                        })
                        .map_err(domain_error)?;
                    self.selection = SelectionState::Track(id);
                    bump(&mut self.next_revision, &mut self.document.revision);
                }
                Command::InsertClip { media_id, track_id } => {
                    let summary = self
                        .media_details
                        .get(&media_id)
                        .filter(|_| {
                            self.availability
                                .get(&media_id)
                                .is_some_and(|a| a.state == Availability::Ready)
                        })
                        .ok_or_else(|| {
                            EngineError::new(
                                ErrorCode::InvalidInput,
                                "Only a verified, ready source can be added to the timeline.",
                            )
                        })?;
                    let clips = if summary.metadata.is_audio_only() {
                        // Phase 4C: audio-only sources append to an audio track.
                        let stream = summary.metadata.primary_audio().ok_or_else(|| {
                            EngineError::new(
                                ErrorCode::InvalidInput,
                                "This source has no decodable audio stream.",
                            )
                        })?;
                        let start = project.track_end(track_id).map_err(domain_error)?;
                        vec![crate::edits::independent_audio(
                            project, summary, stream, track_id, start,
                        )?]
                    } else {
                        // Phase 4B: the source's primary audio joins as a linked clip.
                        let clip = append_clip(project, summary, track_id)?;
                        crate::edits::with_linked_audio(project, summary, clip)?
                    };
                    let id = clips[0].id;
                    let label = format!(
                        "add {}{}",
                        crate::view::file_name(summary),
                        if clips.len() == 2 { " with audio" } else { "" }
                    );
                    let base = project.clips().len();
                    let insert: Vec<_> = clips
                        .into_iter()
                        .enumerate()
                        .map(|(i, c)| (base + i, c))
                        .collect();
                    let change = project.replace_clips(&[], &insert).map_err(domain_error)?;
                    let before = self.document.revision;
                    let after = bump(&mut self.next_revision, &mut self.document.revision);
                    self.selection = SelectionState::Clip(id);
                    self.history.record(Edit {
                        change: change.into(),
                        before,
                        after,
                        label,
                    });
                }
                Command::RemoveClip { clip_id } => {
                    // A linked pair is removed as one undoable transaction.
                    let mut members = vec![clip_id];
                    members.extend(project.linked_partner(clip_id).map(|c| c.id));
                    let change = project.replace_clips(&members, &[]).map_err(domain_error)?;
                    let label = change
                        .removed
                        .first()
                        .and_then(|(_, clip)| self.media_details.get(&clip.media_id))
                        .map_or_else(
                            || "remove clip".to_owned(),
                            |m| format!("remove {}", crate::view::file_name(m)),
                        );
                    let before = self.document.revision;
                    let after = bump(&mut self.next_revision, &mut self.document.revision);
                    if matches!(self.selection, SelectionState::Clip(id) if members.contains(&id)) {
                        self.selection = SelectionState::None;
                    }
                    self.history.record(Edit {
                        change: change.into(),
                        before,
                        after,
                        label,
                    });
                }
                command @ (Command::PlaceClip { .. }
                | Command::MoveClip { .. }
                | Command::TrimClip { .. }
                | Command::SplitClip { .. }
                | Command::PlaceAudioStream { .. }
                | Command::UnlinkClip { .. }
                | Command::LinkClips { .. }) => {
                    let outcome = crate::edits::apply(
                        project,
                        command,
                        &self.media_details,
                        &self.availability,
                    )?;
                    let before = self.document.revision;
                    let after = bump(&mut self.next_revision, &mut self.document.revision);
                    self.selection = outcome.selection;
                    notice = outcome.notice;
                    self.history.record(Edit {
                        change: outcome.change.into(),
                        before,
                        after,
                        label: outcome.label,
                    });
                }
                command @ (Command::SetTrackGain { .. }
                | Command::SetTrackMute { .. }
                | Command::SetTrackSolo { .. }) => {
                    let (track_id, change): (TrackId, fn(&mut TrackControls, i32)) = match command {
                        Command::SetTrackGain { track_id, .. } => (track_id, |c, v| c.gain_cdb = v),
                        Command::SetTrackMute { track_id, .. } => {
                            (track_id, |c, v| c.mute = v != 0)
                        }
                        Command::SetTrackSolo { track_id, .. } => {
                            (track_id, |c, v| c.solo = v != 0)
                        }
                        _ => unreachable!("matched above"),
                    };
                    let value = match command {
                        Command::SetTrackGain { gain_cdb, .. } => gain_cdb,
                        Command::SetTrackMute { mute: v, .. }
                        | Command::SetTrackSolo { solo: v, .. } => i32::from(v),
                        _ => unreachable!("matched above"),
                    };
                    let track = project
                        .tracks()
                        .iter()
                        .find(|t| t.id == track_id)
                        .ok_or_else(|| {
                            EngineError::new(ErrorCode::NotFound, "That track no longer exists.")
                        })?;
                    if track.kind != TrackKind::Audio {
                        return Err(EngineError::new(
                            ErrorCode::InvalidInput,
                            "Only audio tracks have volume, mute and solo.",
                        ));
                    }
                    let name = track.name.clone();
                    let controls = track.controls;
                    let mut updated = controls;
                    change(&mut updated, value);
                    if updated != controls {
                        project
                            .set_track_controls(track_id, updated)
                            .map_err(domain_error)?;
                        let label = match command {
                            Command::SetTrackGain { .. } => format!(
                                "set {name} volume to {}",
                                crate::view::gain_text(updated.gain_cdb)
                            ),
                            Command::SetTrackMute { mute: true, .. } => format!("mute {name}"),
                            Command::SetTrackMute { .. } => format!("unmute {name}"),
                            Command::SetTrackSolo { solo: true, .. } => format!("solo {name}"),
                            _ => format!("unsolo {name}"),
                        };
                        let before = self.document.revision;
                        let after = bump(&mut self.next_revision, &mut self.document.revision);
                        self.history.record(Edit {
                            change: Change::Track {
                                id: track_id,
                                before: controls,
                                after: updated,
                            },
                            before,
                            after,
                            label,
                        });
                    }
                }
                Command::Undo | Command::Redo => {
                    let undo = matches!(envelope.command, Command::Undo);
                    let outcome = if undo {
                        self.history.undo(
                            project,
                            &mut self.document.revision,
                            &mut self.next_revision,
                            self.selection,
                        )
                    } else {
                        self.history.redo(
                            project,
                            &mut self.document.revision,
                            &mut self.next_revision,
                            self.selection,
                        )
                    }
                    .map_err(domain_error)?;
                    match outcome {
                        Some(selection) => self.selection = selection,
                        None => {
                            return Err(EngineError::new(
                                ErrorCode::InvalidInput,
                                if undo {
                                    "Nothing to undo."
                                } else {
                                    "Nothing to redo."
                                },
                            ));
                        }
                    }
                }
                Command::Select { selection } => {
                    project
                        .validate_selection(selection)
                        .map_err(domain_error)?;
                    self.selection = selection;
                }
                _ => {
                    return Err(EngineError::new(
                        ErrorCode::Unsupported,
                        "This operation is unavailable in this milestone.",
                    ));
                }
            }
            self.edit_notice = notice;
        }
        let monitor = self
            .evaluation_index
            .as_ref()
            .and_then(|i| i.monitor(self.selection))
            .map(|t| t.id);
        if affects_evaluation || old_monitor != monitor {
            self.evaluation_generation = RequestId::new();
        }
        self.snapshot()
    }

    pub fn playback_plan(
        &mut self,
        scope: Option<ProjectId>,
        input: TimelineSeek,
    ) -> Result<crate::timeline_plan::TimelinePlan, EngineError> {
        let evaluation = self.evaluate(scope, input, None)?;
        let index = self.evaluation_index.as_ref().ok_or_else(|| {
            EngineError::new(ErrorCode::Internal, "Timeline index is unavailable.")
        })?;
        Ok(crate::timeline_plan::TimelinePlan {
            project: evaluation.project_id,
            generation: evaluation.generation,
            timebase: evaluation.timebase,
            track: evaluation.track,
            extent: evaluation.extent,
            position: evaluation.position,
            index: index.clone(),
            media: self.media_details.clone(),
            availability: self.availability.clone(),
        })
    }

    /// Constant-size response and O(log clips-on-track) lookup after index build.
    fn evaluate(
        &mut self,
        scope: Option<ProjectId>,
        input: TimelineSeek,
        explicit_track: Option<TrackId>,
    ) -> Result<TimelineEvaluation, EngineError> {
        let started = std::time::Instant::now();
        let invalid = |error: String| EngineError::new(ErrorCode::InvalidInput, error);
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| EngineError::new(ErrorCode::NotFound, "No project is open."))?;
        if scope != Some(project.id()) {
            return Err(EngineError::new(
                ErrorCode::StaleProject,
                "Timeline request belongs to another project.",
            ));
        }
        if self.file_job.as_ref().is_some_and(|job| !job.saving) {
            return Err(EngineError::new(
                ErrorCode::Busy,
                "Project opening is in progress.",
            ));
        }
        let position = match input {
            TimelineSeek::Ticks(value) => value,
            TimelineSeek::Milliseconds(ms) => TimelineTime(
                SourceTime {
                    ticks: ms,
                    timebase: Timebase::new(1, 1000).expect("constant"),
                }
                .rescale(project.timebase(), Rounding::Floor)
                .map_err(|e| invalid(e.to_string()))?
                .ticks,
            ),
        };
        if position.0 < 0 {
            return Err(invalid("Timeline position must be nonnegative.".into()));
        }
        // The view uses signed integer milliseconds for coordinates/readouts.
        // Reject an unrepresentable display position before changing owner state.
        SourceTime {
            ticks: position.0,
            timebase: project.timebase(),
        }
        .rescale(Timebase::new(1, 1000).expect("constant"), Rounding::Floor)
        .map_err(|e| invalid(e.to_string()))?;
        let index = self
            .evaluation_index
            .get_or_insert_with(|| Arc::new(TimelineIndex::new(project)));
        let track = match explicit_track {
            // The index now holds audio tracks too; video evaluation stays video-only.
            Some(id) => Some(
                index
                    .track(id)
                    .filter(|t| t.kind == TrackKind::Video)
                    .ok_or_else(|| invalid("Monitor a video track.".into()))?,
            ),
            None => index.monitor(self.selection),
        };
        // Every audio track plays, so audio may outlast the monitored picture
        // or be all there is.
        let extent = track
            .map_or(Ok(TimelineTime::ZERO), |t| index.end(t.id))
            .and_then(|picture| Ok(picture.max(index.audio_end()?)))
            .map_err(|e| invalid(e.to_string()))?;
        let clip = track
            .map_or(Ok(None), |t| index.active(project.id(), t.id, position))
            .map_err(|e| invalid(e.to_string()))?;
        let target = if let Some(clip) = clip {
            let summary = ClipSummary {
                stream: clip.stream,
                link: clip.link,
                id: clip.id,
                media_id: clip.media_id,
                track_id: clip.track_id,
                start: clip.start,
                duration: clip.duration,
                source_in: clip.source_in,
            };
            let state = self.availability.get(&clip.media_id);
            if !state.is_some_and(|s| s.state == Availability::Ready) {
                TimelineTarget::Unavailable {
                    clip: summary,
                    path: state.map(|s| s.path.clone()).unwrap_or_default(),
                    availability: state.map_or(Availability::Unavailable, |s| s.state),
                    detail: state.map_or_else(
                        || "Source has not been verified.".into(),
                        |s| s.detail.clone(),
                    ),
                }
            } else if let Some(media) = self.media_details.get(&clip.media_id) {
                let clock = media
                    .metadata
                    .video()
                    .ok_or_else(|| invalid("This clip's source has no video stream.".into()))?
                    .timebase;
                let mapping = map_clip_source(clip, position, project.timebase(), clock)
                    .map_err(|e| invalid(e.to_string()))?;
                if mapping.requested.ticks >= mapping.end_exclusive.ticks {
                    TimelineTarget::NoFrame {
                        clip: summary,
                        media: Box::new(media.clone()),
                        requested: mapping.requested,
                        end_exclusive: mapping.end_exclusive,
                        detail: "No source tick remains inside this clip after ceiling conversion."
                            .into(),
                    }
                } else {
                    TimelineTarget::Frame {
                        clip: summary,
                        media: Box::new(media.clone()),
                        requested: mapping.requested,
                        end_exclusive: mapping.end_exclusive,
                    }
                }
            } else {
                return Err(invalid("Verified source metadata is missing.".into()));
            }
        } else {
            TimelineTarget::Blank
        };
        self.timeline_position = position;
        Ok(TimelineEvaluation {
            project_id: project.id(),
            generation: self.evaluation_generation,
            document_revision: self.document.revision,
            position,
            timebase: project.timebase(),
            track: track.map(|t| MonitoredTrack {
                id: t.id,
                name: t.name.clone(),
            }),
            extent,
            target,
            evaluation_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        })
    }

    fn snapshot(&self) -> Result<ProjectSnapshot, EngineError> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| EngineError::new(ErrorCode::NotFound, "No project is open."))?;
        let mut clip_counts = BTreeMap::<TrackId, usize>::new();
        for clip in project.clips() {
            *clip_counts.entry(clip.track_id).or_default() += 1;
        }
        Ok(ProjectSnapshot {
            id: project.id(),
            evaluation_generation: self.evaluation_generation,
            name: project.name().into(),
            timebase: project.timebase(),
            tracks: project
                .tracks()
                .iter()
                .map(|track| TrackSummary {
                    id: track.id,
                    name: track.name.clone(),
                    kind: track.kind,
                    clip_count: clip_counts.get(&track.id).copied().unwrap_or(0),
                    controls: track.controls,
                })
                .collect(),
            media_count: project.media().len(),
            clip_count: project.clips().len(),
            selection: self.selection,
            media: project
                .media()
                .iter()
                .filter_map(|asset| self.media_details.get(&asset.id).cloned())
                .collect(),
            source_media: self.source_media,
            clips: project
                .clips()
                .iter()
                .map(|clip| ClipSummary {
                    stream: clip.stream,
                    link: clip.link,
                    id: clip.id,
                    media_id: clip.media_id,
                    track_id: clip.track_id,
                    start: clip.start,
                    duration: clip.duration,
                    source_in: clip.source_in,
                })
                .collect(),
            history: self.history.status(),
            edit_notice: self.edit_notice.clone(),
            media_state: project
                .media()
                .iter()
                .map(|asset| {
                    self.availability
                        .get(&asset.id)
                        .cloned()
                        .unwrap_or(MediaAvailability {
                            id: asset.id,
                            path: asset.path.clone(),
                            state: Availability::Unavailable,
                            detail: "Source has not been verified.".into(),
                        })
                })
                .collect(),
            document: DocumentStatus {
                dirty: self.document.saved_revision != Some(self.document.revision),
                ..self.document.clone()
            },
        })
    }

    fn begin_file(&mut self, envelope: &CommandEnvelope) -> Result<bool, EngineError> {
        if !matches!(
            envelope.command,
            Command::SaveProject { .. } | Command::OpenProject { .. }
        ) {
            return Ok(false);
        }
        if self.file_job.is_some() {
            return Err(EngineError::new(
                ErrorCode::Busy,
                "A project file operation is already in progress.",
            ));
        }
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| EngineError::new(ErrorCode::NotFound, "No project is open."))?;
        if envelope.project_id != Some(project.id()) {
            return Err(EngineError::new(
                ErrorCode::StaleProject,
                "Project changed before this operation.",
            ));
        }
        let job = match &envelope.command {
            Command::SaveProject { path } => {
                let path = path
                    .as_ref()
                    .or(self.document.path.as_ref())
                    .ok_or_else(|| {
                        EngineError::new(
                            ErrorCode::InvalidInput,
                            "Use Save As to choose a project file.",
                        )
                    })?;
                let sources = project
                    .media()
                    .iter()
                    .map(|m| {
                        self.sources.get(&m.id).cloned().unwrap_or(SavedSource {
                            id: m.id,
                            metadata: None,
                            fingerprint: None,
                        })
                    })
                    .collect();
                FileJob::save(
                    envelope.request_id,
                    self.document.revision,
                    Document::new(project.clone(), sources)?,
                    path.clone(),
                )?
            }
            Command::OpenProject { path } => FileJob::open(envelope.request_id, path.clone())?,
            _ => return Ok(false),
        };
        self.document.operation_id = Some(envelope.request_id);
        self.document.state = if job.saving { "saving" } else { "opening" }.into();
        self.document.error = None;
        self.file_job = Some(job);
        Ok(true)
    }

    /// Completion is polled by the cheap command owner, never by the UI doing I/O.
    pub fn poll_file(&mut self) -> Option<EngineEvent> {
        let result = self.file_job.as_mut()?.poll()?;
        let job = self.file_job.take()?;
        let request_id = job.request_id;
        match result {
            Ok(FileOutput::Saved(path)) => {
                self.document.path = Some(path);
                self.document.saved_revision = Some(job.revision);
                self.document.state = "saved".into();
            }
            Ok(FileOutput::Opened(doc, path)) => {
                self.project = Some(doc.project);
                self.evaluation_generation = RequestId::new();
                self.evaluation_index = None;
                self.timeline_position = TimelineTime::ZERO;
                self.selection = SelectionState::None;
                self.history.clear();
                self.edit_notice = None;
                self.source_media = None;
                self.media_details.clear();
                self.availability.clear();
                self.sources = doc.sources.into_iter().map(|s| (s.id, s)).collect();
                if let Some(project) = &self.project {
                    for asset in project.media() {
                        if let Some(metadata) =
                            self.sources.get(&asset.id).and_then(|s| s.metadata.clone())
                        {
                            self.media_details.insert(
                                asset.id,
                                MediaSummary {
                                    id: asset.id,
                                    path: asset.path.clone(),
                                    metadata,
                                },
                            );
                        }
                        self.availability.insert(
                            asset.id,
                            MediaAvailability {
                                id: asset.id,
                                path: asset.path.clone(),
                                state: Availability::Checking,
                                detail: "Checking source content and metadata…".into(),
                            },
                        );
                    }
                }
                self.document = DocumentStatus {
                    path: Some(path),
                    revision: 0,
                    saved_revision: Some(0),
                    state: "opened".into(),
                    operation_id: Some(request_id),
                    ..Default::default()
                };
            }
            Err(error) => {
                self.document.state = "failed".into();
                self.document.error = Some(error.to_string());
                // Include the failed status without acknowledging a successful command.
                return Some(EngineEvent::FileFailed {
                    request_id,
                    error,
                    snapshot: self.snapshot().ok(),
                });
            }
        }
        Some(match self.snapshot() {
            Ok(snapshot) => EngineEvent::Completed {
                request_id,
                snapshot,
            },
            Err(error) => EngineEvent::Failed { request_id, error },
        })
    }
}

/// The append-to-end policy: the whole source, starting where the track's last
/// clip ends. Duration is the source's reported duration rescaled (floor) from
/// its own clock to the project clock; the clip starts at the source's first PTS.
fn append_clip(
    project: &Project,
    source: &MediaSummary,
    track_id: TrackId,
) -> Result<Clip, EngineError> {
    let track = project
        .tracks()
        .iter()
        .find(|track| track.id == track_id)
        .ok_or_else(|| EngineError::new(ErrorCode::NotFound, "That track no longer exists."))?;
    if track.kind != TrackKind::Video {
        return Err(EngineError::new(
            ErrorCode::InvalidInput,
            "Video sources can only be added to a video track.",
        ));
    }
    let info = source.metadata.video().ok_or_else(|| {
        EngineError::new(ErrorCode::InvalidInput, "This source has no video stream.")
    })?;
    let ticks = info
        .duration_ticks
        .filter(|ticks| *ticks > 0)
        .ok_or_else(|| {
            EngineError::new(
                ErrorCode::InvalidInput,
                "This source has no known duration, so it cannot be placed yet.",
            )
        })?;
    let overflow = || {
        EngineError::new(
            ErrorCode::InvalidInput,
            "Clip timing overflows the timeline clock.",
        )
    };
    let duration = SourceTime {
        ticks,
        timebase: info.timebase,
    }
    .rescale(project.timebase(), Rounding::Floor)
    .map_err(|_| overflow())?;
    let duration = u64::try_from(duration.ticks)
        .ok()
        .and_then(|ticks| TimelineDuration::new(ticks).ok())
        .ok_or_else(|| {
            EngineError::new(
                ErrorCode::InvalidInput,
                "This source is shorter than one timeline tick.",
            )
        })?;
    let start = project.track_end(track_id).map_err(domain_error)?;
    start.checked_add(duration).map_err(|_| overflow())?;
    Ok(Clip {
        stream: cutcap_core::ClipStream::Video,
        link: None,
        id: ClipId::new(),
        media_id: source.id,
        track_id,
        start,
        duration,
        source_in: SourceTime {
            ticks: info.start_pts,
            timebase: info.timebase,
        },
    })
}

fn domain_error(error: ProjectError) -> EngineError {
    let code = match error {
        ProjectError::MissingReference => ErrorCode::NotFound,
        _ => ErrorCode::InvalidInput,
    };
    EngineError::new(code, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        engine: &mut Engine,
        project_id: Option<ProjectId>,
        command: Command,
    ) -> EngineEvent {
        engine.handle(CommandEnvelope {
            request_id: RequestId::new(),
            project_id,
            command,
        })
    }

    fn create(engine: &mut Engine) -> ProjectSnapshot {
        match request(
            engine,
            None,
            Command::NewProject {
                name: "Test".into(),
            },
        ) {
            EngineEvent::Completed { snapshot, .. } => snapshot,
            event => panic!("unexpected {event:?}"),
        }
    }

    #[test]
    fn create_add_and_select_round_trip() {
        let mut engine = Engine::default();
        let initial = create(&mut engine);
        assert_eq!(initial.tracks.len(), 2);
        let event = request(
            &mut engine,
            Some(initial.id),
            Command::AddTrack {
                kind: TrackKind::Video,
            },
        );
        let EngineEvent::Completed { snapshot, .. } = event else {
            panic!("add failed")
        };
        assert_eq!(snapshot.tracks.len(), 3);
        assert_eq!(
            snapshot.selection,
            SelectionState::Track(snapshot.tracks[2].id)
        );
        let event = request(
            &mut engine,
            Some(initial.id),
            Command::Select {
                selection: SelectionState::None,
            },
        );
        assert!(
            matches!(event, EngineEvent::Completed { snapshot, .. } if snapshot.selection == SelectionState::None)
        );
    }

    #[test]
    fn stale_and_unsupported_commands_never_modify_project() {
        let mut engine = Engine::default();
        let initial = create(&mut engine);
        let event = request(
            &mut engine,
            Some(ProjectId::new()),
            Command::AddTrack {
                kind: TrackKind::Audio,
            },
        );
        assert!(matches!(
            event,
            EngineEvent::Failed {
                error: EngineError {
                    code: ErrorCode::StaleProject,
                    ..
                },
                ..
            }
        ));
        let event = request(
            &mut engine,
            Some(initial.id),
            Command::ImportMedia {
                paths: vec!["missing.mp4".into()],
            },
        );
        assert!(matches!(
            event,
            EngineEvent::Failed {
                error: EngineError {
                    code: ErrorCode::Unsupported,
                    ..
                },
                ..
            }
        ));
        assert_eq!(engine.snapshot().unwrap(), initial);
        let event = request(
            &mut engine,
            Some(initial.id),
            Command::Select {
                selection: SelectionState::Clip(ClipId::new()),
            },
        );
        assert!(matches!(event, EngineEvent::Failed { .. }));
        assert_eq!(engine.snapshot().unwrap(), initial);
    }

    #[test]
    fn invalid_new_project_preserves_current_project_and_request_correlation() {
        let mut engine = Engine::default();
        let initial = create(&mut engine);
        let request_id = RequestId::new();
        let event = engine.handle(CommandEnvelope {
            request_id,
            project_id: None,
            command: Command::NewProject { name: "".into() },
        });
        assert!(matches!(event, EngineEvent::Failed { request_id: id, .. } if id == request_id));
        assert_eq!(engine.snapshot().unwrap(), initial);
    }

    #[test]
    fn real_asset_registration_is_atomic_scoped_and_selection_is_session_state() {
        let mut engine = Engine::default();
        let initial = create(&mut engine);
        let metadata=cutcap_media::parse_probe(br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"1/1000","start_pts":0,"duration_ts":3000}],"format":{"format_name":"matroska"}}"#).unwrap();
        let media = MediaSummary {
            id: MediaId::new(),
            path: "C:/source video.mkv".into(),
            metadata: metadata.into(),
        };
        let register = |asset: MediaSummary| Command::RegisterMedia {
            media: Box::new(asset),
            fingerprint: None,
        };
        assert!(matches!(
            request(&mut engine, Some(ProjectId::new()), register(media.clone())),
            EngineEvent::Failed {
                error: EngineError {
                    code: ErrorCode::StaleProject,
                    ..
                },
                ..
            }
        ));
        assert_eq!(engine.snapshot().unwrap(), initial);
        let mut invalid = media.clone();
        invalid.metadata.video_mut().unwrap().width = 0;
        assert!(matches!(
            request(&mut engine, Some(initial.id), register(invalid)),
            EngineEvent::Failed { .. }
        ));
        assert_eq!(engine.snapshot().unwrap(), initial);
        assert!(matches!(
            request(&mut engine, Some(initial.id), register(media.clone())),
            EngineEvent::Completed { .. }
        ));
        let registered = engine.snapshot().unwrap();
        assert_eq!(registered.media_count, 1);
        assert_eq!(registered.media, vec![media.clone()]);
        assert_eq!(registered.source_media, Some(media.id));
        assert_eq!(registered.selection, SelectionState::None);
        let mut duplicate = media.clone();
        duplicate.id = MediaId::new();
        assert!(matches!(
            request(&mut engine, Some(initial.id), register(duplicate)),
            EngineEvent::Failed { .. }
        ));
        assert_eq!(engine.snapshot().unwrap(), registered);
        assert!(matches!(
            request(
                &mut engine,
                Some(initial.id),
                Command::SelectMedia {
                    media_id: MediaId::new()
                }
            ),
            EngineEvent::Failed { .. }
        ));
        assert_eq!(engine.snapshot().unwrap(), registered);
        let next = create(&mut engine);
        assert!(next.media.is_empty());
        assert_eq!(next.source_media, None);
        assert!(matches!(
            request(&mut engine, Some(initial.id), register(media)),
            EngineEvent::Failed { .. }
        ));
        assert_eq!(engine.snapshot().unwrap(), next);
    }

    #[test]
    fn edits_before_save_completion_remain_dirty_and_disk_keeps_consistent_revision() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("revision.cutcap");
        let mut engine = Engine::default();
        let initial = create(&mut engine);
        let saving = request(
            &mut engine,
            Some(initial.id),
            Command::SaveProject {
                path: Some(path.clone()),
            },
        );
        assert!(matches!(saving, EngineEvent::Updated { .. }));
        assert!(matches!(
            request(
                &mut engine,
                Some(initial.id),
                Command::AddTrack {
                    kind: TrackKind::Video
                }
            ),
            EngineEvent::Completed { .. }
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let completed = loop {
            if let Some(event) = engine.poll_file() {
                break event;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let EngineEvent::Completed { snapshot, .. } = completed else {
            panic!("save failed")
        };
        assert!(snapshot.document.dirty);
        assert_eq!(
            snapshot.document.saved_revision,
            Some(initial.document.revision)
        );
        assert_eq!(snapshot.document.revision, initial.document.revision + 1);
        assert_eq!(snapshot.tracks.len(), 3);
        let disk = crate::persistence::open(&path, &cutcap_media::Cancellation::default()).unwrap();
        assert_eq!(disk.project.tracks().len(), 2);
        assert_eq!(disk.project.id(), initial.id);
    }
}

#[cfg(test)]
#[path = "editing_tests.rs"]
mod editing_tests;

#[cfg(test)]
#[path = "evaluation_measurements.rs"]
mod evaluation_measurements;

#[cfg(test)]
#[path = "clip_edit_tests.rs"]
mod clip_edit_tests;

#[cfg(test)]
#[path = "linked_edit_tests.rs"]
mod linked_edit_tests;

#[cfg(test)]
#[path = "audio_edit_tests.rs"]
mod audio_edit_tests;

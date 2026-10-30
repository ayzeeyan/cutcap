//! The lightweight command owner. Future media jobs must not run in this loop.
use cutcap_contracts::*;
use cutcap_core::{Project, ProjectError, Track};

#[derive(Default)]
pub struct Engine {
    project: Option<Project>,
    selection: SelectionState,
}

impl Engine {
    pub fn handle(&mut self, envelope: CommandEnvelope) -> EngineEvent {
        let request_id = envelope.request_id;
        match self.execute(envelope) {
            Ok(snapshot) => EngineEvent::Completed {
                request_id,
                snapshot,
            },
            Err(error) => EngineEvent::Failed { request_id, error },
        }
    }

    fn execute(&mut self, envelope: CommandEnvelope) -> Result<ProjectSnapshot, EngineError> {
        if let Command::NewProject { name } = envelope.command {
            let mut project =
                Project::new(name, Timebase::DEFAULT_TIMELINE).map_err(domain_error)?;
            for (name, kind) in [("Video 1", TrackKind::Video), ("Audio 1", TrackKind::Audio)] {
                project
                    .add_track(Track {
                        id: TrackId::new(),
                        name: name.into(),
                        kind,
                    })
                    .map_err(domain_error)?;
            }
            self.project = Some(project);
            self.selection = SelectionState::None;
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
            match envelope.command {
                Command::GetSnapshot => {}
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
                        })
                        .map_err(domain_error)?;
                    self.selection = SelectionState::Track(id);
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
                        "This operation is unavailable in Phase 0.",
                    ));
                }
            }
        }
        self.snapshot()
    }

    fn snapshot(&self) -> Result<ProjectSnapshot, EngineError> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| EngineError::new(ErrorCode::NotFound, "No project is open."))?;
        Ok(ProjectSnapshot {
            id: project.id(),
            name: project.name().into(),
            timebase: project.timebase(),
            tracks: project
                .tracks()
                .iter()
                .map(|track| TrackSummary {
                    id: track.id,
                    name: track.name.clone(),
                    kind: track.kind,
                })
                .collect(),
            media_count: project.media().len(),
            clip_count: project.clips().len(),
            selection: self.selection,
        })
    }
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
}

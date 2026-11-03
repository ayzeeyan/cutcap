use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

use cutcap_contracts::*;
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::{AppWindow, MediaRow, TrackRow, bridge::EngineBridge, media_ui::MediaUi};

#[derive(Default)]
struct ViewState {
    snapshot: Option<ProjectSnapshot>,
    pending: HashSet<RequestId>,
    desired_media: Option<MediaId>,
    waiting_project: Option<RequestId>,
    registering: Option<RequestId>,
    inspecting_media: bool,
}

fn submit(
    window: &AppWindow,
    bridge: &EngineBridge,
    state: &RefCell<ViewState>,
    command: Command,
) -> Option<RequestId> {
    let request_id = RequestId::new();
    let project_id = state.borrow().snapshot.as_ref().map(|snapshot| snapshot.id);
    match bridge.submit(CommandEnvelope {
        request_id,
        project_id,
        command,
    }) {
        Ok(()) => {
            state.borrow_mut().pending.insert(request_id);
            window.set_pending_count(state.borrow().pending.len() as i32);
            Some(request_id)
        }
        Err(error) => {
            tracing::warn!(code = ?error.code, "{error}");
            window.set_status_text(error.to_string().into());
            window.set_status_error(true);
            None
        }
    }
}

/// All closures, models, the timer and weak upgrades live on the Slint thread.
pub struct Coordinator {
    pub timer: Timer,
    pub media: Rc<RefCell<MediaUi>>,
}

pub fn connect(
    window: &AppWindow,
    bridge: Rc<EngineBridge>,
) -> Result<Coordinator, std::io::Error> {
    let state = Rc::new(RefCell::new(ViewState::default()));
    let media = Rc::new(RefCell::new(MediaUi::new(window)?));
    let weak = window.as_weak();
    let owner = bridge.clone();
    let view = state.clone();
    let source = media.clone();
    window.on_new_project(move || {
        if let Some(window) = weak.upgrade()
            && let Some(request_id) = submit(
                &window,
                &owner,
                &view,
                Command::NewProject {
                    name: "Untitled project".into(),
                },
            )
        {
            let mut state = view.borrow_mut();
            state.waiting_project = Some(request_id);
            state.desired_media = None;
            state.inspecting_media = false;
            source.borrow_mut().reset(&window);
            window.set_engine_ready(false);
        }
    });
    let weak = window.as_weak();
    let owner = bridge.clone();
    let view = state.clone();
    window.on_add_track(move |audio| {
        if let Some(window) = weak.upgrade() {
            submit(
                &window,
                &owner,
                &view,
                Command::AddTrack {
                    kind: if audio {
                        TrackKind::Audio
                    } else {
                        TrackKind::Video
                    },
                },
            );
        }
    });
    let weak = window.as_weak();
    let owner = bridge.clone();
    let view = state.clone();
    window.on_select_track(move |index| {
        let selected = usize::try_from(index).ok().and_then(|index| {
            view.borrow()
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.tracks.get(index).map(|track| track.id))
        });
        if let (Some(window), Some(id)) = (weak.upgrade(), selected) {
            view.borrow_mut().inspecting_media = false;
            submit(
                &window,
                &owner,
                &view,
                Command::Select {
                    selection: SelectionState::Track(id),
                },
            );
        }
    });
    let weak = window.as_weak();
    let view = state.clone();
    let source = media.clone();
    window.on_import_media(move || {
        if let Some(window) = weak.upgrade() {
            let project = view.borrow().snapshot.as_ref().map(|s| s.id);
            if let Some(project) = project {
                source.borrow_mut().open_dialog(&window, project);
            }
        }
    });
    let weak = window.as_weak();
    let source = media.clone();
    window.on_cancel_import(move || {
        if let Some(window) = weak.upgrade() {
            source.borrow_mut().cancel_import(&window);
        }
    });
    let weak = window.as_weak();
    let source = media.clone();
    window.on_cancel_preview(move || {
        if let Some(window) = weak.upgrade() {
            source.borrow_mut().cancel_preview(&window);
        }
    });
    let weak = window.as_weak();
    let source = media.clone();
    window.on_seek_source(move |ms| {
        if let Some(window) = weak.upgrade() {
            source.borrow_mut().seek(&window, ms);
        }
    });
    let weak = window.as_weak();
    let source = media.clone();
    window.on_seek_text(move |text| {
        if let Some(window) = weak.upgrade() {
            if let Ok(ms) = text.trim().parse::<i32>() {
                source.borrow_mut().seek(&window, ms);
            } else {
                window.set_status_text("Enter a whole source time in milliseconds.".into());
                window.set_status_error(true);
            }
        }
    });
    let weak = window.as_weak();
    let source = media.clone();
    let view = state.clone();
    let owner = bridge.clone();
    window.on_select_media(move |index| {
        if let Some(window) = weak.upgrade() {
            let id = usize::try_from(index).ok().and_then(|i| {
                view.borrow()
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.media.get(i).map(|m| m.id))
            });
            if let Some(id) = id
                && submit(
                    &window,
                    &owner,
                    &view,
                    Command::SelectMedia { media_id: id },
                )
                .is_some()
            {
                view.borrow_mut().desired_media = Some(id);
                view.borrow_mut().inspecting_media = true;
                source.borrow_mut().pending_selection(&window);
            }
        }
    });
    let weak = window.as_weak();
    let owner = bridge.clone();
    let view = state.clone();
    window.on_clear_selection(move || {
        if let Some(window) = weak.upgrade() {
            submit(
                &window,
                &owner,
                &view,
                Command::Select {
                    selection: SelectionState::None,
                },
            );
        }
    });

    let timer = Timer::default();
    let weak = window.as_weak();
    let owner = bridge.clone();
    let view = state.clone();
    let source = media.clone();
    timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        if let Err(error) = owner.drain(|event| {
            match event {
                EngineEvent::Completed {
                    request_id,
                    snapshot,
                } => {
                    if view.borrow().snapshot.is_none() {
                        tracing::info!(
                            scale_factor = window.window().scale_factor(),
                            physical_size = ?window.window().size(), "native window ready"
                        );
                    }
                    view.borrow_mut().pending.remove(&request_id);
                    if view
                        .borrow()
                        .waiting_project
                        .is_some_and(|waiting| waiting != request_id)
                    {
                        window.set_pending_count(view.borrow().pending.len() as i32);
                        return;
                    }
                    if view.borrow().waiting_project == Some(request_id) {
                        view.borrow_mut().waiting_project = None;
                    }
                    let project_changed = view
                        .borrow()
                        .snapshot
                        .as_ref()
                        .is_some_and(|previous| previous.id != snapshot.id);
                    apply_snapshot(&window, &snapshot, view.borrow().inspecting_media);
                    if snapshot.source_media == view.borrow().desired_media
                        && let Some(selected) = snapshot
                            .media
                            .iter()
                            .find(|m| Some(m.id) == snapshot.source_media)
                    {
                        source
                            .borrow_mut()
                            .select(&window, snapshot.id, selected.clone());
                    }
                    if project_changed {
                        window.invoke_focus_workspace();
                    }
                    tracing::info!(
                        project = %snapshot.id, tracks = snapshot.tracks.len(),
                        selection = ?snapshot.selection, "command completed"
                    );
                    view.borrow_mut().snapshot = Some(snapshot);
                    window.set_status_text("Ready · project is in memory".into());
                    window.set_status_error(false);
                    if view.borrow().registering == Some(request_id) {
                        view.borrow_mut().registering = None;
                        window
                            .set_media_status("Video imported. Select an entry to preview.".into());
                        window.set_media_error(false);
                    }
                }
                EngineEvent::Failed { request_id, error } => {
                    view.borrow_mut().pending.remove(&request_id);
                    tracing::warn!(code = ?error.code, "command failed: {error}");
                    window.set_status_text(error.to_string().into());
                    window.set_status_error(true);
                    if view.borrow().registering == Some(request_id) {
                        view.borrow_mut().registering = None;
                        source.borrow().import_error(&window, &error.to_string());
                    }
                    if view.borrow().waiting_project == Some(request_id) {
                        view.borrow_mut().waiting_project = None;
                        window.set_engine_ready(true);
                    }
                }
            }
            window.set_pending_count(view.borrow().pending.len() as i32);
        }) {
            tracing::error!("{error}");
            view.borrow_mut().pending.clear();
            window.set_pending_count(0);
            window.set_engine_ready(false);
            window.set_status_text(error.to_string().into());
            window.set_status_error(true);
        }
        let current_project = if view.borrow().waiting_project.is_none() {
            view.borrow().snapshot.as_ref().map(|s| s.id)
        } else {
            None
        };
        // End the mutable MediaUi borrow before registration/error handling.
        let registration = source.borrow_mut().poll(&window, current_project);
        if let Some(registration) = registration
            && current_project == Some(registration.project_id)
        {
            let id = registration.media.id;
            if let Some(request) = submit(
                &window,
                &owner,
                &view,
                Command::RegisterMedia {
                    media: Box::new(registration.media),
                },
            ) {
                let mut state = view.borrow_mut();
                state.registering = Some(request);
                state.desired_media = Some(id);
                state.inspecting_media = true;
            } else {
                source
                    .borrow()
                    .import_error(&window, "The project queue is busy. Try importing again.");
            }
        }
        source.borrow_mut().benchmark_tick(&window, current_project);
    });
    submit(
        window,
        &bridge,
        &state,
        Command::NewProject {
            name: "Untitled project".into(),
        },
    );
    Ok(Coordinator { timer, media })
}

fn apply_snapshot(window: &AppWindow, snapshot: &ProjectSnapshot, inspecting_media: bool) {
    window.set_engine_ready(true);
    window.set_project_name(snapshot.name.clone().into());
    window.set_media_count(snapshot.media_count as i32);
    let assets: Vec<MediaRow> = snapshot
        .media
        .iter()
        .map(|asset| MediaRow {
            name: asset
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
                .into(),
            detail: format!(
                "{} × {} · {}\n{} · {}",
                asset.metadata.width,
                asset.metadata.height,
                asset.metadata.codec,
                asset
                    .metadata
                    .duration_ticks
                    .map(|ticks| format!(
                        "{}{}",
                        if asset.metadata.duration_is_estimate {
                            "~"
                        } else {
                            ""
                        },
                        crate::preview::format_time(SourceTime {
                            ticks,
                            timebase: asset.metadata.timebase
                        })
                    ))
                    .unwrap_or_else(|| "Duration unknown".into()),
                if asset.metadata.audio_streams.is_empty() {
                    "No audio"
                } else {
                    "Audio present"
                }
            )
            .into(),
            selected: snapshot.source_media == Some(asset.id),
        })
        .collect();
    let existing = window.get_media_entries();
    if let Some(model) = existing.as_any().downcast_ref::<VecModel<MediaRow>>() {
        for (index, row) in assets.iter().enumerate() {
            if index < model.row_count() {
                if model.row_data(index).as_ref() != Some(row) {
                    model.set_row_data(index, row.clone());
                }
            } else {
                model.push(row.clone());
            }
        }
        while model.row_count() > assets.len() {
            model.remove(model.row_count() - 1);
        }
    } else {
        window.set_media_entries(ModelRc::new(VecModel::from(assets)));
    }
    let rows: Vec<TrackRow> = snapshot
        .tracks
        .iter()
        .map(|track| TrackRow {
            name: track.name.clone().into(),
            kind: match track.kind {
                TrackKind::Video => "VIDEO",
                TrackKind::Audio => "AUDIO",
            }
            .into(),
            selected: snapshot.selection == SelectionState::Track(track.id),
        })
        .collect();
    // Preserve repeater item identity: replacing the model destroys a focused
    // track button and prevents subsequent keyboard shortcuts from arriving.
    let existing = window.get_tracks();
    if let Some(model) = existing.as_any().downcast_ref::<VecModel<TrackRow>>() {
        for (index, row) in rows.iter().enumerate() {
            if index < model.row_count() {
                if model.row_data(index).as_ref() != Some(row) {
                    model.set_row_data(index, row.clone());
                }
            } else {
                model.push(row.clone());
            }
        }
        while model.row_count() > rows.len() {
            model.remove(model.row_count() - 1);
        }
    } else {
        window.set_tracks(ModelRc::new(VecModel::from(rows)));
    }
    let selected = snapshot
        .tracks
        .iter()
        .find(|track| snapshot.selection == SelectionState::Track(track.id));
    window.set_selection_title(
        selected
            .map_or("Project", |track| track.name.as_str())
            .into(),
    );
    window.set_selection_detail(
        selected
            .map_or("Project overview", |track| match track.kind {
                TrackKind::Video => "Empty video track",
                TrackKind::Audio => "Empty audio track",
            })
            .into(),
    );
    window.set_has_selection(selected.is_some());
    let source = snapshot
        .media
        .iter()
        .find(|m| Some(m.id) == snapshot.source_media)
        .filter(|_| inspecting_media);
    window.set_inspecting_media(source.is_some());
    if let Some(asset) = source {
        let info = &asset.metadata;
        let rate = |r: Option<Ratio>| {
            r.map(|r| format!("{}/{}", r.numerator, r.denominator))
                .unwrap_or_else(|| "Unknown".into())
        };
        window.set_selection_title(
            asset
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
                .into(),
        );
        window.set_selection_detail("Source video · read only".into());
        window.set_media_details(format!("Codec: {}\nCoded size: {} × {}\nPixel format: {}\nDisplay aspect: {}\nSample aspect: {}\nRotation: {}° CCW\nAverage rate: {} fps\nNominal rate: {} fps\nTimebase: {}/{} s\nStart PTS: {}\nContainer: {}\nVideo stream: {} ({})\nAudio streams: {:?}\n{}", info.codec,info.width,info.height,info.pixel_format.as_deref().unwrap_or("Unknown"),rate(info.display_aspect),rate(info.sample_aspect),info.rotation,rate(info.average_rate),rate(info.nominal_rate),info.timebase.numerator(),info.timebase.denominator(),info.start_pts,info.container,info.stream_index,info.stream_id.as_deref().unwrap_or("no container ID"),info.audio_streams,if info.duration_ticks.is_none(){"Duration unknown."}else if info.duration_is_estimate{"Duration uses a container estimate."}else{"Duration reported by video stream."}).into());
    }
    window.set_timebase_label(
        format!(
            "{}/{} s per tick",
            snapshot.timebase.numerator(),
            snapshot.timebase.denominator()
        )
        .into(),
    );
}

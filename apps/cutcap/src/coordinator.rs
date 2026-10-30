use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

use cutcap_contracts::*;
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::{AppWindow, TrackRow, bridge::EngineBridge};

#[derive(Default)]
struct ViewState {
    snapshot: Option<ProjectSnapshot>,
    pending: HashSet<RequestId>,
}

fn submit(window: &AppWindow, bridge: &EngineBridge, state: &RefCell<ViewState>, command: Command) {
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
        }
        Err(error) => {
            tracing::warn!(code = ?error.code, "{error}");
            window.set_status_text(error.to_string().into());
            window.set_status_error(true);
        }
    }
}

/// All closures, models, the timer and weak upgrades live on the Slint thread.
pub fn connect(window: &AppWindow, bridge: Rc<EngineBridge>) -> Timer {
    let state = Rc::new(RefCell::new(ViewState::default()));
    let weak = window.as_weak();
    let owner = bridge.clone();
    let view = state.clone();
    window.on_new_project(move || {
        if let Some(window) = weak.upgrade() {
            submit(
                &window,
                &owner,
                &view,
                Command::NewProject {
                    name: "Untitled project".into(),
                },
            );
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
    timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
        let Some(window) = weak.upgrade() else { return; };
        if let Err(error) = owner.drain(|event| {
            match event {
                EngineEvent::Completed { request_id, snapshot } => {
                    if view.borrow().snapshot.is_none() {
                        tracing::info!(scale_factor = window.window().scale_factor(), physical_size = ?window.window().size(), "native window ready");
                    }
                    view.borrow_mut().pending.remove(&request_id);
                    let project_changed = view.borrow().snapshot.as_ref().is_some_and(|previous| previous.id != snapshot.id);
                    apply_snapshot(&window, &snapshot);
                    if project_changed { window.invoke_focus_workspace(); }
                    tracing::info!(project = %snapshot.id, tracks = snapshot.tracks.len(), selection = ?snapshot.selection, "command completed");
                    view.borrow_mut().snapshot = Some(snapshot);
                    window.set_status_text("Ready · project is in memory".into());
                    window.set_status_error(false);
                },
                EngineEvent::Failed { request_id, error } => {
                    view.borrow_mut().pending.remove(&request_id);
                    tracing::warn!(code = ?error.code, "command failed: {error}");
                    window.set_status_text(error.to_string().into());
                    window.set_status_error(true);
                },
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
    });
    submit(
        window,
        &bridge,
        &state,
        Command::NewProject {
            name: "Untitled project".into(),
        },
    );
    timer
}

fn apply_snapshot(window: &AppWindow, snapshot: &ProjectSnapshot) {
    window.set_engine_ready(true);
    window.set_project_name(snapshot.name.clone().into());
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
    window.set_timebase_label(
        format!(
            "{}/{} s per tick",
            snapshot.timebase.numerator(),
            snapshot.timebase.denominator()
        )
        .into(),
    );
}

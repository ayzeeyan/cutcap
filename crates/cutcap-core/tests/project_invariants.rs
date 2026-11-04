use cutcap_contracts::*;
use cutcap_core::*;

fn fixture() -> (Project, Clip) {
    let mut project = Project::new("Fixture", Timebase::DEFAULT_TIMELINE).unwrap();
    let track_id = TrackId::new();
    let media_id = MediaId::new();
    project
        .add_track(Track {
            id: track_id,
            name: "Video 1".into(),
            kind: TrackKind::Video,
        })
        .unwrap();
    project
        .add_media(MediaAsset {
            id: media_id,
            path: "source.mp4".into(),
            kind: TrackKind::Video,
        })
        .unwrap();
    let clip = Clip {
        id: ClipId::new(),
        media_id,
        track_id,
        start: TimelineTime::ZERO,
        duration: TimelineDuration::new(720_000).unwrap(),
        source_in: SourceTime {
            ticks: -9_000,
            timebase: Timebase::new(1, 90_000).unwrap(),
        },
    };
    (project, clip)
}

#[test]
fn valid_project_round_trips_with_stable_ids_and_source_clock() {
    let (mut project, clip) = fixture();
    project.insert_clip(clip).unwrap();
    let json = serde_json::to_string(&project).unwrap();
    let loaded: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(project, loaded);
    assert!(!json.contains("pixels"));
    assert_eq!(loaded.clips()[0].source_in.ticks, -9_000);
}

#[test]
fn failed_insertions_are_atomic() {
    let (mut project, mut clip) = fixture();
    let original = project.clone();
    clip.track_id = TrackId::new();
    assert_eq!(
        project.insert_clip(clip.clone()),
        Err(ProjectError::MissingReference)
    );
    assert_eq!(project, original);
    clip.track_id = project.tracks()[0].id;
    clip.start = TimelineTime(-1);
    assert_eq!(
        project.insert_clip(clip.clone()),
        Err(ProjectError::NegativeStart)
    );
    clip.start = TimelineTime(i64::MAX);
    assert_eq!(
        project.insert_clip(clip),
        Err(ProjectError::Time(TimeError::Overflow))
    );
    assert_eq!(project, original);
}

#[test]
fn same_track_overlap_rejected_but_adjacent_clips_allowed() {
    let (mut project, clip) = fixture();
    project.insert_clip(clip.clone()).unwrap();
    let mut next = clip.clone();
    next.id = ClipId::new();
    next.start = TimelineTime(1);
    assert_eq!(
        project.insert_clip(next.clone()),
        Err(ProjectError::Overlap)
    );
    next.start = clip.end().unwrap();
    project.insert_clip(next).unwrap();
    project.validate().unwrap();
}

#[test]
fn malformed_graphs_and_future_schemas_are_rejected_on_load() {
    let (mut project, clip) = fixture();
    project.insert_clip(clip).unwrap();
    let value = serde_json::to_value(&project).unwrap();
    for replacement in [0, 2, 999] {
        let mut bad = value.clone();
        bad["schema_version"] = replacement.into();
        assert!(serde_json::from_value::<Project>(bad).is_err());
    }
    let mut bad = value.clone();
    bad["tracks"] = serde_json::json!([]);
    assert!(serde_json::from_value::<Project>(bad).is_err());
    let mut bad = value.clone();
    bad["clips"][0]["duration"] = 0.into();
    assert!(serde_json::from_value::<Project>(bad).is_err());
    let mut bad = value.clone();
    bad["tracks"][0]["kind"] = "Audio".into();
    assert!(serde_json::from_value::<Project>(bad).is_err());
    let mut bad = value.clone();
    let duplicate = bad["clips"][0].clone();
    bad["clips"].as_array_mut().unwrap().push(duplicate);
    assert!(serde_json::from_value::<Project>(bad).is_err());
}

#[test]
fn selection_names_ids_and_limits_are_validated() {
    let (mut project, _) = fixture();
    assert!(
        project
            .validate_selection(SelectionState::Track(TrackId::new()))
            .is_err()
    );
    project
        .validate_selection(SelectionState::Track(project.tracks()[0].id))
        .unwrap();
    assert!(Project::new("  ", project.timebase()).is_err());
    assert!(Project::new("bad\nname", project.timebase()).is_err());
    let track = project.tracks()[0].clone();
    assert_eq!(project.add_track(track), Err(ProjectError::DuplicateId));
    while project.tracks().len() < MAX_TRACKS {
        project
            .add_track(Track {
                id: TrackId::new(),
                name: "Track".into(),
                kind: TrackKind::Audio,
            })
            .unwrap();
    }
    assert_eq!(
        project.add_track(Track {
            id: TrackId::new(),
            name: "Track".into(),
            kind: TrackKind::Video
        }),
        Err(ProjectError::Limit("tracks"))
    );
}

#[test]
fn loading_rejects_missing_media_overlap_and_unknown_fields() {
    let (mut project, clip) = fixture();
    project.insert_clip(clip.clone()).unwrap();
    let original = serde_json::to_value(&project).unwrap();
    let mut missing_media = original.clone();
    missing_media["media"] = serde_json::json!([]);
    assert!(serde_json::from_value::<Project>(missing_media).is_err());
    let mut overlap = original.clone();
    let mut other = clip;
    other.id = ClipId::new();
    other.start = TimelineTime(1);
    overlap["clips"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(other).unwrap());
    assert!(serde_json::from_value::<Project>(overlap).is_err());
    let mut unknown = original;
    unknown["decoded_pixels"] = serde_json::json!([]);
    assert!(serde_json::from_value::<Project>(unknown).is_err());
}

#[test]
fn removal_restoration_and_append_point_preserve_identity_and_order() {
    let (mut project, first) = fixture();
    let track = first.track_id;
    assert_eq!(project.track_end(track), Ok(TimelineTime::ZERO));
    project.insert_clip(first.clone()).unwrap();
    let mut second = first.clone();
    second.id = ClipId::new();
    second.start = project.track_end(track).unwrap();
    assert_eq!(second.start, TimelineTime(720_000));
    project.insert_clip(second.clone()).unwrap();
    assert_eq!(project.track_end(track), Ok(TimelineTime(1_440_000)));
    let before = project.clone();

    let (index, removed) = project.remove_clip(first.id).unwrap();
    assert_eq!((index, &removed), (0, &first));
    assert_eq!(project.clips().len(), 1);
    assert_eq!(
        project.media().len(),
        1,
        "removing a clip keeps its media asset"
    );
    assert_eq!(
        project.remove_clip(first.id),
        Err(ProjectError::MissingReference)
    );

    project.insert_clip_at(index, removed).unwrap();
    assert_eq!(project, before, "exact identity, timing and order restored");
    assert_eq!(project.clip(second.id), Some(&second));
    assert_eq!(
        project.track_end(TrackId::new()),
        Err(ProjectError::MissingReference)
    );
}

#[test]
fn restoring_at_an_index_still_validates_overlap_and_kinds() {
    let (mut project, clip) = fixture();
    project.insert_clip(clip.clone()).unwrap();
    let before = project.clone();
    let mut overlapping = clip.clone();
    overlapping.id = ClipId::new();
    overlapping.start = TimelineTime(1);
    assert_eq!(
        project.insert_clip_at(0, overlapping),
        Err(ProjectError::Overlap)
    );
    assert_eq!(project, before);
    let mut late = clip;
    late.id = ClipId::new();
    late.start = TimelineTime(i64::MAX - 10);
    assert!(matches!(
        project.insert_clip_at(5, late),
        Err(ProjectError::Time(_))
    ));
    assert_eq!(project, before);
}

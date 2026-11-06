use cutcap_contracts::*;
use cutcap_core::*;

fn fixture() -> (Project, TrackId, TrackId, MediaId) {
    let mut p = Project::new("Evaluation", Timebase::new(1, 1000).unwrap()).unwrap();
    let video = TrackId::new();
    let audio = TrackId::new();
    let media = MediaId::new();
    p.add_track(Track {
        id: video,
        name: "Video".into(),
        kind: TrackKind::Video,
    })
    .unwrap();
    p.add_track(Track {
        id: audio,
        name: "Audio".into(),
        kind: TrackKind::Audio,
    })
    .unwrap();
    p.add_media(MediaAsset {
        id: media,
        path: "owned.mkv".into(),
        kind: TrackKind::Video,
    })
    .unwrap();
    (p, video, audio, media)
}
fn clip(track: TrackId, media: MediaId, start: i64, duration: u64) -> Clip {
    Clip {
        id: ClipId::new(),
        track_id: track,
        media_id: media,
        start: TimelineTime(start),
        duration: TimelineDuration::new(duration).unwrap(),
        source_in: SourceTime {
            ticks: 5000,
            timebase: Timebase::new(1, 1000).unwrap(),
        },
    }
}
#[test]
fn half_open_cuts_gaps_and_empty_regions_are_exact() {
    let (mut p, track, audio, media) = fixture();
    let a = clip(track, media, 100, 200);
    let b = clip(track, media, 300, 100);
    let c = clip(track, media, 600, 1);
    for c in [&c, &b, &a] {
        p.insert_clip(c.clone()).unwrap();
    }
    let index = TimelineIndex::new(&p);
    let at = |t| {
        index
            .active(p.id(), track, TimelineTime(t))
            .unwrap()
            .map(|c| c.id)
    };
    for t in [0, 99, 400, 599, 601] {
        assert_eq!(at(t), None);
    }
    for t in [100, 299] {
        assert_eq!(at(t), Some(a.id));
    }
    for t in [300, 399] {
        assert_eq!(at(t), Some(b.id));
    }
    assert_eq!(at(600), Some(c.id));
    assert_eq!(index.end(track).unwrap(), TimelineTime(601));
    assert!(matches!(
        index.active(ProjectId::new(), track, TimelineTime(0)),
        Err(EvaluationError::StaleProject)
    ));
    assert!(matches!(
        index.active(p.id(), audio, TimelineTime(0)),
        Err(EvaluationError::NotVideoTrack)
    ));
    assert!(matches!(
        index.active(p.id(), track, TimelineTime(-1)),
        Err(EvaluationError::NegativePosition)
    ));
}
#[test]
fn monitoring_uses_selected_video_clip_track_or_first_video() {
    let (mut p, first, audio, media) = fixture();
    let second = TrackId::new();
    p.add_track(Track {
        id: second,
        name: "Second".into(),
        kind: TrackKind::Video,
    })
    .unwrap();
    let c = clip(second, media, 0, 1000);
    p.insert_clip(c.clone()).unwrap();
    let index = TimelineIndex::new(&p);
    assert_eq!(index.monitor(SelectionState::None).unwrap().id, first);
    assert_eq!(
        index.monitor(SelectionState::Track(audio)).unwrap().id,
        first
    );
    assert_eq!(
        index.monitor(SelectionState::Track(second)).unwrap().id,
        second
    );
    assert_eq!(
        index.monitor(SelectionState::Clip(c.id)).unwrap().id,
        second
    );
}
#[test]
fn rational_source_mapping_keeps_offset_and_signed_pts() {
    let (_, track, _, media) = fixture();
    let mut c = clip(track, media, 720_000, 720_000);
    let project = Timebase::DEFAULT_TIMELINE;
    let stream = Timebase::new(1, 90_000).unwrap();
    let map = map_clip_source(&c, TimelineTime(1_080_000), project, stream).unwrap();
    assert_eq!(map.requested.ticks, 495_000);
    assert_eq!(map.end_exclusive.ticks, 540_000);
    assert!(
        map.accepts(SourceTime {
            ticks: 495_000,
            timebase: stream
        })
        .unwrap()
    );
    assert!(
        !map.accepts(SourceTime {
            ticks: 540_000,
            timebase: stream
        })
        .unwrap()
    );
    c.source_in.ticks = -500;
    let signed = map_clip_source(
        &c,
        TimelineTime(864_000),
        project,
        Timebase::new(1, 1000).unwrap(),
    )
    .unwrap();
    assert_eq!(signed.requested.ticks, -300);
    assert_eq!(signed.end_exclusive.ticks, 500);
    assert!(map_clip_source(&c, TimelineTime(1_440_000), project, stream).is_err());
}
#[test]
fn sum_rounds_once_and_short_source_ranges_never_leak() {
    let (_, track, _, media) = fixture();
    let mut c = clip(track, media, 0, 5);
    c.source_in = SourceTime {
        ticks: 1,
        timebase: Timebase::new(1, 5).unwrap(),
    };
    let map = map_clip_source(
        &c,
        TimelineTime(1),
        Timebase::new(1, 5).unwrap(),
        Timebase::new(1, 1).unwrap(),
    )
    .unwrap();
    assert_eq!(map.requested.ticks, 1); // ceil(.2+.2), not ceil(.2)+ceil(.2)
    assert_eq!(map.end_exclusive.ticks, 2);
    let c = clip(track, media, 0, 1);
    let map = map_clip_source(
        &c,
        TimelineTime(0),
        Timebase::DEFAULT_TIMELINE,
        Timebase::new(1, 1000).unwrap(),
    )
    .unwrap();
    assert!(
        map.accepts(SourceTime {
            ticks: 5000,
            timebase: map.requested.timebase
        })
        .unwrap()
    );
    assert!(
        !map.accepts(SourceTime {
            ticks: 5001,
            timebase: map.requested.timebase
        })
        .unwrap()
    );
    assert!(
        !map.accepts(SourceTime {
            ticks: 10_001,
            timebase: Timebase::new(1, 2000).unwrap()
        })
        .unwrap()
    );
}
#[test]
fn vfr_ceiling_outside_clip_and_arithmetic_overflow_are_explicit() {
    let (_, track, _, media) = fixture();
    let mut c = clip(track, media, 0, 600);
    c.source_in.ticks = 0;
    let clock = Timebase::new(1, 1000).unwrap();
    let map = map_clip_source(&c, TimelineTime(201), clock, clock).unwrap();
    assert!(
        !map.accepts(SourceTime {
            ticks: 700,
            timebase: clock
        })
        .unwrap()
    );
    assert!(
        !map.accepts(SourceTime {
            ticks: 200,
            timebase: clock
        })
        .unwrap()
    );
    c.source_in.ticks = i64::MAX;
    assert!(matches!(
        map_clip_source(&c, TimelineTime(1), clock, clock),
        Err(EvaluationError::Time(TimeError::Overflow))
    ));
    c.source_in = SourceTime {
        ticks: i64::MIN,
        timebase: Timebase::new(u32::MAX, u32::MAX).unwrap(),
    };
    assert!(
        map_clip_source(
            &c,
            TimelineTime(0),
            clock,
            Timebase::new(u32::MAX, u32::MAX).unwrap()
        )
        .is_ok()
    );
}

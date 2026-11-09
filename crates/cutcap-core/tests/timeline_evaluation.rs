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
        controls: Default::default(),
    })
    .unwrap();
    p.add_track(Track {
        id: audio,
        name: "Audio".into(),
        kind: TrackKind::Audio,
        controls: Default::default(),
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
        stream: cutcap_core::ClipStream::Video,
        link: None,
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
    assert_eq!(index.next(track, TimelineTime(99)).unwrap().id, a.id);
    assert_eq!(index.next(track, TimelineTime(100)).unwrap().id, b.id);
    assert_eq!(index.next(track, TimelineTime(400)).unwrap().id, c.id);
    assert!(index.next(track, TimelineTime(600)).is_none());
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
    // Phase 4B indexes audio tracks (audio transport); the empty audio track
    // has no active clip, and the video monitor never selects it.
    assert!(matches!(
        index.active(p.id(), audio, TimelineTime(0)),
        Ok(None)
    ));
    assert_eq!(
        index.audio_tracks().map(|t| t.id).collect::<Vec<_>>(),
        vec![audio]
    );
    assert_ne!(
        index.monitor(SelectionState::Track(audio)).map(|t| t.id),
        Some(audio)
    );
    assert!(matches!(
        index.active(p.id(), TrackId::new(), TimelineTime(0)),
        Err(EvaluationError::NotVideoTrack)
    ));
    assert!(matches!(
        index.active(p.id(), track, TimelineTime(-1)),
        Err(EvaluationError::NegativePosition)
    ));
}

#[test]
fn inverse_pts_mapping_uses_actual_intervals_and_single_rational_rounding() {
    let (_, track, _, media) = fixture();
    let mut c = clip(track, media, 100, 2000);
    c.source_in = SourceTime {
        ticks: -1,
        timebase: Timebase::new(1, 3).unwrap(),
    };
    let project = Timebase::new(1, 1000).unwrap();
    let pts = |ticks| SourceTime {
        ticks,
        timebase: Timebase::new(1, 1000).unwrap(),
    };
    // -333ms is the first eligible original source tick. Exact difference is 1/3ms.
    assert_eq!(
        frame_timeline_time(&c, pts(-333), project).unwrap(),
        TimelineTime(101)
    );
    assert_eq!(
        frame_timeline_time(&c, pts(200), project).unwrap(),
        TimelineTime(634)
    );
    assert_eq!(
        frame_timeline_time(&c, pts(700), project).unwrap(),
        TimelineTime(1134)
    );
    assert!(frame_timeline_time(&c, pts(-334), project).is_err());
    assert!(frame_timeline_time(&c, pts(1667), project).is_err());
    // Exact cut never belongs to the outgoing frame, including a short fractional clip.
    c.duration = TimelineDuration::new(1).unwrap();
    assert!(frame_timeline_time(&c, pts(-333), project).is_err());
    c.source_in = SourceTime {
        ticks: i64::MIN,
        timebase: project,
    };
    c.duration = TimelineDuration::new(100).unwrap();
    assert_eq!(
        frame_timeline_time(&c, pts(i64::MIN), project).unwrap(),
        TimelineTime(100)
    );
    c.source_in = SourceTime {
        ticks: i64::MAX,
        timebase: Timebase::new(u32::MAX, 1).unwrap(),
    };
    assert!(frame_timeline_time(&c, pts(i64::MAX), project).is_err());
}
#[test]
fn monitoring_uses_selected_video_clip_track_or_first_video() {
    let (mut p, first, audio, media) = fixture();
    let second = TrackId::new();
    p.add_track(Track {
        id: second,
        name: "Second".into(),
        kind: TrackKind::Video,
        controls: Default::default(),
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

#[test]
fn audio_sample_mapping_rounds_once_and_agrees_with_the_video_instant() {
    let project = Timebase::DEFAULT_TIMELINE; // 1/720000
    let video = Timebase::new(1, 15360).unwrap();
    let clip = Clip {
        id: ClipId::new(),
        media_id: MediaId::new(),
        track_id: TrackId::new(),
        start: TimelineTime(720_000),
        duration: TimelineDuration::new(7_200_000).unwrap(),
        // 0.5 s + 1/15360 s into the source, in the video clock.
        source_in: SourceTime {
            ticks: 7681,
            timebase: video,
        },
        stream: ClipStream::Audio { index: 1 },
        link: None,
    };
    // Epoch starting exactly at the clip start: offset = ceil(source_in * 48000).
    // 7681/15360 s * 48000 = 24003.125 -> 24004.
    assert_eq!(
        source_sample_offset(&clip, clip.start, project, 48_000).unwrap(),
        24_004
    );
    // Starting 0.25 s into the clip adds exactly 12000 samples.
    let origin = TimelineTime(720_000 + 180_000);
    assert_eq!(
        source_sample_offset(&clip, origin, project, 48_000).unwrap(),
        36_004
    );
    // Starting 1 s before the clip: the clip begins at output sample 48000.
    let early = TimelineTime(0);
    assert_eq!(
        samples_until(early, clip.start, project, 48_000).unwrap(),
        48_000
    );
    let offset = source_sample_offset(&clip, early, project, 48_000).unwrap();
    assert_eq!(
        offset + 48_000,
        24_004,
        "the same source sample at the clip start"
    );
    // 44.1 kHz: 0.5000651 s * 44100 = 22052.87 -> 22053.
    assert_eq!(
        source_sample_offset(&clip, clip.start, project, 44_100).unwrap(),
        22_053
    );
    // Output sample -> timeline floor: sample 1 at 48 kHz is 15 project ticks.
    assert_eq!(
        timeline_at_sample(early, 1, project, 48_000).unwrap(),
        TimelineTime(15)
    );
    assert_eq!(
        timeline_at_sample(early, 48_000, project, 48_000).unwrap(),
        TimelineTime(720_000)
    );
    // Partial-tick boundaries round up to the first sample at or after.
    assert_eq!(
        samples_until(early, TimelineTime(16), project, 48_000).unwrap(),
        2
    );
    assert_eq!(
        samples_until(TimelineTime(30), TimelineTime(0), project, 48_000).unwrap(),
        -2
    );
}

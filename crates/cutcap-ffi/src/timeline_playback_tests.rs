//! Virtual-time scheduler tests. Frame availability is explicitly controlled;
//! no assertions depend on decoder speed or sleeping for a wall-clock boundary.
use super::*;
use cutcap_core::{MediaAsset, Project, TimelineIndex, Track};
use std::{collections::BTreeMap, path::Path, time::Duration};

struct Idle;
impl MediaBackend for Idle {
    fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
        Err(MediaError::Worker)
    }
    fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
        Err(MediaError::Worker)
    }
    fn stream_interval(
        &self,
        _: &DecodeRequest,
        _: SourceTime,
        cancel: &Cancellation,
        _: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        while !cancel.is_cancelled() {
            std::thread::park_timeout(Duration::from_millis(10));
        }
        Err(MediaError::Cancelled)
    }
}

struct TerminalProducer {
    fail: bool,
}
impl MediaBackend for TerminalProducer {
    fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
        Err(MediaError::Worker)
    }
    fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
        Err(MediaError::Worker)
    }
    fn stream_interval(
        &self,
        r: &DecodeRequest,
        _: SourceTime,
        _: &Cancellation,
        emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        for ticks in [0, 500, 900] {
            emit(DecodedFrame {
                ticket: r.ticket,
                requested: r.position,
                actual: SourceTime {
                    ticks,
                    timebase: r.position.timebase,
                },
                image: RgbaFrame::new(1, 1, 4, vec![0, 0, 0, 255])?,
                decode_us: 1,
                process_id: 1,
            })?;
        }
        if self.fail {
            Err(MediaError::Invalid("terminal decoder failure".into()))
        } else {
            Ok(())
        }
    }
}

#[test]
fn terminal_errors_bypass_future_pixels_but_successful_eof_waits_for_the_queue() {
    for fail in [false, true] {
        let (plan, clips) = fixture(1000);
        let mut p = TimelinePlayback::start(Arc::new(TerminalProducer { fail })).unwrap();
        p.begin(plan, false, false, Instant::now()).unwrap();
        let mut slot = p.new_slot(clips[0].clone(), TimelineTime::ZERO, 0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        // The producer never blocks (3 frames fit the queue), so it can finish
        // entirely between a poll's frame takes and its terminal read; that
        // poll would observe the terminal result with nothing taken yet. Wait
        // for all three frames to be queued first, so the first poll takes two
        // and the terminal result is observed afterwards, in either order.
        while p.workers[0].progress().unwrap().decoded < 3 {
            assert!(
                Instant::now() < deadline,
                "producer did not queue its frames"
            );
            std::thread::park_timeout(Duration::from_millis(1));
        }
        while slot.error.is_none() && !slot.finished {
            slot.poll(&p.workers[0], p.clock.timebase).unwrap();
            assert!(
                Instant::now() < deadline,
                "terminal result must not wait for playback/copy"
            );
            std::thread::park_timeout(Duration::from_millis(1));
        }
        assert_eq!(slot.pending.len(), 2);
        if fail {
            assert!(
                slot.error
                    .as_deref()
                    .unwrap()
                    .contains("terminal decoder failure")
            );
        } else {
            assert!(!slot.eof);
            slot.pending.pop_front();
            slot.poll(&p.workers[0], p.clock.timebase).unwrap();
            assert!(slot.eof);
            assert_eq!(slot.pending.back().unwrap().frame.actual.ticks, 900);
        }
    }
}
fn fixture(second_start: i64) -> (TimelinePlan, Vec<Clip>) {
    let clock = Timebase::new(1, 1000).unwrap();
    let mut project = Project::new("Transport", clock).unwrap();
    let track = Track {
        id: TrackId::new(),
        name: "Video 1".into(),
        kind: TrackKind::Video,
    };
    project.add_track(track.clone()).unwrap();
    let mut media = BTreeMap::new();
    let mut availability = BTreeMap::new();
    let mut clips = Vec::new();
    for (start, origin) in [(0, 0), (second_start, 5000)] {
        let id = MediaId::new();
        project
            .add_media(MediaAsset {
                id,
                path: "controlled.mkv".into(),
                kind: TrackKind::Video,
            })
            .unwrap();
        let mut metadata = parse_probe(br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":2,"height":2,"time_base":"1/1000","start_pts":0,"duration_ts":3000}],"format":{"format_name":"matroska"}}"#).unwrap();
        metadata.start_pts = origin;
        media.insert(
            id,
            MediaSummary {
                id,
                path: "controlled.mkv".into(),
                metadata,
            },
        );
        availability.insert(
            id,
            MediaAvailability {
                id,
                path: "controlled.mkv".into(),
                state: Availability::Ready,
                detail: String::new(),
            },
        );
        let clip = Clip {
            id: ClipId::new(),
            track_id: track.id,
            media_id: id,
            start: TimelineTime(start),
            duration: TimelineDuration::new(1000).unwrap(),
            source_in: SourceTime {
                ticks: origin,
                timebase: clock,
            },
        };
        project.insert_clip(clip.clone()).unwrap();
        clips.push(clip);
    }
    (
        TimelinePlan {
            project: project.id(),
            generation: RequestId::new(),
            timebase: clock,
            track: Some(MonitoredTrack {
                id: track.id,
                name: track.name,
            }),
            extent: TimelineTime(second_start + 1000),
            position: TimelineTime::ZERO,
            index: Arc::new(TimelineIndex::new(&project)),
            media,
            availability,
        },
        clips,
    )
}
fn transport(plan: TimelinePlan, now: Instant) -> (TimelinePlayback, MediaHost) {
    let backend = Arc::new(Idle);
    let mut playback = TimelinePlayback::start(backend.clone()).unwrap();
    playback.begin(plan, true, false, now).unwrap();
    (playback, MediaHost::start(backend).unwrap())
}
fn seed(slot: &mut Slot, pts: &[i64], eof: bool, clock: Timebase) {
    slot.pending.clear();
    slot.eof = eof;
    for &ticks in pts {
        let actual = SourceTime {
            ticks,
            timebase: slot.bounds.requested.timebase,
        };
        slot.pending.push_back(TimedFrame {
            position: frame_timeline_time(&slot.clip, actual, clock).unwrap(),
            frame: DecodedFrame {
                ticket: slot.ticket,
                requested: slot.requested,
                actual,
                image: RgbaFrame::new(
                    1,
                    1,
                    4,
                    vec![u8::try_from(ticks.rem_euclid(255)).unwrap(), 0, 0, 255],
                )
                .unwrap(),
                decode_us: 1,
                process_id: 123,
            },
        });
    }
}
fn start_first(p: &mut TimelinePlayback, clips: &[Clip], m: &mut MediaHost, now: Instant) {
    let mut slot = p.new_slot(clips[0].clone(), TimelineTime::ZERO, 0).unwrap();
    seed(&mut slot, &[0, 500], true, p.clock.timebase);
    p.active = Some(slot);
    p.poll(now, m);
    assert_eq!(p.view.state, "playing");
    assert_eq!(p.view.buffer_stalls, 0);
}
#[test]
fn adjacent_cut_owns_incoming_frame_offset_eof_and_end_are_distinct() {
    let now = Instant::now();
    let (plan, clips) = fixture(1000);
    let (mut p, mut m) = transport(plan, now);
    start_first(&mut p, &clips, &mut m, now);
    seed(
        p.prepared.as_mut().unwrap(),
        &[5000, 5500],
        true,
        Timebase::new(1, 1000).unwrap(),
    );
    p.poll(now + Duration::from_millis(999), &mut m);
    assert_eq!(p.frame_clip, Some(clips[0].id));
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 500);
    assert_eq!(p.view.state, "playing"); // decoder EOF does not end a timeline clip early
    p.poll(now + Duration::from_millis(1000), &mut m);
    assert_eq!(p.position, TimelineTime(1000));
    assert_eq!(p.frame_clip, Some(clips[1].id));
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 5000);
    assert_eq!(p.view.handoffs, 1);
    p.copied(p.frame.as_ref().unwrap().generation);
    assert_eq!(p.view.copied_clip_id, Some(clips[1].id.to_string()));
    assert_eq!(p.view.copied_ticks, Some(1000));
    p.poll(now + Duration::from_millis(1999), &mut m);
    assert_eq!(p.view.state, "playing");
    p.poll(now + Duration::from_millis(2000), &mut m);
    assert_eq!(p.view.state, "ended");
    assert_eq!(p.position, TimelineTime(2000));
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 5500);
    assert!(!p.view.desired_playing);
}
#[test]
fn gap_is_blank_clock_advances_and_only_next_clip_is_prepared() {
    let now = Instant::now();
    let (plan, clips) = fixture(2000);
    let (mut p, mut m) = transport(plan, now);
    start_first(&mut p, &clips, &mut m, now);
    seed(
        p.prepared.as_mut().unwrap(),
        &[5000, 5500],
        true,
        p.clock.timebase,
    );
    let starts = p.view.decoder_starts;
    p.poll(now + Duration::from_secs(1), &mut m);
    assert!(p.frame.is_none());
    assert!(p.active.is_none());
    p.poll(now + Duration::from_millis(1500), &mut m);
    assert_eq!(p.position, TimelineTime(1500));
    assert_eq!(p.preview().state, "blank");
    assert_eq!(p.view.decoder_starts, starts);
    p.poll(now + Duration::from_secs(2), &mut m);
    assert_eq!(p.frame_clip, Some(clips[1].id));
    assert_eq!(p.position, TimelineTime(2000));
    assert_eq!(p.view.buffer_stalls, 0);
}
#[test]
fn late_handoff_freezes_at_exact_cut_clears_old_image_and_recovers() {
    let now = Instant::now();
    let (plan, clips) = fixture(1000);
    let (mut p, mut m) = transport(plan, now);
    start_first(&mut p, &clips, &mut m, now);
    p.poll(now + Duration::from_millis(1300), &mut m);
    assert_eq!(p.position, TimelineTime(1000));
    assert_eq!(p.view.state, "buffering");
    assert!(p.frame.is_none());
    p.poll(now + Duration::from_secs(8), &mut m);
    assert_eq!(p.position, TimelineTime(1000));
    assert_eq!(p.view.buffer_stalls, 1);
    p.pause(now + Duration::from_secs(10)).unwrap();
    p.resume(now + Duration::from_secs(10));
    assert_eq!(p.view.state, "buffering");
    p.poll(now + Duration::from_millis(10500), &mut m);
    assert_eq!(p.position, TimelineTime(1000));
    seed(
        p.active.as_mut().unwrap(),
        &[5000, 5500],
        true,
        p.clock.timebase,
    );
    p.poll(now + Duration::from_secs(9), &mut m);
    assert_eq!(p.view.state, "playing");
    assert_eq!(p.position, TimelineTime(1000));
    p.poll(now + Duration::from_millis(9250), &mut m);
    assert_eq!(p.position, TimelineTime(1250));
    assert_eq!(p.frame_clip, Some(clips[1].id));
}
#[test]
fn startup_waits_without_false_stall_and_genuine_starvation_freezes_clock() {
    let now = Instant::now();
    let (plan, _) = fixture(1000);
    let (mut p, mut m) = transport(plan, now);
    p.poll(now, &mut m);
    p.poll(now + Duration::from_secs(1), &mut m);
    assert_eq!(p.view.state, "starting");
    assert_eq!(p.position, TimelineTime::ZERO);
    assert_eq!(p.view.buffer_stalls, 0);
    seed(
        p.active.as_mut().unwrap(),
        &[0, 500],
        false,
        p.clock.timebase,
    );
    p.poll(now + Duration::from_secs(2), &mut m);
    assert_eq!(p.view.state, "playing");
    assert_eq!(p.view.buffer_stalls, 0);
    p.poll(now + Duration::from_millis(2500), &mut m);
    assert_eq!(p.view.state, "buffering");
    assert_eq!(p.position, TimelineTime(500));
    p.poll(now + Duration::from_secs(10), &mut m);
    assert_eq!(p.position, TimelineTime(500));
    assert_eq!(p.view.buffer_stalls, 1);
    seed(
        p.active.as_mut().unwrap(),
        &[750, 900],
        false,
        p.clock.timebase,
    );
    p.poll(now + Duration::from_secs(11), &mut m);
    assert_eq!(p.view.state, "playing");
    p.poll(now + Duration::from_millis(11250), &mut m);
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 750);
}
#[test]
fn pause_removes_wall_time_and_seek_preserves_intent_without_old_copy_permissions() {
    let now = Instant::now();
    let (plan, clips) = fixture(1000);
    let (mut p, mut m) = transport(plan, now);
    start_first(&mut p, &clips, &mut m, now);
    p.pause(now + Duration::from_millis(300)).unwrap();
    p.poll(now + Duration::from_secs(20), &mut m);
    assert_eq!(p.position, TimelineTime(300));
    assert_eq!(p.view.state, "paused");
    p.resume(now + Duration::from_secs(21));
    p.poll(now + Duration::from_millis(21200), &mut m);
    assert_eq!(p.position, TimelineTime(500));
    let old_generation = p.frame.as_ref().unwrap().generation;
    let epoch = p.epoch;
    let (mut plan, _) = fixture(1000);
    plan.position = TimelineTime(1200);
    let project = plan.project;
    let generation = plan.generation;
    p.begin(plan, false, true, now).unwrap();
    assert!(!p.copy_allowed(old_generation));
    assert_ne!(p.epoch, epoch);
    assert!(p.valid_scope(project, generation));
    assert!(!p.valid_scope(project, RequestId::new()));
    assert!(!p.view.desired_playing);
    p.cancel().unwrap();
    assert!(!p.valid_scope(project, generation));
}
#[test]
fn prepared_failure_is_deferred_until_entry_and_empty_tracks_end_at_zero() {
    let now = Instant::now();
    let (plan, clips) = fixture(1000);
    let (mut p, mut m) = transport(plan, now);
    start_first(&mut p, &clips, &mut m, now);
    p.prepared.as_mut().unwrap().error = Some("decoder startup failed".into());
    p.poll(now + Duration::from_millis(500), &mut m);
    assert_eq!(p.view.state, "playing");
    p.poll(now + Duration::from_secs(1), &mut m);
    assert_eq!(p.view.state, "failed");
    assert_eq!(p.position, TimelineTime(1000));
    assert!(p.frame.is_none());
    let (mut plan, _) = fixture(1000);
    plan.extent = TimelineTime::ZERO;
    p.begin(plan, true, false, now).unwrap();
    p.poll(now, &mut m);
    assert_eq!(p.view.state, "ended");
    assert_eq!(p.position, TimelineTime::ZERO);
}
#[test]
fn vfr_intervals_do_not_advance_images_by_nominal_fps_and_short_clips_can_be_blank() {
    let now = Instant::now();
    let (plan, clips) = fixture(1000);
    let (mut p, mut m) = transport(plan, now);
    let mut slot = p.new_slot(clips[0].clone(), TimelineTime::ZERO, 0).unwrap();
    seed(&mut slot, &[200, 700], true, p.clock.timebase);
    p.active = Some(slot);
    p.poll(now, &mut m);
    assert!(p.frame.is_none());
    assert_eq!(p.view.state, "playing");
    p.poll(now + Duration::from_millis(200), &mut m);
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 200);
    p.poll(now + Duration::from_millis(699), &mut m);
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 200);
    p.poll(now + Duration::from_millis(700), &mut m);
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 700);
    p.active.as_mut().unwrap().pending.clear();
    p.clear_image();
    p.poll(now + Duration::from_millis(999), &mut m);
    assert!(p.frame.is_none());
    assert_eq!(p.view.state, "playing");
}
#[test]
fn clock_floor_is_exact_overflow_checked_and_not_display_refresh_dependent() {
    let now = Instant::now();
    let mut clock = TimelineClock {
        timebase: Timebase::new(1, 720000).unwrap(),
        ..Default::default()
    };
    clock.resume(now);
    assert_eq!(
        clock.at(now + Duration::from_nanos(1388)).unwrap(),
        TimelineTime(0)
    );
    assert_eq!(
        clock.at(now + Duration::from_nanos(1389)).unwrap(),
        TimelineTime(1)
    );
    assert_eq!(
        clock.at(now + Duration::from_millis(1001)).unwrap(),
        TimelineTime(720720)
    );
    clock.freeze(TimelineTime(i64::MAX));
    clock.resume(now);
    assert!(clock.at(now + Duration::from_secs(1)).is_err());
}

#[test]
fn mixed_source_clocks_and_negative_offsets_map_to_the_same_exact_cut() {
    for (clock, origin, successor) in [
        (Timebase::new(1, 30000).unwrap(), 150000, 165000),
        (Timebase::new(1, 1000).unwrap(), -5000, -4500),
    ] {
        let now = Instant::now();
        let (mut plan, mut clips) = fixture(1000);
        clips[1].source_in = SourceTime {
            ticks: origin,
            timebase: clock,
        };
        let media = plan.media.get_mut(&clips[1].media_id).unwrap();
        media.metadata.timebase = clock;
        media.metadata.start_pts = origin;
        // Build the authoritative index with this exact source-in, keeping identities.
        let mut project = Project::new("Mixed", plan.timebase).unwrap();
        project
            .add_track(Track {
                id: clips[0].track_id,
                name: "Video".into(),
                kind: TrackKind::Video,
            })
            .unwrap();
        for c in &clips {
            project
                .add_media(MediaAsset {
                    id: c.media_id,
                    path: format!("{}.mkv", c.media_id).into(),
                    kind: TrackKind::Video,
                })
                .unwrap();
            project.insert_clip(c.clone()).unwrap();
        }
        plan.project = project.id();
        plan.index = Arc::new(TimelineIndex::new(&project));
        let (mut p, mut m) = transport(plan, now);
        start_first(&mut p, &clips, &mut m, now);
        seed(
            p.prepared.as_mut().unwrap(),
            &[origin, successor],
            true,
            p.clock.timebase,
        );
        p.pause(now + Duration::from_millis(1100)).unwrap();
        assert_eq!(p.position, TimelineTime(1000));
        assert!(p.frame.is_none());
        p.poll(now + Duration::from_secs(5), &mut m);
        assert_eq!(p.view.state, "paused");
        assert_eq!(
            p.frame.as_ref().unwrap().actual,
            SourceTime {
                ticks: origin,
                timebase: clock
            }
        );
        p.resume(now + Duration::from_secs(6));
        p.poll(now + Duration::from_millis(6500), &mut m);
        assert_eq!(p.position, TimelineTime(1500));
        assert_eq!(p.frame.as_ref().unwrap().actual.ticks, successor);
    }
}

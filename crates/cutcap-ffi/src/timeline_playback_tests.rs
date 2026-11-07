//! Virtual-time scheduler tests. Frame availability is explicitly controlled;
//! no assertions depend on decoder speed or sleeping for a wall-clock boundary.
use super::*;
use cutcap_core::{ClipStream, MediaAsset, Project, TimelineIndex, Track};
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
            stream: cutcap_core::ClipStream::Video,
            link: None,
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

// ---- Phase 4B: the audio master -------------------------------------------

/// Video decoders park (frames are seeded); audio is a deterministic ramp.
struct AvBackend;
impl MediaBackend for AvBackend {
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
    fn stream_pcm(
        &self,
        request: &cutcap_media::PcmRequest,
        cancel: &Cancellation,
        emit: &mut (dyn FnMut(cutcap_media::PcmChunk) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        let end = request.end_sample.unwrap_or(request.start_sample + 480_000);
        let mut j = request.start_sample.max(0);
        while j < end {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            let stop = (j + 1024).min(end);
            emit(cutcap_media::PcmChunk {
                first_sample: j,
                channels: 2,
                samples: (j..stop).flat_map(|n| [n as f32 / 1e6, 0.0]).collect(),
                process_id: 9,
            })?;
            j = stop;
        }
        Ok(())
    }
}

/// Two linked A/V pairs back to back on V1/A1 (cut at `cut` ms), 1/1000 clock.
fn av_fixture(cut: i64) -> (TimelinePlan, Vec<Clip>) {
    let clock = Timebase::new(1, 1000).unwrap();
    let mut project = Project::new("AV", clock).unwrap();
    let (video, audio) = (TrackId::new(), TrackId::new());
    for (id, kind, name) in [
        (video, TrackKind::Video, "Video 1"),
        (audio, TrackKind::Audio, "Audio 1"),
    ] {
        project
            .add_track(Track {
                id,
                name: name.into(),
                kind,
            })
            .unwrap();
    }
    let probe = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":2,"height":2,"time_base":"1/1000","start_pts":0,"duration_ts":3000},{"index":1,"codec_type":"audio","codec_name":"pcm_s16le","sample_rate":"48000","channels":2,"time_base":"1/48000","start_pts":0,"duration_ts":144000}],"format":{"format_name":"matroska"}}"#;
    let (mut media, mut availability, mut clips) = (BTreeMap::new(), BTreeMap::new(), Vec::new());
    for start in [0, cut] {
        let id = MediaId::new();
        project
            .add_media(MediaAsset {
                id,
                path: "av.mkv".into(),
                kind: TrackKind::Video,
            })
            .unwrap();
        media.insert(
            id,
            MediaSummary {
                id,
                path: "av.mkv".into(),
                metadata: parse_probe(probe).unwrap(),
            },
        );
        availability.insert(
            id,
            MediaAvailability {
                id,
                path: "av.mkv".into(),
                state: Availability::Ready,
                detail: String::new(),
            },
        );
        let link = Some(LinkId::new());
        let v = Clip {
            id: ClipId::new(),
            track_id: video,
            media_id: id,
            start: TimelineTime(start),
            duration: TimelineDuration::new(1000).unwrap(),
            source_in: SourceTime {
                ticks: 0,
                timebase: clock,
            },
            stream: ClipStream::Video,
            link,
        };
        let a = Clip {
            id: ClipId::new(),
            track_id: audio,
            stream: ClipStream::Audio { index: 1 },
            ..v.clone()
        };
        let n = project.clips().len();
        project
            .replace_clips(&[], &[(n, v.clone()), (n + 1, a)])
            .unwrap();
        clips.push(v);
    }
    (
        TimelinePlan {
            project: project.id(),
            generation: RequestId::new(),
            timebase: clock,
            track: Some(MonitoredTrack {
                id: video,
                name: "Video 1".into(),
            }),
            extent: TimelineTime(cut + 1000),
            position: TimelineTime::ZERO,
            index: Arc::new(TimelineIndex::new(&project)),
            media,
            availability,
        },
        clips,
    )
}

fn av_transport(plan: TimelinePlan, now: Instant) -> (TimelinePlayback, MediaHost) {
    let backend: Arc<dyn MediaBackend> = Arc::new(AvBackend);
    let audio = crate::audio_transport::AudioTransport::start(
        backend.clone(),
        crate::audio_output::AudioOutput::virtual_device(48_000),
    )
    .unwrap();
    let mut p = TimelinePlayback::start_with_audio(backend.clone(), Some(audio), None).unwrap();
    p.begin(plan, true, false, now).unwrap();
    (p, MediaHost::start(backend).unwrap())
}

fn audio(p: &TimelinePlayback) -> &crate::audio_transport::AudioTransport {
    p.audio.as_ref().unwrap()
}

#[test]
fn audio_is_the_master_clock_and_wall_time_alone_never_advances_playback() {
    let now = Instant::now();
    let (plan, clips) = av_fixture(1000);
    let (mut p, mut m) = av_transport(plan, now);
    assert_eq!(p.view.clock_source, "audio");
    let mut slot = p.new_slot(clips[0].clone(), TimelineTime::ZERO, 0).unwrap();
    seed(&mut slot, &[0, 250, 500, 750], true, p.clock.timebase);
    p.active = Some(slot);
    audio(&p).wait_buffered(6000);
    p.poll(now, &mut m);
    assert_eq!(p.view.state, "playing");
    // Five wall-clock seconds pass but the device has played nothing yet.
    p.poll(now + Duration::from_secs(5), &mut m);
    assert_eq!(
        p.position,
        TimelineTime::ZERO,
        "no device position: the timeline waits"
    );
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 0);
    // The device plays 250 ms: the picture follows the audio exactly.
    audio(&p).consume(12_000, now);
    p.poll(now + Duration::from_secs(5), &mut m);
    assert_eq!(p.position, TimelineTime(250));
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 250);
    // A/V offset at presentation is measured against the audio clock.
    let generation = p.frame.as_ref().unwrap().generation;
    p.copied(generation);
    assert_eq!(p.view.av_offset_ms, Some(0.0));
    // Pause freezes both; the device consumes nothing more while paused.
    p.pause(now + Duration::from_secs(6)).unwrap();
    assert_eq!((p.position, p.view.state), (TimelineTime(250), "paused"));
    audio(&p).consume(12_000, now);
    // A real endpoint still plays the 20 ms period it already had: the paused
    // playhead must not move (found on the Windows device by the interop test).
    audio(&p).consume_in_flight(960, now);
    p.poll(now + Duration::from_secs(7), &mut m);
    assert_eq!(p.position, TimelineTime(250));
    p.resume(now + Duration::from_secs(8));
    assert_eq!(p.view.state, "playing");
    audio(&p).wait_buffered(12_000);
    audio(&p).consume(12_000, now);
    p.poll(now + Duration::from_secs(8), &mut m);
    // Resumed, the clock is again what the device played, in-flight period included.
    assert_eq!(p.position, TimelineTime(520));
    assert_eq!(p.frame.as_ref().unwrap().actual.ticks, 500);
}

#[test]
fn late_video_never_stops_the_audio_master_and_a_seek_restarts_both() {
    let now = Instant::now();
    let (plan, clips) = av_fixture(1000);
    let (mut p, mut m) = av_transport(plan, now);
    let mut slot = p.new_slot(clips[0].clone(), TimelineTime::ZERO, 0).unwrap();
    seed(&mut slot, &[0, 100], false, p.clock.timebase); // the decoder stalls after 100 ms
    p.active = Some(slot);
    audio(&p).wait_buffered(6000);
    p.poll(now, &mut m);
    assert_eq!(p.view.state, "playing");
    for step in 1..=4 {
        audio(&p).wait_buffered(4_800);
        audio(&p).consume(4_800, now);
        p.poll(now, &mut m);
        assert_eq!(
            p.position,
            TimelineTime(step * 100),
            "audio keeps the clock running"
        );
        assert_eq!(
            p.view.state, "playing",
            "no system-clock buffering under an audio master"
        );
    }
    assert_eq!(
        p.frame.as_ref().unwrap().actual.ticks,
        100,
        "the last available picture stays"
    );
    assert_eq!(p.view.video_late, 1, "one late episode, counted once");
    assert_eq!(p.view.buffer_stalls, 0);
    // Seek to 1.5 s: a new audio epoch starts from the new position.
    let epoch = audio(&p).view().epoch;
    let (mut plan, _) = av_fixture(1000);
    plan.position = TimelineTime(1500);
    p.begin(plan, true, true, now).unwrap();
    assert!(audio(&p).view().epoch > epoch);
    assert_eq!(
        audio(&p).clock(now),
        None,
        "the old device position is gone"
    );
    // Stop ends the audio epoch too.
    p.stop().unwrap();
    assert_eq!(audio(&p).view().state, "idle");
}

#[test]
fn audio_runs_through_a_cut_and_the_end_stops_the_device() {
    let now = Instant::now();
    let (plan, clips) = av_fixture(1000);
    let (mut p, mut m) = av_transport(plan, now);
    let mut slot = p.new_slot(clips[0].clone(), TimelineTime::ZERO, 0).unwrap();
    seed(&mut slot, &[0, 500], true, p.clock.timebase);
    p.active = Some(slot);
    audio(&p).wait_buffered(6000);
    p.poll(now, &mut m);
    seed(
        p.prepared.as_mut().unwrap(),
        &[0, 500],
        true,
        p.clock.timebase,
    );
    let mut played = 0;
    while played < 96_000 {
        audio(&p).wait_buffered(4_800);
        audio(&p).consume(4_800, now);
        played += 4_800;
        p.poll(now, &mut m);
    }
    assert_eq!(p.view.state, "ended");
    assert_eq!(p.position, TimelineTime(2000));
    assert_eq!(
        p.view.handoffs, 1,
        "the video cut was taken on the audio clock"
    );
    assert_eq!(
        audio(&p).view().state,
        "idle",
        "the device epoch ends with the timeline"
    );
}

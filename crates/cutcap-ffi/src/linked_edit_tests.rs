//! Phase 4B linked audio/video editing against the command owner directly:
//! placement, move, trim, split, removal and exact undo/redo of a pair.
use super::*;
use cutcap_core::ClipStream;

/// A 10 s source: H.264-like video on 1/15360 and a stereo 48 kHz audio
/// stream (index 1) on its own 1/48000 clock starting 1024 ticks later.
fn av_probe() -> Vec<u8> {
    br#"{"streams":[
        {"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,"time_base":"1/15360","start_pts":0,"duration_ts":153600},
        {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"channel_layout":"stereo","sample_fmt":"fltp","time_base":"1/48000","start_pts":1024,"duration_ts":480000,"disposition":{"default":1}}
    ],"format":{"format_name":"mov,mp4"}}"#
        .to_vec()
}

fn video_only_probe() -> Vec<u8> {
    br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"1/15360","start_pts":0,"duration_ts":153600}],"format":{"format_name":"matroska"}}"#.to_vec()
}

fn send(
    engine: &mut Engine,
    project: ProjectId,
    command: Command,
) -> Result<ProjectSnapshot, EngineError> {
    match engine.handle(CommandEnvelope {
        request_id: RequestId::new(),
        project_id: Some(project),
        command,
    }) {
        EngineEvent::Completed { snapshot, .. } => Ok(snapshot),
        EngineEvent::Failed { error, .. } => Err(error),
        event => panic!("unexpected {event:?}"),
    }
}

struct Bench {
    engine: Engine,
    project: ProjectId,
    media: MediaId,
    video: TrackId,
    audio: TrackId,
}

impl Bench {
    fn new(probe: &[u8]) -> Self {
        let mut engine = Engine::default();
        let snapshot = match engine.handle(CommandEnvelope {
            request_id: RequestId::new(),
            project_id: None,
            command: Command::NewProject {
                name: "Linked".into(),
            },
        }) {
            EngineEvent::Completed { snapshot, .. } => snapshot,
            event => panic!("unexpected {event:?}"),
        };
        let media = MediaSummary {
            id: MediaId::new(),
            path: "C:/clips/take.mp4".into(),
            metadata: cutcap_media::parse_probe(probe).unwrap().into(),
        };
        let id = media.id;
        send(
            &mut engine,
            snapshot.id,
            Command::RegisterMedia {
                media: Box::new(media),
                fingerprint: None,
            },
        )
        .unwrap();
        Self {
            engine,
            project: snapshot.id,
            media: id,
            video: snapshot.tracks[0].id,
            audio: snapshot.tracks[1].id,
        }
    }
    fn send(&mut self, command: Command) -> Result<ProjectSnapshot, EngineError> {
        send(&mut self.engine, self.project, command)
    }
    fn place(&mut self, track: TrackId, at: i64) -> Result<ProjectSnapshot, EngineError> {
        self.send(Command::PlaceClip {
            media_id: self.media,
            track_id: track,
            position: TimelineSeek::Ticks(TimelineTime(at)),
        })
    }
    fn clips(&self) -> Vec<ClipSummary> {
        self.engine.snapshot().unwrap().clips
    }
    fn pair(&self) -> (ClipSummary, ClipSummary) {
        let clips = self.clips();
        let video = clips
            .iter()
            .find(|c| c.stream == ClipStream::Video)
            .unwrap()
            .clone();
        let audio = clips
            .iter()
            .find(|c| c.stream != ClipStream::Video)
            .unwrap()
            .clone();
        (video, audio)
    }
    fn depth(&self) -> usize {
        self.engine.history.status().depth
    }
    fn add_track(&mut self, kind: TrackKind) -> TrackId {
        match self.send(Command::AddTrack { kind }).unwrap().selection {
            SelectionState::Track(id) => id,
            other => panic!("{other:?}"),
        }
    }
}

const SECOND: i64 = 720_000; // default project clock 1/720000

fn assert_linked(video: &ClipSummary, audio: &ClipSummary) {
    assert!(video.link.is_some());
    assert_eq!(video.link, audio.link);
    assert_eq!(audio.stream, ClipStream::Audio { index: 1 });
    assert_eq!(
        (video.media_id, video.start, video.duration, video.source_in),
        (audio.media_id, audio.start, audio.duration, audio.source_in),
        "a pair shares media, timing and the same source instant"
    );
}

#[test]
fn placement_adds_the_primary_audio_as_one_undoable_linked_pair() {
    let mut b = Bench::new(&av_probe());
    let snapshot = b.place(b.video, SECOND).unwrap();
    assert_eq!(snapshot.clips.len(), 2);
    let (video, audio) = b.pair();
    assert_eq!(video.track_id, b.video);
    assert_eq!(audio.track_id, b.audio);
    assert_linked(&video, &audio);
    // The shared in-point is the video stream's first PTS in its own clock.
    assert_eq!(
        video.source_in,
        SourceTime {
            ticks: 0,
            timebase: Timebase::new(1, 15360).unwrap()
        }
    );
    assert_eq!(snapshot.selection, SelectionState::Clip(video.id));
    assert_eq!(b.depth(), 1);
    assert_eq!(
        b.engine.history.status().undo.as_deref(),
        Some("add take.mp4 with audio")
    );
    b.send(Command::Undo).unwrap();
    assert!(b.clips().is_empty());
    b.send(Command::Redo).unwrap();
    assert_eq!(
        b.pair(),
        (video, audio),
        "redo restores both identities exactly"
    );
    // Append (Add to timeline) follows the same policy.
    b.send(Command::InsertClip {
        media_id: b.media,
        track_id: b.video,
    })
    .unwrap();
    let clips = b.clips();
    assert_eq!(clips.len(), 4);
    let appended: Vec<_> = clips
        .iter()
        .filter(|c| c.start.0 == SECOND + 10 * SECOND)
        .collect();
    assert_eq!(appended.len(), 2);
    assert_linked(
        appended[0].min_by_video(appended[1]).0,
        appended[0].min_by_video(appended[1]).1,
    );
}

trait Pair {
    fn min_by_video<'a>(&'a self, other: &'a ClipSummary) -> (&'a ClipSummary, &'a ClipSummary);
}
impl Pair for ClipSummary {
    fn min_by_video<'a>(&'a self, other: &'a ClipSummary) -> (&'a ClipSummary, &'a ClipSummary) {
        if self.stream == ClipStream::Video {
            (self, other)
        } else {
            (other, self)
        }
    }
}

#[test]
fn video_only_sources_are_unchanged_and_blocked_audio_rejects_the_whole_placement() {
    let mut silent = Bench::new(&video_only_probe());
    let snapshot = silent.place(silent.video, 0).unwrap();
    assert_eq!(snapshot.clips.len(), 1);
    assert_eq!(snapshot.clips[0].stream, ClipStream::Video);
    assert_eq!(snapshot.clips[0].link, None);

    let mut b = Bench::new(&av_probe());
    b.place(b.video, 0).unwrap();
    let second_video = b.add_track(TrackKind::Video);
    let before = b.clips();
    let depth = b.depth();
    // The only audio track is occupied at 0: never insert the video alone.
    let error = b.place(second_video, 2 * SECOND).unwrap_err();
    assert!(
        error.message.contains("No audio track is free"),
        "{}",
        error.message
    );
    assert_eq!(b.clips(), before);
    assert_eq!(b.depth(), depth);
    // A second audio track makes room; the first free one is used.
    let second_audio = b.add_track(TrackKind::Audio);
    b.place(second_video, 2 * SECOND).unwrap();
    let audio = b
        .clips()
        .into_iter()
        .find(|c| c.start.0 == 2 * SECOND && c.stream != ClipStream::Video)
        .unwrap();
    assert_eq!(audio.track_id, second_audio);
}

#[test]
fn moving_either_member_moves_the_pair_and_blocked_partners_reject_atomically() {
    let mut b = Bench::new(&av_probe());
    b.place(b.video, 0).unwrap();
    let (video, audio) = b.pair();
    // Drag the video 3 s later: the audio follows on its own track.
    b.send(Command::MoveClip {
        clip_id: video.id,
        track_id: b.video,
        position: TimelineSeek::Ticks(TimelineTime(3 * SECOND)),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    let (v, a) = b.pair();
    assert_eq!((v.start.0, a.start.0), (3 * SECOND, 3 * SECOND));
    assert_eq!((v.track_id, a.track_id), (b.video, b.audio));
    assert_linked(&v, &a);
    assert_eq!(b.depth(), 2, "one gesture, one history entry");
    // Drag the audio to another audio track and earlier: video follows in time only.
    let other = b.add_track(TrackKind::Audio);
    b.send(Command::MoveClip {
        clip_id: audio.id,
        track_id: other,
        position: TimelineSeek::Ticks(TimelineTime(SECOND)),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    let (v, a) = b.pair();
    assert_eq!((v.start.0, a.start.0), (SECOND, SECOND));
    assert_eq!((v.track_id, a.track_id), (b.video, other));
    // Audio cannot go on a video track.
    let error = b
        .send(Command::MoveClip {
            clip_id: audio.id,
            track_id: b.video,
            position: TimelineSeek::Ticks(TimelineTime(0)),
            anchor: ClipEdge::Start,
        })
        .unwrap_err();
    assert!(error.message.contains("audio track"), "{}", error.message);
    // A clip on the partner's track blocks the move: nothing changes.
    let silent = MediaSummary {
        id: MediaId::new(),
        path: "C:/clips/other.mkv".into(),
        metadata: cutcap_media::parse_probe(&video_only_probe())
            .unwrap()
            .into(),
    };
    let silent_id = silent.id;
    b.send(Command::RegisterMedia {
        media: Box::new(silent),
        fingerprint: None,
    })
    .unwrap();
    b.send(Command::PlaceClip {
        media_id: silent_id,
        track_id: b.video,
        position: TimelineSeek::Ticks(TimelineTime(30 * SECOND)),
    })
    .unwrap();
    let before = b.clips();
    let error = b
        .send(Command::MoveClip {
            clip_id: audio.id,
            track_id: other,
            position: TimelineSeek::Ticks(TimelineTime(25 * SECOND)),
            anchor: ClipEdge::Start,
        })
        .unwrap_err();
    assert!(error.message.contains("overlap"), "{}", error.message);
    assert_eq!(b.clips(), before);
    // Undo walks back one gesture at a time, both members together.
    b.send(Command::Undo).unwrap(); // the silent placement
    b.send(Command::Undo).unwrap(); // the audio drag
    let (v, a) = b.pair();
    assert_eq!(
        (v.start.0, a.start.0, a.track_id),
        (3 * SECOND, 3 * SECOND, b.audio)
    );
}

#[test]
fn trims_from_either_member_change_both_on_the_shared_source_grid() {
    let mut b = Bench::new(&av_probe());
    b.place(b.video, 0).unwrap();
    let (video, audio) = b.pair();
    // Start trim requested 1 ms in (720 project ticks): aligned up to the
    // video clock's grid (375 ticks), identical for the audio member.
    b.send(Command::TrimClip {
        clip_id: audio.id,
        edge: ClipEdge::Start,
        position: TimelineSeek::Ticks(TimelineTime(720)),
    })
    .unwrap();
    let (v, a) = b.pair();
    assert_eq!(v.start.0, 750);
    assert_eq!(
        v.source_in.ticks, 16,
        "750 project ticks = 16 ticks of 1/15360"
    );
    assert_linked(&v, &a);
    // End trim from the video member.
    b.send(Command::TrimClip {
        clip_id: video.id,
        edge: ClipEdge::End,
        position: TimelineSeek::Ticks(TimelineTime(4 * SECOND)),
    })
    .unwrap();
    let (v, a) = b.pair();
    assert_eq!(v.start.0 + v.duration.ticks() as i64, 4 * SECOND);
    assert_linked(&v, &a);
    b.send(Command::Undo).unwrap();
    b.send(Command::Undo).unwrap();
    assert_eq!(b.pair(), (video, audio));
}

#[test]
fn splitting_a_pair_creates_two_exact_pairs_and_undoes_as_one_step() {
    let mut b = Bench::new(&av_probe());
    b.place(b.video, 0).unwrap();
    let (video, audio) = b.pair();
    b.send(Command::SplitClip {
        clip_id: audio.id,
        position: TimelineSeek::Ticks(TimelineTime(4 * SECOND)),
    })
    .unwrap();
    let clips = b.clips();
    assert_eq!(clips.len(), 4);
    let left: Vec<_> = clips.iter().filter(|c| c.start.0 == 0).collect();
    let right: Vec<_> = clips.iter().filter(|c| c.start.0 == 4 * SECOND).collect();
    assert_eq!((left.len(), right.len()), (2, 2));
    let (lv, la) = left[0].min_by_video(left[1]);
    let (rv, ra) = right[0].min_by_video(right[1]);
    assert_linked(lv, la);
    assert_linked(rv, ra);
    assert_eq!(
        (lv.id, la.id),
        (video.id, audio.id),
        "the left halves keep their identity"
    );
    assert_eq!(lv.link, video.link, "and their link");
    assert_ne!(rv.link, video.link, "the right halves form a new pair");
    assert_eq!(
        rv.source_in.ticks,
        4 * 15360,
        "4 s on the video clock, exact"
    );
    assert_eq!(lv.duration.ticks(), 4 * SECOND as u64);
    assert_eq!(b.depth(), 2);
    b.send(Command::Undo).unwrap();
    assert_eq!(b.pair(), (video.clone(), audio.clone()));
    b.send(Command::Redo).unwrap();
    assert_eq!(b.clips(), clips, "redo reproduces identical ids and values");
}

#[test]
fn removing_either_member_removes_the_pair_and_undo_restores_identities() {
    let mut b = Bench::new(&av_probe());
    b.place(b.video, 0).unwrap();
    let (video, audio) = b.pair();
    b.send(Command::Select {
        selection: SelectionState::Clip(audio.id),
    })
    .unwrap();
    let snapshot = b.send(Command::RemoveClip { clip_id: audio.id }).unwrap();
    assert!(snapshot.clips.is_empty());
    assert_eq!(snapshot.selection, SelectionState::None);
    b.send(Command::Undo).unwrap();
    assert_eq!(b.pair(), (video, audio));
}

#[test]
fn timeline_rows_carry_stream_link_and_exact_audio_sample_intervals() {
    let mut b = Bench::new(&av_probe());
    b.place(b.video, SECOND).unwrap();
    b.send(Command::TrimClip {
        clip_id: b.pair().0.id,
        edge: ClipEdge::Start,
        position: TimelineSeek::Ticks(TimelineTime(SECOND + SECOND / 2)),
    })
    .unwrap();
    let snapshot = b.engine.snapshot().unwrap();
    let rows = crate::view::clip_views(&snapshot);
    let video = rows.iter().find(|r| r.stream == "video").unwrap();
    let audio = rows.iter().find(|r| r.stream == "audio").unwrap();
    assert_eq!(audio.audio_stream, Some(1));
    assert_eq!(audio.link_id, video.link_id);
    assert!(audio.link_id.is_some());
    assert_eq!(audio.track_index, 1);
    // Audio rows never carry filmstrip inputs; video rows never waveform inputs.
    assert!(audio.source_key.is_empty() && audio.thumb_aspect == 0.0);
    assert!(video.audio_key.is_empty() && video.audio_rate == 0);
    assert_eq!(audio.audio_rate, 48_000);
    // 0.5 s into the source on the 48 kHz grid, 9.5 s long.
    assert_eq!(audio.source_in_samples, 24_000);
    assert_eq!(audio.source_end_samples, 480_000);
    assert_eq!(audio.start_ticks, video.start_ticks);
    assert_eq!(audio.end_ticks, video.end_ticks);
}

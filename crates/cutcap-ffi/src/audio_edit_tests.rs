//! Phase 4C editing against the command owner: audio-only sources, chosen
//! streams, Unlink/Link, independent edits and track-control history.
use super::*;
use cutcap_core::ClipStream;

const SECOND: i64 = 720_000; // default project clock 1/720000

/// 10 s of video on 1/15360, primary audio (stream 1, 48 kHz) and an
/// alternate language (stream 2, 44.1 kHz, 12 s, starting 1 s in).
fn av_probe() -> Vec<u8> {
    br#"{"streams":[
        {"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,"time_base":"1/15360","start_pts":0,"duration_ts":153600},
        {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"channel_layout":"stereo","sample_fmt":"fltp","time_base":"1/48000","start_pts":0,"duration_ts":480000,"disposition":{"default":1}},
        {"index":2,"codec_type":"audio","codec_name":"aac","sample_rate":"44100","channels":1,"channel_layout":"mono","sample_fmt":"fltp","time_base":"1/44100","start_pts":44100,"duration_ts":529200,"disposition":{"default":0}}
    ],"format":{"format_name":"mov,mp4"}}"#
        .to_vec()
}

/// 6 s of MP3-like audio on a very fine clock with a nonzero start.
fn mp3_probe() -> Vec<u8> {
    br#"{"streams":[
        {"index":0,"codec_type":"audio","codec_name":"mp3","sample_rate":"44100","channels":2,"channel_layout":"stereo","sample_fmt":"fltp","time_base":"1/14112000","start_pts":353600,"duration_ts":84672000,"disposition":{"default":1}}
    ],"format":{"format_name":"mp3","duration":"6.0"}}"#
        .to_vec()
}

struct Bench {
    engine: Engine,
    project: ProjectId,
    video: TrackId,
    audio: TrackId,
}

impl Bench {
    fn new() -> Self {
        let mut engine = Engine::default();
        let snapshot = match engine.handle(CommandEnvelope {
            request_id: RequestId::new(),
            project_id: None,
            command: Command::NewProject { name: "Mix".into() },
        }) {
            EngineEvent::Completed { snapshot, .. } => snapshot,
            event => panic!("unexpected {event:?}"),
        };
        Self {
            engine,
            project: snapshot.id,
            video: snapshot.tracks[0].id,
            audio: snapshot.tracks[1].id,
        }
    }
    fn send(&mut self, command: Command) -> Result<ProjectSnapshot, EngineError> {
        match self.engine.handle(CommandEnvelope {
            request_id: RequestId::new(),
            project_id: Some(self.project),
            command,
        }) {
            EngineEvent::Completed { snapshot, .. } => Ok(snapshot),
            EngineEvent::Failed { error, .. } => Err(error),
            event => panic!("unexpected {event:?}"),
        }
    }
    fn import(&mut self, probe: &[u8], path: &str) -> MediaId {
        let metadata = cutcap_media::parse_source_probe(probe).unwrap();
        let media = MediaSummary {
            id: MediaId::new(),
            path: path.into(),
            metadata,
        };
        let id = media.id;
        self.send(Command::RegisterMedia {
            media: Box::new(media),
            fingerprint: None,
        })
        .unwrap();
        id
    }
    fn at(seconds: f64) -> TimelineSeek {
        TimelineSeek::Ticks(TimelineTime((seconds * SECOND as f64) as i64))
    }
    fn place(
        &mut self,
        media: MediaId,
        track: TrackId,
        seconds: f64,
    ) -> Result<ProjectSnapshot, EngineError> {
        self.send(Command::PlaceClip {
            media_id: media,
            track_id: track,
            position: Self::at(seconds),
        })
    }
    fn clips(&self) -> Vec<ClipSummary> {
        self.engine.snapshot().unwrap().clips
    }
    fn depth(&self) -> usize {
        self.engine.history.status().depth
    }
    fn generation(&self) -> RequestId {
        self.engine.snapshot().unwrap().evaluation_generation
    }
    fn add_audio(&mut self) -> TrackId {
        match self
            .send(Command::AddTrack {
                kind: TrackKind::Audio,
            })
            .unwrap()
            .selection
        {
            SelectionState::Track(id) => id,
            other => panic!("{other:?}"),
        }
    }
    fn controls(&self, track: TrackId) -> TrackControls {
        self.engine
            .snapshot()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.id == track)
            .unwrap()
            .controls
    }
    fn pair(&self) -> (ClipSummary, ClipSummary) {
        let clips = self.clips();
        (
            clips
                .iter()
                .find(|c| c.stream == ClipStream::Video)
                .unwrap()
                .clone(),
            clips
                .iter()
                .find(|c| c.stream != ClipStream::Video)
                .unwrap()
                .clone(),
        )
    }
}

fn duration(seconds: i64) -> TimelineDuration {
    TimelineDuration::new((seconds * SECOND) as u64).unwrap()
}

#[test]
fn an_audio_only_source_becomes_an_independent_clip_on_an_audio_track() {
    let mut b = Bench::new();
    let media = b.import(&mp3_probe(), "C:/music/song.mp3");
    let snapshot = b.engine.snapshot().unwrap();
    assert!(snapshot.media[0].metadata.is_audio_only());
    // No fake picture: a video track refuses it and nothing changes.
    let error = b.place(media, b.video, 0.0).unwrap_err();
    assert!(error.message.contains("audio track"), "{error}");
    assert!(b.clips().is_empty());
    b.place(media, b.audio, 2.0).unwrap();
    let clips = b.clips();
    assert_eq!(clips.len(), 1, "no companion video clip");
    let clip = &clips[0];
    assert_eq!(clip.stream, ClipStream::Audio { index: 0 });
    assert_eq!(clip.link, None);
    assert_eq!(clip.start, TimelineTime(2 * SECOND));
    assert_eq!(clip.duration, duration(6));
    // The stream's first sample, on the stream's own sample clock.
    assert_eq!(clip.source_in.timebase, Timebase::new(1, 44_100).unwrap());
    assert_eq!(clip.source_in.ticks, 1105);
    assert_eq!(b.depth(), 1);
    b.send(Command::Undo).unwrap();
    assert!(b.clips().is_empty());
    b.send(Command::Redo).unwrap();
    assert_eq!(b.clips().len(), 1);
}

#[test]
fn add_to_timeline_appends_audio_only_sources_to_the_audio_track() {
    let mut b = Bench::new();
    let media = b.import(&mp3_probe(), "C:/music/song.mp3");
    for expected in [0, 6] {
        b.send(Command::InsertClip {
            media_id: media,
            track_id: b.audio,
        })
        .unwrap();
        assert_eq!(
            b.clips().last().unwrap().start,
            TimelineTime(expected * SECOND)
        );
    }
    assert!(
        b.send(Command::InsertClip {
            media_id: media,
            track_id: b.video,
        })
        .is_err()
    );
}

#[test]
fn a_chosen_stream_is_placed_explicitly_and_keeps_its_identity() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    let (video, audio) = (b.video, b.audio);
    let place = |b: &mut Bench, stream, track, at| {
        b.send(Command::PlaceAudioStream {
            media_id: media,
            stream,
            track_id: Some(track),
            position: Bench::at(at),
        })
    };
    assert!(place(&mut b, 9, audio, 0.0).is_err(), "unknown stream");
    assert!(place(&mut b, 2, video, 0.0).is_err(), "video track");
    assert!(b.clips().is_empty());
    place(&mut b, 2, audio, 1.0).unwrap();
    let clip = b.clips()[0].clone();
    assert_eq!(clip.stream, ClipStream::Audio { index: 2 });
    assert_eq!(clip.link, None);
    assert_eq!(clip.duration, duration(12));
    assert_eq!(clip.source_in.timebase, Timebase::new(1, 44_100).unwrap());
    assert_eq!(clip.source_in.ticks, 44_100, "the stream starts 1 s in");
    // Only the placed stream exists: files are not expanded into every stream.
    assert_eq!(b.clips().len(), 1);
    // A second stream of the same file needs its own free track.
    assert!(place(&mut b, 1, audio, 2.0).is_err(), "overlap");
    let other = b.add_audio();
    place(&mut b, 1, other, 2.0).unwrap();
    assert_eq!(b.clips().len(), 2);
    assert_eq!(b.depth(), 2);
}

#[test]
fn without_a_chosen_track_a_stream_goes_on_the_first_free_audio_track() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    let second = b.add_audio();
    let place = |b: &mut Bench| {
        b.send(Command::PlaceAudioStream {
            media_id: media,
            stream: 2,
            track_id: None,
            position: Bench::at(0.0),
        })
    };
    place(&mut b).unwrap();
    assert_eq!(b.clips()[0].track_id, b.audio);
    place(&mut b).unwrap();
    assert_eq!(b.clips()[1].track_id, second);
    let error = place(&mut b).unwrap_err();
    assert!(error.message.contains("No audio track is free"), "{error}");
    assert_eq!(b.clips().len(), 2);
}

#[test]
fn a_video_source_on_an_audio_track_stays_rejected() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    let error = b.place(media, b.audio, 0.0).unwrap_err();
    assert!(error.message.contains("video track"), "{error}");
}

#[test]
fn unlink_is_one_exact_undoable_step_and_then_members_edit_independently() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    b.place(media, b.video, 1.0).unwrap();
    let (video, audio) = b.pair();
    assert!(video.link.is_some());
    let depth = b.depth();
    b.send(Command::UnlinkClip { clip_id: audio.id }).unwrap();
    assert_eq!(b.depth(), depth + 1);
    let (v, a) = b.pair();
    assert_eq!(v.link, None);
    assert_eq!(a.link, None);
    // Nothing but the link changed: identity, timing, stream, exact source_in.
    assert_eq!(
        (v.id, v.start, v.duration, v.source_in, v.stream),
        (
            video.id,
            video.start,
            video.duration,
            video.source_in,
            video.stream
        )
    );
    assert_eq!(
        (a.id, a.start, a.duration, a.source_in, a.stream),
        (
            audio.id,
            audio.start,
            audio.duration,
            audio.source_in,
            audio.stream
        )
    );
    // Undo restores the same pair, redo unlinks it again.
    b.send(Command::Undo).unwrap();
    let (v2, a2) = b.pair();
    assert_eq!((v2.link, a2.link), (video.link, audio.link));
    assert_eq!(v2, video);
    assert_eq!(a2, audio);
    b.send(Command::Redo).unwrap();
    assert_eq!(b.pair().0.link, None);

    // Move only the audio.
    b.send(Command::MoveClip {
        clip_id: audio.id,
        track_id: audio.track_id,
        position: Bench::at(4.0),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    let (v, a) = b.pair();
    assert_eq!(
        v,
        ClipSummary {
            link: None,
            ..video.clone()
        },
        "video did not move"
    );
    assert_eq!(a.start, TimelineTime(4 * SECOND));
    assert_eq!(a.source_in, audio.source_in);

    // Trim only the audio's start by 1 s: its source moves, the video's does not.
    b.send(Command::TrimClip {
        clip_id: audio.id,
        edge: ClipEdge::Start,
        position: Bench::at(5.0),
    })
    .unwrap();
    let (v, a) = b.pair();
    assert_eq!(v.start, video.start);
    assert_eq!(a.start, TimelineTime(5 * SECOND));
    assert_eq!(a.source_in.ticks, audio.source_in.ticks + 15_360);
    assert_eq!(a.duration, duration(9));

    // Split only the audio: two audio clips, one untouched video.
    b.send(Command::SplitClip {
        clip_id: audio.id,
        position: Bench::at(7.0),
    })
    .unwrap();
    let clips = b.clips();
    assert_eq!(clips.len(), 3);
    assert_eq!(
        clips
            .iter()
            .filter(|c| c.stream == ClipStream::Video)
            .count(),
        1
    );
    let right = clips
        .iter()
        .find(|c| c.start == TimelineTime(7 * SECOND))
        .unwrap();
    assert_eq!(right.link, None);
    assert_eq!(right.source_in.ticks, audio.source_in.ticks + 3 * 15_360);

    // Remove only the audio's left part.
    b.send(Command::RemoveClip { clip_id: audio.id }).unwrap();
    assert_eq!(b.clips().len(), 2);

    // Unlinking something unlinked is refused and records nothing.
    let depth = b.depth();
    assert!(b.send(Command::UnlinkClip { clip_id: video.id }).is_err());
    assert_eq!(b.depth(), depth);
}

#[test]
fn linked_pairs_keep_moving_together_until_unlinked() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    b.place(media, b.video, 1.0).unwrap();
    let (video, audio) = b.pair();
    b.send(Command::MoveClip {
        clip_id: video.id,
        track_id: video.track_id,
        position: Bench::at(3.0),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    let (v, a) = b.pair();
    assert_eq!(
        (v.start, a.start),
        (TimelineTime(3 * SECOND), TimelineTime(3 * SECOND))
    );
    assert_eq!(a.link, audio.link);
}

#[test]
fn relinking_needs_exact_compatibility_never_just_the_same_file() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    b.place(media, b.video, 1.0).unwrap();
    let (video, audio) = b.pair();
    let link = |b: &mut Bench, x, y| {
        b.send(Command::LinkClips {
            clip_id: x,
            partner_id: y,
        })
    };
    // Already linked.
    assert!(link(&mut b, video.id, audio.id).is_err());
    b.send(Command::UnlinkClip { clip_id: video.id }).unwrap();
    // Same family is refused.
    let snapshot = b.engine.snapshot().unwrap();
    assert_eq!(
        crate::edits::link_candidate(&snapshot.clips, &snapshot.clips[0]).map(|c| c.id),
        Some(audio.id),
        "the exact partner is offered"
    );
    assert!(link(&mut b, video.id, video.id).is_err());
    // Move the audio away: same file, but no longer compatible.
    b.send(Command::MoveClip {
        clip_id: audio.id,
        track_id: audio.track_id,
        position: Bench::at(2.0),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    let snapshot = b.engine.snapshot().unwrap();
    assert!(crate::edits::link_candidate(&snapshot.clips, &snapshot.clips[0]).is_none());
    let depth = b.depth();
    assert!(link(&mut b, video.id, audio.id).is_err());
    assert_eq!(b.depth(), depth, "a refused link records nothing");
    // Moved back it is exact again; the link is one undo step.
    b.send(Command::Undo).unwrap();
    link(&mut b, audio.id, video.id).unwrap();
    let (v, a) = b.pair();
    assert!(v.link.is_some());
    assert_eq!(v.link, a.link);
    b.send(Command::Undo).unwrap();
    assert_eq!(b.pair().0.link, None);
}

#[test]
fn independent_audio_is_bounded_by_its_own_stream_not_the_video() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    // Stream 2: 12 s long although the video is 10 s.
    b.send(Command::PlaceAudioStream {
        media_id: media,
        stream: 2,
        track_id: Some(b.audio),
        position: Bench::at(0.0),
    })
    .unwrap();
    let clip = b.clips()[0].clone();
    // It already covers the whole stream: no extension past the end.
    assert!(
        b.send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: Bench::at(13.0),
        })
        .is_err()
    );
    b.send(Command::TrimClip {
        clip_id: clip.id,
        edge: ClipEdge::End,
        position: Bench::at(11.0),
    })
    .unwrap();
    b.send(Command::TrimClip {
        clip_id: clip.id,
        edge: ClipEdge::Start,
        position: Bench::at(0.5),
    })
    .unwrap();
    let trimmed = b.clips()[0].clone();
    // A start trim on the 44.1 kHz grid stays a whole number of samples.
    assert_eq!(
        trimmed.source_in.timebase,
        Timebase::new(1, 44_100).unwrap()
    );
    let rational = trimmed.source_in.ticks as f64 / 44_100.0;
    assert!((rational - 1.5).abs() < 0.002, "{rational}");
    // It can grow back, and only to the stream's first sample.
    b.send(Command::TrimClip {
        clip_id: clip.id,
        edge: ClipEdge::Start,
        position: Bench::at(0.0),
    })
    .unwrap();
    let back = b.clips()[0].clone();
    assert_eq!(back.source_in.ticks, 44_100);
    assert_eq!(back.start, TimelineTime::ZERO);
}

#[test]
fn an_unlinked_clip_outside_its_stream_range_stays_valid_but_cannot_grow() {
    // The audio starts 1024 ticks after the video it was linked to.
    let probe = br#"{"streams":[
        {"index":0,"codec_type":"video","codec_name":"h264","width":320,"height":180,"time_base":"1/15360","start_pts":0,"duration_ts":153600},
        {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"time_base":"1/48000","start_pts":1024,"duration_ts":240000,"disposition":{"default":1}}
    ],"format":{"format_name":"mov,mp4"}}"#;
    let mut b = Bench::new();
    let media = b.import(probe, "C:/clips/short.mp4");
    b.place(media, b.video, 0.0).unwrap();
    let (_, audio) = b.pair();
    b.send(Command::UnlinkClip { clip_id: audio.id }).unwrap();
    // Shrinking the end works although the in-point precedes the stream.
    b.send(Command::TrimClip {
        clip_id: audio.id,
        edge: ClipEdge::End,
        position: Bench::at(8.0),
    })
    .unwrap();
    assert_eq!(b.pair().1.duration, duration(8));
    // The stream ends near 5 s: the clip cannot grow further past it.
    assert!(
        b.send(Command::TrimClip {
            clip_id: audio.id,
            edge: ClipEdge::End,
            position: Bench::at(8.5),
        })
        .is_err()
    );
    assert_eq!(b.pair().1.duration, duration(8));
}

#[test]
fn each_track_control_is_one_undo_step_and_leaves_the_transport_scope_alone() {
    let mut b = Bench::new();
    let media = b.import(&av_probe(), "C:/clips/film.mp4");
    b.place(media, b.video, 0.0).unwrap();
    let (generation, depth, track) = (b.generation(), b.depth(), b.audio);
    b.send(Command::SetTrackGain {
        track_id: track,
        gain_cdb: -600,
    })
    .unwrap();
    b.send(Command::SetTrackMute {
        track_id: track,
        mute: true,
    })
    .unwrap();
    b.send(Command::SetTrackSolo {
        track_id: track,
        solo: true,
    })
    .unwrap();
    assert_eq!(b.depth(), depth + 3);
    assert_eq!(b.generation(), generation, "no control restarts playback");
    assert_eq!(
        b.controls(track),
        TrackControls {
            gain_cdb: -600,
            mute: true,
            solo: true
        }
    );
    let status = b.engine.history.status();
    assert!(status.undo_track && status.undo.unwrap().contains("solo"));
    b.send(Command::Undo).unwrap();
    assert!(!b.controls(track).solo);
    assert!(b.controls(track).mute);
    b.send(Command::Undo).unwrap();
    assert!(!b.controls(track).mute);
    assert_eq!(b.controls(track).gain_cdb, -600);
    b.send(Command::Undo).unwrap();
    assert_eq!(b.controls(track), TrackControls::default());
    assert_eq!(
        b.generation(),
        generation,
        "undo of a control keeps playing"
    );
    b.send(Command::Redo).unwrap();
    assert_eq!(b.controls(track).gain_cdb, -600);
    assert_eq!(b.generation(), generation);
    // Undo of a clip edit still ends the transport scope.
    b.send(Command::Undo).unwrap();
    assert!(b.engine.history.status().redo_track);
    b.send(Command::Undo).unwrap();
    assert_ne!(b.generation(), generation);
    assert!(b.clips().is_empty());
}

#[test]
fn track_controls_are_validated_and_a_no_op_records_nothing() {
    let mut b = Bench::new();
    let depth = b.depth();
    let revision = b.engine.snapshot().unwrap().document.revision;
    b.send(Command::SetTrackGain {
        track_id: b.audio,
        gain_cdb: 0,
    })
    .unwrap();
    b.send(Command::SetTrackMute {
        track_id: b.audio,
        mute: false,
    })
    .unwrap();
    assert_eq!(b.depth(), depth);
    assert_eq!(b.engine.snapshot().unwrap().document.revision, revision);
    for (track, gain_cdb) in [(b.audio, 601), (b.audio, -6001), (b.video, -600)] {
        assert!(
            b.send(Command::SetTrackGain {
                track_id: track,
                gain_cdb,
            })
            .is_err()
        );
    }
    assert!(
        b.send(Command::SetTrackMute {
            track_id: TrackId::new(),
            mute: true,
        })
        .is_err()
    );
    assert_eq!(b.depth(), depth);
}

#[test]
fn undoing_a_control_after_saving_returns_to_the_saved_revision_logic() {
    let mut b = Bench::new();
    let before = b.engine.snapshot().unwrap().document.revision;
    b.send(Command::SetTrackMute {
        track_id: b.audio,
        mute: true,
    })
    .unwrap();
    let after = b.engine.snapshot().unwrap().document.revision;
    assert_ne!(before, after);
    b.send(Command::Undo).unwrap();
    assert_eq!(b.engine.snapshot().unwrap().document.revision, before);
}

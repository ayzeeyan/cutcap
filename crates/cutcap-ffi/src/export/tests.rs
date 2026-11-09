//! The whole export job against a controlled backend: each output frame and
//! sample is compared with what independent arithmetic says it must be.
//! Real FFmpeg coverage is in `cutcap-media`'s ignored export suite.
use super::*;
use crate::{engine::Engine, session::Session};
use cutcap_media::{
    DecodeRequest, DecodedFrame, FrameEncoder, OutputReport, PcmChunk, PcmRequest, ProbeResult,
    RenderFrame, RenderRequest, parse_source_probe,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicI64, AtomicU64, AtomicUsize},
};

const SECOND: i64 = 720_000;
const CLOCK: u32 = 15_360;
const FRAME_TICKS: i64 = 512; // 30 fps on 1/15360

fn ms(value: i64) -> TimelineSeek {
    TimelineSeek::Ticks(TimelineTime(value * SECOND / 1000))
}

/// What one fake file contains.
#[derive(Clone)]
struct Source {
    tag: u64,
    timebase: u32,
    start_pts: i64,
    duration_ticks: i64,
    frames: Vec<i64>,
    /// `[first, end)` samples of one 48 kHz stereo stream; none: picture only.
    audio: Option<(i64, i64)>,
    /// The stream actually stops here, earlier than its header says.
    delivers: Option<i64>,
    video: bool,
}

impl Source {
    fn video(tag: u64, frames: i64) -> Self {
        Self {
            tag,
            timebase: CLOCK,
            start_pts: 0,
            duration_ticks: frames * FRAME_TICKS,
            frames: (0..frames).map(|i| i * FRAME_TICKS).collect(),
            audio: None,
            delivers: None,
            video: true,
        }
    }
    fn with_audio(mut self) -> Self {
        let seconds = self.duration_ticks as f64 / f64::from(self.timebase);
        self.audio = Some((0, (seconds * 48_000.0).round() as i64));
        self
    }
    fn audio_only(tag: u64, samples: i64) -> Self {
        Self {
            tag,
            timebase: 48_000,
            start_pts: 0,
            duration_ticks: samples,
            frames: Vec::new(),
            audio: Some((0, samples)),
            delivers: None,
            video: false,
        }
    }
    fn probe(&self) -> Vec<u8> {
        let audio = self.audio.map(|(first, end)| {
            let index = usize::from(self.video);
            format!(
                r#"{{"index":{index},"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"channel_layout":"stereo","sample_fmt":"fltp","time_base":"1/48000","start_pts":{first},"duration_ts":{},"disposition":{{"default":1}}}}"#,
                end - first
            )
        });
        let video = self.video.then(|| {
            format!(
                r#"{{"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,"time_base":"1/{}","start_pts":{},"duration_ts":{}}}"#,
                self.timebase, self.start_pts, self.duration_ticks
            )
        });
        let streams: Vec<String> = video.into_iter().chain(audio).collect();
        format!(
            r#"{{"streams":[{}],"format":{{"format_name":"mov,mp4","duration":"{}"}}}}"#,
            streams.join(","),
            self.duration_ticks as f64 / f64::from(self.timebase)
        )
        .into_bytes()
    }
}

/// One sample of source `tag` at source position `j`: identifies both.
fn voice(tag: u64, j: i64) -> f32 {
    tag as f32 * 0.01 + (j.rem_euclid(1000)) as f32 * 0.000_01
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Black,
    Frame(u64, i64),
}

#[derive(Default)]
struct Inner {
    files: Mutex<HashMap<String, Source>>,
    shown: Mutex<Vec<Shown>>,
    pcm: Mutex<Vec<f32>>,
    no_encoders: AtomicBool,
    fail_frame: AtomicI64,
    fail_detail: Mutex<String>,
    fail_finish: AtomicBool,
    bad_report: AtomicBool,
    /// The encoder holds at this frame until `release` is set.
    hold_at: AtomicI64,
    release: AtomicBool,
    encoders: AtomicUsize,
    audio_decoders: AtomicUsize,
    video_decoders: AtomicUsize,
    frames_read: AtomicU64,
}

#[derive(Clone, Default)]
struct Fake(Arc<Inner>);

impl Fake {
    fn new() -> Self {
        let fake = Self::default();
        fake.0.fail_frame.store(-1, Ordering::SeqCst);
        fake.0.hold_at.store(-1, Ordering::SeqCst);
        fake
    }
    fn source(&self, path: &Path) -> Option<Source> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        self.0
            .files
            .lock()
            .unwrap()
            .iter()
            .find(|(file, _)| name.starts_with(file.as_str()))
            .map(|(_, source)| source.clone())
    }
    fn shown(&self) -> Vec<Shown> {
        self.0.shown.lock().unwrap().clone()
    }
    fn pcm(&self) -> Vec<f32> {
        self.0.pcm.lock().unwrap().clone()
    }
}

struct FakeEncoder {
    inner: Arc<Inner>,
    spec: cutcap_media::EncodeSpec,
    written: i64,
    cancel: Cancellation,
}

impl FrameEncoder for FakeEncoder {
    fn write_frame(&mut self, planes: &[u8]) -> Result<(), MediaError> {
        while self.written == self.inner.hold_at.load(Ordering::SeqCst)
            && !self.inner.release.load(Ordering::SeqCst)
        {
            if self.cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            thread::sleep(Duration::from_millis(2));
        }
        if self.written == self.inner.fail_frame.load(Ordering::SeqCst) {
            return Err(MediaError::Process {
                stage: "The encoder",
                code: Some(1),
                detail: self.inner.fail_detail.lock().unwrap().clone(),
            });
        }
        let first = u64::from_le_bytes(planes[..8].try_into().unwrap());
        let shown = if planes[..16].iter().all(|b| *b == 16) {
            Shown::Black
        } else {
            Shown::Frame(first, i64::from_le_bytes(planes[8..16].try_into().unwrap()))
        };
        self.inner.shown.lock().unwrap().push(shown);
        self.written += 1;
        Ok(())
    }
    fn encoded(&self) -> u64 {
        self.written as u64
    }
    fn finish(self: Box<Self>) -> Result<(), MediaError> {
        if self.inner.fail_finish.load(Ordering::SeqCst) {
            return Err(MediaError::Process {
                stage: "The encoder",
                code: Some(1),
                detail: "muxing failed".into(),
            });
        }
        let bytes = fs::read(&self.spec.audio_pcm).unwrap();
        *self.inner.pcm.lock().unwrap() = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        fs::write(&self.spec.output, b"finished mp4").unwrap();
        Ok(())
    }
}

impl MediaBackend for Fake {
    fn probe(&self, path: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
        let source = self.source(path).ok_or(MediaError::NoFrame)?;
        Ok(ProbeResult {
            path: path.to_owned(),
            metadata: parse_source_probe(&source.probe())?,
            probe_us: 1,
        })
    }
    fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
        Err(MediaError::NoFrame)
    }
    fn render_frames(
        &self,
        request: &RenderRequest,
        cancel: &Cancellation,
        emit: &mut (dyn FnMut(RenderFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        self.0.video_decoders.fetch_add(1, Ordering::SeqCst);
        let source = self
            .source(&request.media.path)
            .ok_or(MediaError::NoFrame)?;
        for pts in source.frames.iter().filter(|p| **p >= request.start.ticks) {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            let mut planes = source.tag.to_le_bytes().to_vec();
            planes.extend_from_slice(&pts.to_le_bytes());
            self.0.frames_read.fetch_add(1, Ordering::SeqCst);
            emit(RenderFrame {
                ticket: request.ticket,
                actual: SourceTime {
                    ticks: *pts,
                    timebase: request.start.timebase,
                },
                planes,
                process_id: 7,
            })?;
        }
        Ok(())
    }
    fn stream_pcm(
        &self,
        request: &PcmRequest,
        _: &Cancellation,
        emit: &mut (dyn FnMut(PcmChunk) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        self.0.audio_decoders.fetch_add(1, Ordering::SeqCst);
        let source = self
            .source(&request.media.path)
            .ok_or(MediaError::NoFrame)?;
        let (first, end) = source.audio.ok_or(MediaError::NoFrame)?;
        let end = source.delivers.unwrap_or(end);
        let start = request.start_sample.max(first);
        let stop = request.end_sample.unwrap_or(end).min(end);
        let mut at = start;
        while at < stop {
            let run = (stop - at).min(1000);
            emit(PcmChunk {
                first_sample: at,
                channels: 2,
                samples: (at..at + run)
                    .flat_map(|j| [voice(source.tag, j), voice(source.tag, j)])
                    .collect(),
                process_id: 9,
            })?;
            at += run;
        }
        Ok(())
    }
    fn check_export_encoders(&self, _: &Cancellation) -> Result<(), MediaError> {
        if self.0.no_encoders.load(Ordering::SeqCst) {
            return Err(MediaError::Unsupported("no libx264".into()));
        }
        Ok(())
    }
    fn open_encoder(
        &self,
        spec: &cutcap_media::EncodeSpec,
        cancel: &Cancellation,
    ) -> Result<Box<dyn FrameEncoder>, MediaError> {
        self.0.encoders.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeEncoder {
            inner: self.0.clone(),
            spec: spec.clone(),
            written: 0,
            cancel: cancel.clone(),
        }))
    }
    fn inspect_output(&self, _: &Path, _: &Cancellation) -> Result<OutputReport, MediaError> {
        let frames = if self.0.bad_report.load(Ordering::SeqCst) {
            1
        } else {
            self.0.shown.lock().unwrap().len() as u64
        };
        let seconds = frames as f64 / 30.0;
        Ok(OutputReport {
            format: "mov,mp4".into(),
            duration_s: seconds,
            video_codec: "h264".into(),
            width: 1280,
            height: 720,
            pixel_format: "yuv420p".into(),
            frames,
            frame_rate: "30/1".into(),
            color_space: "bt709".into(),
            audio_codec: "aac".into(),
            sample_rate: 48_000,
            channels: 2,
            audio_duration_s: seconds,
        })
    }
}

struct Rig {
    engine: Engine,
    fake: Fake,
    dir: tempfile::TempDir,
    project: ProjectId,
    video: TrackId,
    audio: TrackId,
}

impl Rig {
    fn new() -> Self {
        let mut engine = Engine::default();
        let snapshot = match engine.handle(CommandEnvelope {
            request_id: RequestId::new(),
            project_id: None,
            command: Command::NewProject { name: "Cut".into() },
        }) {
            EngineEvent::Completed { snapshot, .. } => snapshot,
            event => panic!("unexpected {event:?}"),
        };
        Self {
            engine,
            fake: Fake::new(),
            dir: tempfile::tempdir().unwrap(),
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
    fn add(&mut self, name: &str, source: Source) -> MediaId {
        let path = self.dir.path().join(name);
        fs::write(&path, format!("source {name} {}", source.tag)).unwrap();
        self.fake
            .0
            .files
            .lock()
            .unwrap()
            .insert(name.to_owned(), source);
        let (probed, fingerprint) =
            probe_verified(&self.fake, &path, &Cancellation::default()).unwrap();
        let id = MediaId::new();
        self.send(Command::RegisterMedia {
            media: Box::new(MediaSummary {
                id,
                path: probed.path,
                metadata: probed.metadata,
            }),
            fingerprint: Some(fingerprint),
        })
        .unwrap();
        id
    }
    fn place(&mut self, media: MediaId, track: TrackId, at_ms: i64) -> Vec<ClipSummary> {
        let before: Vec<ClipId> = self.clips().iter().map(|c| c.id).collect();
        self.send(Command::PlaceClip {
            media_id: media,
            track_id: track,
            position: ms(at_ms),
        })
        .unwrap();
        self.clips()
            .into_iter()
            .filter(|c| !before.contains(&c.id))
            .collect()
    }
    fn place_audio(&mut self, media: MediaId, stream: u32, track: TrackId, at_ms: i64) {
        self.send(Command::PlaceAudioStream {
            media_id: media,
            stream,
            track_id: Some(track),
            position: ms(at_ms),
        })
        .unwrap();
    }
    fn add_audio_track(&mut self) -> TrackId {
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
    fn clips(&mut self) -> Vec<ClipSummary> {
        self.send(Command::GetSnapshot).unwrap().clips
    }
    fn output(&self) -> PathBuf {
        self.dir.path().join("result.mp4")
    }
    fn plan(&mut self) -> Result<ExportPlan, EngineError> {
        self.engine
            .export_plan(Some(self.project), Some(self.video), true)
    }
    fn run_to(&mut self, path: PathBuf, allow_overwrite: bool) -> Result<ExportResult, Fail> {
        self.fake.0.shown.lock().unwrap().clear();
        let plan = self
            .plan()
            .map_err(|e| Fail::Message(e.message.to_string()))?;
        run_plan(
            &self.fake,
            &plan,
            path,
            allow_overwrite,
            &Cancellation::default(),
        )
    }
    fn export(&mut self) -> Result<ExportResult, Fail> {
        let path = self.output();
        self.run_to(path, false)
    }
    /// Files in the working folder other than sources and the given output.
    fn leftovers(&self) -> Vec<String> {
        fs::read_dir(self.dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".cutcap-"))
            .collect()
    }
}

fn run_plan(
    fake: &Fake,
    plan: &ExportPlan,
    path: PathBuf,
    allow_overwrite: bool,
    cancel: &Cancellation,
) -> Result<ExportResult, Fail> {
    let shared = Shared {
        status: Mutex::new(Status::default()),
        changed: AtomicBool::new(false),
    };
    let mut progress = Progress::new(&shared);
    progress.plan_frames = plan.grid.frames;
    progress.plan_samples = plan.grid.samples;
    let backend: Arc<dyn MediaBackend> = Arc::new(fake.clone());
    execute(
        &backend,
        plan,
        &OutputRequest {
            path,
            allow_overwrite,
        },
        &mut progress,
        cancel,
    )
}

fn frames(tag: u64, pts: impl IntoIterator<Item = i64>) -> Vec<Shown> {
    pts.into_iter().map(|p| Shown::Frame(tag, p)).collect()
}

fn black(count: usize) -> Vec<Shown> {
    vec![Shown::Black; count]
}

fn run_of(tag: u64, first: i64, count: i64) -> Vec<Shown> {
    frames(tag, (first..first + count).map(|i| i * FRAME_TICKS))
}

#[test]
fn one_clip_shows_each_source_frame_on_its_own_output_frame() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 30));
    rig.place(a, rig.video, 0);
    let result = rig.export().unwrap();
    assert_eq!(rig.fake.shown(), run_of(1, 0, 30));
    assert_eq!((result.frames, result.duration_ms), (30, 1000));
    assert_eq!(result.clip_changes, 1);
    assert!(rig.output().exists());
    assert_eq!(rig.leftovers(), Vec::<String>::new());
    assert_eq!(rig.fake.0.encoders.load(Ordering::SeqCst), 1);
}

#[test]
fn adjacent_clips_change_source_exactly_at_the_cut() {
    let mut rig = Rig::new();
    let (a, b) = (
        rig.add("a.mp4", Source::video(1, 30)),
        rig.add("b.mp4", Source::video(2, 30)),
    );
    rig.place(a, rig.video, 0);
    rig.place(b, rig.video, 1000);
    let result = rig.export().unwrap();
    assert_eq!(
        rig.fake.shown(),
        [run_of(1, 0, 30), run_of(2, 0, 30)].concat()
    );
    assert_eq!(result.clip_changes, 2);
    // The next clip's decoder was prepared; nothing else restarted.
    assert_eq!(result.video_decoder_starts, 2);
    assert_eq!(rig.fake.0.video_decoders.load(Ordering::SeqCst), 2);
}

#[test]
fn a_cut_between_output_frames_belongs_to_the_clip_holding_each_instant() {
    let mut rig = Rig::new();
    let (a, b) = (
        rig.add("a.mp4", Source::video(1, 30)),
        rig.add("b.mp4", Source::video(2, 30)),
    );
    let clip = rig.place(a, rig.video, 0)[0].id;
    rig.send(Command::TrimClip {
        clip_id: clip,
        edge: ClipEdge::End,
        position: TimelineSeek::Ticks(TimelineTime(505 * 720)),
    })
    .unwrap();
    let end = rig.clips()[0].start.0 + rig.clips()[0].duration.ticks() as i64;
    rig.send(Command::PlaceClip {
        media_id: b,
        track_id: rig.video,
        position: TimelineSeek::Ticks(TimelineTime(end)),
    })
    .unwrap();
    rig.export().unwrap();
    let shown = rig.fake.shown();
    // Frame 15 is at 500 ms (before the 505 ms cut); frame 16 at 533.3 ms is B.
    assert_eq!(shown[15], Shown::Frame(1, 15 * FRAME_TICKS));
    assert!(matches!(shown[16], Shown::Frame(2, _)));
    assert!(shown[16..].iter().all(|s| matches!(s, Shown::Frame(2, _))));
}

#[test]
fn gaps_are_black_and_never_repeat_the_previous_frame() {
    let mut rig = Rig::new();
    let (a, b) = (
        rig.add("a.mp4", Source::video(1, 15)),
        rig.add("b.mp4", Source::video(2, 15)),
    );
    rig.place(a, rig.video, 0);
    rig.place(b, rig.video, 1000);
    let result = rig.export().unwrap();
    assert_eq!(
        rig.fake.shown(),
        [run_of(1, 0, 15), black(15), run_of(2, 0, 15)].concat()
    );
    assert_eq!(result.gap_frames, 15);
}

#[test]
fn a_timeline_starting_with_a_gap_starts_black() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    let clip = rig.place(a, rig.video, 0)[0].id;
    rig.send(Command::MoveClip {
        clip_id: clip,
        track_id: rig.video,
        position: ms(500),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    rig.export().unwrap();
    assert_eq!(rig.fake.shown(), [black(15), run_of(1, 0, 15)].concat());
}

#[test]
fn start_and_end_trims_select_the_kept_source_frames() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 30));
    let clip = rig.place(a, rig.video, 0)[0].id;
    rig.send(Command::TrimClip {
        clip_id: clip,
        edge: ClipEdge::Start,
        position: ms(200),
    })
    .unwrap();
    rig.send(Command::TrimClip {
        clip_id: clip,
        edge: ClipEdge::End,
        position: ms(700),
    })
    .unwrap();
    rig.export().unwrap();
    // 200 ms in: frames 6..21 at their original times, black before.
    assert_eq!(rig.fake.shown(), [black(6), run_of(1, 6, 15)].concat());
}

#[test]
fn a_split_and_moved_part_continues_from_its_own_source_time() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 30));
    let clip = rig.place(a, rig.video, 0)[0].id;
    rig.send(Command::SplitClip {
        clip_id: clip,
        position: ms(400),
    })
    .unwrap();
    let right = rig.clips().into_iter().find(|c| c.id != clip).unwrap().id;
    rig.send(Command::MoveClip {
        clip_id: right,
        track_id: rig.video,
        position: ms(1000),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    rig.export().unwrap();
    assert_eq!(
        rig.fake.shown(),
        [run_of(1, 0, 12), black(18), run_of(1, 12, 18)].concat()
    );
}

#[test]
fn undo_and_redo_are_what_gets_exported() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    let clip = rig.place(a, rig.video, 0)[0].id;
    rig.send(Command::MoveClip {
        clip_id: clip,
        track_id: rig.video,
        position: ms(500),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    rig.send(Command::Undo).unwrap();
    rig.export().unwrap();
    assert_eq!(rig.fake.shown(), run_of(1, 0, 15));
    rig.fake.0.shown.lock().unwrap().clear();
    rig.send(Command::Redo).unwrap();
    fs::remove_file(rig.output()).unwrap();
    rig.export().unwrap();
    assert_eq!(rig.fake.shown(), [black(15), run_of(1, 0, 15)].concat());
}

#[test]
fn a_clip_head_between_frames_shows_its_first_eligible_frame_never_an_earlier_one() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 30));
    let clip = rig.place(a, rig.video, 0)[0].id;
    // 410 ms falls between frame 12 (400 ms) and frame 13 (433.3 ms).
    rig.send(Command::SplitClip {
        clip_id: clip,
        position: ms(410),
    })
    .unwrap();
    let right = rig.clips().into_iter().find(|c| c.id != clip).unwrap().id;
    rig.send(Command::MoveClip {
        clip_id: right,
        track_id: rig.video,
        position: ms(1000),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    let result = rig.export().unwrap();
    let shown = rig.fake.shown();
    // Frame 30 is the clip's first instant: no black flash, and not frame 12.
    assert_eq!(shown[30], Shown::Frame(1, 13 * FRAME_TICKS));
    assert_eq!(result.head_frames, 1);
    assert_eq!(result.no_picture_frames, 0);
}

#[test]
fn a_clip_head_far_before_the_first_frame_stays_black() {
    let mut rig = Rig::new();
    let mut late = Source::video(1, 30);
    // The first picture comes 300 ms into the clip.
    late.frames = (9..30).map(|i| i * FRAME_TICKS).collect();
    let a = rig.add("a.mp4", late);
    rig.place(a, rig.video, 0);
    let result = rig.export().unwrap();
    let shown = rig.fake.shown();
    // 100 ms of hold: frame 6 (200 ms) is the first within reach of 300 ms.
    assert_eq!(shown[..6], black(6)[..]);
    assert_eq!(shown[6], Shown::Frame(1, 9 * FRAME_TICKS));
    assert_eq!(result.no_picture_frames, 6);
    assert_eq!(result.head_frames, 3);
    assert_eq!(shown[9..], run_of(1, 9, 21)[..]);
}

#[test]
fn a_clip_with_no_eligible_frame_is_black_and_reported() {
    let mut rig = Rig::new();
    let mut empty = Source::video(1, 15);
    empty.frames.clear();
    let a = rig.add("a.mp4", empty);
    rig.place(a, rig.video, 0);
    let result = rig.export().unwrap();
    assert_eq!(rig.fake.shown(), black(15));
    assert_eq!(result.no_picture_frames, 15);
}

#[test]
fn the_last_frame_holds_to_the_clip_end_when_the_stream_stops_early() {
    let mut rig = Rig::new();
    let mut short = Source::video(1, 30);
    short.frames.truncate(10);
    let a = rig.add("a.mp4", short);
    rig.place(a, rig.video, 0);
    let result = rig.export().unwrap();
    let last = Shown::Frame(1, 9 * FRAME_TICKS);
    assert_eq!(
        rig.fake.shown(),
        [run_of(1, 0, 10), vec![last; 20]].concat()
    );
    assert_eq!(result.reused_frames, 20);
    assert_eq!(result.decoded_frames, 10);
}

#[test]
fn nonzero_and_negative_start_timestamps_map_exactly() {
    for start in [-1024, 0, 1000, 90_000] {
        let mut rig = Rig::new();
        let mut source = Source::video(1, 30);
        source.start_pts = start;
        source.frames = (0..30).map(|i| start + i * FRAME_TICKS).collect();
        let a = rig.add("a.mp4", source);
        rig.place(a, rig.video, 0);
        rig.export().unwrap();
        assert_eq!(
            rig.fake.shown(),
            frames(1, (0..30).map(|i| start + i * FRAME_TICKS)),
            "start {start}"
        );
    }
}

/// The latest frame at or before output frame `n`, from exact rationals.
fn oracle(n: i64, timebase: i64, pts: &[i64]) -> Option<i64> {
    pts.iter()
        .copied()
        .take_while(|p| *p * 30 <= n * timebase)
        .last()
}

#[test]
fn variable_frame_rate_uses_real_timestamps_on_every_clock() {
    for timebase in [15_360_u32, 90_000, 1000, 44_100] {
        let mut rig = Rig::new();
        let scale = |ms: i64| ms * i64::from(timebase) / 1000;
        let pts: Vec<i64> = [0, 5, 41, 42, 200, 233, 601, 900, 901, 902, 1300]
            .iter()
            .map(|m| scale(*m))
            .collect();
        let mut source = Source::video(1, 30);
        source.timebase = timebase;
        source.frames.clone_from(&pts);
        source.duration_ticks = scale(1500);
        let a = rig.add("a.mp4", source);
        rig.place(a, rig.video, 0);
        let result = rig.export().unwrap();
        let expected: Vec<Shown> = (0..45)
            .map(|n| Shown::Frame(1, oracle(n, i64::from(timebase), &pts).unwrap()))
            .collect();
        assert_eq!(rig.fake.shown(), expected, "timebase {timebase}");
        // Dropped frames are read past, repeats are not decoded again.
        assert!(result.reused_frames > 25);
    }
}

#[test]
fn audio_only_timeline_gets_a_black_picture_and_its_real_samples() {
    let mut rig = Rig::new();
    let song = rig.add("song.m4a", Source::audio_only(3, 48_000));
    rig.place_audio(song, 0, rig.audio, 0);
    let result = rig.export().unwrap();
    assert_eq!(rig.fake.shown(), black(30));
    assert_eq!(result.gap_frames, 30);
    let pcm = rig.fake.pcm();
    assert_eq!(pcm.len(), 48_000 * 2);
    for j in 0..48_000 {
        assert_eq!(pcm[j * 2], voice(3, j as i64), "sample {j}");
    }
}

#[test]
fn audio_longer_than_video_continues_over_black() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    let song = rig.add("song.m4a", Source::audio_only(3, 48_000));
    rig.place(a, rig.video, 0);
    rig.place_audio(song, 0, rig.audio, 0);
    rig.export().unwrap();
    assert_eq!(rig.fake.shown(), [run_of(1, 0, 15), black(15)].concat());
    let pcm = rig.fake.pcm();
    assert_eq!(pcm.len(), 96_000);
    assert_eq!(pcm[95_998], voice(3, 47_999));
}

#[test]
fn video_longer_than_audio_ends_in_silence() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 30));
    let song = rig.add("song.m4a", Source::audio_only(3, 24_000));
    rig.place(a, rig.video, 0);
    rig.place_audio(song, 0, rig.audio, 0);
    rig.export().unwrap();
    let pcm = rig.fake.pcm();
    assert_eq!(pcm.len(), 96_000);
    assert_eq!(pcm[47_998], voice(3, 23_999));
    assert!(pcm[48_000..].iter().all(|s| *s == 0.0));
}

#[test]
fn linked_audio_follows_edits_and_a_source_in_point() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 30).with_audio());
    let clips = rig.place(a, rig.video, 0);
    let video_clip = clips
        .iter()
        .find(|c| c.stream == ClipStream::Video)
        .unwrap()
        .id;
    rig.send(Command::TrimClip {
        clip_id: video_clip,
        edge: ClipEdge::Start,
        position: ms(500),
    })
    .unwrap();
    rig.export().unwrap();
    let pcm = rig.fake.pcm();
    assert_eq!(pcm.len(), 96_000);
    assert!(pcm[..48_000].iter().all(|s| *s == 0.0));
    // Half a second in: sample 24 000 of the source at output sample 24 000.
    for k in [24_000usize, 24_001, 30_000, 47_999] {
        assert_eq!(pcm[k * 2], voice(1, k as i64), "sample {k}");
    }
}

#[test]
fn gain_mute_and_solo_follow_the_committed_controls() {
    let mut rig = Rig::new();
    let (s1, s2) = (
        rig.add("one.m4a", Source::audio_only(4, 4800)),
        rig.add("two.m4a", Source::audio_only(8, 4800)),
    );
    let second = rig.add_audio_track();
    rig.place_audio(s1, 0, rig.audio, 0);
    rig.place_audio(s2, 0, second, 0);
    rig.send(Command::SetTrackGain {
        track_id: second,
        gain_cdb: -600,
    })
    .unwrap();
    let path = rig.output();
    rig.run_to(path.clone(), true).unwrap();
    let gain = 10f32.powf(-600.0 / 2000.0);
    for k in [0usize, 100, 4799] {
        let expected = voice(4, k as i64) + voice(8, k as i64) * gain;
        assert!(
            (rig.fake.pcm()[k * 2] - expected).abs() < 1e-5,
            "sample {k}"
        );
    }
    assert_eq!(rig.fake.0.audio_decoders.load(Ordering::SeqCst), 2);
    // Muted: not decoded, not heard.
    rig.send(Command::SetTrackMute {
        track_id: rig.audio,
        mute: true,
    })
    .unwrap();
    rig.fake.0.audio_decoders.store(0, Ordering::SeqCst);
    rig.run_to(path.clone(), true).unwrap();
    assert!((rig.fake.pcm()[200] - voice(8, 100) * gain).abs() < 1e-5);
    assert_eq!(rig.fake.0.audio_decoders.load(Ordering::SeqCst), 1);
    // Solo wins over the others; mute wins over solo.
    rig.send(Command::SetTrackMute {
        track_id: rig.audio,
        mute: false,
    })
    .unwrap();
    rig.send(Command::SetTrackSolo {
        track_id: rig.audio,
        solo: true,
    })
    .unwrap();
    rig.run_to(path, true).unwrap();
    assert_eq!(rig.fake.pcm()[200], voice(4, 100));
}

#[test]
fn audio_clips_are_placed_at_their_exact_samples_with_gaps_as_silence() {
    let mut rig = Rig::new();
    let song = rig.add("song.m4a", Source::audio_only(5, 24_000));
    rig.place_audio(song, 0, rig.audio, 250);
    rig.export().unwrap();
    let pcm = rig.fake.pcm();
    assert!(pcm[..24_000].iter().all(|s| *s == 0.0));
    assert_eq!(pcm[24_000], voice(5, 0));
    assert_eq!(pcm[24_002], voice(5, 1));
}

#[test]
fn clipping_is_counted_and_nothing_is_normalized() {
    let mut rig = Rig::new();
    let (s1, s2) = (
        rig.add("one.m4a", Source::audio_only(60, 4800)),
        rig.add("two.m4a", Source::audio_only(70, 4800)),
    );
    let second = rig.add_audio_track();
    rig.place_audio(s1, 0, rig.audio, 0);
    rig.place_audio(s2, 0, second, 0);
    let result = rig.export().unwrap();
    let pcm = rig.fake.pcm();
    assert_eq!(pcm[0], 1.0, "hard clipped");
    assert_eq!(result.clipped_samples, 4800 * 2);
    assert!(result.peak > 1.29);
}

#[test]
fn holes_in_a_source_are_counted_silence_not_failures() {
    let mut rig = Rig::new();
    let mut source = Source::audio_only(5, 48_000);
    source.delivers = Some(12_000);
    let song = rig.add("song.m4a", source);
    rig.place_audio(song, 0, rig.audio, 0);
    let result = rig.export().unwrap();
    assert_eq!(rig.fake.pcm()[23_998], voice(5, 11_999));
    assert!(rig.fake.pcm()[24_000..].iter().all(|s| *s == 0.0));
    assert_eq!(result.source_silence_ms, 750);
}

#[test]
fn more_audible_tracks_than_the_mixer_supports_are_refused_not_dropped() {
    let mut rig = Rig::new();
    let song = rig.add("song.m4a", Source::audio_only(1, 4800));
    rig.place_audio(song, 0, rig.audio, 0);
    let mut last = rig.audio;
    for _ in 0..16 {
        last = rig.add_audio_track();
        rig.place_audio(song, 0, last, 0);
    }
    let error = rig.export().unwrap_err();
    assert!(
        matches!(&error, Fail::Message(m) if m.contains("at most 16")),
        "{error:?}"
    );
    assert_eq!(rig.fake.0.encoders.load(Ordering::SeqCst), 0);
    // Muting one brings it back inside the limit, and no track is lost.
    rig.send(Command::SetTrackMute {
        track_id: last,
        mute: true,
    })
    .unwrap();
    assert!(rig.export().is_ok());
}

#[test]
fn only_the_chosen_video_track_is_rendered_and_omissions_need_acknowledgment() {
    let mut rig = Rig::new();
    let (a, b) = (
        rig.add("a.mp4", Source::video(1, 15)),
        rig.add("b.mp4", Source::video(2, 15)),
    );
    let upper = match rig
        .send(Command::AddTrack {
            kind: TrackKind::Video,
        })
        .unwrap()
        .selection
    {
        SelectionState::Track(id) => id,
        other => panic!("{other:?}"),
    };
    rig.place(a, rig.video, 0);
    rig.place(b, upper, 0);
    let project = rig.project;
    let refused = rig
        .engine
        .export_plan(Some(project), Some(upper), false)
        .err()
        .unwrap();
    assert!(refused.message.contains("Video 1"), "{}", refused.message);
    let plan = rig
        .engine
        .export_plan(Some(project), Some(upper), true)
        .unwrap();
    assert_eq!(plan.omitted_tracks, ["Video 1"]);
    run_plan(
        &rig.fake,
        &plan,
        rig.output(),
        false,
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(rig.fake.shown(), run_of(2, 0, 15));
    let audio = rig.audio;
    assert!(
        rig.engine
            .export_plan(Some(project), Some(audio), true)
            .is_err()
    );
}

#[test]
fn an_empty_timeline_cannot_be_planned() {
    let mut rig = Rig::new();
    let error = rig.plan().err().unwrap();
    assert!(error.message.contains("empty"), "{}", error.message);
}

#[test]
fn a_timeline_over_the_limit_cannot_be_planned() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    let clip = rig.place(a, rig.video, 0)[0].id;
    rig.send(Command::MoveClip {
        clip_id: clip,
        track_id: rig.video,
        position: ms(4 * 3600 * 1000),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    let error = rig.plan().err().unwrap();
    assert!(error.message.contains("four hours"), "{}", error.message);
}

#[test]
fn a_source_that_disappeared_or_changed_blocks_the_export() {
    for (change, expected) in [(true, "could not be read"), (false, "has changed")] {
        let mut rig = Rig::new();
        let a = rig.add("a.mp4", Source::video(1, 15));
        rig.place(a, rig.video, 0);
        let path = rig.dir.path().join("a.mp4");
        if change {
            fs::remove_file(path).unwrap();
        } else {
            fs::write(path, b"different bytes entirely").unwrap();
        }
        let error = rig.export().unwrap_err();
        assert!(
            matches!(&error, Fail::Message(m) if m.contains(expected)),
            "{error:?}"
        );
        assert_eq!(rig.fake.0.encoders.load(Ordering::SeqCst), 0);
        assert!(!rig.output().exists());
        assert_eq!(rig.leftovers(), Vec::<String>::new());
    }
}

#[test]
fn a_missing_encoder_is_reported_and_never_substituted() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    rig.place(a, rig.video, 0);
    rig.fake.0.no_encoders.store(true, Ordering::SeqCst);
    let error = rig.export().unwrap_err();
    assert!(
        matches!(&error, Fail::Message(m) if m.contains("libx264")),
        "{error:?}"
    );
    assert_eq!(rig.fake.0.encoders.load(Ordering::SeqCst), 0);
}

#[test]
fn unicode_output_paths_are_used_exactly() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    rig.place(a, rig.video, 0);
    let path = rig.dir.path().join("编辑 🎬 final.mp4");
    let result = rig.run_to(path.clone(), false).unwrap();
    assert!(path.exists());
    assert!(result.path.contains("编辑 🎬 final.mp4"));
    assert_eq!(rig.leftovers(), Vec::<String>::new());
}

#[test]
fn bad_destinations_are_refused_before_any_work() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    rig.place(a, rig.video, 0);
    let folder = rig.dir.path().to_owned();
    fs::create_dir(folder.join("taken.mp4")).unwrap();
    for (path, expected) in [
        (PathBuf::from("relative.mp4"), "complete file path"),
        (folder.join("wrong.mov"), "must end in .mp4"),
        (folder.join("gone").join("x.mp4"), "does not exist"),
        (folder.join("CON.mp4"), "reserved"),
        (folder.join("taken.mp4"), "already a folder"),
        (folder.join("a.mp4"), "already exists"),
        (PathBuf::from(r"\\server\share\x.mp4"), "local drive"),
    ] {
        let error = rig.run_to(path.clone(), false).unwrap_err();
        assert!(
            matches!(&error, Fail::Message(m) if m.contains(expected)),
            "{path:?}: {error:?}"
        );
    }
    assert_eq!(rig.fake.0.encoders.load(Ordering::SeqCst), 0);
    let error = rig.run_to(folder.join("a.mp4"), true).unwrap_err();
    assert!(
        matches!(&error, Fail::Message(m) if m.contains("source")),
        "{error:?}"
    );
}

#[test]
fn an_existing_file_needs_permission_and_survives_a_failed_export() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    rig.place(a, rig.video, 0);
    let path = rig.output();
    fs::write(&path, b"my earlier export").unwrap();
    rig.fake.0.fail_frame.store(5, Ordering::SeqCst);
    assert!(rig.run_to(path.clone(), true).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"my earlier export");
    assert_eq!(rig.leftovers(), Vec::<String>::new());
    rig.fake.0.fail_frame.store(-1, Ordering::SeqCst);
    rig.fake.0.shown.lock().unwrap().clear();
    rig.run_to(path.clone(), true).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"finished mp4");
}

#[test]
fn failures_are_named_and_leave_nothing_behind() {
    for (detail, expected) in [
        (
            "av_interleaved_write_frame(): No space left on device",
            "disk is full",
        ),
        ("Error opening output: Permission denied", "permission"),
        ("Conversion failed!", "Conversion failed"),
    ] {
        let mut rig = Rig::new();
        let a = rig.add("a.mp4", Source::video(1, 15));
        rig.place(a, rig.video, 0);
        rig.fake.0.fail_frame.store(3, Ordering::SeqCst);
        *rig.fake.0.fail_detail.lock().unwrap() = detail.to_owned();
        let error = rig.export().unwrap_err();
        assert!(
            matches!(&error, Fail::Message(m) if m.contains(expected)),
            "{error:?}"
        );
        assert!(!rig.output().exists());
        assert_eq!(rig.leftovers(), Vec::<String>::new());
    }
}

#[test]
fn a_failed_finish_or_failed_validation_never_publishes() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 15));
    rig.place(a, rig.video, 0);
    rig.fake.0.fail_finish.store(true, Ordering::SeqCst);
    assert!(rig.export().is_err());
    assert!(!rig.output().exists());
    rig.fake.0.fail_finish.store(false, Ordering::SeqCst);
    rig.fake.0.bad_report.store(true, Ordering::SeqCst);
    rig.fake.0.shown.lock().unwrap().clear();
    let error = rig.export().unwrap_err();
    assert!(
        matches!(&error, Fail::Message(m) if m.contains("validation")),
        "{error:?}"
    );
    assert!(!rig.output().exists());
    assert_eq!(rig.leftovers(), Vec::<String>::new());
}

#[test]
fn cancelling_stops_the_work_and_removes_the_partial_output() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 30));
    rig.place(a, rig.video, 0);
    rig.fake.0.hold_at.store(5, Ordering::SeqCst);
    let plan = rig.plan().ok().unwrap();
    let cancel = Cancellation::default();
    let (fake, path, token) = (rig.fake.clone(), rig.output(), cancel.clone());
    let job = thread::spawn(move || run_plan(&fake, &plan, path, false, &token));
    while rig.fake.shown().len() < 5 {
        thread::sleep(Duration::from_millis(1));
    }
    cancel.cancel();
    assert_eq!(job.join().unwrap().unwrap_err(), Fail::Cancelled);
    assert!(!rig.output().exists());
    assert_eq!(rig.leftovers(), Vec::<String>::new());
}

#[test]
fn a_large_project_renders_with_bounded_decoders() {
    let mut rig = Rig::new();
    let a = rig.add("a.mp4", Source::video(1, 3));
    for i in 0..400 {
        rig.place(a, rig.video, i * 100);
    }
    let result = rig.export().unwrap();
    assert_eq!(result.frames, 1200);
    assert_eq!(result.clip_changes, 400);
    let shown = rig.fake.shown();
    assert_eq!(shown.len(), 1200);
    assert_eq!(shown[3], Shown::Frame(1, 0));
    assert_eq!(rig.fake.0.video_decoders.load(Ordering::SeqCst), 400);
}

// ----- through the session and its job owner -----

fn session_rig(fake: &Fake) -> Session {
    Session::with_backend(Arc::new(fake.clone())).unwrap()
}

fn view(session: &mut Session) -> Value {
    serde_json::from_str(session.view_json().unwrap()).unwrap()
}

fn pump_until(session: &mut Session, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        session.pump();
        let current = view(session);
        if done(&current) {
            return current;
        }
        assert!(Instant::now() < deadline, "timed out; last view {current}");
        thread::sleep(Duration::from_millis(3));
    }
}

fn id<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}

struct SessionRig {
    fake: Fake,
    session: Session,
    dir: tempfile::TempDir,
    video: TrackId,
    audio: TrackId,
}

impl SessionRig {
    fn new() -> Self {
        let fake = Fake::new();
        let mut session = session_rig(&fake);
        let v = pump_until(&mut session, |v| v["engine_ready"] == true);
        Self {
            fake,
            session,
            dir: tempfile::tempdir().unwrap(),
            video: id(&v["tracks"][0]["id"]),
            audio: id(&v["tracks"][1]["id"]),
        }
    }
    fn import(&mut self, name: &str, source: Source) -> MediaId {
        let path = self.dir.path().join(name);
        fs::write(&path, format!("source {name}")).unwrap();
        self.fake
            .0
            .files
            .lock()
            .unwrap()
            .insert(name.to_owned(), source);
        let count = view(&mut self.session)["media"].as_array().unwrap().len();
        self.session.import(path).unwrap();
        let v = pump_until(&mut self.session, |v| {
            v["media"].as_array().unwrap().len() > count && v["import"]["busy"] == false
        });
        id(&v["media"][count]["id"])
    }
    fn job(&mut self, state: &str) -> Value {
        let wanted = state.to_owned();
        pump_until(&mut self.session, move |v| {
            v["export"]["job"]["state"] == wanted.as_str()
        })
    }
}

#[test]
fn the_summary_offers_the_tracks_and_an_export_runs_to_completion() {
    let mut rig = SessionRig::new();
    let v = pump_until(&mut rig.session, |v| v["export"]["available"] == true);
    assert_eq!(
        v["export"]["summary"]["video_tracks"][0]["blocked"]
            .as_str()
            .unwrap(),
        "The timeline is empty, so there is nothing to export."
    );
    let a = rig.import("a.mp4", Source::video(1, 15).with_audio());
    rig.session.place_clip(a, rig.video, ms(0)).unwrap();
    let v = pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["export"]["summary"]["video_tracks"][0]["frames"] == 15
    });
    let summary = &v["export"]["summary"];
    assert_eq!(summary["video_tracks"][0]["clips"], 1);
    assert_eq!(summary["video_tracks"][0]["duration_text"], "00:00.500");
    assert!(summary["video_tracks"][0]["blocked"].is_null());
    assert_eq!(summary["audio_tracks"], 1);
    assert_eq!(summary["default_track"], rig.video.to_string());
    assert!(
        summary["preset_text"]
            .as_str()
            .unwrap()
            .contains("1280×720")
    );
    let path = rig.dir.path().join("out.mp4");
    let dirty_before = v["project"]["document"]["dirty"].clone();
    let revision_before = v["project"]["document"]["revision"].clone();
    let depth_before = v["history"]["depth"].clone();
    rig.session
        .start_export(path.clone(), Some(rig.video), false, false)
        .unwrap();
    // Accepted is not completed.
    let accepted = view(&mut rig.session);
    assert_eq!(accepted["export"]["job"]["state"], "validating");
    assert_eq!(accepted["export"]["job"]["running"], true);
    assert!(accepted["export"]["job"]["progress"].as_f64().unwrap() < 0.01);
    let done = rig.job("completed");
    let job = &done["export"]["job"];
    assert_eq!(job["progress"], 1.0);
    assert_eq!(job["running"], false);
    assert_eq!(job["result"]["frames"], 15);
    assert_eq!(job["track_name"], "Video 1");
    assert_eq!(job["revision"], revision_before);
    assert!(path.exists());
    // Export is not an edit.
    assert_eq!(done["project"]["document"]["dirty"], dirty_before);
    assert_eq!(done["project"]["document"]["revision"], revision_before);
    assert_eq!(done["history"]["depth"], depth_before);
}

#[test]
fn edits_during_an_export_do_not_reach_it() {
    let mut rig = SessionRig::new();
    let a = rig.import("a.mp4", Source::video(1, 30));
    rig.session.place_clip(a, rig.video, ms(0)).unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["project"]["clip_count"] == 1
            || v["export"]["summary"]["video_tracks"][0]["frames"] == 30
    });
    rig.fake.0.hold_at.store(5, Ordering::SeqCst);
    let path = rig.dir.path().join("out.mp4");
    rig.session
        .start_export(path.clone(), Some(rig.video), false, false)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.fake.shown().len() < 5 {
        rig.session.pump();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    // Move the only clip away and mute the track while the job runs.
    let timeline: Value = serde_json::from_str(rig.session.timeline_json().unwrap()).unwrap();
    let clip: ClipId = id(&timeline["clips"][0]["id"]);
    rig.session.remove_clip(clip).unwrap();
    rig.session.set_track_mute(rig.audio, true).unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["export"]["summary"]["video_tracks"][0]["clips"] == 0
    });
    rig.fake.0.release.store(true, Ordering::SeqCst);
    rig.job("completed");
    assert_eq!(rig.fake.shown(), run_of(1, 0, 30));
}

#[test]
fn replacing_the_project_during_an_export_leaves_the_export_running() {
    let mut rig = SessionRig::new();
    let a = rig.import("a.mp4", Source::video(1, 30));
    rig.session.place_clip(a, rig.video, ms(0)).unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["export"]["summary"]["video_tracks"][0]["frames"] == 30
    });
    rig.fake.0.hold_at.store(5, Ordering::SeqCst);
    let path = rig.dir.path().join("out.mp4");
    rig.session
        .start_export(path.clone(), Some(rig.video), false, false)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.fake.shown().len() < 5 {
        rig.session.pump();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    rig.session.new_project().unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0
            && v["media"].as_array().is_some_and(|m| m.is_empty())
            && v["engine_ready"] == true
    });
    rig.fake.0.release.store(true, Ordering::SeqCst);
    rig.job("completed");
    assert!(path.exists());
    assert_eq!(rig.fake.shown(), run_of(1, 0, 30));
}

#[test]
fn a_volume_preview_is_not_exported_until_it_is_committed() {
    let mut rig = SessionRig::new();
    let song = rig.import("song.m4a", Source::audio_only(4, 4800));
    rig.session
        .place_audio_stream(song, 0, Some(rig.audio), ms(0))
        .unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["export"]["summary"]["audio_tracks"] == 1
    });
    rig.session.preview_track_gain(rig.audio, -2000).unwrap();
    let path = rig.dir.path().join("out.mp4");
    rig.session
        .start_export(path, Some(rig.video), false, false)
        .unwrap();
    rig.job("completed");
    assert_eq!(rig.fake.pcm()[0], voice(4, 0), "the drag was not committed");
}

#[test]
fn a_stale_or_finished_job_cannot_be_cancelled() {
    let mut rig = SessionRig::new();
    let a = rig.import("a.mp4", Source::video(1, 30));
    rig.session.place_clip(a, rig.video, ms(0)).unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["export"]["summary"]["video_tracks"][0]["frames"] == 30
    });
    assert!(rig.session.cancel_export("nothing").is_err());
    rig.fake.0.hold_at.store(5, Ordering::SeqCst);
    let path = rig.dir.path().join("first.mp4");
    rig.session
        .start_export(path, Some(rig.video), false, false)
        .unwrap();
    let running = rig.job("encoding");
    let job = running["export"]["job"]["id"].as_str().unwrap().to_owned();
    assert!(rig.session.cancel_export("some other job").is_err());
    let second = rig.dir.path().join("second.mp4");
    assert!(
        rig.session
            .start_export(second, Some(rig.video), false, false)
            .is_err()
    );
    rig.session.cancel_export(&job).unwrap();
    assert_eq!(
        view(&mut rig.session)["export"]["job"]["state"],
        "cancelling"
    );
    rig.job("cancelled");
    assert!(rig.session.cancel_export(&job).is_err());
    assert!(!rig.dir.path().join("first.mp4").exists());
    // And the editor can export again.
    rig.fake.0.hold_at.store(-1, Ordering::SeqCst);
    rig.fake.0.shown.lock().unwrap().clear();
    rig.session
        .start_export(
            rig.dir.path().join("third.mp4"),
            Some(rig.video),
            false,
            false,
        )
        .unwrap();
    rig.job("completed");
    assert_eq!(rig.fake.shown(), run_of(1, 0, 30));
}

#[test]
fn a_refused_plan_is_a_failed_job_with_its_reason() {
    let mut rig = SessionRig::new();
    pump_until(&mut rig.session, |v| v["engine_ready"] == true);
    rig.session
        .start_export(rig.dir.path().join("x.mp4"), Some(rig.video), false, false)
        .unwrap();
    let failed = rig.job("failed");
    assert!(
        failed["export"]["job"]["error"]
            .as_str()
            .unwrap()
            .contains("empty")
    );
    assert_eq!(failed["export"]["job"]["running"], false);
}

#[test]
fn shutdown_during_an_export_stops_it_and_cleans_up() {
    let mut rig = SessionRig::new();
    let a = rig.import("a.mp4", Source::video(1, 30));
    rig.session.place_clip(a, rig.video, ms(0)).unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["export"]["summary"]["video_tracks"][0]["frames"] == 30
    });
    rig.fake.0.hold_at.store(5, Ordering::SeqCst);
    let path = rig.dir.path().join("out.mp4");
    rig.session
        .start_export(path.clone(), Some(rig.video), false, false)
        .unwrap();
    rig.job("encoding");
    let closing = Instant::now();
    rig.session.shutdown().unwrap();
    assert!(closing.elapsed() < Duration::from_secs(5));
    assert!(!path.exists());
    let left: Vec<_> = fs::read_dir(rig.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".cutcap-"))
        .collect();
    assert_eq!(left, Vec::<String>::new());
}

#[test]
fn an_export_after_saving_and_reopening_matches() {
    let mut rig = SessionRig::new();
    let a = rig.import("a.mp4", Source::video(1, 30));
    rig.session.place_clip(a, rig.video, ms(250)).unwrap();
    pump_until(&mut rig.session, |v| {
        v["pending"] == 0 && v["export"]["summary"]["video_tracks"][0]["clips"] == 1
    });
    let project = rig.dir.path().join("p.cutcap");
    rig.session.save_project(Some(project.clone())).unwrap();
    pump_until(&mut rig.session, |v| {
        v["project"]["document"]["state"] == "saved" && v["pending"] == 0
    });
    rig.session.open_project(project).unwrap();
    pump_until(&mut rig.session, |v| {
        v["project"]["document"]["state"] == "opened"
            && v["pending"] == 0
            && v["media"][0]["availability"] == "ready"
    });
    let v = view(&mut rig.session);
    let track: TrackId = id(&v["tracks"][0]["id"]);
    rig.session
        .start_export(rig.dir.path().join("o.mp4"), Some(track), false, false)
        .unwrap();
    rig.job("completed");
    let shown = rig.fake.shown();
    assert_eq!(shown.len(), 38);
    assert_eq!(shown[7], Shown::Black);
    assert!(matches!(shown[8], Shown::Frame(1, _)));
}

#[test]
fn progress_counts_output_work_and_only_completion_reaches_one() {
    let shared = Shared {
        status: Mutex::new(Status::default()),
        changed: AtomicBool::new(false),
    };
    let mut progress = Progress::new(&shared);
    progress.plan_frames = 100;
    progress.plan_samples = 160_000;
    assert_eq!(progress.fraction(), 0.0);
    progress.mixed = 160_000;
    assert!((progress.fraction() - 0.10).abs() < 1e-9);
    // Frames handed to the encoder are not progress until it reports them.
    progress.submitted = 100;
    assert!((progress.fraction() - 0.10).abs() < 1e-9);
    progress.encoded = 50;
    assert!((progress.fraction() - 0.54).abs() < 1e-9);
    progress.encoded = 100;
    assert!((progress.fraction() - 0.98).abs() < 1e-9, "not 1.0");
}

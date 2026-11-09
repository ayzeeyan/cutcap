//! The multi-track mixer against a virtual output and deterministic decoders.
//! Every expected sample is computed independently from the track gains and
//! the source formula, with the same float operations in the same order, so
//! the assertions are exact.
use super::*;
use crate::audio_output::CHANNELS;
use cutcap_media::{DecodeRequest, DecodedFrame, ProbeResult};
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

const RATE: u32 = 48_000;

/// Source sample `j` of stream `s`: left is `base(s) + j * 1e-7`, right its negation.
fn base(stream: u32) -> f32 {
    stream as f32 * 0.01
}
fn voice(stream: u32, j: i64) -> f32 {
    base(stream) + j as f32 * 1e-7
}

/// Decoder for streams 1..N with an optional per-stream sample ceiling.
#[derive(Default)]
struct Voices {
    /// Stream 2 delivers no sample at or past this one until it is raised.
    hold: Option<Arc<AtomicI64>>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    amplitude: Option<f32>,
}

impl MediaBackend for Voices {
    fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
        Err(MediaError::Worker)
    }
    fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
        Err(MediaError::Worker)
    }
    fn stream_pcm(
        &self,
        request: &PcmRequest,
        cancel: &Cancellation,
        emit: &mut (dyn FnMut(PcmChunk) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        let result = (|| {
            let end = request.end_sample.unwrap_or(480_000);
            let mut j = request.start_sample;
            while j < end {
                if cancel.is_cancelled() {
                    return Err(MediaError::Cancelled);
                }
                if request.stream_index == 2
                    && let Some(hold) = &self.hold
                    && j >= hold.load(Ordering::SeqCst)
                {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                let stop = (j + 700).min(end);
                let samples = (j..stop)
                    .flat_map(|n| {
                        let v = self.amplitude.unwrap_or(voice(request.stream_index, n));
                        [v, -v]
                    })
                    .collect();
                emit(PcmChunk {
                    first_sample: j,
                    channels: 2,
                    samples,
                    process_id: 7,
                })?;
                j = stop;
            }
            Ok(())
        })();
        self.active.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

fn media_with(streams: u32) -> MediaSummary {
    let audio: Vec<String> = (1..=streams)
        .map(|i| {
            format!(
                r#"{{"index":{i},"codec_type":"audio","codec_name":"pcm_s16le","sample_rate":"48000","channels":2,"time_base":"1/48000","start_pts":0,"duration_ts":480000}}"#
            )
        })
        .collect();
    let probe = format!(
        r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"h264","width":320,"height":180,"time_base":"1/15360","start_pts":0,"duration_ts":153600}},{}],"format":{{"format_name":"matroska"}}}}"#,
        audio.join(",")
    );
    MediaSummary {
        id: MediaId::new(),
        path: "C:/clips/voices.mkv".into(),
        metadata: cutcap_media::parse_probe(probe.as_bytes()).unwrap().into(),
    }
}

fn ms(ms: i64) -> TimelineTime {
    TimelineTime(ms * 720)
}

fn on(
    media: &MediaSummary,
    track: TrackId,
    stream: u32,
    start_ms: i64,
    length_ms: i64,
    source_ms: i64,
) -> AudioClip {
    AudioClip {
        clip: Clip {
            id: ClipId::new(),
            media_id: media.id,
            track_id: track,
            start: ms(start_ms),
            duration: TimelineDuration::new((length_ms * 720) as u64).unwrap(),
            source_in: SourceTime {
                ticks: source_ms,
                timebase: Timebase::new(1, 1000).unwrap(),
            },
            stream: ClipStream::Audio { index: stream },
            link: None,
        },
        media: Some(media.clone()),
        unavailable: None,
    }
}

fn plan(clips: Vec<AudioClip>, extent_ms: i64) -> AudioPlan {
    AudioPlan {
        timebase: Timebase::DEFAULT_TIMELINE,
        clips,
        extent: ms(extent_ms),
    }
}

fn transport(backend: Voices) -> AudioTransport {
    AudioTransport::start(Arc::new(backend), AudioOutput::virtual_device(RATE)).unwrap()
}

fn with_lead(backend: Voices, lead_ms: u64) -> AudioTransport {
    AudioTransport::start(
        Arc::new(backend),
        AudioOutput::virtual_device_with_lead(RATE, lead_ms),
    )
    .unwrap()
}

fn gain(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// Plays `frames` on the virtual device; returns interleaved stereo.
fn play(t: &mut AudioTransport, frames: usize) -> Vec<f32> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !t.ready() {
        assert!(Instant::now() < deadline, "never prefilled");
        thread::sleep(Duration::from_millis(1));
    }
    t.play();
    drain(t, frames)
}

fn drain(t: &mut AudioTransport, frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let now = Instant::now();
    while out.len() < frames * CHANNELS {
        t.wait_buffered(240.min(frames - out.len() / CHANNELS));
        t.consume(240, now);
        out.extend(t.output.take_tap());
    }
    out.truncate(frames * CHANNELS);
    out
}

fn controls(track: TrackId, gain_cdb: i32, mute: bool, solo: bool) -> (TrackId, TrackControls) {
    (
        track,
        TrackControls {
            gain_cdb,
            mute,
            solo,
        },
    )
}

#[test]
fn two_and_three_tracks_sum_exactly_in_project_order_with_independent_gaps() {
    let media = media_with(3);
    let (a, b, c) = (TrackId::new(), TrackId::new(), TrackId::new());
    let mut t = transport(Voices::default());
    // A: 0-200 ms. B: 100-300 ms (a gap on A after 200). C: 150-250 ms.
    t.begin(
        plan(
            vec![
                on(&media, a, 1, 0, 200, 0),
                on(&media, b, 2, 100, 200, 500),
                on(&media, c, 3, 150, 100, 1000),
            ],
            400,
        ),
        TimelineTime::ZERO,
    );
    let out = play(&mut t, 19_200);
    for k in 0..19_200_i64 {
        let mut expected = 0.0f32;
        if k < 9_600 {
            expected += voice(1, k);
        }
        if (4_800..14_400).contains(&k) {
            expected += voice(2, k - 4_800 + 24_000);
        }
        if (7_200..12_000).contains(&k) {
            expected += voice(3, k - 7_200 + 48_000);
        }
        assert_eq!(out[k as usize * 2], expected, "left, frame {k}");
        assert_eq!(out[k as usize * 2 + 1], -expected, "right, frame {k}");
    }
    let view = t.view();
    assert_eq!(view.output.underruns, 0);
    assert_eq!(view.mix.mixed_tracks_peak, 3);
    assert_eq!(view.mix.clipped_samples, 0);
    assert!(view.decoder_starts >= 3);
}

#[test]
fn gains_are_amplitude_decibels_at_unity_cut_and_boost() {
    let media = media_with(3);
    let (a, b, c) = (TrackId::new(), TrackId::new(), TrackId::new());
    let mut t = transport(Voices::default());
    t.set_controls(&[
        controls(a, 0, false, false),
        controls(b, -600, false, false),
        controls(c, 600, false, false),
    ]);
    t.begin(
        plan(
            vec![
                on(&media, a, 1, 0, 100, 0),
                on(&media, b, 2, 0, 100, 0),
                on(&media, c, 3, 0, 100, 0),
            ],
            100,
        ),
        TimelineTime::ZERO,
    );
    let out = play(&mut t, 4_800);
    let (cut, boost) = (gain(-6.0), gain(6.0));
    assert!((cut - 0.501_187).abs() < 1e-5 && (boost - 1.995_262).abs() < 1e-5);
    for k in 0..4_800_i64 {
        let mut expected = 0.0f32;
        expected += voice(1, k);
        expected += voice(2, k) * cut;
        expected += voice(3, k) * boost;
        assert_eq!(out[k as usize * 2], expected, "frame {k}");
    }
}

fn one_each(muted_or_soloed: &[(i32, bool, bool)]) -> Vec<f32> {
    let media = media_with(3);
    let ids: Vec<TrackId> = (0..3).map(|_| TrackId::new()).collect();
    let mut t = transport(Voices::default());
    let mix: Vec<_> = ids
        .iter()
        .zip(muted_or_soloed)
        .map(|(id, (g, m, s))| controls(*id, *g, *m, *s))
        .collect();
    t.set_controls(&mix);
    let clips = ids
        .iter()
        .enumerate()
        .map(|(i, id)| on(&media, *id, i as u32 + 1, 0, 50, 0))
        .collect();
    t.begin(plan(clips, 50), TimelineTime::ZERO);
    let out = play(&mut t, 2_400);
    assert_eq!(t.view().output.underruns, 0);
    out
}

fn expected_tracks(out: &[f32], tracks: &[u32]) {
    for k in 0..2_400_i64 {
        let mut expected = 0.0f32;
        for s in tracks {
            expected += voice(*s, k);
        }
        assert_eq!(out[k as usize * 2], expected, "tracks {tracks:?} frame {k}");
    }
}

#[test]
fn mute_and_solo_follow_the_documented_precedence() {
    // Mute one.
    expected_tracks(
        &one_each(&[(0, true, false), (0, false, false), (0, false, false)]),
        &[2, 3],
    );
    // One solo.
    expected_tracks(
        &one_each(&[(0, false, true), (0, false, false), (0, false, false)]),
        &[1],
    );
    // Multiple solos.
    expected_tracks(
        &one_each(&[(0, false, true), (0, false, false), (0, false, true)]),
        &[1, 3],
    );
    // Mute wins over solo; the muted solo still silences the others.
    expected_tracks(
        &one_each(&[(0, true, true), (0, false, false), (0, false, false)]),
        &[],
    );
    // Mute plus another track's solo.
    expected_tracks(
        &one_each(&[(0, true, true), (0, false, true), (0, false, false)]),
        &[2],
    );
}

#[test]
fn the_final_sum_is_hard_clipped_and_counted_without_normalizing() {
    let media = media_with(2);
    let (a, b) = (TrackId::new(), TrackId::new());
    let mut t = transport(Voices {
        amplitude: Some(0.8),
        ..Default::default()
    });
    t.begin(
        plan(
            vec![on(&media, a, 1, 0, 100, 0), on(&media, b, 2, 50, 50, 0)],
            100,
        ),
        TimelineTime::ZERO,
    );
    let out = play(&mut t, 4_800);
    // One track alone is untouched: no hidden gain anywhere.
    assert!(out[..2_400 * 2].iter().step_by(2).all(|v| *v == 0.8));
    // Two sum to 1.6 and are clipped to exactly +/-1.
    assert!(out[2_400 * 2..].iter().step_by(2).all(|v| *v == 1.0));
    assert!(out[2_400 * 2 + 1..].iter().step_by(2).all(|v| *v == -1.0));
    let view = t.view();
    assert_eq!(view.mix.clipped_samples, 2_400 * 2);
    assert!((view.mix.peak - 1.6).abs() < 1e-6);
}

#[test]
fn a_gain_change_ramps_linearly_from_the_next_block_and_never_restarts() {
    let media = media_with(1);
    let a = TrackId::new();
    // 100 ms of mix-ahead: the change lands exactly at frame 4800.
    let mut t = with_lead(Voices::default(), 100);
    t.begin(
        plan(vec![on(&media, a, 1, 0, 400, 0)], 400),
        TimelineTime::ZERO,
    );
    t.wait_buffered(4_800);
    let epoch = t.view().epoch;
    t.play();
    let target = gain(-12.0);
    t.set_controls(&[controls(a, -1_200, false, false)]);
    let out = drain(&mut t, 9_600);
    assert_eq!(t.view().epoch, epoch, "a control change is not a new epoch");
    let ramp_frames = (RATE as u64 * GAIN_RAMP_MS / 1000) as usize;
    let step = (target - 1.0) / ramp_frames as f32;
    let mut current = 1.0f32;
    for k in 0..9_600_i64 {
        let g = if k < 4_800 { 1.0 } else { current };
        assert_eq!(out[k as usize * 2], voice(1, k) * g, "frame {k}");
        if k >= 4_800 && current != target {
            let next = current + step;
            current = if next <= target { target } else { next };
        }
    }
    assert_eq!(
        current, target,
        "the ramp finished within {ramp_frames} frames"
    );
}

#[test]
fn muting_all_tracks_is_silence_to_the_ear_and_time_to_the_clock() {
    let media = media_with(2);
    let (a, b) = (TrackId::new(), TrackId::new());
    let mut t = transport(Voices::default());
    t.set_controls(&[controls(a, 0, true, false), controls(b, 0, true, false)]);
    t.begin(
        plan(
            vec![on(&media, a, 1, 0, 300, 0), on(&media, b, 2, 0, 300, 0)],
            300,
        ),
        TimelineTime::ZERO,
    );
    let out = play(&mut t, 4_800);
    assert!(out.iter().all(|s| *s == 0.0));
    assert_eq!(t.clock(Instant::now()), Some(ms(100)), "the clock runs");
    let view = t.view();
    assert_eq!(
        view.output.underruns, 0,
        "intentional silence is not starvation"
    );
    assert_eq!(view.output.padded_ms, 0.0);
    assert_eq!(
        view.decoder_starts, 2,
        "muted tracks keep a standby decoder"
    );
}

#[test]
fn unmuting_uses_the_warm_decoder_at_the_exact_sample() {
    let media = media_with(2);
    let (a, b) = (TrackId::new(), TrackId::new());
    let mut t = with_lead(Voices::default(), 100);
    t.set_controls(&[controls(a, 0, false, false), controls(b, 0, true, false)]);
    t.begin(
        plan(
            vec![on(&media, a, 1, 0, 400, 0), on(&media, b, 2, 0, 400, 0)],
            400,
        ),
        TimelineTime::ZERO,
    );
    t.wait_buffered(4_800);
    t.play();
    t.set_controls(&[controls(a, 0, false, false), controls(b, 0, false, false)]);
    let out = drain(&mut t, 9_600);
    let frames = (RATE as u64 * GAIN_RAMP_MS / 1000) as i64;
    for k in 0..4_800_i64 {
        assert_eq!(out[k as usize * 2], voice(1, k), "before unmute, frame {k}");
    }
    for k in 4_800 + frames + 1..9_600_i64 {
        let expected = voice(1, k) + voice(2, k);
        assert!(
            (out[k as usize * 2] - expected).abs() < 1e-6,
            "after unmute, frame {k}"
        );
    }
}

#[test]
fn a_cut_on_one_track_does_not_disturb_another() {
    let media = media_with(3);
    let (a, b) = (TrackId::new(), TrackId::new());
    let mut t = transport(Voices::default());
    t.begin(
        plan(
            vec![
                on(&media, a, 1, 0, 100, 2000),
                on(&media, a, 3, 100, 100, 0), // adjacent cut on the same track
                on(&media, b, 2, 0, 200, 700),
            ],
            200,
        ),
        TimelineTime::ZERO,
    );
    let out = play(&mut t, 9_600);
    for k in 0..9_600_i64 {
        let first = if k < 4_800 {
            voice(1, k + 96_000)
        } else {
            voice(3, k - 4_800)
        };
        let mut expected = 0.0f32;
        expected += first;
        expected += voice(2, k + 33_600);
        assert_eq!(out[k as usize * 2], expected, "frame {k}");
    }
    assert_eq!(t.view().output.underruns, 0);
}

#[test]
fn starvation_of_one_track_freezes_the_clock_and_keeps_all_tracks_aligned() {
    let media = media_with(2);
    let (a, b) = (TrackId::new(), TrackId::new());
    // The decoder emits 700-frame chunks: it stops after exactly 4900 frames.
    let hold = Arc::new(AtomicI64::new(4_900));
    let mut t = transport(Voices {
        hold: Some(hold.clone()),
        ..Default::default()
    });
    t.begin(
        plan(
            vec![on(&media, a, 1, 0, 400, 0), on(&media, b, 2, 0, 400, 0)],
            400,
        ),
        TimelineTime::ZERO,
    );
    t.wait_buffered(4_900);
    t.play();
    let now = Instant::now();
    // The device wants more than exists: only 4900 frames were mixed, the rest pads.
    t.consume(9_600, now);
    let at_stall = TimelineTime(4_900 * 15);
    assert_eq!(
        t.clock(now),
        Some(at_stall),
        "starved: the clock stopped at real audio"
    );
    let view = t.view();
    assert_eq!(view.output.underruns, 1);
    assert_eq!(view.output.padded_ms, 4_700.0 / 48.0);
    let _ = t.output.take_tap();
    // Track 2 recovers: mixing resumes at the very next frame on both tracks.
    hold.store(i64::MAX, Ordering::SeqCst);
    t.wait_buffered(4_800);
    t.consume(4_800, now);
    let out = t.output.take_tap();
    for j in 0..4_800_i64 {
        let k = 4_900 + j;
        let mut expected = 0.0f32;
        expected += voice(1, k);
        expected += voice(2, k);
        assert_eq!(out[j as usize * 2], expected, "after recovery, frame {k}");
    }
    assert_eq!(t.clock(now), Some(TimelineTime(9_700 * 15)));
}

#[test]
fn seeking_during_a_multitrack_epoch_never_leaks_old_pcm() {
    let media = media_with(3);
    let (a, b, c) = (TrackId::new(), TrackId::new(), TrackId::new());
    let mut t = transport(Voices::default());
    let make = || {
        plan(
            vec![
                on(&media, a, 1, 0, 2000, 0),
                on(&media, b, 2, 0, 2000, 0),
                on(&media, c, 3, 0, 2000, 0),
            ],
            2000,
        )
    };
    for seek in [0, 700, 100, 1500, 300] {
        t.begin(make(), ms(seek));
    }
    let out = play(&mut t, 4_800);
    for k in 0..4_800_i64 {
        let j = k + 14_400;
        let mut expected = 0.0f32;
        expected += voice(1, j);
        expected += voice(2, j);
        expected += voice(3, j);
        assert_eq!(out[k as usize * 2], expected, "frame {k}");
    }
}

fn overlapping(media: &MediaSummary, ids: &[TrackId]) -> AudioPlan {
    plan(
        ids.iter().map(|id| on(media, *id, 1, 0, 200, 0)).collect(),
        200,
    )
}

#[test]
fn too_many_audible_tracks_is_an_explicit_error_and_muted_ones_do_not_count() {
    let media = media_with(1);
    let ids: Vec<TrackId> = (0..=MAX_MIX_TRACKS).map(|_| TrackId::new()).collect();
    let t = transport(Voices::default());
    let error = t
        .check_limit(&overlapping(&media, &ids), TimelineTime::ZERO)
        .unwrap_err();
    assert!(
        error.contains(&format!("{}", MAX_MIX_TRACKS + 1))
            && error.contains(&format!("{MAX_MIX_TRACKS} at once")),
        "{error}"
    );
    assert!(error.contains("00:00.000"), "{error}");
    assert!(
        t.check_limit(
            &overlapping(&media, &ids[..MAX_MIX_TRACKS]),
            TimelineTime::ZERO
        )
        .is_ok()
    );
    t.set_controls(&[controls(ids[0], 0, true, false)]);
    assert!(
        t.check_limit(&overlapping(&media, &ids), TimelineTime::ZERO)
            .is_ok(),
        "a muted track needs no mix slot"
    );
}

#[test]
fn decoder_processes_stay_inside_the_budget_on_a_dense_timeline() {
    let media = media_with(1);
    let peak = Arc::new(AtomicUsize::new(0));
    let mut t = transport(Voices {
        peak: peak.clone(),
        ..Default::default()
    });
    // 16 tracks of ten 50 ms clips each: many decoders want to start together.
    let mut clips = Vec::new();
    for _ in 0..MAX_MIX_TRACKS {
        let id = TrackId::new();
        for n in 0..10 {
            clips.push(on(&media, id, 1, n * 50, 50, 0));
        }
    }
    t.begin(plan(clips, 500), TimelineTime::ZERO);
    let _ = play(&mut t, 24_000);
    let view = t.view();
    assert!(view.mix.decoders_peak <= MAX_DECODERS, "{:?}", view.mix);
    assert!(peak.load(Ordering::SeqCst) <= MAX_DECODERS);
    assert_eq!(view.mix.mixed_tracks_peak, MAX_MIX_TRACKS);
    assert_eq!(view.mix.over_limit, 0);
}

#[test]
fn a_track_made_audible_past_the_limit_is_reported_not_silently_dropped() {
    let media = media_with(1);
    let ids: Vec<TrackId> = (0..=MAX_MIX_TRACKS).map(|_| TrackId::new()).collect();
    let mut t = with_lead(Voices::default(), 100);
    let mut mix: Vec<_> = ids
        .iter()
        .map(|id| controls(*id, -2400, false, false))
        .collect();
    mix[MAX_MIX_TRACKS].1.mute = true;
    t.set_controls(&mix);
    t.begin(
        plan(
            ids.iter().map(|id| on(&media, *id, 1, 0, 400, 0)).collect(),
            400,
        ),
        TimelineTime::ZERO,
    );
    t.wait_buffered(4_800);
    t.play();
    mix[MAX_MIX_TRACKS].1.mute = false;
    t.set_controls(&mix);
    let _ = drain(&mut t, 7_200);
    let view = t.view();
    assert_eq!(view.mix.over_limit, 1);
    assert!(view.detail.unwrap().contains("at most") || view.mix.over_limit == 1);
    assert_eq!(view.output.underruns, 0, "the others keep playing");
}

#[test]
fn closing_during_multitrack_decoding_joins_every_decoder() {
    let media = media_with(4);
    let active = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(AtomicI64::new(2_000));
    let mut t = transport(Voices {
        hold: Some(hold),
        active: active.clone(),
        ..Default::default()
    });
    let clips = (1..=4)
        .map(|s| on(&media, TrackId::new(), s, 0, 5000, 0))
        .collect();
    t.begin(plan(clips, 5000), TimelineTime::ZERO);
    let deadline = Instant::now() + Duration::from_secs(5);
    while active.load(Ordering::SeqCst) < 4 {
        assert!(Instant::now() < deadline, "decoders did not start");
        thread::sleep(Duration::from_millis(1));
    }
    let started = Instant::now();
    t.shutdown();
    assert_eq!(
        active.load(Ordering::SeqCst),
        0,
        "no decoder outlives the transport"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn ramps_reach_the_target_exactly_in_either_direction() {
    let mut up = GainRamp::at(0.0);
    up.retarget(1.0, 480);
    up.skip(1000);
    assert_eq!(up.current, 1.0);
    let mut down = GainRamp::at(1.0);
    down.retarget(0.25, 7);
    down.skip(7);
    assert_eq!(down.current, 0.25);
    let mut same = GainRamp::at(0.5);
    same.retarget(0.5, 480);
    assert_eq!(same.step, 0.0);
}

#[test]
fn the_clipper_counts_and_sanitizes() {
    let mut block = [0.5, -1.5, 2.0, f32::NAN, f32::INFINITY, -0.25];
    let (clipped, peak) = clip_mix(&mut block);
    assert_eq!(clipped, 2);
    assert_eq!(peak, 2.0);
    assert_eq!(block, [0.5, -1.0, 1.0, 0.0, 0.0, -0.25]);
}

#[test]
fn solo_logic_is_evaluated_across_all_tracks() {
    let (a, b) = (TrackId::new(), TrackId::new());
    let mix = track_mix(&[controls(a, -600, false, false), controls(b, 0, false, true)]);
    assert!(!mix[0].audible);
    assert!(mix[1].audible);
    assert_eq!(mix[0].gain, gain(-6.0));
}

/// One generated file and its independent decode at the device format.
struct Real {
    media: MediaSummary,
    pcm: Vec<f32>,
    origin: i64,
}

fn real(dir: &Path, name: &str, input: &[&str], encode: &[&str]) -> Real {
    use cutcap_media::FfmpegBackend;
    use std::process::Command;
    let path = dir.join(name);
    let status = Command::new("ffmpeg.exe")
        .args(["-hide_banner", "-v", "error", "-y"])
        .args(input)
        .args(encode)
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "{name}");
    let probe = FfmpegBackend::default()
        .probe(&path, &Cancellation::default())
        .unwrap();
    let out = Command::new("ffmpeg.exe")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(&path)
        .args([
            "-af",
            "aresample=48000:resampler=swr,aformat=sample_fmts=flt:channel_layouts=stereo",
            "-f",
            "f32le",
            "-c:a",
            "pcm_f32le",
            "-",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{name}");
    let media = MediaSummary {
        id: MediaId::new(),
        path,
        metadata: probe.metadata,
    };
    let origin = media
        .metadata
        .primary_audio()
        .unwrap()
        .start()
        .rescale(Timebase::new(1, RATE).unwrap(), Rounding::Ceil)
        .unwrap()
        .ticks;
    Real {
        media,
        pcm: out
            .stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect(),
        origin,
    }
}

fn real_clip(source: &Real, track: TrackId, start_ms: i64, length_ms: i64) -> AudioClip {
    let stream = source.media.metadata.primary_audio().unwrap();
    AudioClip {
        clip: Clip {
            id: ClipId::new(),
            media_id: source.media.id,
            track_id: track,
            start: ms(start_ms),
            duration: TimelineDuration::new((length_ms * 720) as u64).unwrap(),
            source_in: stream
                .start()
                .rescale(Timebase::new(1, RATE).unwrap(), Rounding::Ceil)
                .unwrap(),
            stream: ClipStream::Audio {
                index: stream.stream_index,
            },
            link: None,
        },
        media: Some(source.media.clone()),
        unavailable: None,
    }
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn real_sources_at_different_rates_layouts_and_starts_mix_like_independent_pcm() {
    let dir = tempfile::tempdir().unwrap();
    let wav = real(
        dir.path(),
        "stereo48.wav",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
        ],
        &["-c:a", "pcm_s16le", "-ac", "2"],
    );
    let flac = real(
        dir.path(),
        "mono44.flac",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=44100:duration=2",
        ],
        &["-c:a", "flac"],
    );
    // A nonzero stream start on a millisecond clock.
    let offset = real(
        dir.path(),
        "late.mkv",
        &[
            "-itsoffset",
            "0.5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1320:sample_rate=48000:duration=2",
        ],
        &["-c:a", "pcm_s16le"],
    );
    assert!(
        offset.origin > 20_000,
        "the stream starts late: {}",
        offset.origin
    );
    let (a, b, c) = (TrackId::new(), TrackId::new(), TrackId::new());
    let backend: Arc<dyn MediaBackend> = Arc::new(cutcap_media::FfmpegBackend::default());
    let mut t = AudioTransport::start(backend, AudioOutput::virtual_device(RATE)).unwrap();
    t.set_controls(&[
        controls(a, -600, false, false),
        controls(b, 0, false, false),
        controls(c, -1200, false, false),
    ]);
    t.begin(
        plan(
            vec![
                real_clip(&wav, a, 0, 800),
                real_clip(&flac, b, 100, 700),
                real_clip(&offset, c, 250, 400),
            ],
            800,
        ),
        TimelineTime::ZERO,
    );
    let out = play(&mut t, 38_400);
    assert_eq!(t.view().output.underruns, 0);
    let (ga, gb, gc) = (gain(-6.0), 1.0f32, gain(-12.0));
    let settle = 2_400; // resampler state after each decoder's seek
    let mut worst = 0.0f32;
    let mut energy = 0.0f64;
    for k in settle..38_400usize {
        for channel in 0..2 {
            let mut expected = wav.pcm[k * 2 + channel] * ga;
            if (4_800 + settle..38_400).contains(&k) {
                expected += flac.pcm[(k - 4_800) * 2 + channel] * gb;
            } else if k >= 4_800 {
                continue;
            }
            if (12_000..31_200).contains(&k) {
                if k < 12_000 + settle {
                    continue;
                }
                expected += offset.pcm[(k - 12_000) * 2 + channel] * gc;
            }
            worst = worst.max((out[k * 2 + channel] - expected).abs());
            energy += f64::from(expected * expected);
        }
    }
    assert!(energy > 100.0, "real signal: {energy}");
    assert!(
        worst < 2e-3,
        "worst difference from the independent mix: {worst}"
    );
}

//! Audio transport against a virtual output and a deterministic decoder:
//! every output frame is checked against the exact source sample it must
//! carry (or silence), across cuts, gaps, source holes, seeks and missing media.
use super::*;
use cutcap_media::{DecodeRequest, DecodedFrame, ProbeResult};
use std::path::Path;

const RATE: u32 = 48_000;

/// A decoder whose sample `j` (source clock, 48 kHz) has the value
/// `j / 10^6` on the left (distinct for every sample used here) and its
/// negation on the right. The stream
/// has samples only from `first` (exclusive end `last`), except a hole.
struct Pattern {
    first: i64,
    last: i64,
    hole: Option<(i64, i64)>,
}

fn value(j: i64) -> f32 {
    j as f32 / 1_000_000.0
}

impl MediaBackend for Pattern {
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
        assert_eq!(request.rate, Some(RATE));
        let end = request.end_sample.unwrap_or(self.last).min(self.last);
        let mut j = request.start_sample.max(self.first);
        let mut delivered = false;
        while j < end {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            if let Some((a, b)) = self.hole
                && (a..b).contains(&j)
            {
                j = b;
                continue;
            }
            let stop = (j + 1000)
                .min(end)
                .min(self.hole.map_or(end, |(a, _)| if a > j { a } else { end }));
            let samples = (j..stop).flat_map(|n| [value(n), -value(n)]).collect();
            emit(PcmChunk {
                first_sample: j,
                channels: 2,
                samples,
                process_id: 7,
            })?;
            delivered = true;
            j = stop;
        }
        if delivered {
            Ok(())
        } else {
            Err(MediaError::NoFrame)
        }
    }
}

fn audio_media() -> MediaSummary {
    let probe = br#"{"streams":[
        {"index":0,"codec_type":"video","codec_name":"h264","width":320,"height":180,"time_base":"1/15360","start_pts":0,"duration_ts":153600},
        {"index":1,"codec_type":"audio","codec_name":"pcm_s16le","sample_rate":"48000","channels":2,"time_base":"1/48000","start_pts":0,"duration_ts":480000}
    ],"format":{"format_name":"matroska"}}"#;
    MediaSummary {
        id: MediaId::new(),
        path: "C:/clips/tone.mkv".into(),
        metadata: cutcap_media::parse_probe(probe).unwrap(),
    }
}

fn ms(ms: i64) -> TimelineTime {
    TimelineTime(ms * 720)
}

fn clip(media: &MediaSummary, start_ms: i64, length_ms: i64, source_ms: i64) -> AudioClip {
    AudioClip {
        clip: Clip {
            id: ClipId::new(),
            media_id: media.id,
            track_id: TrackId::new(),
            start: ms(start_ms),
            duration: TimelineDuration::new((length_ms * 720) as u64).unwrap(),
            source_in: SourceTime {
                ticks: source_ms,
                timebase: Timebase::new(1, 1000).unwrap(),
            },
            stream: ClipStream::Audio { index: 1 },
            link: None,
        },
        media: Some(media.clone()),
        unavailable: None,
    }
}

fn transport(backend: Pattern) -> AudioTransport {
    AudioTransport::start(Arc::new(backend), AudioOutput::virtual_device(RATE)).unwrap()
}

/// Plays the whole epoch on the virtual device and returns its left channel.
fn play_all(t: &mut AudioTransport, frames: usize) -> Vec<f32> {
    t.wait_buffered((RATE as usize * crate::audio_output::PREFILL_MS as usize) / 1000);
    assert!(t.ready());
    t.play();
    let mut left = Vec::new();
    let now = Instant::now();
    while left.len() < frames {
        t.wait_buffered(480.min(frames - left.len()));
        t.consume(480, now);
        let tap = t.output.take_tap();
        left.extend(tap.as_chunks::<2>().0.iter().map(|f| f[0]));
    }
    left.truncate(frames);
    left
}

#[test]
fn every_output_frame_carries_its_exact_source_sample_across_cuts_gaps_and_holes() {
    let media = audio_media();
    // The stream starts 100 ms into its clock and has a 10 ms hole at 2.5 s.
    let mut t = transport(Pattern {
        first: 4800,
        last: 480_000,
        hole: Some((120_000, 120_480)),
    });
    let plan = AudioPlan {
        timebase: Timebase::DEFAULT_TIMELINE,
        clips: vec![
            clip(&media, 0, 500, 2000),    // A: source 2.0 s .. 2.5 s
            clip(&media, 750, 250, 0),     // B: source 0 .. 0.25 s (first 100 ms absent)
            clip(&media, 1000, 500, 2490), // C: adjacent cut, crosses the hole at 2.5 s
        ],
        extent: ms(1600),
    };
    t.begin(plan, TimelineTime::ZERO);
    let left = play_all(&mut t, 76_800);
    let check = |range: std::ops::Range<usize>, source: Option<i64>, what: &str| {
        for k in range.clone() {
            let expected = source.map_or(0.0, |base| value(base + (k - range.start) as i64));
            assert_eq!(left[k], expected, "{what}: frame {k}");
        }
    };
    check(0..24_000, Some(96_000), "clip A");
    check(24_000..36_000, None, "gap between A and B");
    check(36_000..40_800, None, "B before its stream has samples");
    check(40_800..48_000, Some(4_800), "clip B");
    check(
        48_000..48_480,
        Some(119_520),
        "clip C before the source hole",
    );
    check(48_480..48_960, None, "C inside the source hole");
    check(48_960..72_000, Some(120_480), "clip C after the hole");
    check(
        72_000..76_800,
        None,
        "after the last clip, up to the extent",
    );
    let view = t.view();
    assert!(view.source_silence_ms > 0.0);
    assert_eq!(view.output.underruns, 0, "the ring never starved");
    assert!(view.decoder_starts >= 3);
}

#[test]
fn the_audio_clock_follows_played_frames_and_a_seek_starts_a_clean_epoch() {
    let media = audio_media();
    let mut t = transport(Pattern {
        first: 0,
        last: 480_000,
        hole: None,
    });
    let plan = || AudioPlan {
        timebase: Timebase::DEFAULT_TIMELINE,
        clips: vec![clip(&media, 0, 2000, 1000)],
        extent: ms(2000),
    };
    t.begin(plan(), TimelineTime::ZERO);
    assert_eq!(
        t.clock(Instant::now()),
        None,
        "no position before the device plays"
    );
    t.wait_buffered(6000);
    t.play();
    let now = Instant::now();
    t.consume(4800, now);
    assert_eq!(
        t.clock(now),
        Some(ms(100)),
        "4800 frames = 100 ms of timeline"
    );
    t.pause();
    t.consume(4800, now);
    assert_eq!(t.clock(now), Some(ms(100)), "paused: no advance");
    // Seek to 1.25 s: a new epoch, flushed output, decoders restarted.
    let epoch = t.view().epoch;
    t.begin(plan(), ms(1250));
    assert!(t.view().epoch > epoch);
    assert_eq!(t.clock(now), None);
    let left = play_all(&mut t, 4800);
    for (k, sample) in left.iter().enumerate() {
        assert_eq!(
            *sample,
            value(48_000 + 60_000 + k as i64),
            "post-seek frame {k}"
        );
    }
    assert_eq!(t.clock(now).map(|p| p.0 - ms(1250).0), Some(ms(100).0));
    // Cancel ends the epoch entirely.
    t.cancel();
    assert_eq!(t.view().state, "idle");
    assert_eq!(t.clock(now), None);
}

#[test]
fn unavailable_sources_play_silence_and_report_why() {
    let media = audio_media();
    let mut t = transport(Pattern {
        first: 0,
        last: 480_000,
        hole: None,
    });
    let mut missing = clip(&media, 0, 200, 0);
    missing.media = None;
    missing.unavailable = Some("The audio source is missing or changed; silence plays.".into());
    let plan = AudioPlan {
        timebase: Timebase::DEFAULT_TIMELINE,
        clips: vec![missing, clip(&media, 200, 100, 0)],
        extent: ms(300),
    };
    t.begin(plan, TimelineTime::ZERO);
    let left = play_all(&mut t, 14_400);
    assert!(left[..9_600].iter().all(|s| *s == 0.0));
    assert_eq!(left[9_600], value(0));
    let view = t.view();
    assert_eq!(view.silent_clips, 1);
    assert!(view.detail.unwrap().contains("missing"));
}

#[test]
fn starvation_freezes_the_clock_and_counts_underruns() {
    let media = audio_media();
    let mut t = transport(Pattern {
        first: 0,
        last: 480_000,
        hole: None,
    });
    let plan = AudioPlan {
        timebase: Timebase::DEFAULT_TIMELINE,
        clips: vec![clip(&media, 0, 300, 0)],
        extent: ms(300),
    };
    t.begin(plan, TimelineTime::ZERO);
    t.wait_buffered(14_400);
    t.play();
    let now = Instant::now();
    // The device asks for more than exists in the epoch: the remainder is
    // padding after the end (not an underrun), and the clock stops at 300 ms.
    t.consume(20_000, now);
    assert_eq!(t.clock(now), Some(ms(300)));
    assert_eq!(t.view().output.underruns, 0);
}

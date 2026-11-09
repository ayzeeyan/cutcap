//! Phase 4B audio decoding and waveform analysis against the real external
//! FFmpeg/FFprobe. Fixtures are generated per test (lossless PCM or FLAC in
//! Matroska, plus one AAC MP4) and compared with PCM decoded independently by
//! the FFmpeg CLI. Run with `--ignored --test-threads=1`.
use cutcap_contracts::*;
use cutcap_media::*;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

static SERIAL: Mutex<()> = Mutex::new(());

fn ffmpeg() -> PathBuf {
    std::env::var_os("CUTCAP_FFMPEG").map_or_else(|| "ffmpeg.exe".into(), PathBuf::from)
}

/// A short 160x90 video with the given audio graph, muxed into `name`.
fn generate(dir: &Path, name: &str, audio: &str, audio_args: &[&str]) -> MediaSummary {
    let path = dir.join(name);
    let status = Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=160x90:r=25:d=4",
        ])
        .args(["-f", "lavfi", "-i", audio])
        .args(["-map", "0:v", "-map", "1:a", "-c:v", "ffv1", "-shortest"])
        .args(audio_args)
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed for {name}");
    let probe = FfmpegBackend::default()
        .probe(&path, &Cancellation::default())
        .unwrap();
    MediaSummary {
        id: MediaId::new(),
        path,
        metadata: probe.metadata,
    }
}

/// Independently decoded interleaved f32 PCM of the first audio stream.
fn oracle(path: &Path, extra: &[&str]) -> Vec<f32> {
    let out = Command::new(ffmpeg())
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0"])
        .args(extra)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn decode(media: &MediaSummary, start: i64, end: Option<i64>, rate: Option<u32>) -> Vec<PcmChunk> {
    let stream = media.metadata.primary_audio().unwrap().stream_index;
    let request = PcmRequest {
        media: media.clone(),
        stream_index: stream,
        start_sample: start,
        end_sample: end,
        rate,
        background: rate.is_none(),
    };
    let mut chunks = Vec::new();
    FfmpegBackend::default()
        .stream_pcm(&request, &Cancellation::default(), &mut |c| {
            chunks.push(c);
            Ok(())
        })
        .unwrap();
    chunks
}

fn contiguous(chunks: &[PcmChunk]) -> (i64, Vec<f32>) {
    let first = chunks[0].first_sample;
    let mut next = first;
    let mut samples = Vec::new();
    for c in chunks {
        assert_eq!(
            c.first_sample, next,
            "chunks must be contiguous for a gap-free source"
        );
        next = c.end_sample();
        samples.extend_from_slice(&c.samples);
    }
    (first, samples)
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn playback_pcm_starts_at_the_requested_source_sample_with_independent_values() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // 48 kHz mono 997 Hz sine, lossless.
    let media = generate(
        dir.path(),
        "sine.mkv",
        "sine=frequency=997:sample_rate=48000:duration=3",
        &["-c:a", "pcm_s16le"],
    );
    let audio = media.metadata.primary_audio().unwrap();
    assert_eq!((audio.sample_rate, audio.channels), (48_000, 1));
    let chunks = decode(&media, 30_000, None, Some(48_000));
    let (first, samples) = contiguous(&chunks);
    assert_eq!(
        first, 30_000,
        "the first delivered sample is the requested one"
    );
    assert_eq!(chunks[0].channels, 2, "playback output is stereo");
    let frames = samples.len() / 2;
    assert_eq!(
        first + frames as i64,
        144_000,
        "to the end of the 3 s stream, no invented samples"
    );
    // Mono reaches both channels through FFmpeg's standard rematrix (-3 dB
    // pan law); values equal an independent decode with the same conversion.
    let reference = oracle(
        &media.path,
        &[
            "-af",
            "aresample=48000,aformat=sample_fmts=flt:channel_layouts=stereo",
        ],
    );
    for (i, frame) in samples.as_chunks::<2>().0.iter().enumerate().step_by(97) {
        let expected = &reference[(30_000 + i) * 2..(30_000 + i) * 2 + 2];
        assert_eq!(frame, expected, "frame {i}");
    }
    assert!(
        samples.iter().any(|s| s.abs() > 0.08),
        "about 0.125 * 0.707 peak"
    );
    // A bounded request stops exactly at its end sample.
    let (first, samples) = contiguous(&decode(&media, 48_000, Some(48_480), Some(48_000)));
    assert_eq!((first, samples.len() / 2), (48_000, 480));
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn resampled_and_offset_streams_keep_the_source_clock_in_output_samples() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // 44.1 kHz stereo FLAC whose container start is 1.5 s (audio and video both offset).
    let media = generate(
        dir.path(),
        "offset44.mkv",
        "sine=frequency=440:sample_rate=44100:duration=3,pan=stereo|c0=c0|c1=-1*c0",
        &["-c:a", "flac", "-output_ts_offset", "1.5"],
    );
    let audio = media.metadata.primary_audio().unwrap();
    assert_eq!(audio.sample_rate, 44_100);
    assert_eq!(audio.channels, 2);
    let start = audio
        .start()
        .rescale(Timebase::new(1, 1000).unwrap(), Rounding::Floor)
        .unwrap()
        .ticks;
    assert_eq!(start, 1500, "the stream really starts at 1.5 s");
    // Ask for source time 2.0 s at 48 kHz: sample 96_000 on the source clock.
    let (first, samples) = contiguous(&decode(&media, 96_000, Some(96_000 + 48_000), Some(48_000)));
    assert_eq!(first, 96_000);
    assert_eq!(samples.len() / 2, 48_000);
    // The right channel is the inverted left channel after resampling too.
    assert!(
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|f| (f[0] + f[1]).abs() < 1e-4)
    );
    // Before the stream start there are no samples: the first one is at 1.5 s.
    let (first, _) = contiguous(&decode(&media, 0, Some(96_000), Some(48_000)));
    assert_eq!(
        first, 72_000,
        "audio before its first sample is not invented"
    );
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn peaks_equal_independently_decoded_pcm_for_silence_constants_impulses_and_channels() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // 48 kHz stereo float: 1 s silence, 1 s sine at 0.5 (left) / 0.25 (right),
    // 1 s constant 0.3, and a single-sample impulse of 1.0 at sample 150_000.
    let graph = "aevalsrc=exprs='if(eq(n\\,150000)\\,1\\,if(lt(t\\,1)\\,0\\,if(lt(t\\,2)\\,0.5*sin(2*PI*440*t)\\,0.3)))|if(lt(t\\,1)\\,0\\,if(lt(t\\,2)\\,0.25*sin(2*PI*440*t)\\,0.3))':s=48000:d=4";
    let media = generate(dir.path(), "levels.mkv", graph, &["-c:a", "pcm_f32le"]);
    let stream = media.metadata.primary_audio().unwrap().stream_index;
    let peaks = FfmpegBackend::default()
        .audio_peaks(&media, stream, 0, 1, &Cancellation::default())
        .unwrap();
    assert_eq!(peaks.len(), 1);
    let p = &peaks[0];
    assert_eq!((p.rate, p.channels), (48_000, 2));
    let reference = oracle(&media.path, &[]);
    for bucket in 0..PEAK_CHUNK_BUCKETS as usize {
        let start = (bucket * 256 * 2).min(reference.len());
        let values = &reference[start..((bucket + 1) * 256 * 2).min(reference.len())];
        if values.is_empty() {
            assert!(p.min[bucket] > p.max[bucket]);
            continue;
        }
        let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert_eq!((p.min[bucket], p.max[bucket]), (lo, hi), "bucket {bucket}");
    }
    assert_eq!((p.min[10], p.max[10]), (0.0, 0.0), "silence");
    assert!(
        p.max[200] > 0.49 && p.min[200] < -0.49,
        "the 0.5 sine dominates the 0.25 channel"
    );
    assert_eq!((p.min[500], p.max[500]), (0.3, 0.3), "constant amplitude");
    assert_eq!(
        p.max[150_000 / 256],
        1.0,
        "a one-sample impulse is never averaged away"
    );
    // A chunk past the end of the 4 s stream is entirely empty.
    let late = FfmpegBackend::default()
        .audio_peaks(&media, stream, 2, 1, &Cancellation::default())
        .unwrap();
    assert!(late[0].min.iter().zip(&late[0].max).all(|(lo, hi)| lo > hi));
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn aac_sources_decode_and_cancellation_reaps_the_decoder() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let media = generate(
        dir.path(),
        "speech.mp4",
        "sine=frequency=300:sample_rate=48000:duration=4",
        &["-c:v", "libx264", "-c:a", "aac", "-b:a", "96k"],
    );
    let audio = media.metadata.primary_audio().unwrap();
    assert_eq!(audio.codec, "aac");
    assert!(audio.initial_padding.is_some(), "AAC priming is reported");
    let (first, samples) = contiguous(&decode(&media, 48_000, Some(96_000), Some(48_000)));
    assert_eq!((first, samples.len() / 2), (48_000, 48_000));
    assert!(
        samples.iter().any(|s| s.abs() > 0.05),
        "real decoded sound (sine amplitude 1/8)"
    );
    // Cancel from inside the consumer: the call returns Cancelled promptly.
    let cancel = Cancellation::default();
    let token = cancel.clone();
    let request = PcmRequest {
        media: media.clone(),
        stream_index: audio.stream_index,
        start_sample: 0,
        end_sample: None,
        rate: Some(48_000),
        background: false,
    };
    let started = std::time::Instant::now();
    let result = FfmpegBackend::default().stream_pcm(&request, &cancel, &mut |_| {
        token.cancel();
        Ok(())
    });
    assert!(matches!(result, Err(MediaError::Cancelled)), "{result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    // Invalid requests are rejected before any process starts.
    let bad = PcmRequest {
        stream_index: 7,
        ..request.clone()
    };
    assert!(
        FfmpegBackend::default()
            .stream_pcm(&bad, &Cancellation::default(), &mut |_| Ok(()))
            .is_err()
    );
    let bad = PcmRequest {
        rate: Some(1_000),
        ..request
    };
    assert!(
        FfmpegBackend::default()
            .stream_pcm(&bad, &Cancellation::default(), &mut |_| Ok(()))
            .is_err()
    );
}

/// An audio-only file: `graph` encoded with `args` into `name`.
fn audio_file(dir: &Path, name: &str, graph: &str, args: &[&str]) -> MediaSummary {
    let path = dir.join(name);
    let status = Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            graph,
        ])
        .args(args)
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed for {name}");
    let probe = FfmpegBackend::default()
        .probe(&path, &Cancellation::default())
        .unwrap();
    MediaSummary {
        id: MediaId::new(),
        path,
        metadata: probe.metadata,
    }
}

const PLAYBACK: &[&str] = &[
    "-af",
    "aresample=48000:resampler=swr,aformat=sample_fmts=flt:channel_layouts=stereo",
];

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn audio_only_formats_decode_on_the_source_clock_like_an_independent_decode() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let tone = "sine=frequency=440:sample_rate=44100:duration=3";
    let tone48 = "sine=frequency=440:sample_rate=48000:duration=3";
    // (file, graph, encoder arguments, expected codec, allowed sample error)
    let cases: &[(&str, &str, &[&str], &str, f32)] = &[
        ("s16.wav", tone48, &["-c:a", "pcm_s16le"], "pcm_s16le", 1e-4),
        ("s24.wav", tone48, &["-c:a", "pcm_s24le"], "pcm_s24le", 1e-4),
        ("f32.wav", tone48, &["-c:a", "pcm_f32le"], "pcm_f32le", 1e-4),
        ("flac.flac", tone, &["-c:a", "flac"], "flac", 2e-3),
        (
            "lame.mp3",
            tone,
            &["-c:a", "libmp3lame", "-b:a", "192k"],
            "mp3",
            3e-2,
        ),
        (
            "aac.m4a",
            tone48,
            &["-c:a", "aac", "-b:a", "192k"],
            "aac",
            3e-2,
        ),
    ];
    for (name, graph, args, codec, tolerance) in cases {
        let media = audio_file(dir.path(), name, graph, args);
        assert!(media.metadata.is_audio_only(), "{name}");
        assert!(media.metadata.video().is_none(), "{name}");
        let audio = media.metadata.primary_audio().unwrap().clone();
        assert_eq!(audio.codec, *codec, "{name}");
        assert!(audio.duration_ticks.is_some(), "{name} needs a duration");
        // Decode 1.0 s from 0.5 s on the source clock, at the device rate.
        let origin = audio
            .start()
            .rescale(Timebase::new(1, 48_000).unwrap(), Rounding::Ceil)
            .unwrap()
            .ticks;
        let (start, end) = (origin + 24_000, origin + 72_000);
        let (first, pcm) = contiguous(&decode(&media, start, Some(end), Some(48_000)));
        assert_eq!(first, start, "{name}: exact first sample");
        assert_eq!(pcm.len() as i64, (end - start) * 2, "{name}: exact length");
        let oracle = oracle(&media.path, PLAYBACK);
        // Skip the resampler/codec settling after the seek.
        let mut worst = 0.0f32;
        for frame in 2_400..48_000usize {
            let index = (first - origin) as usize + frame;
            for channel in 0..2 {
                worst = worst.max((pcm[frame * 2 + channel] - oracle[index * 2 + channel]).abs());
            }
        }
        assert!(worst < *tolerance, "{name}: worst sample error {worst}");
        assert!(pcm.iter().any(|s| s.abs() > 0.05), "{name}: real sound");
    }
    // A format outside the verified set is refused for what it is.
    let ogg = dir.path().join("vorbis.ogg");
    let status = Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            tone,
        ])
        .args(["-c:a", "libvorbis"])
        .arg(&ogg)
        .status()
        .unwrap();
    if status.success() {
        assert!(matches!(
            FfmpegBackend::default().probe(&ogg, &Cancellation::default()),
            Err(MediaError::Unsupported(_))
        ));
    }
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn a_non_primary_stream_decodes_and_analyses_its_own_samples() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two.mkv");
    let status = Command::new(ffmpeg())
        .args(["-hide_banner", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=160x90:r=25:d=2"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1320:sample_rate=48000:duration=2,volume=0.5",
        ])
        .args([
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let probe = FfmpegBackend::default()
        .probe(&path, &Cancellation::default())
        .unwrap();
    assert_eq!(probe.metadata.audio().len(), 2);
    let media = MediaSummary {
        id: MediaId::new(),
        path: path.clone(),
        metadata: probe.metadata,
    };
    let second = media.metadata.audio()[1].stream_index;
    let request = PcmRequest {
        media: media.clone(),
        stream_index: second,
        start_sample: 4_800,
        end_sample: Some(14_400),
        rate: Some(48_000),
        background: false,
    };
    let mut chunks = Vec::new();
    FfmpegBackend::default()
        .stream_pcm(&request, &Cancellation::default(), &mut |c| {
            chunks.push(c);
            Ok(())
        })
        .unwrap();
    let (first, pcm) = contiguous(&chunks);
    assert_eq!(first, 4_800);
    let out = Command::new(ffmpeg())
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(&path)
        .args(["-map", "0:a:1"])
        .args(PLAYBACK)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    let expected: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    for (i, sample) in pcm.iter().enumerate() {
        assert!(
            (sample - expected[4_800 * 2 + i]).abs() < 1e-4,
            "sample {i}"
        );
    }
    // Two streams of one file give two different peak sets.
    let backend = FfmpegBackend::default();
    let cancel = Cancellation::default();
    let a = backend
        .audio_peaks(
            &media,
            media.metadata.audio()[0].stream_index,
            0,
            1,
            &cancel,
        )
        .unwrap();
    let b = backend.audio_peaks(&media, second, 0, 1, &cancel).unwrap();
    assert!(
        a[0].max[100] > b[0].max[100] * 1.5,
        "{} vs {}",
        a[0].max[100],
        b[0].max[100]
    );
}

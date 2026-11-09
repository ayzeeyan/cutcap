use cutcap_contracts::*;
use cutcap_media::*;
use std::{path::PathBuf, sync::Mutex};

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn interval_preroll_preserves_covering_real_pts_vfr_offsets_and_bframes() {
    let _guard = SERIAL.lock().unwrap();
    for (name, target, lower) in [
        ("colors-vfr.mkv", 201, 0),
        ("colors-vfr.mkv", 1499, 200),
        ("offset.mkv", 6251, 5500),
        ("long-gop-bframes.mp4", 37531, 0),
    ] {
        let media = media(name);
        let oracle = std::process::Command::new(
            std::env::var_os("CUTCAP_FFPROBE").unwrap_or_else(|| "ffprobe.exe".into()),
        )
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-show_entries",
            "frame=pts",
            "-of",
            "json",
        ])
        .arg(&media.path)
        .output()
        .unwrap();
        assert!(oracle.status.success());
        let json: serde_json::Value = serde_json::from_slice(&oracle.stdout).unwrap();
        let eligible: Vec<i64> = json["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["pts"].as_i64().unwrap())
            .filter(|pts| *pts >= lower)
            .collect();
        let first = eligible.iter().rposition(|pts| *pts <= target).unwrap_or(0);
        let clock = media.metadata.video().unwrap().timebase;
        let request = DecodeRequest {
            ticket: PreviewTicket {
                project_id: ProjectId::new(),
                media_id: media.id,
                request_id: RequestId::new(),
            },
            media,
            position: SourceTime {
                ticks: target,
                timebase: clock,
            },
        };
        let mut actual = Vec::new();
        let mut pid = None;
        FfmpegBackend::default()
            .stream_interval(
                &request,
                SourceTime {
                    ticks: lower,
                    timebase: clock,
                },
                &Cancellation::default(),
                &mut |f| {
                    if let Some(pid) = pid {
                        assert_eq!(pid, f.process_id);
                    } else {
                        pid = Some(f.process_id);
                    }
                    actual.push(f.actual.ticks);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(actual, eligible[first..], "{name}");
    }
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn persistent_interval_cancellation_reaps_the_owned_child_and_rejects_bad_bounds() {
    let _guard = SERIAL.lock().unwrap();
    let media = media("long-gop-bframes.mp4");
    let clock = media.metadata.video().unwrap().timebase;
    let request = DecodeRequest {
        ticket: PreviewTicket {
            project_id: ProjectId::new(),
            media_id: media.id,
            request_id: RequestId::new(),
        },
        position: media.metadata.video().unwrap().start(),
        media,
    };
    let token = Cancellation::default();
    let mut pid = None;
    let result =
        FfmpegBackend::default().stream_interval(&request, request.position, &token, &mut |f| {
            pid = Some(f.process_id);
            token.cancel();
            Err(MediaError::Cancelled)
        });
    assert!(matches!(result, Err(MediaError::Cancelled)));
    assert!(pid.is_some());
    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist.exe")
            .args(["/FI", &format!("PID eq {}", pid.unwrap()), "/NH"])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout).contains("ffmpeg"));
    }
    assert!(
        FfmpegBackend::default()
            .stream_interval(
                &request,
                SourceTime {
                    ticks: request.position.ticks + 1,
                    timebase: clock
                },
                &Cancellation::default(),
                &mut |_| Ok(())
            )
            .is_err()
    );
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn persistent_long_gop_bframes_match_independent_frame_pts_and_seek() {
    let _guard = SERIAL.lock().unwrap();
    let media = media("long-gop-bframes.mp4");
    let oracle = std::process::Command::new(
        std::env::var_os("CUTCAP_FFPROBE").unwrap_or_else(|| "ffprobe.exe".into()),
    )
    .args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_frames",
        "-show_entries",
        "frame=pts,pict_type",
        "-of",
        "json",
    ])
    .arg(&media.path)
    .output()
    .unwrap();
    assert!(oracle.status.success());
    let json: serde_json::Value = serde_json::from_slice(&oracle.stdout).unwrap();
    let frames = json["frames"].as_array().unwrap();
    assert!(frames.iter().any(|f| f["pict_type"] == "B"));
    let expected: Vec<i64> = frames.iter().map(|f| f["pts"].as_i64().unwrap()).collect();
    for target in [media.metadata.video().unwrap().start_pts, 37530] {
        let request = DecodeRequest {
            ticket: PreviewTicket {
                project_id: ProjectId::new(),
                media_id: media.id,
                request_id: RequestId::new(),
            },
            position: SourceTime {
                ticks: target,
                timebase: media.metadata.video().unwrap().timebase,
            },
            media: media.clone(),
        };
        let mut actual = Vec::new();
        let mut pid = None;
        FfmpegBackend::default()
            .stream(&request, &Cancellation::default(), &mut |frame| {
                if let Some(first) = pid {
                    assert_eq!(first, frame.process_id);
                } else {
                    pid = Some(frame.process_id);
                }
                actual.push(frame.actual.ticks);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            actual,
            expected
                .iter()
                .copied()
                .filter(|pts| *pts >= target)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn persistent_framed_vfr_packets_preserve_pts_and_colors() {
    let _guard = SERIAL.lock().unwrap();
    let media = media("colors-vfr.mkv");
    let request = DecodeRequest {
        ticket: PreviewTicket {
            project_id: ProjectId::new(),
            media_id: media.id,
            request_id: RequestId::new(),
        },
        position: media.metadata.video().unwrap().start(),
        media,
    };
    let mut frames = Vec::new();
    FfmpegBackend::default()
        .stream(&request, &Cancellation::default(), &mut |frame| {
            frames.push(frame);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        frames.iter().map(|f| f.actual.ticks).collect::<Vec<_>>(),
        [0, 200, 700, 1500, 2300]
    );
    let pid = frames[0].process_id;
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.process_id, pid);
        color(frame, index % 3);
    }
    let offset = self::media("offset.mkv");
    let request = DecodeRequest {
        ticket: PreviewTicket {
            project_id: ProjectId::new(),
            media_id: offset.id,
            request_id: RequestId::new(),
        },
        position: SourceTime {
            ticks: 6250,
            timebase: offset.metadata.video().unwrap().timebase,
        },
        media: offset,
    };
    let mut first = None;
    FfmpegBackend::default()
        .stream(&request, &Cancellation::default(), &mut |frame| {
            if first.is_none() {
                color(&frame, 1);
                first = Some(frame.actual.ticks);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(first, Some(6250));
}
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
fn media(name: &str) -> MediaSummary {
    let probe = FfmpegBackend::default()
        .probe(&fixture(name), &Cancellation::default())
        .unwrap();
    MediaSummary {
        id: MediaId::new(),
        path: probe.path,
        metadata: probe.metadata,
    }
}
fn frame(media: &MediaSummary, ticks: i64) -> DecodedFrame {
    FfmpegBackend::default()
        .decode(
            &DecodeRequest {
                ticket: PreviewTicket {
                    project_id: ProjectId::new(),
                    media_id: media.id,
                    request_id: RequestId::new(),
                },
                media: media.clone(),
                position: SourceTime {
                    ticks,
                    timebase: media.metadata.video().unwrap().timebase,
                },
            },
            &Cancellation::default(),
        )
        .unwrap()
}
fn color(frame: &DecodedFrame, channel: usize) {
    let pixel = &frame.image.pixels()[0..4];
    assert!(pixel[channel] > 250, "unexpected pixel {pixel:?}");
    for index in 0..3 {
        if index != channel {
            assert!(pixel[index] < 5, "unexpected pixel {pixel:?}");
        }
    }
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe; run explicitly, never bundle tools"]
fn cfr_seek_colors_beginning_middle_and_last_frame() {
    let _guard = SERIAL.lock().unwrap();
    let media = media("colors-cfr.mkv");
    assert_eq!(
        (
            media.metadata.video().unwrap().width,
            media.metadata.video().unwrap().height
        ),
        (320, 180)
    );
    for (target, pts, channel) in [
        (0, 0, 0),
        (1, 250, 0),
        (999, 1000, 1),
        (1250, 1250, 1),
        (2000, 2000, 2),
        (2749, 2750, 2),
    ] {
        let decoded = frame(&media, target);
        assert_eq!(
            decoded.actual,
            SourceTime {
                ticks: pts,
                timebase: media.metadata.video().unwrap().timebase
            }
        );
        color(&decoded, channel);
    }
    assert!(matches!(
        FfmpegBackend::default().decode(
            &DecodeRequest {
                ticket: PreviewTicket {
                    project_id: ProjectId::new(),
                    media_id: media.id,
                    request_id: RequestId::new()
                },
                media: media.clone(),
                position: SourceTime {
                    ticks: 2999,
                    timebase: media.metadata.video().unwrap().timebase
                }
            },
            &Cancellation::default()
        ),
        Err(MediaError::NoFrame)
    ));
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn vfr_ceiling_policy_uses_real_pts_not_nominal_frame_rate() {
    let _guard = SERIAL.lock().unwrap();
    let media = media("colors-vfr.mkv");
    for (target, pts, channel) in [
        (0, 0, 0),
        (1, 200, 1),
        (201, 700, 2),
        (701, 1500, 0),
        (1501, 2300, 1),
        (2300, 2300, 1),
    ] {
        let decoded = frame(&media, target);
        assert_eq!(decoded.actual.ticks, pts);
        color(&decoded, channel);
    }
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn metadata_audio_orientation_unicode_and_nonzero_start() {
    let _guard = SERIAL.lock().unwrap();
    let av = media("audio-video.mp4");
    assert_eq!(av.metadata.video().unwrap().codec, "h264");
    assert_eq!(av.metadata.video().unwrap().audio_streams.len(), 1);
    let rotated = media("rotated.mp4");
    assert_eq!(rotated.metadata.video().unwrap().rotation, 90);
    let decoded = frame(&rotated, rotated.metadata.video().unwrap().start_pts);
    assert_eq!((decoded.image.width(), decoded.image.height()), (180, 320));
    let original = frame(&av, av.metadata.video().unwrap().start_pts);
    // The same compressed packets, decoded with a 90-degree CCW display matrix.
    // (x,y) -> (y,width-1-x); verify content as well as portrait dimensions.
    for (x, y) in [(40, 30), (100, 60), (250, 120)] {
        let source = (y * 320 + x) * 4;
        let destination = ((319 - x) * 180 + y) * 4;
        for channel in 0..3 {
            assert!(
                original.image.pixels()[source + channel]
                    .abs_diff(decoded.image.pixels()[destination + channel])
                    <= 5
            );
        }
    }
    let unicode = media("warna 视频 space.mkv");
    color(&frame(&unicode, 1000), 1);
    let offset = media("offset.mkv");
    assert_eq!(offset.metadata.video().unwrap().start_pts, 5000);
    let decoded = frame(&offset, 6250);
    assert_eq!(decoded.actual.ticks, 6250);
    color(&decoded, 1);
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn executable_failure_and_pre_cancelled_decode_are_typed() {
    let _guard = SERIAL.lock().unwrap();
    let media = media("colors-cfr.mkv");
    let backend = FfmpegBackend::with_executables(
        "cutcap-not-installed.exe".into(),
        "cutcap-not-installed.exe".into(),
    );
    assert!(matches!(
        backend.probe(&media.path, &Cancellation::default()),
        Err(MediaError::Io { .. })
    ));
    let request = DecodeRequest {
        ticket: PreviewTicket {
            project_id: ProjectId::new(),
            media_id: media.id,
            request_id: RequestId::new(),
        },
        position: SourceTime {
            ticks: media.metadata.video().unwrap().start_pts + 86400000,
            timebase: media.metadata.video().unwrap().timebase,
        },
        media,
    };
    assert!(matches!(
        backend.decode(&request, &Cancellation::default()),
        Err(MediaError::Io { .. })
    ));
    // Cancel at submission; process::tests separately proves termination of an active child.
    let token = Cancellation::default();
    token.cancel();
    assert!(matches!(
        FfmpegBackend::default().decode(&request, &token),
        Err(MediaError::Cancelled)
    ));
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn missing_corrupt_unsupported_and_pre_cancelled_inputs() {
    let _guard = SERIAL.lock().unwrap();
    let backend = FfmpegBackend::default();
    assert!(matches!(
        backend.probe(&fixture("no-such-file.mp4"), &Cancellation::default()),
        Err(MediaError::Io { .. })
    ));
    // Phase 4C: audio-only files are sources in their own right.
    let audio_only = backend
        .probe(&fixture("audio-only.mkv"), &Cancellation::default())
        .unwrap();
    assert!(audio_only.metadata.is_audio_only());
    assert!(audio_only.metadata.video().is_none());
    assert!(
        backend
            .probe(&fixture("corrupt.mp4"), &Cancellation::default())
            .is_err()
    );
    let token = Cancellation::default();
    token.cancel();
    assert!(matches!(
        backend.probe(&fixture("colors-cfr.mkv"), &token),
        Err(MediaError::Cancelled)
    ));
}

fn thumbs(
    name: &str,
    height: u32,
    ticks: &[i64],
) -> (MediaSummary, Vec<Result<Thumbnail, MediaError>>) {
    let media = media(name);
    let batch: Vec<ThumbnailRequest> = ticks
        .iter()
        .enumerate()
        .map(|(n, t)| ThumbnailRequest {
            token: n as u64 + 100,
            position: SourceTime {
                ticks: *t,
                timebase: media.metadata.video().unwrap().timebase,
            },
        })
        .collect();
    let results = FfmpegBackend::default()
        .extract_thumbnails(&media, height, &batch, &Cancellation::default())
        .unwrap();
    (media, results)
}

fn centre(t: &Thumbnail) -> [u8; 3] {
    let (w, h) = (t.image.width() as usize, t.image.height() as usize);
    let i = (h / 2 * w + w / 2) * 4;
    let p = &t.image.pixels()[i..i + 3];
    [p[0], p[1], p[2]]
}

fn is(rgb: [u8; 3], color: &str) -> bool {
    let [r, g, b] = rgb.map(i32::from);
    match color {
        "red" => r > 200 && g < 60 && b < 60,
        "green" => g > 90 && r < 60 && b < 60,
        "blue" => b > 200 && r < 60 && g < 60,
        _ => false,
    }
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn batched_thumbnails_return_real_pixels_at_the_first_pts_at_or_after_each_sample() {
    let _guard = SERIAL.lock().unwrap();
    // CFR 4 fps: frames 0-3 red, 4-7 green, 8-11 blue; PTS 0,250,...,2750 (1/1000).
    let (media, results) = thumbs("colors-cfr.mkv", 36, &[0, 100, 1100, 2100, 2750, 2751]);
    let expected = [
        (0, "red"),
        (250, "red"),
        (1250, "green"),
        (2250, "blue"),
        (2750, "blue"),
    ];
    for (result, (pts, color)) in results.iter().zip(expected) {
        let t = result.as_ref().unwrap();
        assert_eq!(t.actual.ticks, pts);
        assert_eq!(t.actual.timebase, media.metadata.video().unwrap().timebase);
        assert!(is(centre(t), color), "{pts}: {:?}", centre(t));
        assert_eq!(
            (t.image.width(), t.image.height()),
            (64, 36),
            "16:9 aspect kept"
        );
    }
    assert_eq!(results[0].as_ref().unwrap().token, 100);
    // Past the last frame: no frame, not a substitute.
    assert!(
        matches!(results[5], Err(MediaError::NoFrame)),
        "{:?}",
        results[5].as_ref().err()
    );
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn thumbnails_use_actual_vfr_and_offset_pts_and_keep_rotation() {
    let _guard = SERIAL.lock().unwrap();
    // VFR PTS 0,200,700,1500,2300: red, green, blue, red, green.
    let (_, vfr) = thumbs("colors-vfr.mkv", 32, &[201, 699, 701, 1499, 2299]);
    let pts: Vec<i64> = vfr
        .iter()
        .map(|r| r.as_ref().unwrap().actual.ticks)
        .collect();
    assert_eq!(
        pts,
        [700, 700, 1500, 1500, 2300],
        "never a nominal-rate frame index"
    );
    assert!(is(centre(vfr[0].as_ref().unwrap()), "blue"));
    assert!(is(centre(vfr[2].as_ref().unwrap()), "red"));
    assert!(is(centre(vfr[4].as_ref().unwrap()), "green"));
    // Nonzero start PTS (5000): absolute positions, not relative to zero.
    let (offset, results) = thumbs("offset.mkv", 36, &[5000, 6100]);
    assert_eq!(offset.metadata.video().unwrap().start_pts, 5000);
    assert_eq!(results[0].as_ref().unwrap().actual.ticks, 5000);
    assert!(results[1].as_ref().unwrap().actual.ticks >= 6100);
    // Display-matrix rotation: portrait thumbnail, not stretched landscape.
    let (_, rotated) = thumbs("rotated.mp4", 64, &[0]);
    let t = rotated[0].as_ref().unwrap();
    assert!(
        t.image.width() < t.image.height(),
        "{}x{}",
        t.image.width(),
        t.image.height()
    );
}

#[test]
#[ignore = "requires external FFmpeg/FFprobe"]
fn thumbnail_batches_reject_bad_requests_and_cancel_promptly() {
    let _guard = SERIAL.lock().unwrap();
    let media = media("long-gop-bframes.mp4");
    let backend = FfmpegBackend::default();
    let at = |ticks| ThumbnailRequest {
        token: 1,
        position: SourceTime {
            ticks,
            timebase: media.metadata.video().unwrap().timebase,
        },
    };
    let cancel = Cancellation::default();
    assert!(
        backend
            .extract_thumbnails(&media, 40, &[], &cancel)
            .is_err()
    );
    assert!(
        backend
            .extract_thumbnails(&media, 40, &[at(0); 7], &cancel)
            .is_err()
    );
    let wrong = ThumbnailRequest {
        token: 1,
        position: SourceTime {
            ticks: 0,
            timebase: Timebase::new(1, 7).unwrap(),
        },
    };
    assert!(
        backend
            .extract_thumbnails(&media, 40, &[wrong], &cancel)
            .is_err()
    );
    assert!(
        backend
            .extract_thumbnails(&media, 40, &[at(i64::MAX)], &cancel)
            .is_err()
    );
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        backend.extract_thumbnails(&media, 40, &[at(0)], &cancel),
        Err(MediaError::Cancelled)
    ));
}

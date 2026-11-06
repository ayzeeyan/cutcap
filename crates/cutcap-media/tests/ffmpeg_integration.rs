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
        let clock = media.metadata.timebase;
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
    let clock = media.metadata.timebase;
    let request = DecodeRequest {
        ticket: PreviewTicket {
            project_id: ProjectId::new(),
            media_id: media.id,
            request_id: RequestId::new(),
        },
        position: media.metadata.start(),
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
    for target in [media.metadata.start_pts, 37530] {
        let request = DecodeRequest {
            ticket: PreviewTicket {
                project_id: ProjectId::new(),
                media_id: media.id,
                request_id: RequestId::new(),
            },
            position: SourceTime {
                ticks: target,
                timebase: media.metadata.timebase,
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
        position: media.metadata.start(),
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
            timebase: offset.metadata.timebase,
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
                    timebase: media.metadata.timebase,
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
    assert_eq!((media.metadata.width, media.metadata.height), (320, 180));
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
                timebase: media.metadata.timebase
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
                    timebase: media.metadata.timebase
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
    assert_eq!(av.metadata.codec, "h264");
    assert_eq!(av.metadata.audio_streams.len(), 1);
    let rotated = media("rotated.mp4");
    assert_eq!(rotated.metadata.rotation, 90);
    let decoded = frame(&rotated, rotated.metadata.start_pts);
    assert_eq!((decoded.image.width(), decoded.image.height()), (180, 320));
    let original = frame(&av, av.metadata.start_pts);
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
    assert_eq!(offset.metadata.start_pts, 5000);
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
            ticks: media.metadata.start_pts + 86400000,
            timebase: media.metadata.timebase,
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
    assert!(matches!(
        backend.probe(&fixture("audio-only.mkv"), &Cancellation::default()),
        Err(MediaError::Unsupported(_))
    ));
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

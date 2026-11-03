use cutcap_contracts::*;
use cutcap_media::*;
use std::{path::PathBuf, sync::Mutex};

static SERIAL: Mutex<()> = Mutex::new(());
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

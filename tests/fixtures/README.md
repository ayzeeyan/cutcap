# Generated media fixtures

Generated locally by `scripts/generate-fixtures.ps1` with the external FFmpeg 9.0.1 CLI. No FFmpeg executable/library is included. Sources are mathematical color fields, FFmpeg test patterns and sine tones, with no third-party source footage. Fixture content is dedicated to CC0-1.0; this does not select an application source-code license.

- colors-cfr.mkv: 320×180, FFV1, 4 fps, 3 s. Frames 0–3 red, 4–7 green, 8–11 blue. PTS 0,250,...,2750 in 1/1000 s. Its title contains forged showinfo text with PTS 999999: a regression against interpreting untrusted tags as decoder timestamps. Unicode/offset copies retain it.
- colors-vfr.mkv: 160×90, FFV1, five frames at PTS 0,200,700,1500,2300 in 1/1000 s; red, green, blue, red, green.
- audio-video.mp4: 320×180 H.264 test pattern at 12 fps with a 440 Hz AAC audio stream.
- audio-only.mkv: unsupported video import case, PCM sine audio only.
- rotated.mp4: video-only copy of audio-video.mp4 with a 90-degree display matrix.
- offset.mkv: CFR color frames with source PTS beginning at 5000, to test absolute timestamp seeking.
- warna 视频 space.mkv: byte-identical CFR fixture for Unicode/space path handling.
- corrupt.mp4: intentionally invalid text, not simulated video.

Performance sources are generated on demand under ignored `.artifacts/performance-sources`; they are not committed. See PHASE_1_REPORT.md for actual measurements and test results. Known PTS/color expectations above are asserted by request-based integration tests. Independent FFprobe -show_frames inspection also confirmed the CFR/VFR PTS during Phase 1 validation.

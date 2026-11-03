# Architecture decisions

## ADR 001 — Three crates (accepted, Phase 0)
`cutcap-contracts` is the leaf: IDs, time values, command/event DTOs and boundary errors. `cutcap-core` depends on contracts and owns validated project state. `cutcap` depends on both and owns the window, worker lifecycle and adapters. No engine crate full of stubs, async runtime or plugin system.

## ADR 002 — Exact ticks, explicit timebase (accepted)
Timeline positions are signed integer ticks in the project's rational seconds-per-tick timebase. Positive durations and checked endpoints reject invalid clips. Default is 1/720000 second: common integer and 30000/1001 frame rates map exactly. This is a storage clock, not the preview rate. Source timestamps carry their own rational timebase and a distinct type. VFR will use decoded presentation timestamps; never infer source time from frame index. Other rates require explicit conversion/rounding at a future media adapter.

## ADR 003 — Single project owner and bounded messages (accepted)
A standard-library worker owns the model. UI submission uses nonblocking `try_send`; events are drained in bounded batches by a Slint timer on the event-loop thread. No worker accesses a Slint component. Cheap operations only in Phase 0. Decode/export will require cancellable execution separate from the command owner. See ENGINE_UI_CONTRACT.md.

## ADR 004 — Independent preview presentation (accepted)
Slint 1.18.1 uses winit + software rendering for the shell. Only the app's preview adapter writes viewer properties. There is no assumption that Slint's internal renderer is a video renderer. Optional wgpu 30.0.1 diagnostics enumerate DX12 adapters without creating a device or sharing textures. No unstable Slint wgpu/winit APIs are enabled. FFmpeg is not a Phase 0 dependency. Phase 1 follows the owner's later CPU-first instruction: use supported CPU frames and measure before considering GPU integration (ADR 010). The originally proposed Phase 1 GPU comparison is deferred until playback measurements demonstrate a need.

## ADR 005 — Toolchain and dependency policy (accepted)
Pin Rust 1.99.1, MSVC target, rustfmt and Clippy in rust-toolchain.toml; manifests declare Rust 1.99 as the supported baseline. Commit Cargo.lock. Slint runtime/build versions match exactly. Other dependency ranges allow compatible updates only through reviewed lockfile changes. CI uses the pinned toolchain, not a floating latest action. Upgrade compiler and Slint deliberately, run all checks and a Windows launch, and record results. Four compiler jobs limit peak memory on 16 GB hardware; no measured runtime performance claims yet.

## ADR 006 — Licensing (provisional distribution path)
No application license has been chosen; packages are non-publishable. Review the Slint royalty-free desktop route with built-in AboutSlint attribution. The installed FFmpeg has `--enable-gpl --enable-version3`; do not bundle or link it by accident. LGPL-compatible shared builds, obligations, codec patents and the final application license must be resolved before distribution. See LICENSE_REVIEW.md.

## ADR 007 — Honest editor shell (accepted)
Real empty tracks, selection, track creation and new-project commands establish the model/UI round trip. View-only zoom, panel splitters, scrolling, workspace reset and About are presentation state. Unsupported media operations remain disabled with explanations. No sample clips or simulated transport.

Initial placement uses the public Slint window API at physical (32,16) on the primary display. Native maximize/restore is available. Persistent/per-monitor workspace geometry is deferred; avoid unstable backend access just to implement saved layouts in Phase 0.

Use Slint logical-key bindings for shortcuts. In 1.18, the framework accepts Escape in its popup handling path even when no popup is open, before application bindings. Clear selection uses Ctrl+Shift+A; Escape remains popup dismissal. No internal framework override is used.

## ADR 008 — Independent subprocess media adapter (accepted, Phase 1)

Add cutcap-media for real FFprobe parsing, controlled FFmpeg frame decoding, process ownership and tests. It depends on contracts, never Slint/core. One concrete MediaBackend trait is justified by tested behavior and a future libavcodec/hardware replacement. Native FFmpeg bindings would add ABI/build/licensing complexity without helping this request-based milestone. External FFmpeg/FFprobe 9.0.1 are development runtime dependencies; no binary is bundled/downloaded/packaged. Argument spawning, bounded pipes, timeouts, cooperative cancellation and kill/reap ownership are mandatory. See LICENSE_REVIEW.md for distribution questions; a subprocess is not automatic license clearance.

## ADR 009 — Contract 2 and metadata session cache (accepted, Phase 1)

Add RegisterMedia/SelectMedia and real metadata/library fields in snapshots. Only the project worker registers MediaAsset; probing/decoding run separately. Source selection does not replace timeline Track/Clip selection. Keep schema 1 unchanged: paths/IDs are serialized, metadata is a re-probable session cache, and no buffers enter Project. The app cap is 128 imported assets. Persistence must later decide fingerprinting, re-probing and explicit per-stream references, particularly for audio. Reserved ImportMedia batch/timeline Seek/Cancel command shapes remain Unsupported; the implemented app media workflow has its own concrete request/token contract.

## ADR 010 — CPU still frames and latest requests (accepted, Phase 1)

One active decode, one replaceable pending request and one replaceable outcome bound backlog. Project/media/request identity gates successes and errors in worker and presenter. Opaque top-down packed RGBA8 is limited to 1280×720 / 3,686,400 bytes and copied once into Slint's supported SharedPixelBuffer on UI. No internal Slint renderer/GPU APIs or unsafe code. The old frame can remain while seeking; changing source clears it. Durable project/import/export terminal outcomes must never use this ephemeral mailbox. Performance evidence and measurement limits are in PHASE_1_REPORT.md; request-based success is not playback pacing evidence.

## ADR 011 — Explicit ceiling PTS selection (accepted with compatibility risk)

Select first decoded original PTS >= requested stream PTS. Requests use signed integers/rational clocks with checked floor/ceil conversion, not frame-rate indexing. FFmpeg preserves copyts, demux-seeks with one-second preroll and noaccurate_seek, then applies explicit PTS filtering. The positive-offset regression exposed automatic accurate_seek discard selecting no frame; explicit filtering fixes it. showinfo provides original PTS/timebase, with first record matched to the first raw output. Missing PTS is failure. Text parsing is tied to the tested FFmpeg 9.0.1 CLI and must be retested on upgrades. Near-end requests after the last PTS may return NoFrame; there is no fabricated fallback or arbitrary-media frame-accuracy promise.

Quarter-turn orientation/aspect is applied before bounded square-pixel presentation. Only SDR conversion is supported; HDR/reflections/non-quarter turns are rejected. Alpha is discarded to opaque. A future color-managed adapter may change these semantics only with explicit compatibility/tests/docs.

Timestamp records must match the uniquely named showinfo filter for the fresh RequestId, not arbitrary stderr containing the word showinfo. A forged media title reproduced false PTS 999999 instead of 0 with the original broad parser; exact per-request context matching fixes it. Both a parser unit test and the real CFR/Unicode/offset fixtures enforce this untrusted-metadata boundary. Integer start_pts is required; a decimal fallback is not claimed as observed PTS.

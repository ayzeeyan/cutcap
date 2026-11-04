# Architecture decisions

## ADR 001 — Three crates (accepted, Phase 0)
`cutcap-contracts` is the leaf: IDs, time values, command/event DTOs and boundary errors. `cutcap-core` depends on contracts and owns validated project state. `cutcap` depends on both and owns the window, worker lifecycle and adapters. No engine crate full of stubs, async runtime or plugin system.

## ADR 002 — Exact ticks, explicit timebase (accepted)
Timeline positions are signed integer ticks in the project's rational seconds-per-tick timebase. Positive durations and checked endpoints reject invalid clips. Default is 1/720000 second: common integer and 30000/1001 frame rates map exactly. This is a storage clock, not the preview rate. Source timestamps carry their own rational timebase and a distinct type. VFR will use decoded presentation timestamps; never infer source time from frame index. Other rates require explicit conversion/rounding at a future media adapter.

## ADR 003 — Single project owner and bounded messages (accepted; drain owner moved by ADR 013)
A standard-library worker owns the model. UI submission uses nonblocking `try_send`; events are drained in bounded batches by a Slint timer on the event-loop thread. No worker accesses a Slint component. Cheap operations only in Phase 0. Decode/export will require cancellable execution separate from the command owner. See ENGINE_UI_CONTRACT.md.

## ADR 004 — Independent preview presentation (Slint details superseded by ADR 012/015)
Slint 1.18.1 uses winit + software rendering for the shell. Only the app's preview adapter writes viewer properties. There is no assumption that Slint's internal renderer is a video renderer. Optional wgpu 30.0.1 diagnostics enumerate DX12 adapters without creating a device or sharing textures. No unstable Slint wgpu/winit APIs are enabled. FFmpeg is not a Phase 0 dependency. Phase 1 follows the owner's later CPU-first instruction: use supported CPU frames and measure before considering GPU integration (ADR 010). The originally proposed Phase 1 GPU comparison is deferred until playback measurements demonstrate a need.

## ADR 005 — Toolchain and dependency policy (accepted; Slint pin removed by ADR 012)
Pin Rust 1.99.1, MSVC target, rustfmt and Clippy in rust-toolchain.toml; manifests declare Rust 1.99 as the supported baseline. Commit Cargo.lock. Slint runtime/build versions match exactly. Other dependency ranges allow compatible updates only through reviewed lockfile changes. CI uses the pinned toolchain, not a floating latest action. Upgrade compiler and Slint deliberately, run all checks and a Windows launch, and record results. Four compiler jobs limit peak memory on 16 GB hardware; no measured runtime performance claims yet.

## ADR 006 — Licensing (provisional distribution path; Slint route no longer applies, see ADR 012)
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

## ADR 012 — Avalonia frontend over the Rust engine (accepted, owner-directed migration)

The owner directed a migration from Slint to Avalonia UI (C#/.NET), keeping Rust for editing semantics, media processing, project state and future compositing. Avalonia 12.1.5 on .NET 10 (SDK 10.0.401 pinned by global.json), Fluent theme with CutCap tokens, Inter typeface (Avalonia.Fonts.Inter), CommunityToolkit.Mvvm 8.4.2 for source-generated observable properties and commands, compiled bindings by default. No WPF/WinUI/web layers. The C# frontend must not become a second engine: it holds no project state and derives every value from the engine view. The Slint app (apps/cutcap) was removed after native parity checks; it remains in history at a3de692. Slint, slint-build and rfd left the workspace; the DX12 diagnostic moved to `cutcap-ffi --bin cutcap-gpu-info`. The Slint `--benchmark-preview` CLI was replaced by an opt-in UI trace (CUTCAP_TRACE_PREVIEW=1) and scripts/native-ui/bench.ps1.

## ADR 013 — Pull-based, versioned C ABI with a handle registry (accepted)

`crates/cutcap-ffi` builds cutcap_engine.dll (cdylib + rlib). The Phase 1 command owner and bridge moved there unchanged; media_ui/preview/coordinator logic became a frontend-independent `Session`. The ABI (docs/NATIVE_ABI.md) uses u64 registry keys instead of pointers (stale/forged/double-destroyed handles return INVALID_HANDLE), catches panics at every export, validates every caller buffer, and has no callbacks: a 16 ms UI timer pumps and pulls a coalesced JSON view plus frames by generation. JSON is acceptable for the small, low-frequency view (≤ a few KB; capped at 8 MiB on read); frames never use it. Unsafe code is confined to abi.rs with SAFETY comments; workspace `unsafe_code = "deny"` still applies elsewhere, and core/contracts/media remain `forbid`. ABI_VERSION 1; contract version 2 and schema 1 unchanged.

## ADR 014 — Display formatting stays next to exact time (accepted)

Durations, frame rates, rotations, PTS captions and inspector rows are formatted in Rust (`cutcap-ffi/src/view.rs`, moved from the Slint presentation mapping with its tests) and sent as strings with the raw values the UI needs (milliseconds for the scrubber). The frontend never re-derives source time from floating point. Go-to text is parsed by the same tested Rust grammar via `seek_text`. Library filter and name search are presentation state in C#.

## ADR 015 — CPU frame copy into a reused WriteableBitmap (accepted)

The engine copies each presented RGBA8 frame once, row by row, into a locked Avalonia WriteableBitmap (PixelFormats.Rgba8888, opaque) that is reused while the size is unchanged; the frame is drawn with uniform fit. Measured copy cost for a 1280×720 frame: ~0.3 ms (first frame ~1 ms) versus 1.0–2.4 ms for Slint's SharedPixelBuffer path. No DX12/texture sharing with Avalonia's compositor is attempted before playback exists and measurements show the CPU path is the bottleneck.

## ADR 016 — Windows packaging (accepted for development distribution)

Release publishing is self-contained win-x64 with ReadyToRun (measured: window on screen 570–890 ms vs 1.0–1.2 s without, ~40 MB lower startup working set). Single-file is not used. Native AOT was evaluated and is not enabled: NuGet audit reports GHSA-p3jp-9v83-76w9 (high) for Microsoft.DotNet.ILCompiler 10.0.12 shipped with SDK 10.0.401, and the advisory is not suppressed; re-evaluate after an SDK update. FFmpeg/FFprobe are not bundled (ADR 006/008). No installer or public release.

## ADR 017 — Persistent framed source transport and ABI 2 (accepted engine contract; frontend handoff pending)

Continuous source decoding uses one FFmpeg process per playback/seek epoch, not one per frame. PAM supplies bounded, self-framed RGBA8 packets; official stats_mux_pre records supply nonce/index/integer PTS/timebase/packet size. Strict index and size pairing rejects mismatches, missing timestamps and non-increasing PTS. Free-form showinfo is not a continuous-playback protocol. Native libraries were evaluated but this installation has only the external static CLI; shipping native libraries would require a reviewed reproducible DLL/ABI/license setup. The MediaBackend stream operation keeps future implementations isolated and the Phase 1 single-frame route remains available.

Three decoded frames and two scheduler lookahead frames bound native queues. Instant is the video-only clock master, with actual PTS and explicit pause/buffering/seek resets. Newest available due frame wins; skipped due frames are counted. EOF does not invent last-frame duration. ABI 2 adds Play/Pause/Stop, exact frame epoch/time and transport view, and records delivery on successful copy. FrameInfo stays 24 bytes, no native callbacks/pointers escape, and source requested position remains distinct from copied/clock positions. See PHASE_2_PLAYBACK_CONTRACT.md. The owner relays the contract to Claude; visual bindings are not claimed complete.

Windows children receive a 1 GiB commit limit, one-process cap and kill-on-close job using pinned win-custody 0.1.2 (MIT OR Apache-2.0). Its narrow Job implementation was inspected; no handwritten unsafe was added outside the established ABI. Assignment occurs immediately after spawn, so the tiny launch window is not claimed sandboxed. Nested-job assignment failures stop/reap the child and surface an error. This is resource/lifetime containment, not a file/network security sandbox.

The owner authorized only a diagnostics hook in FrameView.Render and a driver opening the actual existing Avalonia app. It records distinct drawn generations separately from decoded/copied counters; physical scanout is not measured. No XAML, styles or production Play bindings are changed.

## ADR 017 — Playback UI bound to ABI 2 state; render-loop pumping during playback (accepted, Phase 2B)

The source transport reads only engine state: the Play/Pause glyph comes from `desired_playing` and the state; the readout comes from the copied frame's time (otherwise the requested position, labelled); the go-to field follows the requested position. There is no frontend clock. The library, inspector and timeline are re-applied only when their own view data changes. While playback is active, EngineHost also pumps from `TopLevel.RequestAnimationFrame` (once per compositor frame), because the 16 ms DispatcherTimer is quantized by the ~15.6 ms Windows timer: measured render gaps over 40 ms fell from ~30–35 % to ~7–16 % (PHASE_2B_PLAYBACK_UI_REPORT.md). The timer stays the idle path; no busy loop. `CUTCAP_PUMP_MODE=timer` restores timer-only pumping for A/B diagnostics. A vsync-aligned offer deadline would remove the remaining one-vsync outliers but is an engine change, left to the engine owner.

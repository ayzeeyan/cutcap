# Architecture decisions

## ADR 001 — Three crates (accepted, Phase 0)
`cutcap-contracts` is the leaf: IDs, time values, command/event DTOs and boundary errors. `cutcap-core` depends on contracts and owns validated project state. `cutcap` depends on both and owns the window, worker lifecycle and adapters. No engine crate full of stubs, async runtime or plugin system.

## ADR 002 — Exact ticks, explicit timebase (accepted)
Timeline positions are signed integer ticks in the project's rational seconds-per-tick timebase. Positive durations and checked endpoints reject invalid clips. Default is 1/720000 second: common integer and 30000/1001 frame rates map exactly. This is a storage clock, not the preview rate. Source timestamps carry their own rational timebase and a distinct type. VFR will use decoded presentation timestamps; never infer source time from frame index. Other rates require explicit conversion/rounding at a future media adapter.

## ADR 003 — Single project owner and bounded messages (accepted)
A standard-library worker owns the model. UI submission uses nonblocking `try_send`; events are drained in bounded batches by a Slint timer on the event-loop thread. No worker accesses a Slint component. Cheap operations only in Phase 0. Decode/export will require cancellable execution separate from the command owner. See ENGINE_UI_CONTRACT.md.

## ADR 004 — Independent preview presentation (accepted)
Slint 1.18.1 uses winit + software rendering for the shell. Only the app's preview adapter writes viewer properties. There is no assumption that Slint's internal renderer is a video renderer. Optional wgpu 30.0.1 diagnostics enumerate DX12 adapters without creating a device or sharing textures. No unstable Slint wgpu/winit APIs are enabled. Phase 1 compares measured CPU frames with isolated GPU integration before adopting either. FFmpeg is not a Phase 0 dependency.

## ADR 005 — Toolchain and dependency policy (accepted)
Pin Rust 1.99.1, MSVC target, rustfmt and Clippy in rust-toolchain.toml; manifests declare Rust 1.99 as the supported baseline. Commit Cargo.lock. Slint runtime/build versions match exactly. Other dependency ranges allow compatible updates only through reviewed lockfile changes. CI uses the pinned toolchain, not a floating latest action. Upgrade compiler and Slint deliberately, run all checks and a Windows launch, and record results. Four compiler jobs limit peak memory on 16 GB hardware; no measured runtime performance claims yet.

## ADR 006 — Licensing (provisional distribution path)
No application license has been chosen; packages are non-publishable. Review the Slint royalty-free desktop route with built-in AboutSlint attribution. The installed FFmpeg has `--enable-gpl --enable-version3`; do not bundle or link it by accident. LGPL-compatible shared builds, obligations, codec patents and the final application license must be resolved before distribution. See LICENSE_REVIEW.md.

## ADR 007 — Honest editor shell (accepted)
Real empty tracks, selection, track creation and new-project commands establish the model/UI round trip. View-only zoom, panel splitters, scrolling, workspace reset and About are presentation state. Unsupported media operations remain disabled with explanations. No sample clips or simulated transport.

Initial placement uses the public Slint window API at physical (32,16) on the primary display. Native maximize/restore is available. Persistent/per-monitor workspace geometry is deferred; avoid unstable backend access just to implement saved layouts in Phase 0.

Use Slint logical-key bindings for shortcuts. In 1.18, the framework accepts Escape in its popup handling path even when no popup is open, before application bindings. Clear selection uses Ctrl+Shift+A; Escape remains popup dismissal. No internal framework override is used.

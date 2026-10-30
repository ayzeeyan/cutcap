# CutCap

A native Windows video editor foundation, built with Rust and Slint. Phase 0 delivers a working editor shell and validated in-memory model. Media import, playback, project saving and export are **not implemented**.

```powershell
cargo run -p cutcap
./scripts/verify.ps1
```

Requires Windows 11 x64, the pinned Rust/MSVC toolchain and Visual Studio C++ tools/Windows SDK. Cargo discovers MSVC; see [Windows setup](docs/SETUP_WINDOWS.md) if linking fails. FFmpeg and a dedicated GPU are not needed to build/run Phase 0.

## What works

- Native window, media/viewer/inspector/timeline/status regions and About with Slint attribution.
- Three focusable drag splitters, clamped panel sizes, workspace reset, timeline zoom and scrolling.
- Real empty video/audio tracks, adding tracks, selection/inspector updates and creating a fresh in-memory project through a bounded command worker.
- UUID identities, exact tick time, explicit source clocks, validated project serialization types and tested domain invariants.
- Optional independent DX12 adapter diagnostic: `cargo run -p cutcap --features gpu-diagnostics -- --gpu-info`.

Unsupported controls are disabled with explanations. The zero playhead and 60-second ruler are static view aids; no media duration or playback is simulated. Closing or creating a new project discards the current in-memory tracks; there is no persistence/recovery yet.

| Shortcut | Action |
|---|---|
| Ctrl+N | New in-memory project |
| Ctrl+Plus / Ctrl+Minus | Timeline zoom (50–200%) |
| Ctrl+0 | Reset layout and zoom |
| Ctrl+Shift+A | Clear selection |
| Esc | Dismiss popup |
| Tab / Shift+Tab, Enter / Space | Focus / activate controls |
| Arrows on a splitter | Resize in 8 logical pixel steps |

## Workspace

`cutcap-contracts` is a leaf containing shared values/messages; `cutcap-core` owns validated project logic; `apps/cutcap` owns Slint, the coordinator, worker lifecycle and preview adapter. Core has no GUI/codec/GPU dependency. The optional wgpu probe is not a compositor. Slint shell rendering and eventual video presentation remain separate.

Start with [AGENTS.md](AGENTS.md), then [architecture](docs/ARCHITECTURE.md), [contract](docs/ENGINE_UI_CONTRACT.md), [decisions](docs/DECISIONS.md), [roadmap](docs/ROADMAP.md) and [Phase 0 evidence](docs/PHASE_0_REPORT.md). Future collaborators use [responsibilities](docs/MODEL_RESPONSIBILITIES.md) and [CLAUDE.md](CLAUDE.md).

The application license is an owner decision still outstanding. Slint's provisional royalty-free desktop route and FFmpeg distribution risks are documented in [license review](docs/LICENSE_REVIEW.md). The installed GPL FFmpeg build is not bundled or linked. No public distribution/installer is provided.

# CutCap

A native Windows video editor foundation, built with Rust and Slint. Phase 1 imports local videos, displays real metadata and asynchronously decodes source still frames. Playback, timeline editing, project saving and export are **not implemented**.

```powershell
cargo run -p cutcap
./scripts/verify.ps1
```

Requires Windows 11 x64, the pinned Rust/MSVC toolchain and Visual Studio C++ tools/Windows SDK. Real import/preview additionally requires external FFmpeg **and FFprobe 9.0.1** on PATH, or explicit CUTCAP_FFMPEG/CUTCAP_FFPROBE paths. Neither executable is bundled or downloaded. Missing tools produce an import/decode error; the shell can still launch. See [Windows setup](docs/SETUP_WINDOWS.md) and [license review](docs/LICENSE_REVIEW.md). No dedicated GPU is needed.

## What works

- Native window, media/viewer/inspector/timeline/status regions and About with Slint attribution.
- Native video picker, bounded metadata probe, real in-memory media registration/library/inspector and source selection.
- Asynchronous timestamped CPU-frame preview, exact source clocks, cancellable latest-only seeks, actual PTS captions and failure feedback. Seek selects the first frame at/after the request; after the last PTS it may report no frame.
- Three focusable drag splitters, clamped panel sizes, workspace reset, timeline zoom and scrolling.
- Real empty video/audio tracks, adding tracks, selection/inspector updates and creating a fresh in-memory project through a bounded command worker.
- UUID identities, exact tick time, explicit source clocks, validated project serialization types and tested domain invariants.
- Optional independent DX12 adapter diagnostic: `cargo run -p cutcap --features gpu-diagnostics -- --gpu-info`.

Use Import video (Ctrl+I), select an asset, then its source slider, whole-millisecond field/Seek or +1 s. The source position is independent of the timeline. Open/Save/Play/Export stay disabled. The zero timeline playhead and 60-second ruler are static view aids. Closing or creating a new project discards in-memory assets/tracks; there is no persistence/recovery. Originals are preserved.

| Shortcut | Action |
|---|---|
| Ctrl+N | New in-memory project |
| Ctrl+I | Import a local source video |
| Ctrl+Plus / Ctrl+Minus | Timeline zoom (50–200%) |
| Ctrl+0 | Reset layout and zoom |
| Ctrl+Shift+A | Clear selection |
| Esc | Dismiss popup |
| Tab / Shift+Tab, Enter / Space | Focus / activate controls |
| Arrows on a splitter | Resize in 8 logical pixel steps |

## Workspace

`cutcap-contracts` is a leaf; `cutcap-core` owns validated project logic; `cutcap-media` owns replaceable probe/decode/process behavior; `apps/cutcap` owns Slint, coordination, workers and preview presentation. Core has no GUI/codec/GPU dependency. The optional wgpu probe is not a compositor. CPU preview uses supported Slint pixel-buffer APIs with bounded copies.

Start with [AGENTS.md](AGENTS.md), then [architecture](docs/ARCHITECTURE.md), [contract](docs/ENGINE_UI_CONTRACT.md), [decisions](docs/DECISIONS.md), [roadmap](docs/ROADMAP.md) and [Phase 1 evidence](docs/PHASE_1_REPORT.md). Historical [Phase 0 evidence](docs/PHASE_0_REPORT.md) is retained. Future collaborators use [responsibilities](docs/MODEL_RESPONSIBILITIES.md) and [CLAUDE.md](CLAUDE.md).

The application license is an owner decision still outstanding. Slint's provisional royalty-free desktop route and FFmpeg distribution risks are documented in [license review](docs/LICENSE_REVIEW.md). The installed GPL FFmpeg build is not bundled or linked. No public distribution/installer is provided.

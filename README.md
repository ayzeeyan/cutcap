# CutCap

A native Windows video editor foundation: Avalonia (C#/.NET 10) over Rust. Import/play source video, save/open `.cutcap` projects, append/select/remove clips and undo/redo. Phase 3C adds exact single-track timeline frame preview through ABI 5: switch to Timeline and seek the ruler or time field. Rust selects the clip/source PTS; cuts, gaps and unavailable sources are explicit. Continuous timeline playback, compositing, audio playback, drag/trim/split and export remain unavailable.

```powershell
dotnet run --project apps/CutCap.Desktop      # builds the Rust engine DLL first
./scripts/verify.ps1                          # Rust + .NET checks and a win-x64 publish
```

Requires Windows 11 x64, the pinned Rust/MSVC toolchain, the .NET 10 SDK (global.json) and Visual Studio C++ tools/Windows SDK. Real import/preview additionally requires external FFmpeg **and FFprobe 9.0.1** on PATH, or explicit CUTCAP_FFMPEG/CUTCAP_FFPROBE paths. Neither executable is bundled or downloaded; without them import reports an error and the rest of the app still runs. See [Windows setup](docs/SETUP_WINDOWS.md) and [license review](docs/LICENSE_REVIEW.md). No dedicated GPU is needed.

## What works

- Windows window with a custom title bar over native caption buttons, resizable asset/player/inspector/timeline workspace, About and keyboard shortcuts sheet.
- Native video picker, bounded metadata probe, real in-memory media registration, media grid with collections and name search, and a metadata inspector.
- Asynchronous timestamped CPU-frame preview with exact source clocks, cancellable latest-only seeks, actual PTS captions and failure feedback. Seeks select the first frame at or after the request; after the last PTS a seek may report no frame.
- Real empty video/audio tracks, adding tracks, track selection (click or arrow keys) and a fresh in-memory project, through a bounded command worker.
- Persistent, bounded video-only playback engine: real CFR/VFR/B-frame PTS, monotonic clock, pause/resume, playing/paused seeks, buffering, EOF and cancellation. See the [playback contract](docs/PHASE_2_PLAYBACK_CONTRACT.md) and [Phase 2 report](docs/PHASE_2_PLAYBACK_REPORT.md) for API integration status and actual editor measurements.
- Timeline ruler/lanes drawn by custom controls, zoom 50–200 % and two-axis scrolling.
- Background project Save/Save As/Open, exact snapshot revisions and unsaved-change protection. Missing/changed media remains in the library; relink the original through the Project menu. See [project format and integrity limits](docs/PROJECT_FORMAT.md).
- Optional DX12 adapter diagnostic: `cargo run -p cutcap-ffi --features gpu-diagnostics --bin cutcap-gpu-info`.

Audio/Text/Stickers/Effects/Transitions/Filters/Adjustment, clip inspector settings, Split, Export and track visibility/mute/lock remain unavailable. Timeline preview monitors the selected video track, otherwise the selected clip's video track, otherwise the first video track; it does not composite stacked tracks. Leaving Source pauses its transport; returning does not auto-play. New/Open/close protect unsaved document changes. Originals are never modified.

| Shortcut | Action |
|---|---|
| Space | Play / pause the source (not while typing) |
| Ctrl+N | New project |
| Ctrl+O | Open .cutcap project |
| Ctrl+S / Ctrl+Shift+S | Save / Save As |
| Ctrl+I | Import a local source video |
| Ctrl+Plus / Ctrl+Minus | Timeline zoom (50–200%) |
| Ctrl+0 | Reset layout and zoom |
| Ctrl+Shift+A | Clear selection |
| Arrows / Page Up / Page Down on the scrubber | Step 100 ms / 1 s |
| Enter in the go-to field | Request the typed source time |
| Arrows in lists, Enter / Space | Move selection / activate |

## Repository

`crates/cutcap-contracts` (IDs, exact time, DTOs), `cutcap-core` (validated project), `cutcap-media` (FFmpeg probe/decode/process ownership) and `cutcap-ffi` (engine session + versioned C ABI → `cutcap_engine.dll`); `src-dotnet/CutCap.Interop` (P/Invoke, SafeHandle, view DTOs); `apps/CutCap.Desktop` (Avalonia views, view models, controls); `tests/` (.NET tests, fixtures, manual UI checks).

Start with [AGENTS.md](AGENTS.md), then [architecture](docs/ARCHITECTURE.md), [native ABI](docs/NATIVE_ABI.md), [contract](docs/ENGINE_UI_CONTRACT.md), [decisions](docs/DECISIONS.md), [roadmap](docs/ROADMAP.md) and the [migration report](docs/AVALONIA_MIGRATION_REPORT.md). Phase reports are retained as history.

The application license is an owner decision still outstanding. The installed GPL FFmpeg build is not bundled or linked. No public distribution or installer is provided.

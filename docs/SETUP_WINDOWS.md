# Windows 11 x64 setup

## Timeline playback (Phase 3D)

ABI/contract 6 add independent Timeline Play/Pause/Stop to the existing editor.
Select Timeline and monitor a video track. Leaving either mode pauses it. Stopped
seeks use ceiling frames; continuous seeks use actual-PTS intervals. Structural
edits stop/re-evaluate. No audio/compositing/export or FFmpeg distribution change.
Read PHASE_3D_TIMELINE_PLAYBACK_CONTRACT.md and the Phase 3D report.

For opt-in Release measurements on an otherwise idle desktop:

```powershell
./scripts/generate-playback-fixtures.ps1
dotnet build tools/CutCap.PlaybackProbe -c Release
./tools/CutCap.PlaybackProbe/bin/Release/net10.0/win-x64/CutCap.PlaybackProbe.exe --timeline-play .artifacts/playback/sources .artifacts/phase3d/timeline-performance.json
python scripts/summarize-timeline-playback.py .artifacts/phase3d/timeline-performance.json .artifacts/phase3d/summary.json
# Use your disposable two-source, >=12s-per-source QA project for cleanup timing:
./tools/CutCap.PlaybackProbe/bin/Release/net10.0/win-x64/CutCap.PlaybackProbe.exe --timeline-cleanup .artifacts/phase3d/native-two-source.cutcap .artifacts/phase3d/cleanup.json
```

Create the output directory first. The playback driver opens the original App,
EngineHost and FrameView; it owns only generated disposable projects. The cleanup
driver uses the real native ABI and reports cancellation→frame-copy and disposal,
not rendering. Do not run builds/tests/other FFmpeg alongside pacing measurements.
JSON/logs remain local; publish only reviewed, sanitized measurement artifacts.

## Timeline preview (Phase 3C)

Switch Player → Timeline, then click the ruler or enter a time. `2160000 ticks` addresses an exact project position; numeric/timecode inputs use milliseconds. The viewer monitors the selected video track (selected clip's track or first video track as fallback). Gaps clear the picture. Timeline Play is intentionally disabled; switching away from Source pauses its transport.

Native diagnostics reuse the existing editor, not a separate demo:

```powershell
# In a disposable directory containing media/A red.mkv and media/B blue.mkv:
tools/CutCap.PlaybackProbe/bin/Release/net10.0/win-x64/CutCap.PlaybackProbe.exe --timeline .artifacts/phase3c .artifacts/phase3c/measurement.json
# Same editor at minimum geometry; holds the final real still for visual QA.
# Close that diagnostic window to finish/write its report:
tools/CutCap.PlaybackProbe/bin/Release/net10.0/win-x64/CutCap.PlaybackProbe.exe --timeline-min .artifacts/phase3c .artifacts/phase3c/minimum.json
```

The two synthetic sources can be generated with the separately installed FFmpeg: `-f lavfi -i color=c=red:s=320x180:r=4:d=3 -c:v ffv1` and the equivalent blue source. Use `-n` and disposable output paths; never overwrite user media. The driver builds a 5,000-clip fixture from a real saved project on a background task. Draw callbacks measure rendering/recording, not physical scanout; run benchmarks without concurrent builds/other media apps.

## Build requirements

- Git for Windows.
- Rust via rustup, with the `x86_64-pc-windows-msvc` host/target. This repository pins 1.99.1, rustfmt and Clippy; `rustup show` installs the pinned toolchain if needed. No nightly.
- .NET SDK 10.0.401 or a later 10.0 feature band (`global.json`, rollForward latestFeature). Verified with 10.0.401. No workloads are needed; NuGet restores Avalonia 12.1.5, CommunityToolkit.Mvvm 8.4.2 and the test packages.
- Visual Studio or Build Tools with Desktop development with C++, MSVC x64 tools and Windows SDK (Rust's linker). This workstation has Visual Studio Community 2026 with MSVC 19.51.36260 and SDK 10.0.26100.0.
- A Windows interactive desktop for launch/UI checks. No Qt, Node, web server, FFmpeg development DLLs, CMake or GPU SDK is required.

Install missing system tools explicitly with their official installers; build scripts never install anything system-wide.

```powershell
Set-Location C:\Users\Izyan\General\CutCap
rustup show
dotnet --version
cargo build --workspace --locked
dotnet build CutCap.slnx
dotnet run --project apps/CutCap.Desktop
```

`CutCap.Interop.csproj` runs `cargo build -p cutcap-ffi` (Debug) or `cargo build -p cutcap-ffi --release` (Release) before building and copies `target\<profile>\cutcap_engine.dll` next to the app; set `-p:CutCapSkipCargo=true` when the DLL is already built. Rust normally discovers Visual Studio's linker; if discovery fails use x64 Developer PowerShell. Do not force Git's `/usr/bin/link.exe` into Cargo linker settings.

## Checks and logs

```powershell
./scripts/verify.ps1            # add -SkipPublish to skip the win-x64 publish
# Or individually:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
cargo build --release
dotnet build CutCap.slnx -c Debug
dotnet test CutCap.slnx -c Debug --no-build
dotnet build CutCap.slnx -c Release
dotnet publish apps/CutCap.Desktop/CutCap.Desktop.csproj -c Release -r win-x64 --self-contained true -o artifacts/publish/win-x64
```

The script stops on failure and saves logs and exit codes in `.artifacts/verification/`. Release Rust uses thin LTO and stripped debug info; Release .NET publishes self-contained ReadyToRun output (about 222 MB, 228 files) to `artifacts/publish/win-x64/CutCap.exe`. Do not benchmark Debug builds. Native AOT is not enabled (ADR 016).

```powershell
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo run -p cutcap-ffi --features gpu-diagnostics --bin cutcap-gpu-info
```

The optional diagnostic uses wgpu 30/DX12, enumerates adapters and prints name/backend/device type/driver. It creates no device or texture.

## External media runtime

Install/configure FFmpeg **and FFprobe** explicitly; CutCap never installs, copies or packages them. The tested development build is 9.0.1-essentials_build-www.gyan.dev (GPLv3/static). Distribution is not cleared; read LICENSE_REVIEW.md before packaging.

```powershell
Get-Command ffmpeg, ffprobe
# Optional per-process configuration when the executables are not on PATH:
$env:CUTCAP_FFMPEG = 'C:\path with spaces\ffmpeg.exe'
$env:CUTCAP_FFPROBE = 'C:\path with spaces\ffprobe.exe'
dotnet run --project apps/CutCap.Desktop
```

Import (Ctrl+I) a local MP4/MOV/M4V/MKV/WebM/AVI/TS. Select a tile to request its first frame; use the scrubber (arrows step 100 ms, Page keys 1 s), the step buttons or the go-to field (`1500`, `1.5s`, `00:01.500`, Enter). The timeline playhead stays at zero. HDR, non-quarter turns, network media and files over the budgets are rejected with a message.

```powershell
cargo test --workspace                                                   # no FFmpeg needed
cargo test -p cutcap-media --test ffmpeg_integration -- --ignored --test-threads=1
dotnet test CutCap.slnx                                                  # media interop tests skip without FFmpeg
./scripts/generate-fixtures.ps1                                          # optional fixture regeneration
```

## Diagnostics and measurement

Engine tracing goes to stderr (`RUST_LOG`, default `cutcap_engine=info,cutcap_media=info`). The app is a GUI executable, so redirect stderr with `Start-Process -RedirectStandardError` to read it. `CUTCAP_TRACE_PREVIEW=1` adds UI-side `ui-present` lines: request → frame in the bitmap, copy time and import → first frame (excludes physical paint/scan-out).

`scripts/native-ui/bench.ps1` drives the published app over real input (import a synthetic 1080p clip, five go-to seeks, idle CPU, memory, close). It sends input only while CutCap owns the foreground and aborts otherwise. Generate its clip with the ffmpeg command in its header. Results from the migration are in docs/measurements.

## Interactive smoke check

Phase 2 engine/interop playback is ABI 2. Claude still owns visual Play/Pause/Stop bindings; the opt-in probe below opens the existing editor and drives its public engine session for measurements. It does not replace views or simulate media time.

```powershell
./scripts/generate-playback-fixtures.ps1
dotnet build tools/CutCap.PlaybackProbe -c Release
# Actual Avalonia FrameView renders, seek latency, dispatcher gaps and host+child resources:
tools/CutCap.PlaybackProbe/bin/Release/net10.0/win-x64/CutCap.PlaybackProbe.exe --ui .artifacts/playback/sources .artifacts/playback/ui.json
# Offscreen ABI delivery (not rendered FPS):
tools/CutCap.PlaybackProbe/bin/Release/net10.0/win-x64/CutCap.PlaybackProbe.exe .artifacts/playback/sources .artifacts/playback/delivery.json
# Unpaced native decoder + framed pipe throughput (not presented FPS):
cargo run -p cutcap-media --release --example measure_decode -- .artifacts/playback/sources/1080p.mp4
```

Run measurement modes sequentially without builds or another benchmark. PAM frame packets are paired with nonce/index/PTS/timebase/size records; missing records are errors. Windows children are resource-limited by a kill-on-close job (1 GiB commit each). Physical scanout is not measured by the render hook. See PHASE_2_PLAYBACK_REPORT.md for actual results and limits.

See tests/ui_smoke.md. Record the actual DPI scale and window sizes tested; CI cannot validate a visible desktop.

## Optional tools already installed (audited)

Phase 3A workflow: Ctrl+S (first save opens Save As), Ctrl+Shift+S, Ctrl+O, and Project menu → Relink selected source. Files end in `.cutcap`; see PROJECT_FORMAT.md for reference portability, revalidation and integrity limits. Open/Save are background operations; wait for the actual engine Saved badge. The playback diagnostic also accepts `--ui-min` to run the existing editor at its 960×600 minimum; its report records actual client size/scale and startup state transitions. Synthetic diagnostic sessions discard their own data on exit; ordinary editor close keeps Save/Discard/Cancel protection.

Git 2.56.0.windows.4, CMake 4.5.0-rc2 (unused), FFmpeg/ffprobe 9.0.1, Rust/Cargo 1.99.1, Windows 11 x64, Intel Core Ultra 7 255H, 16 GB RAM, Intel Graphics driver 32.0.101.9150. Phase 3A observed .NET SDK **10.0.403**: global.json requests 10.0.401 with `rollForward: latestFeature`; it does not enforce that exact installed patch. No system tool was installed for this milestone.

The GitHub workflow uses hosted Windows: formatting, all-features Clippy/tests, locked Rust builds, .NET build/test/Release build and a win-x64 publish that is not uploaded. A configured workflow is not evidence of a remote run.

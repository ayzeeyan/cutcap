# Windows 11 x64 setup

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

Git 2.56.0.windows.4, CMake 4.5.0-rc2 (unused), FFmpeg/ffprobe 9.0.1, Rust/Cargo 1.99.1, .NET SDK 10.0.401, Windows 11 10.0.26300, Intel Core Ultra 7 255H, 16 GB RAM, Intel Graphics driver 32.0.101.9150.

The GitHub workflow uses hosted Windows: formatting, all-features Clippy/tests, locked Rust builds, .NET build/test/Release build and a win-x64 publish that is not uploaded. A configured workflow is not evidence of a remote run.

# Windows 11 x64 setup

## Build requirements

- Git for Windows.
- Rust via rustup, with the `x86_64-pc-windows-msvc` host/target. This repository pins 1.99.1, rustfmt and Clippy; `rustup show` installs the local pinned toolchain if needed. No nightly.
- Visual Studio or Build Tools with Desktop development with C++, MSVC x64 tools and Windows SDK. Visual Studio 2022 is suitable with that workload; this workstation actually has Visual Studio Community 2026 and was verified with MSVC 19.51.36260 plus SDK 10.0.26100.0.
- A Windows interactive desktop for launch/UI checks. No Qt, Node, web server, FFmpeg development DLLs, CMake or GPU SDK is required for the default build.

Install missing system tools explicitly using their official installers and required workloads; do not run a silent system-wide installer from a build script. The initial machine already had the toolchain. Only a per-user Rust 1.99.1 toolchain and Cargo dependencies were downloaded during scaffolding.

```powershell
Set-Location C:\Users\Izyan\General\CutCap
rustup show
rustc --version
cargo --version
git --version
cargo build --workspace --locked
cargo run -p cutcap
```

Rust normally discovers Visual Studio's linker without `cl.exe` on PATH. If discovery fails, use x64 Developer PowerShell for your installed Visual Studio and verify the C++ workload/SDK. Do not force Git's `/usr/bin/link.exe` into Cargo linker settings. Run `vswhere` under `C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe` to find the installation.

## Checks and logs

```powershell
cargo fmt --all
./scripts/verify.ps1
# Or individually:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
cargo build --release
```

The script stops on failure and saves logs and exit codes in `.artifacts/verification/`. Build output is `target\debug\cutcap.exe` or `target\release\cutcap.exe`. Cargo.lock is committed; use `--locked` in CI and reproducible builds. The build uses four compiler jobs to limit memory pressure. The default feature set excludes GPU diagnostics and all codec linkage. Release uses thin LTO and stripped debug info; do not benchmark debug builds as release performance.

```powershell
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo run -p cutcap --features gpu-diagnostics -- --gpu-info
```

The optional diagnostic uses wgpu 30/DX12, enumerates adapters and prints name/backend/device type/driver. It creates no device or texture and is not a rendering benchmark. A DX12-compatible driver is needed for that command only. The UI deliberately selects Slint winit/software via public APIs; it does not rely on GPU video presentation or `SLINT_BACKEND` overrides.

## Phase 1 external media runtime

Install/configure FFmpeg **and FFprobe** explicitly; CutCap never installs, copies or packages them. The tested development build is 9.0.1-essentials_build-www.gyan.dev (GPLv3/static). The owner authorized local development use; distribution is not cleared. Read LICENSE_REVIEW.md before packaging. Native codec development headers/DLLs are unnecessary for this adapter.

```powershell
Get-Command ffmpeg, ffprobe
ffmpeg -version
ffprobe -version
# Optional per-process configuration when the executables are not on PATH:
$env:CUTCAP_FFMPEG = 'C:\path with spaces\ffmpeg.exe'
$env:CUTCAP_FFPROBE = 'C:\path with spaces\ffprobe.exe'
cargo run -p cutcap
```

Use Import video/Ctrl+I, choose a local MP4/MOV/M4V/MKV/WebM/AVI/TS, and inspect the real library/inspector. The first non-cover video stream is used; support depends on the installed decoder. Select an entry to request its initial frame. Use source slider, Source ms + Seek/Enter, or +1 s; timeline playhead stays zero. Ceiling selection may report NoFrame after the last PTS even inside nominal duration; try earlier. HDR/non-quarter turns/network media and files over current budgets are rejected. Playback/audio/export remain unavailable. Missing executables/files, invalid media and process failures appear as errors without a fake successful import.

```powershell
# Committed small fixtures require no generation for default tests.
cargo test --workspace
# Real external-process integration; never downloads tools:
cargo test -p cutcap-media --test ffmpeg_integration -- --ignored --test-threads=1
# Optional synthetic fixture regeneration and representative source generation:
./scripts/generate-fixtures.ps1 -Performance
cargo build --release
./scripts/measure-preview.ps1 -Runs 3
```

The measurement script runs the **same release CutCap UI**, registers each generated 720p/1080p/4K source and presents ten frames per source. JSON/raw logs and sampled app working-set/private memory are in ignored .artifacts/performance. Timing ends at Slint image assignment, not physical display/scanout; memory excludes child processes and sampling can miss peaks. Run without concurrent Cargo builds and record background workload, hardware, sample count and variation. Do not use debug measurements as release performance. Developer CLI: `cutcap.exe --benchmark-preview <source-directory> <report.json>`; it drives existing media coordination, creates no separate prototype and writes the report after the event loop stops.

Default CI needs no FFmpeg installation. It runs meaningful pure parsing/clock/buffer/latest-worker and subprocess ownership tests; external FFmpeg tests are explicitly ignored unless invoked with --ignored. The ignored child-process test is a helper invoked by its ownership tests, not an untested behavior claim.

For debugging, run a debug build from PowerShell or use the MSVC Rust debugger in your editor. Set `$env:RUST_LOG = 'cutcap=debug'` and `$env:RUST_BACKTRACE = '1'` for a session as needed. Logs contain project/request state, not media buffers. Release suppresses the console window; to collect output, redirect stdout/stderr when launching with Start-Process and read the resulting files. No file-based rotating logger, crash dump collector or recovery system is implemented.

## Interactive smoke check

Launch, confirm all panels, select Video 1 / Audio 1, add both track kinds, clear selection/Ctrl+Shift+A, use zoom buttons and Ctrl+Plus/Minus, scroll the timeline, drag all three splitters and use arrow keys on focused splitters, then Reset layout/Ctrl+0. New Project/Ctrl+N resets to two empty tracks. About must show the Slint attribution; Escape/click outside/Close dismiss it. Confirm unavailable buttons remain disabled and show tooltips. Close the window and check that the process/worker exit cleanly. Verify minimum/normal/maximized sizes and record actual DPI scale. Do not claim media playback from this check.

## Optional tools already installed (audited)

Git 2.56.0.windows.4, CMake 4.5.0-rc2, FFmpeg/ffprobe 9.0.1, Rust/Cargo 1.99.1, Windows 11 10.0.26300, Intel Core Ultra 7 255H, 16,582,352,896 bytes reported RAM, Intel Graphics driver 32.0.101.9150. CMake is a release candidate and unused. FFmpeg is the GPL/static Gyan development runtime; it is not a bundled distribution runtime.

The GitHub workflow uses hosted Windows, formatting, all-features Clippy/tests, locked debug and release builds, a Cargo/build cache and read-only repository permission. No special credentials or automatic publishing. A configured workflow is not evidence of a remote run; the initial repository has no remote configured.

# Development workflow

[AGENTS.md](../AGENTS.md) is the primary authority. This document explains the routine rather than introducing a second rule set.

1. Read relevant architecture/contract/ADRs and inspect affected call sites. Identify whether the task changes contracts, domain state, coordination, UI or a native adapter.
2. Use a small `codex/…` branch. Parallel contributors use isolated branches/worktrees with explicit shared-file ownership. Preserve unrelated work.
3. Make the smallest real implementation. Update docs/DECISIONS.md for boundary, schema, dependency or unstable API changes. Do not add mock functionality to make a milestone appear complete.
4. Format with `cargo fmt --all` (C# follows .editorconfig and warnings-as-errors); run `./scripts/verify.ps1`. It fails at the first failed command and writes logs/results under ignored `.artifacts/verification/`. Read failures; fix the cause.
5. For `gpu-diagnostics`, additionally run all-features Clippy/tests and `cargo run -p cutcap-ffi --features gpu-diagnostics --bin cutcap-gpu-info`. A missing DX12 adapter is reported, not a reason to make the software shell fail.
   For media changes, also run `cargo test -p cutcap-media --test ffmpeg_integration -- --ignored --test-threads=1` with the documented external tools. Default tests deliberately need no FFmpeg installation. Generate fixtures only from the supplied synthetic sources; never replace expected colors/PTS to accommodate a decoder bug. Performance is separate: publish Release and run scripts/native-ui/bench.ps1 on an idle desktop without concurrent builds.
6. Launch after UI changes. Exercise affected actions, keyboard/focus, splitters, scrolling and unavailable states at minimum/normal sizes. Record actual DPI conditions. CI cannot validate a visible desktop.
7. Self-review coupling, time arithmetic, failure atomicity, shutdown/backpressure, resource budgets, licensing and documentation truth. Never claim an unmeasured performance improvement.
8. Hand off implementation, changed files, actual checks/results, limitations, architecture consequences and next step. A checklist requires evidence, not confidence.

Rust 1.99.1, the .NET 10.0.401 SDK and Avalonia 12.1.5 are pinned; Cargo.lock and Directory.Packages.props are committed. Upgrades are deliberate changes, not incidental lockfile regeneration. Four compiler jobs reduce compilation pressure on 16 GB hardware. Native dependencies belong behind adapters with reproducible setup and license records. See SETUP_WINDOWS.md for tooling and LICENSE_REVIEW.md before adding FFmpeg binaries.

No release publishing, secrets or user telemetry belong in validation. The Windows workflow runs on hosted Windows with repository-read permission and cache only. Local scripts use PowerShell and standard Cargo/dotnet commands.

For persistence changes, read PROJECT_FORMAT.md: byte/structure/graph validation precedes adoption, Save completion marks the captured revision only, and failures preserve both the live project and prior file. Test with disposable source directories. Close the local editor before running verify.ps1: Windows locks loaded engine DLLs and prevents a rebuild from replacing them. global.json requests SDK 10.0.401 but currently permits latestFeature roll-forward (Phase 3A used 10.0.403); record the actual SDK, not just the requested version.

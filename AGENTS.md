# CutCap engineering constitution

Read this file before editing. It is the repository's primary engineering authority, subordinate to explicit instructions from the human owner. Read the relevant architecture, contract, decisions and task context next. Apply these rules with engineering judgment: a justified, documented exception is preferable to speculative infrastructure.

## Product and scope

CutCap is a native, offline-first Windows 11 x64 non-linear video editor. The stack is deliberately two-language: **C#/.NET 10 + Avalonia 12 own presentation and interaction; Rust (MSVC) owns editing semantics, media processing, timeline/project state, future wgpu/DX12 compositing and heavy computation.** FFmpeg is the media backend. The C# frontend must never grow into a second editing engine. The baseline machine has a Core Ultra 7 255H, integrated Intel graphics and 16 GB RAM. Never assume dedicated-GPU memory, cloud availability or online accounts. Phase 2B integrates real source playback. Phase 3A adds project persistence/relinking and startup preroll stabilization; Phase 3B adds append/remove/select clip editing with bounded Rust-owned undo/redo through ABI 4. Trim/split/move and timeline playback remain future work. Consult docs/ROADMAP.md before expanding scope.

## Architecture and ownership

- Inspect the existing repository and call sites before modifying a module. State which modules a substantial task affects.
- Dependency direction is `CutCap.Desktop → CutCap.Interop → (C ABI) cutcap-ffi`, and `cutcap-ffi → cutcap-core → cutcap-contracts`, `cutcap-ffi → cutcap-media → cutcap-contracts`, with cutcap-ffi also using contracts directly. Keep it acyclic. Core never imports UI, wgpu, FFmpeg or file-system services. Only CutCap.Interop calls native code; view models never see native pointers or handles.
- Contracts contain stable IDs, exact time values, commands, view DTOs and boundary errors. Core owns domain invariants. The app owns process/window lifecycle, command worker, coordination and preview presentation.
- One command owner mutates the project. The Avalonia UI (dispatcher) thread owns views, view models and the frame bitmap, and is the only thread that calls the engine session. Rust workers send owned messages to the session; nothing native calls back into .NET. The frontend pulls the coalesced view and frames (docs/NATIVE_ABI.md).
- Do not decode, encode, probe files, generate waveforms, perform blocking I/O, wait on channels or do expensive model work in UI handlers or ABI calls. ABI commands only enqueue (`try_send`) and the pump only polls. Keep command/result queues bounded, drain batches bounded, and surface queue-full/disconnection errors.
- New expensive tasks need their own bounded, cancellable execution path. Do not turn the command worker into a decoder. Document queue budgets, backpressure, ownership and shutdown order.
- Preserve Phase 1's single probe and latest decoder (one active/pending/completed). Match ProjectId/MediaId/RequestId for successes and errors. Keep its ceiling source-PTS policy explicit, validate actual timestamps, and never use the ephemeral preview mailbox for durable results. Retest CLI compatibility, time/orientation/buffer contracts and child cleanup before changing the FFmpeg adapter.
- Preserve Phase 2's persistent worker (one active/one pending epoch), three decoded frames and two scheduler lookahead frames. Follow PHASE_2_PLAYBACK_CONTRACT.md: real PTS, monotonic Rust clock, separate requested/offered/copied/rendered meanings, bounded backpressure and current-epoch terminal errors. Never call decoded/copied FPS physical presented FPS. Keep the Phase 1 isolated fallback and Windows per-child job limits; framing/PTS changes require real-media regression evidence.
- Keep rendering, codec, presentation and serialized state separate. Preview/export must eventually evaluate the same timeline/effect semantics. A native compatibility adapter may change without rewriting the model or UI.
- Major moves, public API changes, schema changes and contract changes need a decision entry and documentation updates in the same change. Never change an API silently. Preserve old data through an explicit migration or clearly reject unsupported versions.

## Rust quality

- Prefer idiomatic Rust, explicit types, clear ownership and small focused modules/public APIs. Follow existing naming and layout. Avoid hidden side effects and unnecessary cloning/allocations in hot paths.
- Compose concrete behavior before adding traits/frameworks. No empty crates, speculative extension systems or generic service layers. Add dependencies only for a real need, after checking version compatibility, features, license and maintenance; update the lockfile intentionally.
- Use typed recoverable errors. Do not `unwrap`, `expect`, index unchecked, or panic for ordinary operational failures. Tests and proven compile-time constants may use assertions. Generated code is third-party output, not a precedent for handwritten code.
- Handwritten unsafe is prohibited unless a measured compatibility need cannot be solved safely. Any exception must be isolated, locally allowed with a rationale, and document pointer validity, lifetime, aliasing, thread and resource invariants plus tests. Core/contracts/media forbid unsafe entirely. The one accepted exception is the C ABI in `crates/cutcap-ffi/src/abi.rs` (ADR 013): it never unwinds a panic across the boundary, never dereferences a caller-supplied session pointer (sessions are registry keys), validates every buffer before use and keeps unsafe out of the rest of the crate.
- Keep the ABI small and versioned. Any signature, struct-layout, status or consumed-view-field change bumps `ABI_VERSION` in Rust and `NativeMethods.AbiVersion` in C# together, with NATIVE_ABI.md, a decision entry and tests on both sides.
- Keep original media unchanged. Edits reference source assets. Never delete or overwrite source media as part of an edit.
- Validate untrusted project/media inputs before use. Deserialization must preserve domain invariants. Future file loaders must bound file bytes, collection sizes, dimensions and decoded allocations before committing state; a collection limit after deserialization is not a complete hostile-file sandbox.
- Avoid data races and uncontrolled shared mutable state. Prefer ownership transfer over broad `Arc<Mutex<...>>` graphs. Do not discard failed operations or report success before a terminal result.

## C# and .NET quality

- Nullable reference types and warnings-as-errors are on for every project (Directory.Build.props). Do not suppress analyzers, NuGet audit or Avalonia warnings without a recorded justification; a security advisory is not suppressed to make a build pass.
- MVVM: strongly typed view models (CommunityToolkit.Mvvm source generators), compiled bindings (`x:DataType`) and no business logic in code-behind. Code-behind is for view glue only (scroll sync, frame presentation, file picker).
- View models hold presentation state only. Project data, selection, media and time come from the engine view; actions are engine calls. Never cache or mutate a parallel copy of project state.
- Native resources: `SessionSafeHandle` owns the engine session; dispose deterministically (after stopping the pump). No raw native pointer in a long-lived object. Use `LibraryImport`, explicit pointer + length strings (UTF-8; UTF-16 for paths) and blittable structs that mirror `#[repr(C)]` layouts.
- No synchronous I/O, waiting or native blocking on the UI thread; use the async file picker and the pump. Avoid uncontrolled event subscriptions: unsubscribe when a view's DataContext changes.
- Keep frames out of view-model properties. The view pulls pixels into its own bitmap by generation.
- Add dependencies only for a real need, after checking version, license, maintenance and trimming/AOT impact; versions are central in Directory.Packages.props.

## Time and model rules

- Persistence uses `.cutcap` envelope format 1 around core schema 1 (docs/PROJECT_FORMAT.md); source expectations/fingerprints are outside Project. Keep file I/O on the owned file worker. Validate byte/structure budgets before typed graph construction and adopt only complete valid documents.
- Document revisions belong to the project owner and are distinct from coalesced view revisions. Mark only the captured successfully written revision saved; newer edits stay dirty. Never equate ABI acceptance with save completion. Preserve the previous file/live project on failure and retain missing references.
- Relative source paths mean project-directory-relative descendants; never reinterpret them against the working directory. Relink requires verified original content/metadata. Do not silently conform changed footage. Preserve unsaved-change confirmation for New/Open/close.
- File cancellation is checked before commit and between chunks. An OS call already in progress is not interruptible. Preserve the bounded shutdown/detached-owned-task policy in PROJECT_FORMAT.md; never introduce a UI-thread wait for filesystem completion or claim universal crash/power-loss safety.

- Authoritative timeline time is integer ticks in an explicit project timebase, never floats, UI coordinates or preview frame numbers. Use checked arithmetic and positive durations.
- Source PTS and timeline positions have distinct types. Source PTS may be negative and comes with its own timebase. VFR requires actual presentation timestamps, not nominal frame-rate multiplication.
- Preserve typed stable IDs, graph references, media/track compatibility, schema version and atomic failed mutations. In schema 1, starts are nonnegative and clips cannot overlap on a track; changing this requires defined compositing semantics and migration review.
- No decoded images, audio samples, GPU handles, caches, selection or panel geometry in serialized Project data. UI selection is session state; layout and zoom are presentation state.

## Testing and completion evidence

- Add meaningful invariant/behavior tests for domain changes, and regression tests for bugs. Never delete, weaken or ignore a test to obtain a pass. Use deterministic fixtures and bounded timeouts for channel tests. Do not add tests that merely mirror trivial styling or implementation details.
- Keep benchmarks separate from unit tests. Measure on representative hardware before claiming speed, memory or frame-pacing improvements. Validate generated media against expected timestamps, duration, image/audio content and independent probes when relevant.
- Run `./scripts/verify.ps1` for substantial changes. It executes Rust formatting, warning-free Clippy, tests, debug and release builds, then .NET Debug build, tests (interop tests use the real DLL), Release build and the self-contained win-x64 publish. For media changes also run the ignored FFmpeg integration suite; for diagnostic changes the all-features checks and DX12 probe (SETUP_WINDOWS.md).
- After UI changes launch on Windows, inspect the actual rendered window and check affected controls at minimum and normal sizes. Exercise keyboard focus/shortcuts, scrolling, disabled states and tooltips. Record which DPI/scaling conditions were actually tested; do not claim full accessibility certification from an enabled feature flag.
- CI does not replace a Windows interactive launch. Report commands, actual exit codes/test counts, relevant evidence and all unexecuted checks truthfully. Do not invent results, benchmarks, screenshots or media.
- Do not claim remote CI passed merely because its workflow exists. Do not publish builds automatically.

## Performance discipline

- Keep caches bounded by bytes and eviction policy, queues by capacity, and in-flight work by concurrency. Track frame size/stride, decoded audio, proxy and GPU allocations explicitly when added.
- Prefer predictable frame pacing and responsiveness to peak throughput. Avoid unnecessary frame copies, but retain a CPU presentation/fallback experiment where practical. Optimize only after profiling; do not add unsafe or unstable APIs based on guessed gains.
- Cancellation must be cooperative and bounded. Reliable terminal results cannot share a lossy preview-frame mailbox. Progress may be coalesced; project mutations and terminal errors may not be silently dropped.
- Shut down after the UI event loop: drop the result receiver before closing requests and joining the cheap worker. Future long-running subsystems must not make window close wait indefinitely.

## UI rules

- Reuse `apps/CutCap.Desktop/Themes` (Tokens, ControlThemes, Styles) and existing controls. Add semantic tokens instead of scattering arbitrary colors; no purple/violet. Icons are authored SVGs in Assets/Icons converted by scripts/generate-icons.py. Keep data mapping/formatting in Rust view DTOs and layout/interaction in focused XAML views.
- Preserve list item identity across engine updates (sync collections in place by id) so keyboard focus and per-item view state survive. Verify a shortcut immediately after clicking a track.
- Preserve Windows window chrome, resizing, logical pixel/DPI behavior, keyboard navigation, named accessible controls, clear hover/focus/selection, readable contrast and tooltips.
- Do not add decorative motion, fake thumbnails, progress, playback state or success indicators. Unavailable operations stay visibly disabled with an explanation; supported controls must work.
- Frame presentation is `Controls/FrameView.cs` over `cutcap_session_copy_frame`. Avalonia's compositor is not the video presentation architecture. Do not use Avalonia internal/unstable APIs or GPU texture interop without a decision recording version pin, risks, CPU fallback and measurement. Timeline surfaces are custom-drawn and viewport-aware; do not add a control per tick or clip.

## Toolchain and licensing

- Rust is pinned by rust-toolchain.toml and the .NET SDK by global.json; package versions are exact (Cargo.lock committed, Directory.Packages.props). Upgrade deliberately and re-run all checks plus a Windows launch. Use PowerShell-compatible tooling. Four compiler jobs are the default on the 16 GB workstation.
- Do not use Git's POSIX `link.exe` as the MSVC linker. Rust discovers Visual Studio; use Developer PowerShell or the installed VS environment if discovery fails.
- Do not bundle the installed GPL-enabled FFmpeg. Phase 1 invokes external FFmpeg/FFprobe as owner-authorized development dependencies; it does not link/copy/package them. A subprocess does not automatically resolve distribution licensing. Read LICENSE_REVIEW.md before adding native binaries, codecs, fonts or other assets.
- The application license is undecided and crates are non-publishable. Do not assign an application license without an owner decision and documented tradeoffs. Keep third-party notices (Avalonia MIT, Inter OFL 1.1) reachable from About. Avoid GPL-only/nonfree components that undermine the intended distribution route.
- Never add credentials, secrets, telemetry, cloud services, release publishing or system-wide installation as incidental scaffolding.

## AI collaboration and handoff

- Read AGENTS.md and relevant docs on each new task; both GPT and Claude obey this authority. CLAUDE.md is a short pointer, not a second constitution.
- Phase 0 is owned entirely by GPT-6.1 Sol. Future specialization is in MODEL_RESPONSIBILITIES.md and is adjustable by evidence and owner direction. Do not depend on another model being available.
- Respect active file/module ownership. Parallel changes require isolated Git branches/worktrees and explicit ownership of shared contracts; default branch prefix is `codex/`. Do not overwrite another contributor's work or revert unrelated changes.
- Prefer small reviewable changes. A shared API change must include producer, consumer, tests, docs and schema/contract-version consideration. Review the actual diff before handoff.
- Distinguish implemented, partial, planned and verified. Never claim completion without evidence. Preserve a reproducible next step instead of handing over an unexplained code dump.

## Required substantial-task report

Conclude with (1) implementation summary, (2) changed files/modules, (3) validation performed, (4) actual results, (5) known limitations/unexecuted checks, (6) architectural consequences, and (7) the recommended next step. Record durable evidence in docs when it matters for later collaborators. These requirements are not a reason to build speculative subsystems or ask for routine implementation approval.

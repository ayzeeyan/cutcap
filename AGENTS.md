# CutCap engineering constitution

Read this file before editing. It is the repository's primary engineering authority, subordinate to explicit instructions from the human owner. Read the relevant architecture, contract, decisions and task context next. Apply these rules with engineering judgment: a justified, documented exception is preferable to speculative infrastructure.

## Product and scope

CutCap is a native, offline-first Windows 11 x64 non-linear video editor. Rust/MSVC, Slint, future independent wgpu/DX12 rendering and FFmpeg are the chosen stack. The baseline machine has a Core Ultra 7 255H, integrated Intel graphics and 16 GB RAM. Never assume dedicated-GPU memory, cloud availability or online accounts. Phase 0 is a shell and validated model, not a functioning media editor. Consult docs/ROADMAP.md before expanding scope.

## Architecture and ownership

- Inspect the existing repository and call sites before modifying a module. State which modules a substantial task affects.
- Dependency direction is `cutcap → cutcap-core → cutcap-contracts`, with the app also using contracts directly. Keep it acyclic. Core never imports Slint, wgpu, FFmpeg or file-system services.
- Contracts contain stable IDs, exact time values, commands, view DTOs and boundary errors. Core owns domain invariants. The app owns process/window lifecycle, command worker, coordination and preview presentation.
- One command owner mutates the project. The Slint event-loop thread owns UI handles/models. Workers send owned messages; they never mutate UI objects. A Weak handle may only be upgraded on its owning UI thread, or through Slint's documented event-loop dispatch API.
- Do not decode, encode, probe files, generate waveforms, perform blocking I/O, wait on channels or do expensive model work in GUI callbacks. Current callbacks use `try_send`. Keep command/result queues bounded, drain batches bounded, and surface queue-full/disconnection errors.
- New expensive tasks need their own bounded, cancellable execution path. Do not turn the command worker into a decoder. Document queue budgets, backpressure, ownership and shutdown order.
- Keep rendering, codec, presentation and serialized state separate. Preview/export must eventually evaluate the same timeline/effect semantics. A native compatibility adapter may change without rewriting the model or UI.
- Major moves, public API changes, schema changes and contract changes need a decision entry and documentation updates in the same change. Never change an API silently. Preserve old data through an explicit migration or clearly reject unsupported versions.

## Rust quality

- Prefer idiomatic Rust, explicit types, clear ownership and small focused modules/public APIs. Follow existing naming and layout. Avoid hidden side effects and unnecessary cloning/allocations in hot paths.
- Compose concrete behavior before adding traits/frameworks. No empty crates, speculative extension systems or generic service layers. Add dependencies only for a real need, after checking version compatibility, features, license and maintenance; update the lockfile intentionally.
- Use typed recoverable errors. Do not `unwrap`, `expect`, index unchecked, or panic for ordinary operational failures. Tests and proven compile-time constants may use assertions. Generated Slint code is third-party output, not a precedent for handwritten Rust.
- Handwritten unsafe is prohibited unless a measured compatibility need cannot be solved safely. Any exception must be isolated, locally allowed with a rationale, and document pointer validity, lifetime, aliasing, thread and resource invariants plus tests. Core/contracts forbid unsafe entirely.
- Keep original media unchanged. Edits reference source assets. Never delete or overwrite source media as part of an edit.
- Validate untrusted project/media inputs before use. Deserialization must preserve domain invariants. Future file loaders must bound file bytes, collection sizes, dimensions and decoded allocations before committing state; a collection limit after deserialization is not a complete hostile-file sandbox.
- Avoid data races and uncontrolled shared mutable state. Prefer ownership transfer over broad `Arc<Mutex<...>>` graphs. Do not discard failed operations or report success before a terminal result.

## Time and model rules

- Authoritative timeline time is integer ticks in an explicit project timebase, never floats, UI coordinates or preview frame numbers. Use checked arithmetic and positive durations.
- Source PTS and timeline positions have distinct types. Source PTS may be negative and comes with its own timebase. VFR requires actual presentation timestamps, not nominal frame-rate multiplication.
- Preserve typed stable IDs, graph references, media/track compatibility, schema version and atomic failed mutations. In schema 1, starts are nonnegative and clips cannot overlap on a track; changing this requires defined compositing semantics and migration review.
- No decoded images, audio samples, GPU handles, caches, selection or panel geometry in serialized Project data. UI selection is session state; layout and zoom are presentation state.

## Testing and completion evidence

- Add meaningful invariant/behavior tests for domain changes, and regression tests for bugs. Never delete, weaken or ignore a test to obtain a pass. Use deterministic fixtures and bounded timeouts for channel tests. Do not add tests that merely mirror trivial styling or implementation details.
- Keep benchmarks separate from unit tests. Measure on representative hardware before claiming speed, memory or frame-pacing improvements. Validate generated media against expected timestamps, duration, image/audio content and independent probes when relevant.
- Run `./scripts/verify.ps1` for substantial changes. It executes formatting, warning-free Clippy, tests, debug and release builds. For diagnostic changes also run the all-features checks and DX12 probe documented in SETUP_WINDOWS.md.
- After UI changes launch on Windows, inspect the actual rendered window and check affected controls at minimum and normal sizes. Exercise keyboard focus/shortcuts, scrolling, disabled states and tooltips. Record which DPI/scaling conditions were actually tested; do not claim full accessibility certification from an enabled feature flag.
- CI does not replace a Windows interactive launch. Report commands, actual exit codes/test counts, relevant evidence and all unexecuted checks truthfully. Do not invent results, benchmarks, screenshots or media.
- Do not claim remote CI passed merely because its workflow exists. Do not publish builds automatically.

## Performance discipline

- Keep caches bounded by bytes and eviction policy, queues by capacity, and in-flight work by concurrency. Track frame size/stride, decoded audio, proxy and GPU allocations explicitly when added.
- Prefer predictable frame pacing and responsiveness to peak throughput. Avoid unnecessary frame copies, but retain a CPU presentation/fallback experiment where practical. Optimize only after profiling; do not add unsafe or unstable APIs based on guessed gains.
- Cancellation must be cooperative and bounded. Reliable terminal results cannot share a lossy preview-frame mailbox. Progress may be coalesced; project mutations and terminal errors may not be silently dropped.
- Shut down after the UI event loop: drop the result receiver before closing requests and joining the cheap worker. Future long-running subsystems must not make window close wait indefinitely.

## UI rules

- Reuse `ui/theme.slint` and existing components. Add semantic tokens instead of scattering arbitrary styling. Keep data adapters in Rust and layout/interaction in focused Slint components.
- Preserve view-model/repeater item identity across selection changes so keyboard focus survives. Reset focus explicitly when a project replacement removes the focused item; verify a shortcut immediately after clicking a track.
- Preserve Windows window chrome, resizing, logical pixel/DPI behavior, keyboard navigation, named accessible controls, clear hover/focus/selection, readable contrast and tooltips.
- Do not add decorative motion, fake thumbnails, progress, playback state or success indicators. Unavailable operations stay visibly disabled with an explanation; supported controls must work.
- The preview adapter is `src/preview.rs`. Slint's shell renderer is not the video presentation architecture. Never import Slint internal crates or enable unstable interop without a decision recording version pin, risks, fallback and measurement.

## Toolchain and licensing

- Rust is pinned by rust-toolchain.toml. Keep runtime/build Slint versions equal and commit Cargo.lock. Use PowerShell-compatible tooling. Four compiler jobs are the default on the 16 GB workstation.
- Do not use Git's POSIX `link.exe` as the MSVC linker. Rust discovers Visual Studio; use Developer PowerShell or the installed VS environment if discovery fails.
- Do not bundle the installed GPL-enabled FFmpeg. Phase 0 invokes/links no FFmpeg. Read LICENSE_REVIEW.md before adding native binaries, codecs, fonts or other assets.
- The application license is undecided and crates are non-publishable. Do not assign an application license or remove Slint attribution without an owner decision and documented tradeoffs. Avoid GPL-only/nonfree components that undermine the intended distribution route.
- Never add credentials, secrets, telemetry, cloud services, release publishing or system-wide installation as incidental scaffolding.

## AI collaboration and handoff

- Read AGENTS.md and relevant docs on each new task; both GPT and Claude obey this authority. CLAUDE.md is a short pointer, not a second constitution.
- Phase 0 is owned entirely by GPT-6.1 Sol. Future specialization is in MODEL_RESPONSIBILITIES.md and is adjustable by evidence and owner direction. Do not depend on another model being available.
- Respect active file/module ownership. Parallel changes require isolated Git branches/worktrees and explicit ownership of shared contracts; default branch prefix is `codex/`. Do not overwrite another contributor's work or revert unrelated changes.
- Prefer small reviewable changes. A shared API change must include producer, consumer, tests, docs and schema/contract-version consideration. Review the actual diff before handoff.
- Distinguish implemented, partial, planned and verified. Never claim completion without evidence. Preserve a reproducible next step instead of handing over an unexplained code dump.

## Required substantial-task report

Conclude with (1) implementation summary, (2) changed files/modules, (3) validation performed, (4) actual results, (5) known limitations/unexecuted checks, (6) architectural consequences, and (7) the recommended next step. Record durable evidence in docs when it matters for later collaborators. These requirements are not a reason to build speculative subsystems or ask for routine implementation approval.

# Claude onboarding

Phase 3E current handoff: ABI/contract 7 add exact clip place/move/trim/split
with one undo step per gesture (docs/PHASE_3E_ADVANCED_TIMELINE_REPORT.md,
ADR 023). Start trims and splits align to the source tick grid so source_in
stays an exact integer; the frontend previews gestures locally and commits
once, using exact tick values from the view. Schema/envelope remain 1.

Phase 3D handoff: ABI/contract 6 support real independent timeline
Play/Pause/Stop across single-video-track cuts and gaps. Read
docs/PHASE_3D_TIMELINE_PLAYBACK_CONTRACT.md and the Phase 3D report. Keep positions,
clock, interval selection, stale gates and source endpoints Rust-owned. The
viewer now routes controls by mode; frames remain outside MVVM and clip documents
are never fetched per frame. No audio/compositing or schema change.

Phase 3C historical handoff: docs/PHASE_3C_TIMELINE_PREVIEW_CONTRACT.md and docs/PHASE_3C_TIMELINE_PREVIEW_REPORT.md describe ABI 5 still preview. Its exact evaluator remains in use; Phase 3D enables continuous Timeline Play under the new contract above. Project schema/envelope remain 1.

Read [AGENTS.md](AGENTS.md) first; it is the single engineering constitution. Then read [ARCHITECTURE](docs/ARCHITECTURE.md), [NATIVE_ABI](docs/NATIVE_ABI.md), [ENGINE_UI_CONTRACT](docs/ENGINE_UI_CONTRACT.md), [DECISIONS](docs/DECISIONS.md), [MODEL_RESPONSIBILITIES](docs/MODEL_RESPONSIBILITIES.md), [PROJECT_FORMAT](docs/PROJECT_FORMAT.md) and the latest [Phase 3A report](docs/PHASE_3A_PROJECT_PERSISTENCE_REPORT.md). The migration and Phase 2/2B reports remain historical evidence.

The frontend is Avalonia (C#/.NET 10) in `apps/CutCap.Desktop`, over `src-dotnet/CutCap.Interop` and the Rust engine DLL built from `crates/cutcap-ffi`. Phase 0/1 engine, media and contracts belong to GPT-6.1 Sol; the interop layer was added during the owner-directed migration and is documented for Sol's continued backend work. Proposed Claude work is Avalonia views, controls, interaction, accessibility and UI validation, with cross-review. This does not authorize concurrent edits without ownership coordination.

Keep project state, time arithmetic, decoding and GPU objects out of C#. Respect media request identities, cancellation, source/timeline separation and the ABI's ownership rules; bump the ABI version and document any change to it.

Run `./scripts/verify.ps1` and launch the Windows app for UI changes. Report actual outcomes and unsupported behavior honestly. This file introduces no rules that override AGENTS.md.

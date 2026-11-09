# Claude onboarding

Phase 4C current handoff: ABI/contract 10, core schema 3 add a bounded multi-track
mixer (docs/PHASE_4C_AUDIO_MIXING_CONTRACT.md and report, ADR 026). All audio tracks
sum in project order into one WASAPI stream with one device clock; volume, mute and
solo are persistent Rust state applied live without touching the epoch. Audio-only
sources, explicit stream placement and Unlink/Link are in. Native checks use
scripts/native-ui/mix-ui.ps1 and mix-bench-ui.ps1.

Phase 4B current handoff: ABI/contract 9, core schema 2 add real audio
(docs/PHASE_4B_AUDIO_WAVEFORMS_REPORT.md, ADR 025). Placed sources with audio
become linked video/audio pairs edited as one transaction; schema 1 migrates in
memory (refusals leave files untouched). Rust decodes exact-PTS PCM, plays it
through WASAPI and uses the device position as the timeline clock; waveforms are
low-priority min/max peaks cached in C# and never stored. Native checks use
scripts/native-ui/av-ui.ps1 and av-bench-ui.ps1 with the loopback recorder.

Phase 4A current handoff: ABI/contract 8 add real video filmstrips
(docs/PHASE_4A_TIMELINE_FILMSTRIPS_REPORT.md, ADR 024). Rust selects each
thumbnail frame (first actual PTS ≥ the sample, ceiling µs→ticks) in bounded,
cancellable, low-priority FFmpeg batches; C# lays cells on a fixed source-time
grid, caches bitmaps in a 48 MiB LRU and draws only cached images. Thumbnails
never enter the Project. Schema/envelope remain 1.

Phase 3E handoff: ABI/contract 7 add exact clip place/move/trim/split
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

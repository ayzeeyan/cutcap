# CutCap roadmap

These are dependency-ordered milestones with exit evidence, not delivery dates. Phase 0 is the native shell/model/contract foundation; see PHASE_0_REPORT.md for verified completion and limits.

| Milestone | Scope and exit evidence |
|---|---|
| 1 — Import and one real frame | Reproducible permitted FFmpeg build; bounded probe/decode job; import one local video, show verified metadata and a timestamped frame through the CPU preview adapter. Handle corrupt/missing files, timeout and cancellation without freezing the UI. Use small deterministic CFR/VFR fixtures with recorded rights. Compare measured CPU-frame cost against an isolated DX12 texture experiment on integrated Intel hardware before choosing a production path. |
| 2 — Seek and playback | Correct source PTS mapping, superseded-seek rejection, bounded caches/proxies, frame pacing and a single stream transport. Verify exact/near-keyframe and VFR seeks against independent probes. Add project save/open, schema migration entry point and atomic writes before users invest in editing. |
| 3 — Minimal editing/history | Insert, trim, split, move and delete clips; cross-track composition rules; undo/redo transactions; serialized projects with stable IDs. Deterministic timeline evaluation shared by preview/export. Test boundaries and overlapping edits. |
| 4 — Audio and synchronization | Device/output selection, bounded audio buffers, resampling and clock ownership; gain/pan and waveforms. Measure A/V drift and underruns. Keep waveform/proxy work cancellable. |
| 5 — Reliable export | One simple preset, explicit stream/timebase/color policy, progress/cancel and safe partial outputs. Compare exported frames/audio/timing to preview evaluation; test independent ffprobe/decode validation and failure cleanup. No unsupported hardware promises. |
| 6 — GPU composition and effects | Measured DX12 path, CPU fallback where practical, device loss, transforms, transitions, basic color/effect stack and cache budgets. Define color/alpha semantics before adding effects. Benchmark representative integrated-GPU projects. |
| 7 — Creative tools | Keyframes, text, typography, masks, speed changes, advanced audio/color and proxies, each with shared preview/export behavior and regression fixtures. Optional local AI only with explicit resource/model licensing and offline controls. |
| 8 — Resilience and public alpha | Autosave/crash recovery, packaging, legal/license/notices review, installer/update policy, accessibility/user testing, project compatibility and measured hardware requirements. Publish only after owner approval and reproducible Windows builds. |

Do not expand into cloud accounts, synchronization or marketplaces to bypass foundational media work. Every milestone includes error paths, memory budgets, tests and documentation. An empty extension trait is not milestone progress.

# Claude onboarding

Read [AGENTS.md](AGENTS.md) first; it is the single engineering constitution. Then read [ARCHITECTURE](docs/ARCHITECTURE.md), [ENGINE_UI_CONTRACT](docs/ENGINE_UI_CONTRACT.md), [DECISIONS](docs/DECISIONS.md) and [MODEL_RESPONSIBILITIES](docs/MODEL_RESPONSIBILITIES.md).

Phase 0 and the Phase 1 media slice belong to GPT-6.1 Sol. Read docs/PHASE_1_REPORT.md and contract version 2 before extending them. Proposed future Claude work is Slint components, interaction, accessibility and UI validation, with cross-review. This is adjustable and does not authorize concurrent edits without ownership coordination. Keep engine time, decoding, GPU objects and mutable project state out of Slint. Respect media request identities, cancellation, source/timeline separation and the preview adapter; document contract changes.

Run `./scripts/verify.ps1` and launch the Windows app for UI changes. Report actual outcomes and unsupported behavior honestly. This file introduces no rules that override AGENTS.md.

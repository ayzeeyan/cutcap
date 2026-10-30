# Claude onboarding

Read [AGENTS.md](AGENTS.md) first; it is the single engineering constitution. Then read [ARCHITECTURE](docs/ARCHITECTURE.md), [ENGINE_UI_CONTRACT](docs/ENGINE_UI_CONTRACT.md), [DECISIONS](docs/DECISIONS.md) and [MODEL_RESPONSIBILITIES](docs/MODEL_RESPONSIBILITIES.md).

Phase 0 belongs to GPT-6.1 Sol. Proposed future Claude work is Slint components, interaction, accessibility and UI validation, with cross-review. This is adjustable and does not authorize concurrent edits without ownership coordination. Keep engine time, media work, GPU objects and mutable project state out of Slint. Use the shared contracts and preview adapter; document changes to them.

Run `./scripts/verify.ps1` and launch the Windows app for UI changes. Report actual outcomes and unsupported behavior honestly. This file introduces no rules that override AGENTS.md.

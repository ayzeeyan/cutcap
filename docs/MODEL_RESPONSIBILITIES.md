# Model responsibilities and collaboration

## Phase 0

GPT-6.1 Sol owns all architecture, Rust implementation, Slint UI, documentation, validation and self-review. No work depends on Claude availability. Model names describe requested roles, not a claim of automatic tool availability. Every contributor follows AGENTS.md.

## Proposed specialization after Phase 0

| GPT-6.1 Sol | Claude Opus 5.5 |
|---|---|
| Core editing model and engine | Slint editor components and layout |
| Media probing, decoding, encoding and audio | UX, interactions, keyboard behavior |
| GPU compositing, media caching and performance | Design tokens, restrained animation and accessibility |
| Project persistence and engine tests | UI testing and cross-review |

The division is adjustable based on project needs, availability and demonstrated results. Neither model is exempt from testing or cross-review; do not assign capability based only on a model label.

## Handoff procedure

An assignment states objective, affected paths, active ownership, accepted contract version, expected behavior, tests and explicit exclusions. UI work normally owns `apps/cutcap/ui` and the presentation mapping in `coordinator.rs`; engine work owns core/contracts/engine/bridge/native adapters. Shared files need a named owner during parallel work. Use isolated branches/worktrees, never concurrent uncoordinated edits to one checkout.

A contract change first describes command/result/error semantics, ordering, cancellation, time units and ownership. Its implementation updates both ends and tests/docs together. Cross-review checks the actual diff and evidence. The receiving developer reads ARCHITECTURE.md, ENGINE_UI_CONTRACT.md, DECISIONS.md, LICENSE_REVIEW.md and the latest phase report, then runs validation on their environment.

Claude's first suitable task is keyboard/accessibility refinement of the existing shell or integrating real media metadata into the library/inspector once Sol supplies the typed snapshot and preview adapter. Claude must not add FFmpeg/wgpu imports to Slint coordination, access the mutable Project from UI callbacks, implement decoding on the event loop, or invent playback state.

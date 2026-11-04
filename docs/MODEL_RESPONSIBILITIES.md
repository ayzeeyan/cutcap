# Model responsibilities and collaboration

## Phase 0

GPT-6.1 Sol owns all architecture, Rust implementation, Slint UI, documentation, validation and self-review. No work depends on Claude availability. Model names describe requested roles, not a claim of automatic tool availability. Every contributor follows AGENTS.md.

## Proposed specialization after Phase 0

| GPT-6.1 Sol | Claude Opus 5.5 |
|---|---|
| Core editing model and engine | Avalonia views, controls and layout |
| Media probing, decoding, encoding and audio | UX, interactions, keyboard behavior |
| GPU compositing, media caching and performance | Design tokens, restrained animation and accessibility |
| Project persistence and engine tests | UI testing and cross-review |

The division is adjustable based on project needs, availability and demonstrated results. Neither model is exempt from testing or cross-review; do not assign capability based only on a model label.

## Handoff procedure

An assignment states objective, affected paths, active ownership, accepted contract version, expected behavior, tests and explicit exclusions. UI work normally owns `apps/CutCap.Desktop` (views, view models, controls, themes); engine work owns core/contracts/media and `crates/cutcap-ffi` (engine, bridge, session, view mapping). The ABI (`cutcap-ffi/src/abi.rs`, `src-dotnet/CutCap.Interop`) is a shared contract: changes need both owners and an ABI version bump. Shared files need a named owner during parallel work. Use isolated branches/worktrees, never concurrent uncoordinated edits to one checkout.

A contract change first describes command/result/error semantics, ordering, cancellation, time units and ownership. Its implementation updates both ends and tests/docs together. Cross-review checks the actual diff and evidence. The receiving developer reads ARCHITECTURE.md, ENGINE_UI_CONTRACT.md, DECISIONS.md, LICENSE_REVIEW.md and the latest phase report, then runs validation on their environment.

Phase 1's real library/inspector metadata and CPU frame adapter are implemented by Sol. Claude's first suitable task is keyboard/accessibility review and refinement of that source viewer, media rows and seeking feedback. Respect contract 2, stable row identity, source/timeline separation, latest ticket/error rejection and preview.rs ownership. Claude must not add codec processing to Slint coordination, access mutable Project from UI callbacks, decode on the event loop, or invent playback state. Sol retains media/backend/cancellation/timing ownership unless a later explicit handoff changes it.

## Avalonia migration

The owner-directed migration (docs/AVALONIA_MIGRATION_REPORT.md) was implemented by Claude Opus 5.5, including the interop layer. The Phase 1 engine/bridge moved into cutcap-ffi unchanged; media_ui/preview/coordinator became the frontend-independent Session. Sol keeps backend ownership: media, cancellation, timing, persistence and the future compositor. Section 18 of the migration report is the backend handoff.

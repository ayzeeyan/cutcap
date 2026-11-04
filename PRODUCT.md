# CutCap product context

<!-- impeccable:product-schema 1 -->

## Platform
Native Windows 11 x64 desktop. This is deliberately outside the skill's web/mobile taxonomy.

## Stack
Avalonia 12/.NET 10 presentation over Rust stable/MSVC, with a versioned native ABI. Rust owns media/project/editing semantics and future wgpu/DX12 composition. External development FFmpeg/FFprobe supply CPU source frames/playback; no codec binary is linked/bundled and no GPU texture interop is implemented. Slint was retired during the owner-directed migration.

## Users and purpose
An approachable, offline-first, non-linear video editor intended to grow from simple editing to sophisticated creative work. No accounts or cloud dependencies.

## Operating context
Primary hardware: Core Ultra 7 255H, integrated Intel Arc, 16 GB RAM. Long editing sessions, a persistent viewer, media library, inspector and multitrack timeline. Keyboard operation and high-DPI Windows use are required.

## Capabilities and constraints
Real local-video import, metadata, cancellable source preview/seeking, bounded video-only source playback `.cutcap` Save/Open/Save As/relink, and first timeline edits (append, select, remove clips; undo/redo) are implemented through ABI 4. Drag/trim/split, timeline playback, audio playback and export remain unavailable. Dirty/saved state and source readiness come from Rust; never fabricate media, progress or successful operations. Preserve original media. Application licensing remains an owner decision.

## Brand commitments
CutCap is the product name. Original identity; do not copy CapCut branding. The user delegates visual and technical decisions and requests implementation without waiting for another developer.

## Evidence on hand
The repository began empty in Phase 0. Phase 1 adds generated synthetic fixtures, invariant/process tests, Windows interaction evidence and measured CPU preview observations. See the phase reports for actual results/limits; no customer research is claimed.

## Principles
Correctness before features; native first; composition over speculation; bounded hardware-aware work; preview and export eventually share timeline semantics.

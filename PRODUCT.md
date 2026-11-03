# CutCap product context

<!-- impeccable:product-schema 1 -->

## Platform
Native Windows 11 x64 desktop. This is deliberately outside the skill's web/mobile taxonomy.

## Stack
User-specified Rust stable, MSVC, Slint, wgpu/DX12 and FFmpeg. Phase 1 invokes external development FFmpeg/FFprobe and presents CPU frames; it does not link codec libraries or integrate GPU textures.

## Users and purpose
An approachable, offline-first, non-linear video editor intended to grow from simple editing to sophisticated creative work. No accounts or cloud dependencies.

## Operating context
Primary hardware: Core Ultra 7 255H, integrated Intel Arc, 16 GB RAM. Long editing sessions, a persistent viewer, media library, inspector and multitrack timeline. Keyboard operation and high-DPI Windows use are required.

## Capabilities and constraints
Phase 1 extends the native shell/model with real local-video import, validated metadata and cancellable source-frame preview/seeking. Playback, timeline editing, persistence and export are unavailable. Never fabricate media, progress or successful operations. Preserve original media. Application licensing remains an owner decision.

## Brand commitments
CutCap is the product name. Original identity; do not copy CapCut branding. The user delegates visual and technical decisions and requests implementation without waiting for another developer.

## Evidence on hand
The repository began empty in Phase 0. Phase 1 adds generated synthetic fixtures, invariant/process tests, Windows interaction evidence and measured CPU preview observations. See the phase reports for actual results/limits; no customer research is claimed.

## Principles
Correctness before features; native first; composition over speculation; bounded hardware-aware work; preview and export eventually share timeline semantics.

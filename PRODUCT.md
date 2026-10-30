# CutCap product context

<!-- impeccable:product-schema 1 -->

## Platform
Native Windows 11 x64 desktop. This is deliberately outside the skill's web/mobile taxonomy.

## Stack
User-specified Rust stable, MSVC, Slint, wgpu/DX12 and FFmpeg. Phase 0 does not link a codec backend or integrate GPU video textures.

## Users and purpose
An approachable, offline-first, non-linear video editor intended to grow from simple editing to sophisticated creative work. No accounts or cloud dependencies.

## Operating context
Primary hardware: Core Ultra 7 255H, integrated Intel Arc, 16 GB RAM. Long editing sessions, a persistent viewer, media library, inspector and multitrack timeline. Keyboard operation and high-DPI Windows use are required.

## Capabilities and constraints
Phase 0 delivers the native shell, valid domain types, typed commands, bounded execution and engineering rules. Playback, media import and export are unavailable. Never fabricate media, progress or successful operations. Preserve original media. Application licensing remains an owner decision.

## Brand commitments
CutCap is the product name. Original identity; do not copy CapCut branding. The user delegates visual and technical decisions and requests implementation without waiting for another developer.

## Evidence on hand
Empty repository at kickoff. No existing assets, product research, performance measurements or media fixtures. This document records the explicit assignment; it makes no invented customer or performance claims.

## Principles
Correctness before features; native first; composition over speculation; bounded hardware-aware work; preview and export eventually share timeline semantics.

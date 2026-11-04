# Licensing and distribution review

Review date: 2026-11-03 (Phase 1); frontend section updated 2026-11-04 (Avalonia migration). This is an engineering inventory and distribution decision record, not a completed legal clearance. No CutCap application license is assigned. Packages set `publish = false`; no installer, public release or third-party binary bundle is produced.

## Application license — owner decision outstanding

Free of charge and offline-first do not determine a source-code license. MIT/Apache-2.0 permit broad reuse and proprietary forks (Apache also supplies an express patent grant); GPL-3.0 requires corresponding-source/compatible downstream distribution for covered derivatives; a proprietary gratis license restricts reuse and needs owner-drafted terms. The owner must choose based on contribution, fork, commercial and plugin goals. Do not add a LICENSE file selecting one by default. Dependencies retain their own licenses independently.

## Frontend: Avalonia and .NET (since the migration)

Slint has been removed from the build (ADR 012); its royalty-free attribution route no longer applies to current builds. Historical Slint builds remain in Git history under their original terms.

The self-contained win-x64 publish ships the .NET 10 runtime and these frontend components. Declared licenses are from the packages' metadata; verify full texts and notices before any distribution.

| Component | Version | Declared license | Notes |
|---|---|---|---|
| Avalonia (Base, Controls, Markup, Win32, Skia, Themes.Fluent, Desktop and its platform packages) | 12.1.5 | MIT | Avalonia.Desktop also pulls X11/macOS/FreeDesktop assemblies that are unused on Windows; a Win32-only package set would shrink the publish |
| Avalonia.Angle.Windows.Natives (`av_libglesv2.dll`) | 2.1.27548.20260419 | BSD-3-Clause (ANGLE) | GPU-backed compositing on Windows |
| SkiaSharp / libSkiaSharp | 3.119.4 | MIT / Skia BSD-3-Clause | 2D rendering |
| HarfBuzzSharp / libHarfBuzzSharp | 8.3.1.3 | MIT / HarfBuzz "Old MIT" | Text shaping |
| Avalonia.Fonts.Inter | 12.1.5 | MIT package; **Inter font SIL OFL 1.1** | Bundled UI typeface; OFL permits bundling with notice, forbids selling the font alone |
| CommunityToolkit.Mvvm | 8.4.2 | MIT | Source-generated observable properties/commands |
| MicroCom.Runtime, Tmds.DBus.Protocol | 0.11.6, transitive | MIT | Avalonia dependencies (D-Bus unused on Windows) |
| .NET 10 runtime (self-contained) | 10.0 | MIT, with its THIRD-PARTY-NOTICES | Microsoft runtime redistribution terms apply |
| xunit.v3, xunit.runner.visualstudio, Microsoft.NET.Test.Sdk | 3.2.2, 3.1.5, 18.10.1 | Apache-2.0 / Apache-2.0 / MIT | Tests only, not shipped |

The About flyout names Avalonia (MIT), Inter (OFL 1.1) and the separately installed FFmpeg. A distribution would still need the complete notice texts (Avalonia, ANGLE, Skia, HarfBuzz, Inter, .NET third-party notices) in the package. The CutCap mark and app icon are original, generated from code (scripts and Assets); no CapCut asset, logo or wordmark is used.

## FFmpeg

Phase 1 invokes external FFmpeg and FFprobe, installed `9.0.1-essentials_build-www.gyan.dev`, built with `--enable-gpl --enable-version3 --enable-static` and GPL libraries including x264/x265. The owner explicitly authorized this local development dependency. These executables are not linked, copied into the repository, downloaded by scripts/CI, redistributed or packaged. They are discovered on PATH or explicitly configured through CUTCAP_FFMPEG/CUTCAP_FFPROBE. This is not an LGPL-compatible shipping default. PATH availability and a subprocess boundary are not automatic license clearance.

The intended distribution investigation is a reproducible shared-library LGPL-compatible build without GPL/nonfree options. Archive the exact build configuration, matching source, modifications, licenses and notices; preserve user rights to replace/relink libraries and avoid conflicting reverse-engineering restrictions. Audit external codec libraries too. FFmpeg's [official compliance guidance](https://ffmpeg.org/legal.html) describes dynamic-linking and source/notice requirements. Optional GPL components change FFmpeg's applicable license; nonfree configurations may be unredistributable. Codec patents and jurisdiction/distributor obligations require separate review; an open-source license does not resolve them.

Before shipping, review the chosen subprocess interaction/distribution model, exact executable build and compatible source/notices, user installation/replacement, codec libraries/patents and any later FFI/DLL loading/relinking obligations. Phase 1 does not settle whether a particular combined distribution is permissible under an eventual application license. Local development executables remain separate from repository artifacts. No auto-download/installer fallback is provided when they are missing; the app reports an error.

Generated test videos in tests/fixtures contain synthetic colors/test patterns/tones authored for this task and are dedicated as CC0-1.0 content in their README. That dedication covers those fixtures only, not CutCap source or an FFmpeg executable. Generation uses the installed tool; copying generated media is not copying the tool. Review codec patent/distribution requirements independently before public release. No third-party footage, music or fonts were downloaded.

## Direct Rust dependency inventory

| Dependency | Selected version | Declared license | Use |
|---|---|---|---|
| serde | 1.0.229 | MIT OR Apache-2.0 | Validated model/value serialization |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | Bounded FFprobe parsing, developer measurement reports and tests |
| uuid | 1.29.0 | Apache-2.0 OR MIT | Typed stable IDs |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | Typed errors |
| tracing | 0.1.44 | MIT | Local diagnostics |
| tracing-subscriber | 0.3.23 | MIT | Local log formatting/filter |
| wgpu | 30.0.1 | MIT OR Apache-2.0 | Optional DX12 diagnostic only |
| pollster | 1.0.1 | Apache-2.0 OR MIT | Optional diagnostic's blocking executor, never UI callbacks |
| win-custody | 0.1.2 (exact) | MIT OR Apache-2.0 | Safe Windows Job resource/lifetime limits around owned media children; no host/job raw handle exposed |

Versions were queried from crates.io and locked with Cargo; exact transitive inventory is Cargo.lock / `cargo metadata --locked`. This table is not a complete transitive notice bundle. Before distributing, generate an SPDX/SBOM and notices for the actual target/features, inspect all selected dependencies/assets, retain license texts, and clear unresolved cases. Lockfile-only optional packages are not evidence of linkage.

Inter is bundled through Avalonia.Fonts.Inter; Cascadia Mono/Consolas are requested from the Windows installation for timecodes, not bundled. The 47 small icon SVGs and the mark are original CutCap source; no external images, fonts or media were downloaded for the UI. Application source licensing remains undecided, including those authored assets. See the phase report for the validation environment and actual supported functionality.

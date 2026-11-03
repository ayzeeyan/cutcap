# Licensing and distribution review

Review date: 2026-11-03 (Phase 1). This is an engineering inventory and distribution decision record, not a completed legal clearance. No CutCap application license is assigned. Packages set `publish = false`; no installer, public release or third-party binary bundle is produced.

## Application license — owner decision outstanding

Free of charge and offline-first do not determine a source-code license. MIT/Apache-2.0 permit broad reuse and proprietary forks (Apache also supplies an express patent grant); GPL-3.0 requires corresponding-source/compatible downstream distribution for covered derivatives; a proprietary gratis license restricts reuse and needs owner-drafted terms. The owner must choose based on contribution, fork, commercial and plugin goals. Do not add a LICENSE file selecting one by default. Dependencies retain their own licenses independently.

## Slint 1.18.1

The runtime/compiler list GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0. Slint is not LGPL. Intended provisional desktop route: the royalty-free 2.0 license, which permits desktop distribution under other application licenses and requires the specified attribution. CutCap's top-level About button contains the built-in AboutSlint widget. Preserve it and verify that it remains accessible. The alternative public webpage badge route exists, but no such distribution page currently exists.

Review the complete conditions, including the desktop scope, attribution, exclusions and terms for modifications, at [Slint terms](https://slint.dev/terms-and-conditions) and the [royalty-free 2.0 license](https://slint.dev/agreements/slint-royalty-free-license.pdf). Choosing Slint's GPL route would introduce different obligations for the application; do not assume a permissive application license overrides them. Commercial licensing remains an alternative, not a Phase 0 purchase. No Slint internals have been modified.

## FFmpeg

Phase 1 invokes external FFmpeg and FFprobe, installed `9.0.1-essentials_build-www.gyan.dev`, built with `--enable-gpl --enable-version3 --enable-static` and GPL libraries including x264/x265. The owner explicitly authorized this local development dependency. These executables are not linked, copied into the repository, downloaded by scripts/CI, redistributed or packaged. They are discovered on PATH or explicitly configured through CUTCAP_FFMPEG/CUTCAP_FFPROBE. This is not an LGPL-compatible shipping default. PATH availability and a subprocess boundary are not automatic license clearance.

The intended distribution investigation is a reproducible shared-library LGPL-compatible build without GPL/nonfree options. Archive the exact build configuration, matching source, modifications, licenses and notices; preserve user rights to replace/relink libraries and avoid conflicting reverse-engineering restrictions. Audit external codec libraries too. FFmpeg's [official compliance guidance](https://ffmpeg.org/legal.html) describes dynamic-linking and source/notice requirements. Optional GPL components change FFmpeg's applicable license; nonfree configurations may be unredistributable. Codec patents and jurisdiction/distributor obligations require separate review; an open-source license does not resolve them.

Before shipping, review the chosen subprocess interaction/distribution model, exact executable build and compatible source/notices, user installation/replacement, codec libraries/patents and any later FFI/DLL loading/relinking obligations. Phase 1 does not settle whether a particular combined distribution is permissible under an eventual application license. Local development executables remain separate from repository artifacts. No auto-download/installer fallback is provided when they are missing; the app reports an error.

Generated test videos in tests/fixtures contain synthetic colors/test patterns/tones authored for this task and are dedicated as CC0-1.0 content in their README. That dedication covers those fixtures only, not CutCap source or an FFmpeg executable. Generation uses the installed tool; copying generated media is not copying the tool. Review codec patent/distribution requirements independently before public release. No third-party footage, music or fonts were downloaded.

## Direct dependency inventory

| Dependency | Selected version | Declared license | Use |
|---|---|---|---|
| slint / slint-build | 1.18.1 | GPL-3.0-only OR Slint royalty-free 2.0 OR Slint software 3.0 | Native UI / build compiler |
| serde | 1.0.229 | MIT OR Apache-2.0 | Validated model/value serialization |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | Bounded FFprobe parsing, developer measurement reports and tests |
| rfd | 0.17.2 | MIT | Native Windows asynchronous file picker; default features disabled |
| uuid | 1.29.0 | Apache-2.0 OR MIT | Typed stable IDs |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | Typed errors |
| tracing | 0.1.44 | MIT | Local diagnostics |
| tracing-subscriber | 0.3.23 | MIT | Local log formatting/filter |
| wgpu | 30.0.1 | MIT OR Apache-2.0 | Optional DX12 diagnostic only |
| pollster | 1.0.1 | Apache-2.0 OR MIT | Optional diagnostic's blocking executor, never UI callbacks |

Versions were queried from crates.io and locked with Cargo; exact transitive inventory is Cargo.lock / `cargo metadata --locked`. This table is not a complete transitive notice bundle. Before distributing, generate an SPDX/SBOM and notices for the actual target/features, inspect all selected dependencies/assets, retain license texts, and clear unresolved cases. Lockfile-only optional packages are not evidence of linkage.

Segoe UI and Consolas are requested from the Windows installation, not bundled. Three original small SVG assets are part of CutCap source; no external images/fonts/media were downloaded for the UI. Application source licensing remains undecided, including those authored assets. See the phase report for the validation environment and actual supported functionality.

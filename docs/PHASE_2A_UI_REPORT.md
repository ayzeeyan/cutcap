> **Historical.** This report describes the Slint frontend, retired by the Avalonia migration (docs/AVALONIA_MIGRATION_REPORT.md). Its last version is commit a3de692 on `claude/phase-2a-ui`; the Night Cut screenshots are docs/images/phase2a-night-*.jpg.

# Phase 2A — Native editor UI/UX

Date: 2026-11-03. Windows 11 x64, branch `claude/phase-2a-ui` from Phase 1 commit 8155d32. Author: Claude Opus 5.5 (UI/UX). This report separates implemented, verified and unverified behaviour. `docs/PHASE_2_PLAYBACK_CONTRACT.md` did not exist, so no playback API was used or invented; all work stays within contract version 2 and project schema 1.

Process: I audited the running Phase 1 build first and made a refinement pass. The owner then rejected that look ("flat, no identity", "bad proportions", "feels like a web page") and asked for a redesign using the skills in `~/.agents/skills` (impeccable, frontend-design). The impeccable launcher would have downloaded and executed a binary on first run, which I did not do without approval; I followed its documented fallback and read its references (new-work, operate, craft-floor, layout) directly. Per its new-work flow I asked the owner three questions. Answers: pain points as above; direction **Splice Room** (from three presented: Splice Room, Windows 11 native, Swiss Grid); layout delegated to me (viewer-first). No concept-seed roll, comps or finish-reviewer subagent ran; the build was code-led.

Third iteration (current): after reviewing Splice Room the owner supplied a reference screenshot of a conventional dark NLE (blue accent, labelled toolbar, media library with filter tabs and thumbnail rows, Source/Timeline preview tabs, round play button, collapsible inspector sections, V1/A1 track badges, status bar) and asked for that look, not pixel-perfect. The current build, **Studio Blue**, follows that reference using only real data: controls the engine cannot back yet (Timeline Preview, Undo/Redo, Save, Export, Split, track visibility/mute/lock, Play) are shown disabled with an explanation, and thumbnails are not invented.

## 1. Initial audit (Phase 1 build, before changes)

Evidence: `docs/images/phase2a-before-timeline.jpg` (owner-supplied), `docs/images/phase2a-before-source.jpg`, and `.artifacts/phase2a/baseline/`. **U** = demonstrable usability problem, **V** = visual/consistency preference.

| # | Priority | Finding | Evidence |
|---|---|---|---|
| 1 | U, high | Track headers scrolled away horizontally with the lanes, so a scrolled timeline lost track identity and the playhead; first ruler label clipped ("s"). | Both before screenshots |
| 2 | U, high | No position/duration readout in the source viewer; the only time display was a raw "Source ms" field. | before-source |
| 3 | U, high | Inspector showed source metadata as one 15-line text blob with debug output (`Audio streams: []`) and rational rates (`4/1 fps`). | before-source |
| 4 | U, med | Declared minimum 1000×600 logical cannot fit 1920×1080 at 175 % or 1920×1200 at 200 % (work area ≈576–590 logical px high). | Arithmetic from Windows scaling |
| 5 | U, med | Zoom showed two identical "100%" labels (readout and reset button); zoom buttons were announced as "−"/"+". | UIA tree dump |
| 6 | U, med | Tooltip on the *disabled* source slider covered Seek/+1 s. | baseline a01 |
| 7 | U, med | After import the library said "Select an entry to preview" although the entry was already selected and previewing. | baseline a03 |
| 8 | U, low | Only the media-row name was clickable (centred), its details were not; no keyboard row navigation. | Code + baseline |
| 9 | V | Fluent purple slider/field accent clashed with the design system; copper accent and middle-dot meta strings read generic; viewer small at default size; empty-state text duplicated. | Screenshots |

## 2. Modified files

- UI (owned): `apps/cutcap/ui/app-window.slint`, `theme.slint`, `models.slint`; `components/action-button.slint`, `inspector-panel.slint`, `media-panel.slint`, `panel-heading.slint`, `splitter.slint`, `timeline-panel.slint`, `viewer-panel.slint`; new `components/media-item.slint`, `source-scrubber.slint`, `tabs.slint`, `time-field.slint`, `track-header.slint`; assets `mark.svg` (new play-mark glyph), `media.svg`, `viewer.svg` (recoloured), new `assets/icons/*.svg` (35 authored icons).
- Rust presentation mapping: new `apps/cutcap/src/presentation.rs`; `coordinator.rs` (`apply_snapshot` mapping, library filter re-apply, seek-text parsing, import/ready status text). `main.rs`: one line, `mod presentation;`.
- Docs: `DESIGN.md` (replaced), this report, `tests/ui_smoke.md`, `docs/images/phase2a-*.jpg`.
- **Not touched:** contracts, core, media crate, `engine.rs`, `bridge.rs`, `media_ui.rs`, `preview.rs`, `benchmark.rs`, callbacks' signatures.

## 3. Design system

Studio Blue, documented in DESIGN.md: cool dark surfaces (stage #0a0b0e, ground #0f1115, panel #181a1f, raised #21242a) with 1 px panel borders; one blue accent (#3d8bff, fill #3468e0) for the playhead, selection, focus and the primary action; track families as information (video blue #2c55a8 badges, audio green #23774a). Segoe UI, with Consolas only for measured time. Panels sit on the ground with 6 px gutters that double as resize handles. A `Timecode` global formats MM:SS.mmm identically to `preview::format_time`. Superseded: the Splice Room tokens (neutral charcoal, tape yellow).

## 4. Components

- **App bar**: mark and name; outlined Import, New Project, Save (disabled); Undo/Redo (disabled, shown from 1180 px); centred project name with "Not saved" (hidden when the toolbars need the room); layout reset, About, Export (disabled).
- **ActionButton**: outlined (default), subtle, primary (blue fill) and icon-only variants; separate spoken `name`; configurable icon size; outer 1.5 px accent focus ring.
- **FilterChip / ViewTab** (new, `tabs.slint`): pill filters and underlined view tabs with tab role and selected state.
- **SourceScrubber**: replaces the std Slider. Blue fill, round white thumb. The display derives only from `position-ms` (or the active drag), never from a writable value, so engine updates cannot be severed. Keys: arrows ±100 ms, Shift+arrows ±1 s, Page ±10 s, Home/End. Slider role with value, min/max/step and increment/decrement actions.
- **TimeField**: replaces std LineEdit (removes the Fluent accent). Keeps the Phase 1 pattern: two-way binding to private text that `changed position-ms` overwrites.
- **Media library**: "All Media / With audio / Silent" filters (filtered in Rust, rows carry `source-index` so selection stays correct), an Import Media card (click target only; it does not accept dropped files), and rows with a 16:9 placeholder tile carrying the real duration, name, duration and resolution, a Video badge, an audio badge and codec/rate. List-item accessibility; Up/Down move selection and focus within the filtered list.
- **Source Preview**: Source Preview / Timeline Preview (disabled) tabs, the source name on the right, picture on a rounded stage, requested/duration timecode beside the scrubber, then the actual-frame caption (left), start / −1 s / round Play (disabled) / +1 s / end (centre) and the go-to field (right).
- **Inspector**: identity card (placeholder tile, name, Video badge, detail) and collapsible section cards built from `InfoSection { title, rows }`; sections with an unchanged title keep their collapsed state and update rows in place. A disabled "Clip properties — Needs a clip" card stands in for transform/opacity until clips exist.
- **TrackHeader**: V1/A1/V2 badge (numbered per family in Rust), name, disabled visibility-or-mute and lock glyphs with explanations; list semantics, Up/Down and lane auto-scroll.
- **Timeline**: toolbar with Video track / Audio track, disabled Split, and zoom out / zoom slider (50–200 %) / zoom in / reset; large blue timeline timecode in the corner (fixed at 00:00.000); fixed header column and ruler synced to the lane viewport; MM:SS ruler labels whose interval adapts to zoom; alternating lane shading; blue playhead.
- **Status bar**: success/error icon and status text, the still-frame note or pending count, and the zoom level.

## 5. Source viewer interactions

Scrubber click/drag, keyboard steps, start, −1 s, +1 s, end, and the go-to field (`1500` ms, `1.5s`, `0:01.5`, `00:01.500`, `h:mm:ss.mmm`; bare decimals such as `1.5` are rejected as ambiguous; parsing unit-tested). The left timecode is the **requested** position; the heading shows the **actual** displayed frame (preview.rs caption), so VFR/ceiling selection is visible (201 ms → PTS 700). Decoding shows a banner with Cancel while keeping the last frame; failures and cancellations show a banner with the reason. Play (round, centred) is visible but disabled with an explanation. The timeline playhead readout stays 00:00.000 and is never moved by source actions.

## 6. Engine contract integration

No contract change. All engine interaction uses existing callbacks (`seek-source(int)`, `seek-text(string)`, `cancel-preview`, `import-media`, `select-media`, `select-track`, `add-track`, `clear-selection`, `new-project`). UI-layer (not engine) changes: AppWindow drops `timebase-label`/`media-details`, adds `inspector-sections: [InfoSection]`, `source-duration-estimated`, `media-filter` and the `media-filter-changed()` callback (which re-applies the last snapshot); `MediaRow` gains `source-index`/`resolution`, `TrackRow` gains `badge`. Only `coordinator.rs` writes them. Model updates stay in place (`presentation::sync_rows`), preserving repeater identity and focus. No frame buffers enter models.

## 7. Accessibility and keyboard

Every control has a role and spoken name (icon buttons included); list items expose selected/index/count; the scrubber exposes value/range/step and actions; the time field exposes value and placeholder. Tab order was recorded via UI Automation for the Splice Room build; the Studio Blue build adds filter chips, the Import card, inspector section headers and the zoom slider to the sequence, and its full order was not re-recorded. Disabled controls are skipped. Up/Down list navigation, Ctrl+I/N/0/Plus/Minus/Shift+A retained. Focus ring is blue on every focusable element. No screen reader was run.

## 8. Build and test results

| Command | Result |
|---|---|
| `cargo build -p cutcap` (debug) | Exit 0, no warnings |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit 0 |
| `cargo test --workspace` | Exit 0; 32 passed, 0 failed, 6 ignored (Phase 1: 29 passed; +3 presentation tests: source-time parsing accept/reject, rate/duration formatting) |
| `./scripts/verify.ps1` (Studio Blue build) | Exit 0; all five stages passed (fmt 0.3 s, clippy 11.8 s, test 24.5 s, debug build 0.6 s, release build 95.7 s). Evidence: `.artifacts/verification/` |

All-features checks, FFmpeg integration tests and the DX12 probe were not rerun; no engine or media code changed.

## 9. Native Windows testing

Debug build, Slint winit/software renderer, laptop display 1920×1200 at Windows scale 1.5. Driven by a PowerShell harness (UI Automation + synthetic input, screenshots of the client area). During the first session the harness sent input while another app had focus, so keystrokes and a click reached the owner's browser. The harness now aborts unless the foreground window belongs to the CutCap process, and the later rounds ran with the owner's consent while they stayed idle. Screenshots: `docs/images/phase2a-studio-*.jpg` (Studio Blue), full sets in `.artifacts/phase2a/ref/` and `.artifacts/phase2a/ref-min/`; the Splice Room round is in `.artifacts/phase2a/after/`.

Studio Blue round (debug build, 1920×1200 at scale 1.5, owner had agreed to idle testing; the foreground guard aborted twice when another app came to the front, and no input was sent then): empty state; native picker import of colors-cfr, audio-video, colors-vfr; Add video/audio track (V2 badge numbering); select colors-cfr; typed `1500` + Enter → actual frame PTS 1500 @ 1/1000 with field, timecode and scrubber in sync; "With audio" filter shows only audio-video.mp4, "All Media" restores the list; tooltip on a filter chip; window at 1800×1050 outer and at the declared minimum (1350×810 client) with no clipping; WM_CLOSE exit with empty stderr and no CutCap/FFmpeg/FFprobe processes left. A harness bug (PowerShell `$H` height parameter aliasing the `$h` window handle) briefly sized the window to the Windows maximum; it was the script, not the app, and was fixed before these results.

Splice Room round (earlier build, same harness):

Verified (observed in screenshots and the app log):
- Launch, empty states, native picker import of colors-cfr, colors-vfr, audio-video; corrupt.mp4 import shows the error banner and status, asset count unchanged.
- Go-to `1.5s` → PTS 18432 @ 1/12288 (1.5 s). Scrubber drag → PTS 19456, then forward → 31744 (+1 s exactly), with scrubber, timecode and field in sync. Typed `1000` → PTS 1000, forward → PTS 2000, field shows 00:02.000 (stale-binding regression passes). VFR `201` → PTS 700.
- Track click → Down moves selection and focus to Audio 1; Ctrl+Shift+A clears selection with focus retained.
- Shift+wheel on lanes: headers stay fixed, ruler follows, note stays readable.
- Normal (1200×700), declared minimum (900×540, exact client 1350×810 px), maximized (1920×1094 px client): no clipping or overlap after the fix round.
- `SLINT_SCALE_FACTOR=1.0`: layout correct at 1200×700 px.
- Close: event loop stopped, stderr empty, zero CutCap/FFmpeg/FFprobe processes afterwards.

Observed problems:
- `SLINT_SCALE_FACTOR=2.0`: the 1200×700 logical preferred size becomes 2400×1400 px and overflows the 1920×1200 screen (see §11).
- `MoveWindow` can size the window below the declared minimum (WM_GETMINMAXINFO not applied to programmatic moves). Interactive edge-drag clamping was not retested in this phase.

Not retested for Studio Blue: corrupt import, VFR 201 ms, scrolled timeline, maximized, scale 1.0/2.0, keyboard track navigation. Not tested: screen reader, real per-monitor DPI changes (only the Slint scale override), multiple monitors, release build interactively, closing during an active decode (Phase 1 evidence only), drag of each splitter in this build, About popup, tooltip readability on every control.

## 10. Known limitations

- Arrow-key stepping is time-based (100 ms / 1 s), not frame-based; frame stepping needs frame durations from a transport contract.
- Requested vs actual position: the caption string comes from preview.rs and still uses "·" separators.
- The media status line hides the idle prompt by comparing to media_ui's literal string "Import a local video."; changing that string re-shows it.
- Media rows and the inspector identity card show a placeholder tile, not a thumbnail: nothing is invented, and poster frames need an engine API. Only the selected source shows a frame, in the viewer.
- The Import Media card is click-to-browse only; drag and drop is not implemented.
- The library filter is presentation state; it resets on restart and is not part of the project.
- Many reference-image controls are visible but disabled (Timeline Preview, Undo/Redo, Save, Export, Split, track eye/mute/lock, clip properties) until the engine supports them.
- Timeline: no clips, no timeline seeking, ruler is a fixed-scale view; track heights are uniform.
- Focus rings drawn outside a control can be clipped inside scroll views.

## 11. Integration requirements for Sol (GPT-6.1)

1. **Initial window size vs work area** (`main.rs`): clamp the initial size to the monitor work area, or start maximized when the preferred size does not fit (observed at scale 2.0).
2. **Playback contract**: when `PHASE_2_PLAYBACK_CONTRACT.md` lands, the Play button, frame stepping (needs frame duration/next-PTS), buffering state and end-of-source indication can bind to it. The UI needs the actual presented position as an integer-ms (or tick) property separate from the requested position, plus a transport state enum (stopped/playing/buffering/ended/error).
3. **Structured preview caption**: expose actual PTS/relative time as separate properties instead of the preformatted caption string, so the UI can format and localise it.
4. **Media status as state, not prose**: an enum (idle/choosing/probing/registering/cancelled/error) would remove the string comparison in media-panel.slint.
5. **Poster frames**: a cancellable, cached poster-frame request per asset (small RGBA, off the UI thread) would let the library and inspector tiles show real thumbnails.
6. **Undo/Redo**: the contract has `Command::Undo/Redo` but the engine does not implement them; the app bar buttons can bind once it does, with `can-undo`/`can-redo` in the snapshot.

## 12. Recommended next step

Review the new look on your own machine at your preferred size. Then commit this branch in small pieces (tokens/icons, components, presentation mapping, docs). When the playback contract exists, wire Play, stepping and transport state into the Source panel without changing its structure.

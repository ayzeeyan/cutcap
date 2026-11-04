# Phase 3B — Timeline editing report

Author: Claude Opus 5.5, sole developer for this milestone (owner-directed). Phase 0/1/2/3A engine, media and contract work remains GPT-6.1 Sol's; Sol's Phase 3A was committed unmodified, with provenance, before any Phase 3B change.

## 1. Starting revision

- Branch at start: `codex/phase-3a-persistence`, HEAD `3af9d0454eb3ab0934ba3c6314315d503ab7fe1a` ("Add the Phase 2B native scenario log"), with Sol's complete Phase 3A work uncommitted in the tree (64 files).
- Remote: `origin https://github.com/ayzeeyan/cutcap.git` (unchanged; never reconfigured).
- Git identity: `ayzeeyan <bantolizyan@gmail.com>` (the owner's configured identity; no other developer's identity was used). Model provenance is recorded in commit messages and trailers.

## 2. Phase 3A commit

- `6ea31b00d64e8350f8d9ab2507417c4b3b958cb0` — `feat(project): complete Phase 3A persistence and media relinking`, committed unmodified with a provenance paragraph ("implemented by GPT-6.1 Sol; committed unmodified by Claude Opus 5.5 at the owner's request").
- Verified before committing: `scripts/verify.ps1` all stages exit 0; 69 Rust and 57 .NET tests; FFmpeg integration 7/7.
- Pushed with `git push -u origin codex/phase-3a-persistence`; `origin/codex/phase-3a-persistence` tracks it. The baseline was never rewritten, squashed or force-pushed.

## 3. Branch

`claude/phase-3b-timeline-editing`, created from `6ea31b0` (not from `main` or the 2B branch). Nothing was pushed to `main` and no merge was created.

## 4. Changed files

Engine (Rust):
- `crates/cutcap-core/src/project.rs`: `insert_clip_at`, `remove_clip`, `clip`, `track_end`; core tests in `crates/cutcap-core/tests/project_invariants.rs`.
- `crates/cutcap-contracts/src/messages.rs`: CONTRACT_VERSION 4, `Command::InsertClip`, `ClipSummary`, `HistoryStatus`, `ProjectSnapshot.clips/history`.
- `crates/cutcap-ffi/src/history.rs` (new): bounded exact history and unique revisions.
- `crates/cutcap-ffi/src/engine.rs`: InsertClip/RemoveClip/Undo/Redo, the append policy (`append_clip`), revisions via `bump`, history reset on New/Open.
- `crates/cutcap-ffi/src/view.rs`, `session.rs`: clip DTOs, clip inspector, the separately cached timeline document, `timeline_generation`/`history` in the view.
- `crates/cutcap-ffi/src/abi.rs`: ABI 4 and six exports.
- Tests: `crates/cutcap-ffi/src/editing_tests.rs` (new), `crates/cutcap-ffi/src/timeline_tests.rs` (new), wiring in `lib.rs`/`tests.rs`.

Interop (C#): `src-dotnet/CutCap.Interop/NativeMethods.cs`, `EngineSession.cs`, `SessionView.cs`; `tests/CutCap.Interop.Tests/TimelineEditingTests.cs` (new), `PersistenceTests.cs` (comment).

Avalonia: `Controls/TimelineGeometry.cs` (new), `Controls/TimelineLanes.cs`, `Controls/TimelineRuler.cs`, `Services/EngineHost.cs`, `ViewModels/TimelineViewModel.cs`, `ViewChanges.cs`, `MainWindowViewModel.cs`, `InspectorViewModel.cs`, `Views/TimelinePanel.axaml(.cs)`, `MainWindow.axaml(.cs)`, `InspectorPanel.axaml`, `ShortcutsView.axaml`; `tests/CutCap.Desktop.Tests/TimelineEditingTests.cs` (new).

Scripts: `scripts/native-ui/timeline-ui.ps1` (new scenario), `scripts/native-ui/timeline-draw.ps1` (new draw-time check), `scripts/native-ui/find.ps1` (bounded retry when a tree is being replaced).

Docs: this report; ARCHITECTURE, ROADMAP, PROJECT_FORMAT, NATIVE_ABI, ENGINE_UI_CONTRACT, DECISIONS (ADR 020), README, PRODUCT, AGENTS (factual phase line only); `docs/measurements/phase3b-timeline.json`, `phase3b-native-scenario.txt`; `docs/images/phase3b-*.jpg`.

## 5. Editing model

- **Owner.** Only the Rust command owner mutates the project. C# forwards commands and draws what the engine reports.
- **InsertClip { media, track }** — explicit append-to-end policy. The whole source is placed on a video track starting at that track's last clip end (`Project::track_end`). Duration = the source's reported `duration_ticks`, rescaled with floor from the stream clock to the project clock (1/720000 s); `source_in` = the stream's start PTS in its own timebase. Rejected atomically (no mutation, revision or history): source not registered or not Ready (missing/changed/checking/unavailable), unknown track, non-video track, unknown or non-positive duration, shorter than one project tick, end overflow, and every core invariant (kind mismatch, overlap, limits, duplicate IDs, stale ProjectId).
- **RemoveClip { clip }** removes one clip. The media asset and the source file are untouched. A removed selected clip is deselected.
- **Select { Clip }** is transient: no revision, no history, no dirtiness; the inspector shows Timeline (track, start, duration, end, exact ticks) and Source (file, source-in PTS, status).
- **Target track (UI policy).** The selected track if it is a video track, else the track of the selected clip, else the first video track. The Add button's tooltip names the source and the target ("Add interview take.mp4 to the end of Video 2").
- **Time.** Timeline position (`TimelineTime`, project ticks), clip-local time (`t − start`, project ticks), source offset (`source_in`, `SourceTime` in the stream timebase) and source PTS are distinct exact types. Timeline → source mapping is `source_in + rescale(t − start, project → source)`. Display milliseconds are floored only for drawing and never flow back. No timeline playback exists; the timeline playhead stays at zero, and source playback is independent of clips (tested).

## 6. Undo/redo

- `crates/cutcap-ffi/src/history.rs`: at most 200 entries (oldest dropped). Each entry holds the exact `Clip` value, its index in the project's clip order and the before/after document revisions.
- Undo of an insertion removes that exact clip; undo of a removal restores the exact clip at its former index (identity, timing, source mapping and order). Redo is symmetric. A divergent edit clears redo. A failed inverse keeps its entry.
- Revisions are unique per session (`next_revision` only grows, `bump`). Undo returns to the edit's `before` revision only when the document is still at its `after` revision; otherwise it takes a fresh revision. So Save → Insert → Undo is clean, Edit → Save → Undo → Redo is clean again, and Save → Insert → Add track → Undo stays dirty (track creation is not undoable).
- Never recorded: failed commands, selection, playback, seeking, probing. History resets on New Project and on adoption of an opened document.

## 7. ABI and contract

- **ABI 3 → 4, CONTRACT_VERSION 3 → 4.** The spec asked to keep ABI 3 unless a break was justified; AGENTS.md requires a bump for any consumed-view-field change, and the bump is also the only guard against a stale ABI-3 DLL. Such a DLL would otherwise pass the version check next to a 3B frontend and then throw `EntryPointNotFoundException` on the first clip command. Nothing existing changed: every ABI 3 export, status number, frame layout and handle rule is retained.
- New exports: `cutcap_session_insert_clip`, `remove_clip`, `select_clip`, `undo`, `redo`, `timeline`. Arguments are validated at the boundary (UUID text, nil, null-with-length → `INVALID_ARGUMENT`). Engine rejections (for example "Nothing to undo.", "Only a verified, ready source can be added to the timeline.") arrive as view status. Typed codes are InvalidInput/NotFound/StaleProject.
- **Separate timeline document.** The view is re-serialized for every playback frame, so clips are not in it. `cutcap_session_timeline` returns `{generation, clips}`, rebuilt at most once per project snapshot. The frontend fetches it only when `SessionView.timeline_generation` changes. No frame data enters JSON.
- Documented in NATIVE_ABI.md, ENGINE_UI_CONTRACT.md and ADR 020; Rust and .NET tests change together.

## 8. Avalonia

- The timeline toolbar enables Undo/Redo (tooltips name the next action), **Add to timeline** (filled button, tooltip names the source and target or why it is disabled) and Delete clip. Split stays visible and disabled ("not available yet").
- Clips are drawn by the existing single lanes visual, not one control per clip. `TimelineGeometry` keeps per-track clip lists sorted by start; the visible clips are found by binary search over the scroll viewport, and pointer hits are resolved the same way. Content width extends past the last clip end, so the whole timeline is reachable. Zoom (50–200%) and horizontal scroll stay aligned with the ruler (5,000-clip screenshot at 02:30).
- Each clip shows its file name, start–end · duration (Rust-formatted strings), boundaries, the teal video-family fill and a 2 px cyan outline when selected. Missing/changed sources draw with a warm fill, a coral dashed outline and a "Missing ·" prefix. No purple or violet; no fake thumbnails or waveforms.
- Keyboard: Ctrl+Z, Ctrl+Y / Ctrl+Shift+Z and Delete (or Backspace, only with a clip selected) are handled in the window's tunnel handler except when a TextBox has focus. Verified natively: in the search field, Ctrl+Z undid typed text and the timeline was unchanged. The Project menu lists Undo/Redo; the shortcuts sheet lists both new rows.
- Accessibility: the lanes and ruler previously had no automation peer (a plain Control), so the drawn surface was invisible to screen readers. They now expose named panes; the lanes' name is kept current by the view model ("Track lanes, 3 clips. Selected clip interview take.mp4, 00:00.000 to 00:20.000", plus ", source unavailable"). Track headers no longer end with a dangling comma.
- The inspector gains kind `clip` (Video badge, Clear selection button); placeholder tabs say honestly that clip settings are not available yet.

## 9. Persistence

- No format change: `.cutcap` format 1 / core schema 1 already persist clips (identity, references, exact start/duration, source-in PTS/timebase) in project order. PROJECT_FORMAT.md documents the edit/revision rules and that history is not serialized.
- Covered cases (Rust session tests, engine tests and real-FFmpeg .NET tests): Save → Insert → Undo (clean); Save → Remove → Undo (clean, exact index); Edit → Save → Undo → Redo (dirty, then clean); Undo → different edit (redo discarded, dirty); edit during a background Save (file holds the captured revision, document stays dirty, Undo returns to clean); Save and reopen with clips (IDs, timing and order preserved, selection and history not persisted); reopen with missing media (clips kept and drawn missing, new placement refused, removal and undo still work); a failed edit preserves the saved/clean document.
- Natively: Save As, New, Open (through the unsaved-changes prompt) restored all 3 clips clean with empty history; reopening after renaming the source showed both affected clips as missing and disabled Add with "interview take.mp4 is unavailable; relink it first".

## 10. Tests

| Command | Result |
|---|---|
| `cargo test --workspace` | **86 passed** (Phase 3A: 69), 12 ignored (7 external FFmpeg tests, 2 subprocess helpers, 3 opt-in measurement/generator tests) |
| `cargo test -p cutcap-media --test ffmpeg_integration -- --ignored --test-threads=1` | **7 passed** |
| `dotnet test CutCap.slnx` (Debug) | **68 passed** (35 desktop + 33 interop; Phase 3A: 57), 0 skipped; the explicit 100k-clip measurement runs only on request |
| `scripts/verify.ps1` (fmt, clippy -D warnings, test, build, release, .NET Debug build/test, Release build, win-x64 self-contained publish) | **all 9 stages exit 0** |
| `git diff --check` | clean |

New coverage: engine (append exactness, invalid references/kinds/duration/not-Ready/stale project are atomic and unrecorded, undo/redo identity/order/selection and saved cleanliness, divergent redo, unrelated-mutation revision, selection never dirties, history bound and reset); session (view contract, clip inspector, save/reopen, missing media, edit during background save, failed edits, playback independence); ABI (argument validation, round trip, rejection via status, invalid handle); core (index restoration, append point, restoration still validates); .NET interop (real FFmpeg insert/remove/undo/redo/divergence/wrong kind, save/reopen across sessions, missing media, playback frames never re-sending the timeline); desktop (clip rectangles at every zoom, minimum width, hit-testing, culling over 10,000 clips, content width, target-track policy, command enabling/tips/accessible name, ViewChanges gating).

## 11. Native verification

`scripts/native-ui/timeline-ui.ps1` drove the real Debug app over real input, sending input only while CutCap owned the foreground. Log: `docs/measurements/phase3b-native-scenario.txt`. Run exit code 0. All steps passed:

- Import two sources (`interview take.mp4` 20 s 1080p H.264, `colors 视频.mkv` 3 s FFV1); select each source; add to Video 1 (0–20 s), append (20–23 s); add a video track, which becomes the target; add to Video 2 (`phase3b-clips.jpg`).
- Click the clip at 5 s on V1 (inspector "Video clip on Video 1", exact ticks `0 + 14400000 @ 1/720000 s`; `phase3b-selected.jpg`); click at 21 s selects the colors clip.
- Delete removed it (`phase3b-deleted.jpg`); Ctrl+Z restored the same clip and selection (`phase3b-undo.jpg`); Ctrl+Y re-removed it; Ctrl+Shift+Z with nothing to redo changed nothing; toolbar Undo/Redo worked.
- Ctrl+Z/Delete in the search field stayed in the text field.
- Zoom 150% and 75% with correct placement and hit-testing at 150% (`phase3b-zoom-150.jpg`); small window 1440×945 (`phase3b-small-window.jpg`).
- Source playback continued (button stayed Pause) while selecting a clip and undoing/redoing (`phase3b-playing.jpg`).
- Save As, New, Open: 3 clips, clean, no history; selecting the V2 clip shows "Video clip on Video 2" (`phase3b-reopened.jpg`).
- Close: exit 0 in 571 ms, no CutCap/FFmpeg/FFprobe left.
- Rename a source, relaunch, Open: missing clips drawn as such; selection announces "source unavailable"; Add disabled with a relink hint (`phase3b-missing.jpg`). Close: exit 0 in 529 ms, nothing left.
- Large project: 5,000 clips opened and scrolled (`phase3b-5000-clips.jpg`).

**Environment finding (not a CutCap defect).** During verification an idle CutCap window used about one CPU core, and the Phase 3A build did the same. Bisection showed no layout passes and no control renders at idle, and the cost disappeared when the window was minimized or given an empty automation tree. The cause is an external UI Automation client on this machine continuously walking the window's accessibility tree; a `codex-computer-use` process was running at the time. Its tree walks also slowed the harness (0.5–0.7 s per lookup), which dominates the UI latencies in the log. CutCap's automation tree was left intact.

## 12. Performance

Details: `docs/measurements/phase3b-timeline.json`.

- **Engine (release, owner only):** insert 31 µs / 100 µs / 1.6 ms and undo or redo 32 µs / 85 µs / 1.9 ms at 1k / 10k / 50k clips. Costs scale with the snapshot clone, not the history.
- **History memory:** 200 entries, about 156 B each, about 31 KB total, independent of clip count.
- **Snapshot/view:** before the split, a 5,000-clip view was 1.68 MB and would have been re-serialized for each playback frame. Now the view stays at 2.5 KB (0.1 ms per frame) and the timeline document (1.68 MB) is built once per edit in 6.7 ms. At 5,000 clips the session-level undo round trip, including queue, pump and test-side JSON parsing, is 60 ms.
- **Draw:** at the 100,000-clip core limit, layout build takes 22 ms (once per edit), culling 6.2 µs per frame (112 visible clips) and a hit-test 1.1 µs. Natively, in the Debug build with 5,000 clips on a 300,252 px wide surface, `TimelineLanes.Render` records 18 clips in 0.45–0.64 ms on average (worst 3.1 ms). Compositor/GPU raster time is not included.
- **Persistence and interaction during save:** saving 5,000 clips takes 110 ms (2.0 MB file). Edits issued during a background save are accepted and processed (tested); the UI thread never waits for the save.

## 13. Limitations

- Only append-to-end insertion of the whole source, video tracks only. No drag, trim, split, move, ripple, insert at the playhead, gaps editing or cross-track moves.
- No timeline playback, evaluation or composition; the timeline playhead stays at zero. Audio clips, export, transitions, effects and keyframes are out of scope.
- Track creation and relinking are not undoable, and history is not grouped into transactions. The 200-entry bound drops the oldest edits silently (the depth is exposed).
- Insert and undo cost grows linearly with the clip count, because each command clones a full snapshot (1.6–1.9 ms at 50k clips). The timeline document is about 340 B per clip (34 MB at the 100k core limit, within the 64 MiB wrapper ceiling).
- Clip display milliseconds are floored, so sub-millisecond boundaries are not drawn; the inspector shows exact ticks.
- The native draw figure measures recording on the UI thread in a Debug build, not compositor raster. Native UI latencies in the log are dominated by UI Automation polling.

## 14. Phase 3C recommendations

1. Timeline playhead and transport: a Rust-owned timeline clock, the timeline → clip → source mapping in a seek/evaluation path, and a monitor switch between Source and Timeline.
2. Trim, split and move through the same exact history, with grouped transactions (one undo for a multi-clip operation) and making AddTrack undoable.
3. Insertion at the playhead or a selected gap, plus drag from the library onto a lane, using the same validation.
4. Incremental snapshots (clip-diff events or a persistent structure) to remove the O(n) snapshot clone per command before project sizes grow.
5. Thumbnails/filmstrips only once there is a real, cancellable, cached decode path for them; never decorative placeholders.
6. Consider an automation-tree budget, for example virtualized peers for long media lists, since external UIA clients can walk the whole tree continuously.

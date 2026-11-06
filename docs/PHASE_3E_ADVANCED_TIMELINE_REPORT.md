# Phase 3E — Advanced timeline editing report

Author: Claude Opus 5.5, sole active developer for this milestone (owner-directed). Phase 3C/3D (GPT-6.1 Sol) are preserved unchanged in history.

## 1. Starting baseline

- `origin/codex/phase-3d-timeline-playback` at `4fc07386f0c4f47c65f956f689ea00933f3cb2c7`, clean working tree, verified to contain Phase 3C (`43d859c`) and Phase 3B (`eca2b0e`). `origin/main` at `0b777c232a8509ceb6f2a531b941837b7c9002bd` contains Phase 3B (PR #2 merge) but not 3C/3D.
- Baseline verification before any edit: `scripts/verify.ps1` all 9 stages exit 0; 111 Rust tests; 79 .NET tests (38 desktop, 41 interop); FFmpeg integration 9/9. These matched the reported Phase 3D results.
- Code/documentation discrepancies found and corrected:
  - DESIGN.md still described Undo/Redo/Split/Delete as disabled. Undo/Redo/Delete were enabled in 3B.
  - DESIGN.md also said Timeline Play was disabled. It was enabled in 3D.
  - The `TimelineViewModel` summary said the playhead "stays at timeline zero". That has been out of date since 3C.
  - Avalonia exposes a `TextBlock`'s text as its UI Automation name. So `AutomationProperties.Name` on the playhead readout never reached UIA, and the native harness now finds the readout by AutomationId.

## 2. Git branch and commit history

- Branch `claude/phase-3e-advanced-timeline`, created from `4fc0738`, not from `main`.
- Phase 3E commits: `743c9b1` feat(core) exact clip transactions and source-tick grid; `c922b30` feat(history) grouped edits, ABI 7 and interop; `67539b8` feat(ui) drag editing, trim handles, split and snapping; `8ab879c` test(timeline) engine/session/ABI/FFmpeg/desktop/native coverage; `2bb34ac` docs; plus this report update. Each intermediate commit was built (Rust clippy `-D warnings` and the .NET solution) in a clean worktree. Pushed with `git push -u origin claude/phase-3e-advanced-timeline` (no force); remote head verified equal to local. Against `origin/main` the branch is 12 commits ahead (Phase 3C 4, Phase 3D 3, Phase 3E 5) and `git merge-tree` reports no conflicts; `main`'s only extra commits are the PR #1/#2 merge commits, and its tree equals the Phase 3B tree it merged.

## 3. New editing commands

| Command | Behaviour |
|---|---|
| `PlaceClip { media, track, position }` | Drag-and-drop: the whole Ready video source at `position` on a video track. |
| `MoveClip { clip, track, position, anchor }` | Changes only start and track. ClipId, MediaId, `source_in` and duration are kept. `anchor` = which edge `position` names. |
| `TrimClip { clip, edge, position }` | End: keeps start and `source_in`. Start: moves start and `source_in` by the same exact offset. |
| `SplitClip { clip, position }` | Interior cut. The left part keeps the ClipId; the right part is a new ClipId. Plus split at the Rust-owned playhead. |

All four are validated and applied as one atomic core transaction (`Project::replace_clips`, exact rollback).

Rejections leave the document, revision, evaluation generation and history unchanged. They cover:
- unknown clip, track or media;
- a non-video track;
- overlap;
- a negative start or overflow;
- leaving the source range;
- a zero-length result;
- a split at or outside the clip edges;
- a no-op move.

## 4. Exact trim/split timing semantics

A clip maps timeline `t` to `ceil(source_in + (t − start) × project_tick)` in stream ticks. `source_in` is an integer number of stream ticks, so an edit that moves `source_in` by `d` project ticks is exact only when `d × project_tick` is a whole number of stream ticks.

These offsets form a grid, `source_step = den(project_tick / stream_tick)`:

| Stream timebase | Grid step (project ticks) | Real time |
|---|---|---|
| 1/15360 s | 375 | 0.52 ms |
| 1/1000 s | 720 | 1 ms |
| 1/90000 s | 8 | 11 µs |

Policy (ADR 023):
- **Split:** rounds to the nearest grid point.
- **Start trim:** rounds up, toward the retained part, so it never lands on a preceding clip.
- **End trim and move:** do not touch `source_in` and accept any tick.
- **Feedback:** any adjustment, at most one stream tick, is reported in the status line ("Aligned to the nearest source tick …").

Result:
- Retained positions keep exactly their previous source tick.
- A left part's exclusive source endpoint equals the right part's `source_in`, so no frame is repeated or skipped at a cut.
- Nothing accumulates: twenty consecutive off-grid splits keep every sampled position's mapping, with a negative start PTS and a 1/15360 clock.

Bounds come from the verified stream range `[start_pts, start_pts + duration_ticks]`. With unknown metadata, only shrinking is allowed. No schema change and no migration were needed.

Natively, on real H.264 1/15360 media:
- trimming the start by 2 s gave `source_in` 30720 (exactly 2.000 s);
- splitting 7 s later gave the right part `source_in` 138240 = 30720 + 7 × 15360.

## 5. Undo/Redo transaction architecture

`history.rs` now stores each gesture as one `ClipChange`: every clip value removed and inserted, with their indices at removal/insertion time. Undo reverts it and redo re-applies it, both atomically.

This reproduces exact identities, ranges and order, including the split's new right-part ClipId. Phase 3B insert/remove use the same structure, and the revision rules are unchanged:
- Save → edit → Undo is clean.
- An intervening non-undoable mutation keeps the document dirty.
- Revisions are never reused.

Other rules:
- Failed edits and selection never create entries.
- A divergent edit clears redo.
- The bound is 200.
- New and Open reset the history.

The frontend previews gestures locally. The engine sees exactly one command per drag, trim, drop, split or cross-track move. The authoritative project never mutates per pointer move.

## 6. Drag/drop UI behaviour

- **Library drag:**
  - A press on a media tile plus 8 px of travel starts an OLE drag carrying an in-process payload (media id, name, duration, readiness); a plain click still selects.
  - Over the lanes, a translucent cyan ghost shows the clip and target track. A coral ghost and a reason mark an invalid drop (overlap, audio lane, unavailable source, unknown duration).
  - Only a valid drop sends `PlaceClip`. **Add to timeline** remains the accessible alternative.
- **Clip gestures:**
  - Press selects; 4 px of travel on the body starts a move.
  - The edge handles trim: up to 8 px wide, never below 3 px, at most a quarter of a short clip. The cursor is SizeWE and the selected clip shows small cyan grips.
  - The original stays dimmed in place while a dashed ghost moves.
  - Vertical movement targets other video lanes; audio lanes show "Video clips go on a video track".
  - Trims clamp to the neighbouring clips and to the source range the view reports (`min_start_ticks`, `max_end_ticks`), committing those exact ticks.
  - Dragging within 36 px of the viewport edge autoscrolls.
  - Escape or losing the pointer cancels. A release where the gesture started is not an edit.
- **Toolbar:**
  - A live hint in the flexible middle column truncates instead of pushing controls off-screen.
  - Split is enabled, with Ctrl+B and Rust's reason when unavailable.
  - The shortcut sheet lists Ctrl+B, drag/trim, Shift and Esc.
  - Keyboard editing keys keep the existing rule: ignored while a TextBox has focus.

## 7. Snapping policy

Snapping is UI assistance, not a time model. Within 8 px (converted to milliseconds at the current zoom), the nearest of these targets wins:
- timeline zero;
- the playhead;
- every clip start and end, excluding the edited clip.

Rules:
- Moves snap either edge; the commit uses `anchor` so Rust computes the start.
- Trims snap the moving edge.
- Drops snap the start.
- A thin cyan guide shows the snapped boundary.
- Shift disables snapping.
- Snapped commits send the target's exact project ticks from the view; unsnapped commits send whole milliseconds, which Rust floors.

Clip edges are sorted once per clip-list change, so each pointer move is a binary search: 1.3 µs at the 100,000-clip limit, snap plus overlap together. Natively, a drop released near 21.150 s committed exactly 15120000 ticks, the end of the clip before it.

## 8. Timeline playback interaction

Every accepted edit renews the evaluation generation (the Phase 3D path):
- the session cancels the active transport;
- it keeps the playhead and re-evaluates the still;
- it never auto-resumes;
- old-epoch frames cannot be copied.

Failed edits leave a playing transport untouched. Split-at-playhead uses the transport clock while playing. Source playback position and state are unaffected.

Verified:
- With real FFmpeg, playback after a split, a move and a gap reached the moved right part, and a trim during playback stopped the transport. Play again delivered the first copied frame in 93 ms.
- Natively, playback ran across the new cut, and seeking across it while playing continued.

## 9. ABI changes

ABI/contract **6 → 7**:
- **New exports:** `place_clip`, `move_clip`, `trim_clip`, `split_clip`, `split_clip_at_playhead`. Units are 0 for ticks and 1 for ms; edge/anchor are 0 for start and 1 for end.
- **Clip rows:** add `end_ms`, `start_ticks`, `end_ticks`, `min_start_ticks/ms` and `max_end_ticks/ms`.
- **Session view:** adds `editing { split_enabled, split_detail, notice }`.
- **Snapshot:** adds `edit_notice`.

Every ABI 6 export, status code, frame layout and handle rule is unchanged. A real interop bug was found and fixed during native verification. The new `ExactEndMs` property and the computed `EndMs` both mapped to JSON `end_ms`, so System.Text.Json rejected the timeline document and the app stayed at "Starting the engine…". `EndMs` is now `[JsonIgnore]`, and the interop suite exercises the deserialization.

## 10. Project schema implications

None. Format/schema 1 already represents every result, because grid alignment keeps `source_in` integral. Phase 3B–3D files open unchanged. Phase 3E files are ordinary format-1 documents. History, gesture previews and notices are not serialized. Save/reopen was verified identical, both natively and with real FFmpeg, across separate processes, together with missing-media editing and relinking after edits.

## 11. Automated test results

| Gate | Result |
|---|---|
| `scripts/verify.ps1` (fmt, clippy -D warnings, test, build, release, .NET Debug build/test, Release build, win-x64 self-contained publish) | **all 9 stages exit 0**, including the Release build and the win-x64 self-contained publish |
| `cargo test --workspace` | **129 passed** (baseline 111; +11 engine, +7 session/ABI), 16 ignored (9 external FFmpeg, subprocess helpers and opt-in measurements/generators) |
| `dotnet test CutCap.slnx` | **91 passed** (desktop 46, baseline 38; interop 45, baseline 41), 0 skipped |
| `cargo test -p cutcap-media --test ffmpeg_integration -- --ignored --test-threads=1` | **9 passed** |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `git diff --check` | clean |

New coverage:

- **Engine** (`clip_edit_tests.rs`, 11):
  - place, and move within/across tracks;
  - overlap, audio-track, negative, overflow and no-op rejection;
  - start trim with grid alignment on 1/15360, nonzero start PTS and source bounds;
  - end trim shrink, extend and overlap;
  - split identity, order, mapping and single-step undo/redo on 1/90000;
  - split edge rejection;
  - twenty repeated off-grid splits with no rounding accumulation (negative PTS);
  - saved state and divergent redo;
  - missing-media editing bounds;
  - generation invalidation;
  - plus one ignored latency measurement.
- **Session/ABI** (`timeline_edit_session_tests.rs`, 7):
  - clip rows with exact ticks and trim limits;
  - split at the Rust playhead, with reasons;
  - transport stopped without auto-resume, and stale frames rejected;
  - edit during a background save, plus save/reopen identities;
  - relink after edits, with missing clips still editable;
  - rejected edits reported as status;
  - ABI argument validation.
- **Core:** atomic transactions and grid arithmetic, through the above.
- **.NET interop with real FFmpeg** (`TimelineAdvancedEditingTests`, 4):
  - VFR and nonzero-start continuity through split, trim and move, comparing the mapped source tick and the actual decoded PTS at every sampled position;
  - playback across an edited cut and gap;
  - drop placement, cross-track move, trims and overlap rejection, with save/reopen across processes and independent source playback.
- **Desktop** (`TimelineGestureTests`, 8):
  - snapper threshold and exclusion;
  - move edge snapping and validity;
  - trim clamps to neighbours, source range and playhead snap;
  - placement validity;
  - handle zones on small clips at 50% zoom;
  - overlap/neighbour lookup over 5,000 clips;
  - split command state from Rust;
  - lookup cost at 100,000 clips.

## 12. Native Windows verification

`scripts/native-ui/timeline-edit-ui.ps1` drove the real Debug app with real mouse and keyboard input, only while CutCap owned the foreground. Exit code 0. Log: `docs/measurements/phase3e-native-scenario.txt`.

1. Import `interview take.mp4` (20 s 1080p H.264) and `colors 视频.mkv`.
2. Drag the interview tile onto Video 1 at 2.000 s: one clip at 1440000 ticks (`phase3e-dropped.jpg`).
3. Move the body +3 s: 00:05.000 to 00:25.000.
4. Add Video 2.
5. Move the clip onto Video 2; the inspector shows "Video clip on Video 2" (`phase3e-moved-to-v2.jpg`).
6. Trim the start to 7 s: `source_in` 30720 at 1/15360.
7. Trim the end to 21 s (`phase3e-trimmed.jpg`).
8. Seek the playhead to 14 s; Split is enabled with "Split the selected clip at the playhead (Ctrl+B)"; Ctrl+B produces 2 clips (`phase3e-split.jpg`).
9. Ctrl+Z restores 1 clip.
10. Ctrl+Y restores 2 clips with the same ticks.
11. Drop the colors tile near 21.150 s: it snaps to 15120000 ticks, the right part's end (`phase3e-snapped-drop.jpg`).
12. An overlapping drop is refused ("Clips cannot overlap", still 3 clips). A move onto the audio lane is refused ("Video clips go on a video track"). Escape cancels a drag in progress (`phase3e-drag-ghost.jpg` shows the dashed ghost and the dimmed original).
13. Play the timeline on Video 2 from 6 s (`phase3e-playing-edited.jpg`).
14. Seek to 13.5 s while playing; playback continues across the 14 s cut (`phase3e-across-cut.jpg`).
15. Save As.
16. Close: exit 0 in 653 ms, no leftover processes. Reopen in a new process.
17. Click each clip: inspector ticks and source-in match the saved file exactly (`identical=True`, `phase3e-reopened.jpg`).
18. Source playback runs, button Pause, with the timeline unchanged (`phase3e-source-playing.jpg`).
19. Close during active source decode: exit 0 in 620 ms.
20. No CutCap, FFmpeg or FFprobe processes remain.

Also checked:
- The minimum-size window (1440×945 physical, 150% scaling): all controls visible (`phase3e-minimum-window.jpg`).
- The 5,000-clip project, scrolled and zoomed natively.

Not validated: other DPI settings, other monitors, or a Release-build native run.

## 13. Performance measurements

Details: `docs/measurements/phase3e-editing.json`.

- **Engine commands (release):**

  | Clips | Move | Trim | Split | Undo / redo |
  |---|---|---|---|---|
  | 1,000 | 39 µs | 42 µs | 38 µs | 37–38 µs |
  | 5,000 | 80 µs | 0.35 ms | 74 µs | 50–56 µs |
  | 20,000 | 0.99 ms | 1.46 ms | 0.74 ms | 0.86–1.08 ms |

  Cost is dominated by the existing O(n) snapshot clone and overlap validation.
- **History:** at most 200 entries of at most three clips each.
- **Pointer moves:** 1.3 µs of snap plus overlap lookup per move at 100,000 clips. Layout and snapper are rebuilt once per accepted edit: 30–56 ms at 100,000 clips.
- **Drawing:** natively, in the Debug build at 5,000 clips, `TimelineLanes.Render` recording averages 0.82–0.91 ms (worst frame 10 ms) while drawing 18 clips. Phase 3B measured 0.45–0.64 ms; the extra branches for grips and the ghost account for the increase.
- **Transport recovery after an edit:** 93 ms to the first copied frame.
- **Save:** 5,000 clips save in 87 ms. An edit during a pending save is accepted and stays dirty.

The native drag-preview segment of `timeline-draw.ps1` did not complete. The Claude desktop app took the foreground, and the input guard aborted without sending input. Drag responsiveness is evidenced by the per-move cost above and by the scenario's gestures, which committed within 149–200 ms of release, including UIA polling.

## 14. Known limitations

- **Editing scope:** no ripple, roll, slip or slide edits; no multi-selection or group moves; no linked audio; no keyboard nudging or trimming (mouse only; Add to timeline and Split remain keyboard and toolbar accessible).
- **Drops:** place the whole source and snap only the start edge.
- **Source-tick alignment:** start trims and splits may move up to one stream tick from the requested position. This is reported, never silent.
- **Cost:** each command clones the full snapshot, about 1 ms at 20,000 clips.
- **Track creation:** still not undoable.
- **Validation coverage:** native runs used the Debug build at 150% only.
- **Environment:** an external UI Automation client on the development machine slows UIA calls and keeps an idle CutCap at about one core. This is not a CutCap defect (Phase 3B finding).

## 15. Pull request

**[ayzeeyan/cutcap#3](https://github.com/ayzeeyan/cutcap/pull/3)**: "feat: integrate timeline preview, playback, and advanced editing (Phases 3C–3E)", base `main`, head `claude/phase-3e-advanced-timeline`. GitHub returned it open and not a draft, with head `2bb34ac` before this report commit. It is not set to auto-merge and is left for the owner to review and merge.

## 16. Branch cleanup results

Each candidate was verified before deletion: tip reachable from a preserved published ref (`git merge-base --is-ancestor`), zero commits outside `origin/main` and `origin/claude/phase-3e-advanced-timeline`, no open PR using it as head or base, not the default branch (`main`), and not referenced by the repository workflow.

| Deleted remote branch | Tip | Preserved in |
|---|---|---|
| `claude/phase-2b-playback-ui` | `3af9d04` | `main` and the 3E branch |
| `codex/phase-2-playback` | `bab1659` | `main` and the 3E branch |
| `codex/phase-3a-persistence` | `6ea31b0` | `main` and the 3E branch |
| `claude/phase-3b-timeline-editing` | `eca2b0e` | `main` (PR #2) and the 3E branch |
| `codex/phase-3c-timeline-preview` | `43d859c` | the 3E branch (PR #3) |
| `codex/phase-3d-timeline-playback` | `4fc0738` | the 3E branch (PR #3) |

Each was deleted by explicit name (`git push origin --delete <branch>`), with no wildcards and no force. After `git fetch origin --prune`, the remote has exactly `main` (`0b777c2`) and `claude/phase-3e-advanced-timeline`, and PR #3 still points at the published head. None were retained: every candidate met every condition. Local branch copies were left untouched. Once PR #3 merges, `claude/phase-3e-advanced-timeline` can be deleted separately, leaving only `main`.

## 17. Recommended next milestone

Phase 4 should add audio-aware editing and synchronization, in this order:
1. Linked audio for imported media.
2. Ripple delete/insert and multi-selection, using the same `ClipChange` transactions.
3. Incremental snapshots or clip-diff events, to remove the O(n) per-command clone before projects grow further.
4. Keyboard trimming and nudging for accessibility.

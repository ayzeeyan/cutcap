# Windows interactive regression checks (Avalonia)

These are manual native UI checks, not automated tests. Record date, build (Debug or published Release), actual DPI scale, window sizes and results in the current report. Use a fresh project and the generated tests/fixtures; no user footage or accounts. Media checks need the documented external FFmpeg/FFprobe. `scripts/native-ui` can drive several of these through real input; it aborts if another window takes the foreground.

| Action | Expected result |
|---|---|
| Launch | Custom title bar (mark, status, project, actions) over native caption buttons; no drawn title text overlaps it. Media, Player, Inspector and Timeline render; two empty tracks; Export disabled. |
| Drag the title bar, double-click it, maximize/restore, snap, resize from edges | Native Windows behavior. |
| Resize to the 960×600 minimum, then large | All four regions usable, nothing clipped; narrow player drops start/end/duration; category tabs may wrap; inspector tabs stay reachable. |
| Drag each grid splitter; focus one and use arrows; Ctrl+0 / Reset layout | Only the adjacent panels change; minimums hold; reset restores proportions and 100 % zoom. |
| Video track / Audio track | Real track count increases with V/A badges numbered per family; the new track is selected. |
| Click a track header, Down/Up, then Ctrl+Plus | Selection moves through the engine; the shortcut works immediately after the click. |
| Ctrl+Plus / Ctrl+Minus, zoom buttons, slider, % button | Ruler spacing and percentage change together; clamp at 50–200 %. |
| Scroll lanes horizontally/vertically, wheel over headers | Ruler follows horizontally; headers follow vertically. |
| Ctrl+I / Import | Native parented picker; Cancel shows "Import cancelled" and registers nothing. |
| Import colors-cfr.mkv | Tile with real 3.0 s (estimated) duration; inspector shows 320 × 180, FFV1, 4 fps; red frame at PTS 0. |
| Go-to 1500 + Enter; scrubber Home then Right ×3 | Green frame PTS 1500; then requested 00:00.300 → actual PTS 500 (ceiling). Field, readout and scrubber agree (Phase 1 stale-binding regression). |
| Import colors-vfr.mkv, seek 201 | Actual PTS 700 @ 1/1000. |
| With audio / Silent / All media; search "vfr" | Only matching tiles; selecting a filtered tile previews that exact source. |
| Categories other than Media; inspector tabs other than Details | Honest "coming later" / "needs a clip" states; nothing pretends to work. |
| Play / Pause / Space; Pause before the first frame; Stop | Button follows engine state and intent; readout advances while playing and holds while paused; Space works from lists but types in text fields; Stop returns to 00:00.000 only once the zero frame arrives. |
| Seek while playing / paused / stopped; rapid scrubbing | Playing stays playing, paused stays paused; only the last scrub target is presented; the go-to field shows the request. |
| Play to the end, then Play | "End of source" with the last frame held; Play restarts from zero. |
| Switch source or New Project during playback; close during playback or a pending seek | Old epoch never drawn; Play disabled with no source; exit 0 and no CutCap/FFmpeg/FFprobe left. |
| Hover disabled controls (Undo, Export, track eye/lock) | Tooltip explains why; no action. |
| Import corrupt.mp4 / a missing file | Clear error, no new tile, existing project and frame preserved. |
| Ctrl+N with media and extra tracks | New project ID, two empty tracks, empty library, cleared preview. |
| Shortcuts, About | Flyouts open and dismiss with Escape or a click outside. |
| Close during a pending decode | Process exits promptly with code 0; no CutCap/FFmpeg/FFprobe processes remain. |

Automated coverage is separate: Rust unit/integration tests (contracts, core, media, session and ABI) and .NET tests (interop boundary against the real DLL, timeline/list logic). An accessibility tree dump or one DPI setting is not a screen-reader or scaling certification.

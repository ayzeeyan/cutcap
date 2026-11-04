# Windows interactive regression checks

These are manual native UI checks, not automated unit tests. Record date, executable/profile, actual DPI scale and results in the current phase report. Use a fresh project and generated tests/fixtures; no user footage or external accounts are needed. Media checks require the documented external FFmpeg/FFprobe.

| Action | Expected result |
|---|---|
| Launch | Native window opens near the primary display origin; app bar, Media, Source, Inspector, Timeline and status line render as floating panels on the ground. Empty viewer offers Import video. No fake assets/playback/progress. |
| Restore, resize to minimum, maximize | Panels remain usable at the declared 900×540 minimum (Source drops start/end buttons below 500 px, nothing clips); preview preserves the actual oriented source aspect (empty state 16:9); library/inspector scroll if needed. |
| Add Video / Add Audio | Real track count increases, distinct named empty track appears and inspector selects it after a worker result. |
| Select a track, then Ctrl+Shift+A, then Ctrl+N | Selection clears and a new project opens without clicking elsewhere. Regression: replacing the full VecModel destroys the focused repeater item; update rows in place and reset workspace focus for project replacement. Slint 1.18 consumes Escape for popup handling, so Escape remains popup dismissal. |
| Ctrl+N with extra tracks | New project ID, two empty tracks, no selection. Previous in-memory tracks are discarded. |
| Ctrl+Minus, Ctrl+Plus, zoom buttons, 100% | Scale/readout and ruler spacing change together; clamp at 50–200%; reset returns 100%. Regression: use logical-key bindings, since raw control-modified text is not a reliable shortcut key. |
| Drag each of three splitters | Only corresponding panel dimension changes; sizes clamp; center remains visible. |
| Tab to splitter, use arrows | Visible focus; resize in 8 logical pixel steps. |
| Ctrl+0 / Reset layout | Default split sizes and 100% zoom return. |
| Timeline horizontal/vertical scrolling (lanes, Shift+wheel, wheel over headers/ruler) | Later ruler marks and overflow tracks become visible; track headers stay fixed horizontally and follow vertical scroll; the ruler follows horizontal scroll. |
| Click a media row or track header, then Up/Down | Selection and focus move to the neighbouring row through the engine; Enter/Space selects; focus ring stays visible. |
| Tab / Shift+Tab and Enter / Space | Enabled controls can be reached and activated; disabled controls are skipped. |
| About | Built-in AboutSlint badge/version visible; Escape or Close dismisses popup. Do not need to open external links. |
| Hover disabled action | Readable tooltip explains unavailable functionality; no operation or success is produced. |
| Ctrl+I / Import video | Native parented file picker; Cancel returns to app without registering an asset. |
| Choose colors-cfr.mkv | Real 320×180 ffv1 metadata, estimated 3 s duration, no audio; initial red frame and actual PTS 0. |
| Go-to field 1000 / 1s / 00:01.000, Enter or Go; then forward (+1 s) | Green at PTS 1000, blue at PTS 2000; field, timecode and scrubber all show the new position; heading shows actual PTS; timeline playhead remains zero. Invalid text (e.g. 1.5) shows an error in the status line. |
| Drag source scrubber repeatedly, then forward; focus scrubber and use arrows / Shift+arrows / Page / Home / End; rapidly change source | Latest desired source/position wins; obsolete frames/errors cannot replace it; scrubber, timecode and field stay in sync (Phase 1 stale-binding regression); UI stays interactive. |
| Import colors-vfr.mkv, seek 201 ms | Blue frame PTS 700 @ 1/1000; real irregular timestamps, not nominal-rate indexing. |
| Import audio-video.mp4 / rotated.mp4 / Unicode path | Authentic audio presence/rate/container; rotated portrait content; space/Unicode path accepted. Play stays disabled. |
| Import corrupt.mp4 / audio-only.mkv | Understandable failure, no new asset, existing project/image preserved. |
| Seek beyond last PTS / rename an imported synthetic file outside app | Explicit NoFrame/missing-file failure; retain last valid selected-source image, allow recovery/new seek. |
| Library filters | With audio / Silent show only matching sources; selecting a filtered row previews that exact source; All Media restores every row; Up/Down stays within the filtered list. |
| Disabled future controls | Timeline Preview, Undo/Redo, Save, Export, Split, Play and track eye/mute/lock are visible, disabled and explain why on hover; none changes state. |
| Cancel decode/import | Actual task cancelled/superseded, no false registration or success; another request works. |
| New Project while media work pending | Assets/selection clear; old probe/frame cannot enter replacement project; keyboard remains usable. |
| Close during pending probe/decode | Window/process and owned FFmpeg/FFprobe children exit; media/project workers join. Verify child PIDs/processes, not just window disappearance. |

Automated coverage is separate: contracts validate IDs/time/serialization; core integration tests cover graph validation/atomic mutations; app tests cover request scope, failures, real channels, backpressure and shutdown. A native accessibility tree or one DPI setting is not a complete screen-reader/scaling certification.

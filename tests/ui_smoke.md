# Windows interactive regression checks

These are manual native UI checks, not automated unit tests. Record date, executable/profile, actual DPI scale and results in docs/PHASE_0_REPORT.md. Use a fresh in-memory project; this checklist never needs user media or external accounts.

| Action | Expected result |
|---|---|
| Launch | Native window opens near the primary display origin; all six regions and status area render. No fake assets/playback/progress. |
| Restore, resize to minimum, maximize | Panels remain usable at the declared minimum; preview stays 16:9; media/inspector scroll if their content is taller than available space. |
| Add Video / Add Audio | Real track count increases, distinct named empty track appears and inspector selects it after a worker result. |
| Select a track, then Ctrl+Shift+A, then Ctrl+N | Selection clears and a new project opens without clicking elsewhere. Regression: replacing the full VecModel destroys the focused repeater item; update rows in place and reset workspace focus for project replacement. Slint 1.18 consumes Escape for popup handling, so Escape remains popup dismissal. |
| Ctrl+N with extra tracks | New project ID, two empty tracks, no selection. Previous in-memory tracks are discarded. |
| Ctrl+Minus, Ctrl+Plus, zoom buttons, 100% | Scale/readout and ruler spacing change together; clamp at 50–200%; reset returns 100%. Regression: use logical-key bindings, since raw control-modified text is not a reliable shortcut key. |
| Drag each of three splitters | Only corresponding panel dimension changes; sizes clamp; center remains visible. |
| Tab to splitter, use arrows | Visible focus; resize in 8 logical pixel steps. |
| Ctrl+0 / Reset layout | Default split sizes and 100% zoom return. |
| Timeline horizontal/vertical scrolling | Later ruler marks and overflow tracks become visible. Track headers scroll with content in Phase 0. |
| Tab / Shift+Tab and Enter / Space | Enabled controls can be reached and activated; disabled controls are skipped. |
| About | Built-in AboutSlint badge/version visible; Escape or Close dismisses popup. Do not need to open external links. |
| Hover disabled action | Readable tooltip explains unavailable functionality; no operation or success is produced. |
| Close | Window and process exit; command worker joins. No lingering task or output files. |

Automated coverage is separate: contracts validate IDs/time/serialization; core integration tests cover graph validation/atomic mutations; app tests cover request scope, failures, real channels, backpressure and shutdown. A native accessibility tree or one DPI setting is not a complete screen-reader/scaling certification.

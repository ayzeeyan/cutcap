# CutCap desktop design system

Mode: Operate. Product truth is in PRODUCT.md; architecture is in docs/ARCHITECTURE.md. The user explicitly delegated visual decisions and required a working native application, so this implementation uses the brief directly without an external concept approval flow.

The visual direction is a restrained contact sheet: regular time/track registrations, calm neutral viewing ground and a warm copper locator. It supports repeated precise operations during long editing sessions. Brand expression stays in the original cut-mark SVG, typography and the single locator/focus accent; it does not compete with future video pixels.

`apps/cutcap/ui/theme.slint` owns semantic colors and shared metrics. Graphite canvas #151719, panel #1c1f22, raised control #25292d, line #363c42, foreground #edf0f3, secondary #b1b9c2, disabled #858f99 and copper #f3a17b. Normal text must maintain 4.5:1 contrast. Selection uses a warm dark fill and border/focus, never color alone for button labeling.

Segoe UI is the native text family; Consolas is confined to measured timecode. Body 13 logical px, labels 12, panel headings 14 with 600 weight. 8/16/24 px grouping rhythm, 4 px control radii, 32 px actions, 44 px panel headings, 58 px tracks and 6 px focusable splitters. Fine ruler/status labels use 10–11 px and remain high contrast. Prefer semantic tokens for additional repeated values.

The toolbar carries project identity, real new-project/reset/about actions and disabled persistence/export. The initial window opens near the primary display's origin; native maximize/restore and resizing remain available. The viewer follows the actual oriented source aspect ratio, with a 16:9 empty state. The library shows real metadata entries, without invented thumbnails. Source seeking stays visibly separate from the timeline. The library/inspector clamp to preserve the center; the timeline has a 210 logical px default and clamped resizing. Its 60-second empty ruler is a view range, not project duration. Headers scroll with content. No decorative motion or fake jobs.

All custom actions have names, disabled states, hover/focus styling, Windows accessibility roles and text tooltips. Tab/Shift+Tab, Enter/Space, global shortcuts and splitter arrows complement pointer controls. High-DPI sizing uses logical pixels; a complete screen-reader and scale-matrix audit remains future work. See PHASE_0_REPORT.md for actual visual/interaction tests.

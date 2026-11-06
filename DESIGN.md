# CutCap desktop design system

Mode: Operate. Product truth is in PRODUCT.md; architecture in docs/ARCHITECTURE.md. Implemented in Avalonia (`apps/CutCap.Desktop/Themes`). The workspace follows the organization and density of a conventional compact editor (the owner's CapCut reference) with an original CutCap identity: no CapCut logo, wordmark, Pro upsell, assets or exact visuals.

## World

Dark, compact, flat and calm. Charcoal panels float on a near-black ground with 6 px gutters that double as resize handles. One cyan marks what is live: current time, selection, keyboard focus and the primary action. Track families carry information colors: teal for video, blue for audio. **No purple or violet anywhere.** Systems the engine cannot back yet are present with an honest state, never faked: no invented clips, thumbnails, waveforms, presets or playback.

## Tokens (Themes/Tokens.axaml)

- **Surfaces:** stage #0C0D0F (behind the picture), ground #111315, surface #191B1E (panels), well #1E2124 (inputs, lane bands), raised #22252A, hover #2A2E34, pressed #33373E.
- **Lines:** line #26292E, border #30343A.
- **Ink:** text #F1F3F4, muted #A5ADB5, faint #868E96 (≥4.5:1 on surface).
- **Accent:** #22D9E6, hover #50E6EE, pressed #1AB9C4, ink on accent #04181A, wash #123236 (selected rows), lane #102629 (selected lane).
- **Families:** video #3CC4CF on fill #17616A; audio #6FA3F5 on fill #26487F.
- **Status:** success #4FD18B, error #FF8F84.
- The Fluent `SystemAccentColor` family and TextBox/Slider/ToolTip/Flyout brushes are overridden with the same tokens, so stock controls cannot leak a default accent.

## Type and metrics

Inter (bundled, OFL 1.1) for UI: body 13, labels 12, fine 11, panel titles 13 semibold, sheet titles 15. Cascadia Mono/Consolas only for measured time (source timecodes, ruler labels, tile durations). Controls are 28 px; title bar 44; panel headers 40; ruler 28; tracks 50; track header column 176. Radii: controls 5, panels 8, tiles 6. Shadows only on flyouts.

## Layout

- **Title bar:** mark/name, engine status (errors in red), centred authoritative project name with Saved/Unsaved changes/file status, Project menu (New/Open/Save/Save As/Relink), Shortcuts, Reset layout, About, disabled primary Export and Windows caption buttons. The window title adds an asterisk for real dirty state. New/Open/close protect unsaved changes with Save/Don't save/Cancel. Labels drop below 1180 px.
- **Asset workspace:** icon-over-label categories (Media, Audio, Text, Stickers, Effects, Transitions, Filters, Adjustment). Media shows a collection list (All media / With audio / Silent), search, Import and a tile grid; the other categories show "coming later" states.
- **Player:** Source / Timeline tabs (single-video-track timeline still preview), source name and actual-frame caption, the fitted picture on the stage, scrubber, cyan time (the shown frame's source time while a playback epoch has delivered one, otherwise the requested position; the accessible name says which) / duration, centred transport (start, −1 s, round Play/Pause that follows the engine's state and intent, Stop, +1 s, end), go-to field. Transient playback states (Starting, Seeking, Buffering, End of source) appear as a small chip at the stage's top-left; failures as the top banner.
- **Inspector:** Details / Video / Speed / Adjustment / Audio tabs. Details shows the selection's identity and collapsible metadata sections; the clip tabs explain they need a clip.
- **Timeline:** disabled Undo/Redo/Split/Delete, Video track / Audio track, timeline playhead time, zoom out / slider / zoom in / percentage; track headers (V1/A1 badges, disabled eye/mute/lock) beside custom-drawn ruler and lanes with a white Rust-positioned playhead. The ruler accepts click-to-seek; Timeline Play stays disabled.

Upper columns are proportional (34 / 41 / 25 %) with minimums 290 / 340 / 240 px; the timeline takes 42 % of the workspace (minimum 170). Window minimum properties are 960×600; the Phase 3A native minimum client measured 960×630 with extended chrome at 150% scaling. Initial size fits the current logical work area so project controls remain on screen. Container queries drop secondary controls in narrow panels (player < 520 px hides start/end, duration and caption).

Timeline monitoring identifies one video track, distinct from source playback. Blank, missing/unavailable and no-frame-inside-clip states clear the picture; they never retain another clip's frame. Timeline Play remains disabled.

## Components and states

- **Buttons** (ControlThemes): tool (flat), filled (raised), primary (cyan). Every interactive element has hover, pressed, disabled (0.38 opacity) and a 1.5 px focus ring (`:focus-visible`), plus a tooltip; disabled tooltips stay available.
- **Tabs:** CategoryTab (icon over label, cyan when selected) and ViewTab (cyan text and underline) on TabStrip; NavItem for vertical lists.
- **Lists** keep item identity across engine updates; media tiles and track headers select through the engine on click or arrows and expose list-item semantics with full accessible names.
- **Icons:** authored 16 px SVGs (1.5 px round stroke) in Assets/Icons, converted to path data by scripts/generate-icons.py and drawn by IconView in the inherited foreground.
- **FrameView** draws the decoded frame with uniform fit; **TimelineRuler/TimelineLanes** draw only the viewport.

No decorative motion.

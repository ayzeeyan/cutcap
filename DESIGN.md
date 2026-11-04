# CutCap desktop design system — Studio Blue

Mode: Operate. Product truth is in PRODUCT.md; architecture is in docs/ARCHITECTURE.md. Phase 2A replaced the Phase 0/1 "contact sheet" look, then a short-lived Splice Room iteration, with this direction. The owner supplied a reference screenshot of a conventional dark editor and asked for that look without pixel-perfect copying; see docs/PHASE_2A_UI_REPORT.md for the audit and evidence.

## World

A familiar, calm editing suite. Surfaces are cool dark greys with thin borders, so panels read as distinct tools without heavy chrome. One blue marks whatever is live: the playhead, the current selection, keyboard focus and the primary action. Track families carry information colours (video blue, audio green). Controls the engine cannot back yet stay visible but disabled, with a tooltip saying why; nothing fakes clips, thumbnails, waveforms or playback.

`apps/cutcap/ui/theme.slint` owns every token.
- **Surfaces:** stage #0a0b0e (behind the picture), ground #0f1115 (gutters, lanes), panel #181a1f, raised #21242a, hover #2a2e35, pressed #33373f.
- **Lines:** #252830 and #343842 (control outlines).
- **Ink:** text #e9ebf0, muted #a3a9b4, faint #808794.
- **Accent:** #3d8bff, used for text, lines and the playhead. Its fill is #3468e0 with white ink; the wash is #1b2740 (selected rows) and the lane tint is #151c2a (selected lane).
- **Families:** video #5b8def on fill #2c55a8, audio #4fc480 on fill #23774a.
- **Status:** success #4fc480, error #ff8f84.

## Type and metrics

Segoe UI is the UI face: body 13, labels 12, fine 11, panel headings 14/600. Consolas is reserved for measured time, where tabular digits matter: the source timecodes, the 19 px timeline timecode, ruler labels and tile durations.

Rhythm is 4/8/12/20 px.
- **Controls:** 30 px tall; transport buttons 34 px round, Play 42 px round.
- **Bars and rows:** app bar 50, panel headings 42, status bar 28, ruler 34, tracks 54, track headers 196.
- **Radii:** controls 5 px, panels 8 px.

Panels have a 1 px border and float on the ground with 6 px gutters; the gutters are the resize handles and show an accent line when dragged or focused.

## Layout

The window stacks four bands:
- **App bar:** mark and name; Import, New Project, Save; Undo/Redo; centred project name; layout reset, About, Export.
- **Upper workspace:** Media Library | Source Preview | Inspector. The library and inspector clamp to 220/232–400/380 px so the viewer stays dominant.
- **Timeline:** full width, 40 % of the workspace by default, clamped from 150 px to 62 %.
- **Status bar.**

The window minimum is 900×540 logical, so the app fits 1920×1080 at 175 % and 1920×1200 at 200 %. Labels and secondary controls drop out progressively at narrow widths (Undo/Redo, the centred title, transport start/end, Split, the zoom slider).

The Source Preview panel holds:
- **Tabs:** Source Preview / Timeline Preview (disabled), with the source name on the right.
- **Stage:** the picture.
- **Scrubber row:** the requested position and duration beside the scrubber.
- **Transport row:** the actual-frame caption (left), centred transport, and the go-to field (right).

Nothing in this panel moves the timeline playhead. The Timeline keeps its track headers and ruler fixed while the lanes scroll in both directions. The ruler is a view range (≥60 s), not the project duration.

## Components and states

**Buttons.** One button vocabulary (`ActionButton`) covers outlined, subtle (toolbars), primary (blue fill) and icon-only buttons; icon-only buttons require a spoken `name`. `FilterChip` provides pill filters and `ViewTab` underlined tabs. Every interactive element has hover, pressed, disabled and focus states (a 1.5 px accent ring) plus a tooltip.

**Icons.** Icons are authored 16 px SVGs (1.5 px stroke, round caps) in `ui/assets/icons`, tinted with `colorize`.

**Lists.** Media rows and track headers select on click, Enter or Space, and move with Up/Down. They expose list-item and selected accessibility state and keep repeater identity.

**Inspector.** Sections are collapsible cards built from `InfoSection`.

**Custom controls.** The scrubber, zoom slider and time field are app-owned controls, so no Fluent accent leaks in. Their displayed values always derive from engine-driven inputs, so user interaction cannot sever bindings.

There is no decorative motion, and no invented thumbnails, clips, waveforms, fabricated progress or playback state.

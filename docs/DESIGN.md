# CutCap interface design

The Phase 4C specification asked for this document to be read and updated, but
none existed: design rules lived in `AGENTS.md`, `Themes/` and the phase reports.
This file records what the app actually does. `Themes/Tokens.axaml` stays the
source of truth for values.

## Palette

| Role | Token | Value | Use |
|---|---|---|---|
| Ground and panels | `Ground`, `Surface`, `Well` | charcoal | Everything behind content. |
| Accent | `Accent` `#22D9E6` | cyan | Selection, focus, current time, the primary action. Nothing else. |
| Video | `VideoFill`, `Video` | teal | Video clips and video badges. |
| Audio | `AudioFill` `#26487F`, `Audio` `#6FA3F5` | navy, blue | Audio clips, audio badges. Peaks are `#A9CBFF`. |
| Linked pair | `#B322D9E6` | cyan, translucent | Outline on both members of a selected pair. |
| Error, mute | `Error`/`MuteOn` `#FF8F84`, `MuteOnWash` `#4A2926` | red | Errors, an active Mute, the CLIP indicator. |
| Solo | `SoloOn` `#F2C14E`, `SoloOnWash` `#4A3C18` | amber | An active Solo only. |

No purple or violet anywhere. New colours become semantic tokens, never literals
in views.

## Timeline

- Lanes are 56 DIP tall and custom drawn; no control per clip or tick.
- Clips are drawn from engine rows. C# computes pixels, never project time.
- An inaudible track (muted, or not soloed while another is) keeps its clips and
  real waveforms, at half opacity.
- A pending waveform chunk draws a faint baseline, never invented peaks.

## Track headers

The header column is 176 DIP and must not grow.

- Video tracks: badge, name, visibility and lock placeholders on one row.
- Audio tracks: the same top row, then a mixer row: Mute (speaker icon, red when
  on), Solo (`S`, amber when on), a compact volume slider, and the engine's gain
  text in a fixed 56 DIP cell so the slider does not shift as digits change.
- The slider moves in 0.5 dB steps from −60 to +6 dB. The value beside it is
  always the one Rust reports.
- Controls that the engine cannot back stay visibly disabled with a tooltip;
  Mute, Solo and volume all change the real mix.

## Library and inspector

- A tile shows the media family (film or music note) and the real duration. No
  thumbnail is invented.
- Inspector identity shows a teal "Video" or blue "Audio" badge.
- A selected source with audio lists each decodable stream with a Place action;
  files are never expanded into one clip per stream automatically.
- A selected clip shows Unlink when it is linked and Link only when the engine
  found one exact partner.

## Export

The Export button is the one cyan primary action in the title bar. It is enabled once
FFmpeg has confirmed both encoders, and shows `Exporting N%` while a job runs.

The dialog is not modal: the editor stays usable, and later edits are not included.
It states the file, the one video track, the preset, length, heard audio tracks and
project revision. If other video tracks hold clips it says so in amber and needs a
check before Start. Reasons an export cannot run are red text, never a silently
disabled button. The bar moves only with work the engine reports, and reaches 100%
only when the finished file is in place.

## Layout floor

Usable at 960×600 DIP: every transport, editing and mixer control stays reachable.
Long toolbar hints give way first (ellipsis); the track header does not.

## Accessibility

Named controls follow state ("Mute Audio 1" / "Unmute Audio 1"), tooltips say what
a control does and its current value, and sliders are keyboard operable (arrow
keys, Esc cancels a drag). No claim of formal accessibility certification is made.

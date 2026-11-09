# Phase 4C — Multi-track mixing, track controls and independent audio editing

Status: implemented and verified on one Windows machine. Not merged. ABI and
contract **10**, core project schema **3**, project envelope `format_version`
still **1**. The binding rules are in `PHASE_4C_AUDIO_MIXING_CONTRACT.md`, and
ADR 026 records the decisions and the alternatives rejected.

## 1. Starting revision

- **Baseline.** Verified `origin/main` at
  `eb03725631f9a863afee18a53db2433b19422ba0`, the Phase 4B merge (PR #5).
- **Branch.** `claude/phase-4c-audio-mixing`, cut from that commit.
- **Baseline gates, run before any change.** Rust 169 passed and 24 ignored,
  Desktop tests 57, Interop tests 52, the real FFmpeg suite 12 plus 4 audio
  tests, and the device test passed.
- **One baseline defect.** `starvation_freezes_the_clock_and_counts_underruns`
  raced between the last push and the end of the epoch. It was fixed first and
  separately, in `626004d`, so later failures could not be blamed on it.
- **Test machine.** Windows 11 (10.0.26300), Intel Core Ultra 7 255H (16 logical
  cores), 150% scaling, default output "Speakers (Realtek(R) Audio)" at 48 kHz,
  FFmpeg/FFprobe 9.0.1 on PATH (external, not bundled).

## 2. Exact scope implemented

| Limitation at the start | Result |
|---|---|
| One monitored audio track | Every audio track is mixed into one output with one clock. |
| Volume, mute and solo were decorative | Real, persistent, undoable, and applied while playing. |
| Linked pairs could not be separated | Unlink removes the link in one undo step, with no timing change. Link re-pairs clips only when exact. |
| Only the primary stream was reachable | Any decodable stream can be placed, explicitly, as an independent clip. |
| Audio-only files were refused | WAV, MP3, AAC/M4A and FLAC import as audio sources. |
| Independent audio editing | Moves, trims and splits of unlinked audio touch only that clip. |
| Audio trims were bounded by the video range | An independent clip is bounded by its own stream. |

Latency calibration UI and export remain out of scope. So do automation,
keyframes, EQ, reverb, noise reduction, time stretching, crossfades, recording,
GPU composition, transitions, ripple and multi-selection.

## 3. Mixer architecture

`crates/cutcap-ffi/src/audio_transport.rs` replaced the single-track feeder.

- **One output, one ring, one clock.** One feeder thread sums every audio track
  and feeds one WASAPI stream. The device position stays the timeline clock.
- **Plan.** The current clips of all audio tracks become sample spans for the
  epoch. The sum runs in ascending project track order and is deterministic.
- **Blocks.** The mixer works in 10 ms blocks, bounded by the next clip boundary
  and the output room. Controls are read once per block.
- **Mix-ahead.** The ring stays 400 ms, but on a real device the mixer keeps only
  150 ms ahead of the render position, with a 120 ms prefill. Decoded PCM queues
  absorb decoder jitter, which the ring used to do. A control therefore reaches
  the output after about 150 ms of queued mix rather than 400 ms.
- **Budgets.** 16 audible overlapping tracks, 24 decoder processes, 24 chunks of
  at most 4096 frames per decoder (at most about 18.9 MiB of decoded PCM).
- **Scheduling.** Exact starts wait for their PCM, so a clip never starts late.
  A track already contributing waits too, keeping every track sample-aligned. If
  that wait outlasts the ring, the output pads, the clock freezes and an underrun
  is counted. A track unmuted mid-clip joins late without blocking the others. An
  inaudible track never blocks.
- **Stale PCM.** Each epoch has an identity, rechecked under the feed lock
  immediately before every push, so PCM from an earlier seek or edit cannot enter
  the output.
- **Render thread.** Copies from the ring only. No file, decoder, join or
  allocation per period.

## 4. Track-gain and mute/solo semantics

- **Gain** is an integer `gain_cdb`, hundredths of a dB, `-6000..=600`. Linear
  amplitude is `10^(gain_cdb/2000)`. It is amplitude gain, not power gain. −60 dB
  is real and non-zero; silence is Mute.
- **Audibility** is `!mute && (!any_solo || solo)`. Mute wins over solo. A muted
  and soloed track is silent and still counts as a solo, so it silences the
  others. Multiple solos are allowed.
- **Live changes** use a 10 ms linear ramp from the next block and never touch the
  epoch. A new epoch starts at the target, so seeks and starts stay sample-exact.
- **Clipping.** The final stereo sum is hard-clipped to `[-1, 1]` and counted.
  There is no limiter and no normalization, so the mix is exactly the sum. No
  claim of transparent limiting is made. When samples clip, the transport shows
  CLIP and the view reports the count and the peak.
- **Ownership.** Rust owns every value. C# holds a pointer position only during a
  drag and displays the value Rust publishes.

## 5. Audio-only media support

Each format below was decoded by the real FFmpeg suite and compared with an
independent decode of the same file:

| Container | Codec |
|---|---|
| WAV | `pcm_s16le`, `pcm_s24le`, `pcm_f32le` |
| MP3 | `mp3` |
| MP4/M4A | `aac` |
| FLAC | `flac` |
| Matroska | `pcm_s16le` (the existing fixture) |

- A source is `Video` or `Audio` metadata. Audio-only metadata has no picture
  fields, and `MediaAsset.kind` is `Audio`. No companion video clip is created.
- Cover art (`attached_pic`) is not treated as video.
- WAV reports no stream start, which is treated as 0.
- A duration known only from the container is marked as an estimate.
- Anything off the list stays `Unsupported` at import.
- The Source monitor shows such a source as audio only, with its transport
  disabled. It plays from the timeline, including a timeline holding nothing but
  audio, which has a real length and shows a blank picture.

## 6. Stream selection and Unlink behavior

- **Selection.** A selected source lists its decodable streams with a Place
  action for each. The chosen stream goes to the first audio track free for its
  whole interval (or a named track). Files are never expanded into one clip per
  stream.
- **Identity.** A clip keeps `stream` and its media id. Waveform keys include the
  stream index, so two streams of one file never share peaks.
- **Unlink** removes the link from both members in one undo step. `source_in`,
  start, duration, track and stream are untouched, so the sound does not change.
- **Link** is offered only for one unlinked video clip and one unlinked audio clip
  with the same media, start, duration and exact `source_in`, and a stream that
  exists. A shared file alone is never enough.
- **Independent clips.** `source_in` is a rational instant, so an independent clip
  uses `1/sample_rate` and a clip that was linked keeps the video clock it had.
  Start trims and splits move by whole source ticks, with one rounding at
  evaluation, so nothing accumulates.
- **Bounds.** An independent clip is limited by its own stream, in its own clock.
  One already reaching outside that range stays valid and cannot grow further.

## 7. Project schema/migration

Schema 3 adds optional `controls` to a track and allows audio-only source
metadata. Defaults are not written.

| Opened document | Result |
|---|---|
| Schema 1 | Migrated in memory as in Phase 4B, then default controls. |
| Schema 2 | Default controls. |
| Schema 1 or 2 carrying non-default controls | Refused, file untouched. |
| Schema 3 | Validated: gain in range, video tracks hold defaults. |
| Schema 0, 4, 999, or anything else | Refused. |

Opening never writes. The next save writes `schema_version: 3`. Pre-Phase 4B saved
metadata without per-stream audio details still verifies against a fresh probe,
which corrects a Phase 4B regression that would have reported such sources as
changed. Selection, drag previews, caches and device state are never written.

## 8. Undo/Redo design

There is one history stack. Entries are clip changes or track control changes
`{track, before, after}`.

- A volume drag is any number of previews and one commit, so one entry. Esc
  cancels. A commit equal to the stored value adds no entry.
- Each Mute or Solo press is one entry.
- Undo and Redo of a control entry apply live and keep playback running.
- Clip edits, including undoing them, still end the epoch, as before.
- Unlink, Link and stream placement are one entry each.

## 9. Audio clock and synchronization

- There is still one audio-master clock: the device position through the segment
  map. Intentional silence (gaps, muted tracks, tracks with no clip, a fully
  muted mix) is real timeline audio to the clock. Only frames the output had to
  pad are starvation, and only those freeze the clock.
- The timeline's length is the later of the picture's end and the last audio
  clip's end, so audio that outlasts the picture is reachable and playable.
- Default latency compensation stays 0. No measured device value is hardcoded,
  and `CUTCAP_AUDIO_LATENCY_MS` remains the only way to set one.
- Device failure stops playback with an error, as in Phase 4B.

## 10. Native ABI changes

`ABI_VERSION`, `NativeMethods.AbiVersion` and `CONTRACT_VERSION` are 10. Every
ABI 9 export, status number, layout and handle rule is unchanged. New exports:
`cutcap_session_place_audio_stream`, `unlink_clip`, `link_clips`,
`set_track_mute`, `set_track_solo`, `preview_track_gain`, `commit_track_gain` and
`cancel_track_gain`, all acceptance-only. `NATIVE_ABI.md` lists signatures and the
new view fields.

## 11. Avalonia integration

- Audio track headers have Mute, Solo, a volume slider and the engine's gain
  text. Each control's automation name and tooltip follow its state.
- The slider sends previews during a drag and one commit at the end. It is not
  the source of truth: the displayed value comes from Rust.
- An inaudible lane keeps its clips and waveform at half opacity.
- The transport shows CLIP when the mix clipped.
- The inspector has Unlink and Link buttons, and the per-stream Place list.
- The library has an Audio picker filter and a music-note tile.
- Frames and mixer state stay outside view models that do not need them. The
  view diff compares stream lists by value, so playback does not re-apply the
  library each frame.
- `docs/DESIGN.md` did not exist although the task named it. It now records the
  real tokens and layout rules.

## 12. Automated test results

Every figure below comes from a run on the final tree.

| Gate | Result |
|---|---|
| `scripts/verify.ps1` (fmt, clippy, tests, Debug and Release builds, .NET tests, self-contained win-x64 publish) | All 9 stages passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Clean |
| `cargo test --workspace --all-features --locked` | **220 passed, 0 failed, 28 ignored** (baseline 169 and 24) |
| .NET Interop tests | 57 passed (baseline 52), with and without an audio device |
| Avalonia desktop tests | 63 passed (baseline 57), with and without an audio device |
| Real FFmpeg suite (ignored by default, run with `--ignored`) | 19 passed: 12 FFmpeg, 6 audio, 1 media |
| Real sources mixed against an independent decode | Passed |
| Real WASAPI output with loopback | Passed |
| Endpoint chain characterization | Passed (§13) |
| `git diff --check`, LF endings, `Cargo.lock` | Clean, no new dependency |

By crate: contracts 6, core 15 + 7, engine 169, media 23. After a race was fixed
(below), the engine suite ran 60 times with no failure, and 12 more with no audio
device.

**The mix is checked against independent arithmetic, not internal counters.**
`audio_mixer_tests.rs` gives each stream a sample formula. Each test recomputes
the expected output itself and compares it sample by sample:

- two and three tracks in project order, with independent gaps;
- amplitude gain at unity, cut and boost;
- every mute and solo combination;
- hard clipping, with its count and without normalization;
- the 10 ms gain ramp, block by block;
- a muted mix advancing the clock in silence;
- a warm decoder rejoining at the exact sample;
- a cut on one track not disturbing another;
- starvation keeping all tracks aligned;
- no stale PCM after a seek during a multi-track epoch;
- both budgets and the explicit over-limit error;
- closing during decoding joining every decoder.

`real_sources_at_different_rates_layouts_and_starts_mix_like_independent_pcm`
mixes real files at different rates, layouts and start times and compares the
result with an FFmpeg decode made separately.

**Other coverage.** Schema 0 to 4 and 999, refused documents left untouched, and
pre-4B metadata. Unlink, Link, independent edits, stream bounds and undo. Audio-only
decoding for each format against an independent decode. Volume drags, repeated solo
toggles, replacing the project during a mix, and missing-then-relinked audio-only
media. A timeline holding only audio, audio that outlasts the picture, and device
failure.

**Defects found during validation, all fixed.**

| Found by | Defect | Fix |
|---|---|---|
| Baseline run | A Phase 4B test raced the end of the epoch. | `626004d`, separately, first. |
| Audit | Projects saved before Phase 4B would report sources with audio as changed. | Saved metadata without stream details matches on the rest. |
| Native bench | A timeline holding only audio had zero length, so Play was disabled, and audio past the picture was unreachable. | Length includes the last audio clip. |
| Native bench | After placing a stream, clicking the same source's tile did nothing, so a second stream could not be placed. | The click returns the inspector to the source, without reloading the monitor. |
| Repeated runs | A test consumed more audio than it had waited for (4 failures in 25 runs). | It waits for all of it. 60 clean runs. |

## 13. Real-device loopback results

Windows 11, "Speakers (Realtek(R) Audio)" in shared mode, 48 kHz, captured by a
WASAPI loopback tap. The app was driven through real pointer and keyboard input,
with CutCap verified as the foreground window before every input. Sources were
generated test tones; no user media was used. Full logs:
`docs/measurements/phase4c-native-scenario.txt` and `phase4c-native-results.json`.

**Mix content.** Each tone is measured independently (10 ms windows), so the
sum can be told apart per track.

| State | 400 Hz (Audio 1) | 800 Hz (Audio 2) | 1200 Hz (alternate stream) |
|---|---|---|---|
| Both playing | 0.22604 | 0.17884 | absent |
| Audio 2 at −11.5 dB | 0.23215 | 0.05500 | absent |
| Audio 1 muted | 0 | 0.06043 | absent |
| Audio 1 muted and soloed (mute wins, Audio 2 silenced) | 0 | 0 | absent |
| Audio 1 soloed | 0.23105 | 0.00001 | absent |
| Restored | 0.20374 | 0.16720 | absent |
| Three streams mixed | 0.23370 | 0.15965 | 0.12496 |

**Reading the levels.** Absolute levels and small differences at this point in
the chain are not the engine's numbers. The tap sits after the Realtek endpoint's
own processing, which is not linear: `the_endpoint_chain_is_characterized_by_known_tones` writes known sines straight to the device, bypassing the mixer, and reads them back between +0.05 and +4.00 dB depending on frequency, level and what else is playing (`phase4c-endpoint-chain.txt`). So a −11.5 dB change measured
−10.24 dB here. The exact gain arithmetic is proven by the PCM oracle tests in
§12, not by this tap. What the tap proves is that real audio left the app through
the real output with the right content: a muted track at 0, a soloed mix without
the other track, a third stream present after placement.

**Workflow, every step through the UI.**

| Check | Result |
|---|---|
| Audio-only import, inspector and Audio badge | Pass (PCM WAV, stream listed) |
| Linked pair, audio-only placement, waveforms from real peaks | Pass |
| Volume drag undo | One entry: "set Audio 2 volume to −11.5 dB" |
| Controls during playback | Playback state unchanged by any control |
| Unlink | One undo step, button state follows, Link offered |
| Independent move, trim, split, with undo and redo | Pass |
| Alternate stream placed explicitly | Pass, with its own waveform |
| Save and reopen | Schema 3 written (`gain_cdb: -550`, `mute: true`) and restored; mix within 0.03 dB; file unchanged by opening |
| Schema 1 project (Phase 3D) | Opens, plays, file unchanged |
| Schema 2 project (real file made by the Phase 4B build) | Opens, plays with sound (rms 0.157), file unchanged |
| Close while decoding, 4 FFmpeg processes running | 663 ms, 0 processes left, output silent |

**Timing, from press to loopback tap (Debug build):** mute 289 ms, unmute 300 ms,
solo 301 ms, first audio 683 ms. Release figures are in §14.

**Phase 4B regression** (`phase4c-phase4b-regression.txt`): the Phase 4B native
workflow still passes against the mixer. Click to engine clock median 50.8 ms,
resume 71.2 ms, correct clicks across a cut, none during a gap or pause, relink,
schema 1, and close during analysis all pass.

**960×600 DIP** (`phase4c-960x600.txt`): Play, Split, Delete, Unlink, Add audio
track, Zoom, Mute, Solo and the volume slider are all reachable. Long hints give
way with an ellipsis and the track header keeps its width.

**What this does not establish.** A loopback tap is inside the Windows audio
path. It does not include the DAC, amplifier, speakers or room, so no acoustic or
speaker-level timing is claimed. The A/V figures compare the tap with a screen
capture, which has its own compositor and capture delay.

## 14. Performance and resource measurements

Release desktop app over the release engine, one Windows machine (Intel Core
Ultra 7 255H, 16 logical cores). Each run: the 180 s 1080p sync source as a linked
pair plus N−1 independent AAC tone tracks, one FFmpeg decoder each, driven through
the real UI. Full data: `docs/measurements/phase4c-mix-release.json`.

| Tracks mixed | 1 | 2 | 4 | 8 | 16 (limit) |
|---|---|---|---|---|---|
| Decoders peak / mixed peak | 1 / 1 | 2 / 2 | 4 / 4 | 8 / 8 | 16 / 16 |
| FFmpeg processes (with picture) | 2 | 3 | 5 | 9 | 17 |
| Underruns / late frames | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| Mix cost, % of real time | 0.070 | 0.113 | 0.132 | 0.314 | 0.729 |
| Mix block mean / max, µs | 7 / 536 | 11 / 259 | 13 / 378 | 31 / 491 | 73 / 1099 |
| Render callback max, µs | 156 | 230 | 118 | 260 | 70 |
| Output occupancy, median ms | 150 | 150 | 150 | 150 | 140 |
| Start: Play to first audio, ms | 364 | 376 | 398 | 396 | 460 |
| Seek to audio, ms | 438 | 468 | 431 | 500 | 563 |
| Solo response, ms | n/a | n/a | 285 | 307 | 289 |
| Volume response, ms | 253 | 243 | 240 | 252 | 231 |
| Mute response, ms | 289 | 292 | 291 | 297 | 290 |
| Click to engine clock, median ms | 51.2 | 59.1 | 65.1 | 64.8 | 62.8 |
| Click to screen flash, median ms | 31.9 | 31.8 | 41.1 | 39.5 | 33.6 |
| Picture frames copied per s | 30.0 | 29.9 | 29.9 | 30.0 | 29.9 |
| App CPU, cores | 0.59 | 0.68 | 0.75 | 0.72 | 0.73 |
| Peak working set, MiB | 407 | 407 | 427 | 440 | 485 |
| Lane draw, median / p95, µs | 235 / 397 | 390 / 585 | 624 / 882 | 607 / 826 | 613 / 817 |
| Close, ms after the prompt | 421 | 411 | 411 | 411 | 409 |

Reading the table:
- **Control response** is press to the midpoint of the level change, held 100 ms,
  on the loopback clock. The 1-track column is the tone-only timeline. Solo needs
  at least one other tone to silence, so it starts at 4 tracks here (the two-track
  value, 301 ms, is in §13).
- **Responses do not grow with track count.** They are set by the mix-ahead
  (150 ms) plus the device buffer and endpoint path, not by the mix.
- **Clipped samples** rose with track count (162 to 608 over a run) because the
  sync source's click decodes above full scale (peak 1.6), so it hard-clips even
  alone. The counter and the CLIP indicator reported this. The tones themselves sum
  well below full scale.
- **Lane draw plateaus from 4 tracks** because only the visible lanes are drawn.
- Picture delivery held 30 fps at every count, which is the responsiveness measure
  available during playback. No hang or dropped input occurred in any run.

**Over the limit (17 tracks).** Play was refused with "17 audible audio tracks
overlap at 00:01.500. CutCap mixes at most 16 at once: mute some tracks or shorten
clips." No FFmpeg process was started.

**Long session (60 s, 4 tracks).** 60 clicks, 0 underruns, 0 late frames. Engine
clock against system clock, 9.5 ppm. Click-to-clock slope −0.039 ms/min, medians
65.0 ms (first 10) and 65.1 ms (last 10), so no drift. Peak working set 424 MiB,
0 processes left after close.

**Compared with Phase 4B one-track (Release).**

| | Phase 4B | Phase 4C, 1 track | Phase 4C, 16 tracks |
|---|---|---|---|
| Output occupancy, median | 400 ms | 150 ms | 140 ms |
| Click to engine clock, median | 51–69 ms | 51 ms | 63 ms |
| First Play to audio | 337–569 ms | 364 ms | 460 ms |
| Long session slope | 0.006 ms/min | not rerun at 1 track | −0.039 ms/min (4 tracks) |
| Peak working set | about 400 MiB | 407 MiB | 485 MiB |

The cut from 400 ms to 150 ms of queued mix is the deliberate change that makes
controls usable during playback. No one-track regression was measured. These
results hold for this machine and this endpoint; no general real-time guarantee is
made, and other audio devices or slower disks may behave differently.

## 15. Known limitations

- **Limits.** 16 audible overlapping tracks and 24 decoders. Sixteen tracks were
  measured with AAC tones from one local file. Sixteen heavier codecs, or slow or
  remote storage, were not tested.
- **Control latency is about 290 ms** from press to tap on this endpoint. This is
  fine for mixing by ear but it is not a live-monitoring path.
- **Hard clipping only.** There is no limiter or normalization by design. An
  overloaded mix distorts, and CLIP reports it.
- **Whole-track gain only.** No clip gain, fades, crossfades or automation, so a
  cut between clips can click if the audio itself is discontinuous there.
- **No output-latency calibration UI.** The default compensation is 0, and the
  delay after the engine (51–65 ms here) is not compensated unless
  `CUTCAP_AUDIO_LATENCY_MS` is set.
- **One default device, shared mode.** No device picker and no hot-switch. A
  device failure stops playback and the next Play uses the system clock.
- **Tracks made audible past the decoder budget** join late and are reported.
- **Audio-only formats are an allowlist** (§5). Others remain unsupported.
- **The Source monitor does not play audio-only files.** They are heard on the
  timeline.
- **No disk cache** for waveforms or filmstrips. They are recomputed per session.
- **Layouts above stereo** use FFmpeg's default downmix, and mono uses its −3 dB
  pan law. Neither is configurable.
- **Verified on one Windows machine and endpoint.** Levels at the loopback tap
  include that endpoint's non-linear processing, and speaker-level timing was not
  measured.
- **FFmpeg stays external** (PATH), as before.

## 16. Git commit hashes

Recorded by the follow-up commit that also records the pull request.

## 17. PR URL and number

Recorded by the same follow-up commit.

## 18. Recommended next milestone

**Output-latency calibration, then Phase 5 export.** A guided loopback/click
calibration that stores a per-device offset closes the last gap in the sync
story without hardcoding one machine's value. Export should follow, reusing this
mixer's exact sample mapping offline so preview and export agree. Clip gain and
fades would fit before or alongside export, but neither is needed for it.

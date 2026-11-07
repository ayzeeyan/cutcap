# Phase 4B — Real audio playback, A/V synchronization and timeline waveforms

Status: implemented and verified on one Windows machine. Not merged. ABI and
contract **9**, core project schema **2**, project envelope `format_version`
still **1**. ADR 025 records the decisions.

## 1. Starting baseline

- **Branch.** `claude/phase-4b-audio-waveforms`, created from verified `origin/main`
  `0a2708c` (the Phase 4A merge, PR #4).
- **What 4A left.** ABI/contract 8: real video filmstrips, video-only timeline
  playback on a system clock, schema 1. Audio tracks existed as empty lanes.
  Probing recorded only the *indexes* of audio streams.
- **Baseline gates.** `verify.ps1` was green at `0a2708c`: fmt, clippy, the
  Rust workspace tests, Debug/Release builds, both .NET test projects and the
  publish.
- **Test machine.** Windows 11 (10.0.26300), Intel Core Ultra 7 255H (16
  logical cores), 150% scaling, default output "Speakers (Realtek(R) Audio)" at
  48 kHz, FFmpeg/FFprobe 9.0.1 on PATH (external, not bundled).

## 2. Model limitations before this phase

- A clip had no stream identity. It implicitly meant "the probed video stream".
  There was no way to say "audio stream 1 of this file".
- There was no relationship between clips. Moving a picture could never move
  its sound.
- Media metadata kept `audio_streams: [index]` without codec, rate, channels,
  layout, timebase or start.
- Timeline playback ran from a monotonic system clock. Nothing produced sound.
- Schema 1 files therefore cannot express audio clips. Migration has to infer
  only what schema 1 can mean, and must refuse anything else.

## 3. Audio metadata

`probe.rs::audio_details` reads every audio stream FFprobe reports and stores an
`AudioStreamMetadata` per stream in `MediaMetadata.audio`:
- stream index, codec, sample rate, channels, channel layout, sample format;
- its own timebase, start PTS, duration ticks;
- the default-disposition flag.

Untrusted values are validated:
- rate 1..=384 000 Hz;
- channels 1..=32;
- a positive timebase;
- each detail entry must name an index listed in `audio_streams`;
- at most 64 streams.

The **primary** audio stream is the default-disposition stream, otherwise the
lowest index. Metadata saved by Phase 4A (no `audio` array) still deserializes
(`metadata_saved_before_audio_details_still_deserializes`). The inspector shows
an "Audio" section per stream, for example "Stream #1 (timeline): AAC · 48000 Hz ·
stereo · fltp". Audio-only files remain unsupported for import, as before
(`Unsupported`).

## 4. Schema and migration

**Schema 2.** Each clip now carries:
- `stream`: `{"kind":"video"}` or `{"kind":"audio","index":N}`;
- `link` (optional): a `LinkId`.

Validation:
- The stream must match the track kind and the media's streams (`KindMismatch`).
- A link group must be exactly one video clip and one audio clip with the same
  media, start, duration and `source_in` (`InvalidLink`).
- `Project::replace_clips` validates every touched link group at the end of the
  transaction, so no edit can leave half a pair.

**Migration (schema 1 to 2, in memory).**
- Every schema-1 clip becomes a video clip.
- A schema-1 clip on an audio track, or a schema-1 document that already
  contains `stream`/`link` fields, is refused with `ProjectError::Migration`.
  Nothing is guessed.
- Opening never writes. The file is rewritten as schema 2 only when the user
  saves.
- A refused migration leaves the file byte-for-byte untouched
  (`schema_one_files_open_migrated_and_refused_migrations_leave_the_file_untouched`).
- Natively, a real schema-1 file saved by the Phase 3D build opened, played and
  kept the same SHA-256 (§12, step 19).
- Schemas 0, 3 and 999 are rejected; 1 and 2 are accepted.

## 5. Linked editing

- **Placement.** Placing a source with audio adds its **primary** audio stream
  as a linked clip on the first audio track that is free for the whole
  interval. Video and audio go in as one transaction and one undo step. If no
  audio track is free, the whole placement is rejected with a reason. Track
  creation is not part of history, so placement never creates a track.
  Video-only sources are unchanged.
- **Move, trim, split, remove.** Any of these, started from either member, is
  applied to both in one transaction and one undo step.
  - Moves keep both members aligned. A blocked partner rejects the whole move
    atomically.
  - Trims change both members on the shared source grid.
  - Split produces two exact pairs with fresh `LinkId`s.
  - Removing either member removes both.
- **Undo/redo.** Undo/redo restores identities, order, links and exact ticks.
- **Joint source grid.** The audio member stores the *same* `source_in` as the
  video member (in the video stream's clock). Audio maps it to samples with one
  ceiling rounding:
  `offset = ceil((source_in + (audio_start − video_start)) × rate)`.
  Output sample `k` of a clip is source sample `k + offset`, so trims and splits
  never accumulate rounding (`audio_sample_mapping_rounds_once_and_agrees_with_the_video_instant`).
- **Clip rows** add:
  - `stream`, `audio_stream`, `link_id`;
  - `audio_key` (source identity plus stream: relinking changes it);
  - `audio_rate`, and `source_in_samples`/`source_end_samples`, mapped exactly
    as playback maps them.

## 6. Decoding

`cutcap-media/src/audio.rs` runs one FFmpeg process per audio decoder:

```
ffmpeg -copyts -noaccurate_seek -seek_timestamp 1 -ss <start − 0.5 s> -map 0:<idx>
  -af aresample=R:resampler=swr,aformat=flt:stereo -c:a pcm_f32le
  -stats_mux_pre pipe:2 -stats_mux_pre_fmt "TAG {n} {pts} {tb} {size}" -f f32le pipe:1
```

- **Real PTS.** Packet PTS from the mux stats equals the source sample index at
  the output rate. The decoder discards samples before the requested sample and
  hands over exact `[first_sample, …)` chunks.
- **Mixing to stereo.** Mono reaches stereo through FFmpeg's −3 dB pan law.
- **Bounded and cancellable.** Bounded chunk queue, cancellation that kills and
  reaps the child, and the same job-object limits as video.
- **Waveform analysis.** A separate path at native rate and channel count, one
  thread, below-normal priority, at most 4 chunks per process:
  - 1 chunk = 1024 buckets × 256 frames = 262 144 frames;
  - min/max per bucket over all channels;
  - `min > max` marks an empty bucket.
- **Integration tests**, verified against independently decoded PCM:
  - `playback_pcm_starts_at_the_requested_source_sample_with_independent_values`;
  - `resampled_and_offset_streams_keep_the_source_clock_in_output_samples`;
  - `peaks_equal_independently_decoded_pcm_for_silence_constants_impulses_and_channels`;
  - `aac_sources_decode_and_cancellation_reaps_the_decoder`.

## 7. Windows output

`ffi/audio_output.rs` uses WASAPI through `wasapi 0.27.0`:
- shared mode, event-driven, autoconvert;
- float stereo at the endpoint mix rate (48 kHz here);
- a 400 ms ring and 120 ms prefill.

The render thread owns every COM object, never waits on FFmpeg or the
filesystem, and allocates nothing per period after start-up.

| Operation | Behaviour |
|---|---|
| Pause | `stop_stream`; buffered audio is kept |
| Reset (seek, stop, edits) | stop plus `reset_stream`, ring cleared, new epoch |
| No endpoint, or `CUTCAP_AUDIO=off` | system clock, with an honest note in the player |
| Device failure | playback stops with an error; the next Play continues on the system clock without sound |

`ffi/audio_transport.rs` feeds the monitored audio track:
- One decoder per clip span. Gaps and unavailable sources are written as real
  silence.
- Each output frame carries its exact source sample across cuts, gaps and holes
  (`every_output_frame_carries_its_exact_source_sample_across_cuts_gaps_and_holes`).

**Proof of real output.** Audible output is shown by recording the endpoint,
never by "a buffer was created":
- The ignored device test plays a quiet 1 s tone and records it by WASAPI
  loopback: 1003 ms of 440 Hz heard, tone ratio 0.995, 0 underruns.
- The native runs record clicks from the app (§12).

## 8. Clock semantics

- **Audio master.** With an audio device, the **timeline clock is the device's
  playback position**:
  - `IAudioClock` is sampled on the render thread after each write;
  - it is extrapolated by at most 40 ms and never past what was written;
  - it is mapped through a `ClockMap` of device-frame segments that separates
    real timeline frames from padding.
- **Starvation.** Timeline time advances only while real timeline audio plays.
  When the output starves (pads), the clock stops and an underrun is counted;
  wall time alone never advances playback.
- **Start.** Playback waits for both the first video frame and the audio
  prefill.
- **Late video.** A late video frame increments `video_late`; it never stops
  the audio master.
- **Pause.** The playhead freezes at the clock value at the moment of pause.
  - A real endpoint still plays the period it already holds. The native
    interop test caught this moving a paused playhead by about 3 ms.
  - While paused, the audio master therefore reports the frozen position.
  - On resume the clock again equals what the device played.
- **Seek, stop, edits, media replacement, open.** These cancel the audio epoch
  (flush, new decoders).
- **End.** The end of the plan stops the device; replay starts a new epoch.
- **Monitored track.** Only the first audio track in project order is
  monitored. There is no mixing (§14).

**Unreported device latency (measured).** WASAPI's position already subtracts
the latency the driver reports: on this device the position trails the
engine's read point by about 940 frames (19.6 ms). `GetStreamLatency` reports
0 ms.
- **What loopback shows.** Every packet is timestamped with the performance
  counter, mapped to Unix time through an `IAudioClock` reading bracketed
  within 1 µs. On that evidence, the endpoint mix carries each click
  **51–69 ms after** the clock says it plays.
- **Spread.** The value differs per stream start and is stable within a run:
  ±0.5 ms over 60 s.
- **Cause.** The delay is added by the device's processing chain after the
  engine. No API reports it.
- **Setting.** It is therefore an explicit, measured setting:
  `CUTCAP_AUDIO_LATENCY_MS` (0–500, default **0**) holds the clock back by that
  much, which delays the picture.
- **Measured effect.** With 67 ms on this machine, click − clock went from
  +66.6 to −12.2 ms, and click − flash from +35 to −31 ms (§13). The default
  stays 0 because the value belongs to the device, not to CutCap.

## 9. Waveforms

- **Engine.** `ffi/waveforms.rs` mirrors the thumbnail service:
  - one low-priority worker; demand replaces demand (≤128 chunks);
  - consecutive chunks of one stream are batched (≤4 per process);
  - an unwanted in-flight batch is cancelled;
  - results wait for at most 32 entries;
  - a project change clears the service.
  - During playback, analysis is throttled to 1 chunk per process with 200 ms
    pauses.
  - Results carry the `audio_key`; a key that is no longer current (relink,
    replacement) is dropped.
- **ABI 9.** Request, announce and copy (§11, NATIVE_ABI.md):
  - Peaks are copied as `buckets` interleaved `(min, max)` floats.
  - A token copies once; a second copy is stale.
- **C# cache.** `WaveformCache` is an LRU by bytes: 16 MiB by default
  (`CUTCAP_WAVEFORM_CACHE_MB`), 8 KiB per chunk, keyed by `(audio_key, chunk)`.
  - Tokens stay stable while pending.
  - Results nobody demands any more are dropped.
  - Failures are remembered as empty chunks.
  - A project replacement voids pending tokens.
- **Planning.** `TimelineLanes.PlanWaveforms` requests visible chunks first,
  left to right, then a small overscan, skipping cached ones, up to 64 per
  plan. It maps pixels to the clip's exact sample interval from Rust.
- **Drawing.** Render only draws cached peaks; no PCM is touched in Render.
- **Project state.** Waveforms never enter the Project, history or saved files.
  Analysis leaves revision and Dirty unchanged
  (`RealWaveformPeaksMatchIndependentPcmAndNeverDirtyTheDocument`).

## 10. UI

**Audio lane identity.** Audio clips are navy (`#26487F`) with a blue border
(`#6E9BE0`). Peaks are drawn in light blue (`#A9CBFF`); chunks still pending get
a faint baseline (`#4D7FB8F0`), never fake peaks. There is no purple or violet.

**Linked pairs.**
- Selecting either member outlines both in cyan (`#B322D9E6`).
- A move or trim preview shows the partner's ghost too.
- Names and the time range stay readable on audio clips.

**Player status.** The detail line names the clock honestly, for example
"video and audio · audio clock", or the reason audio is unavailable.

**Small windows.** Usable at 960×600 DIP (1440×900 physical at 150%): every
transport and editing control is reachable, and the waveform and outlines are
visible.

Screenshots (`.artifacts/av/…`, not committed: they show generated test media
only):
- the linked pair on V1/A1 with real waveform spikes and the 5 s ramp;
- split, trimmed and moved pairs;
- missing pairs drawn as "Missing" on both members;
- the relinked state;
- the old project;
- 960×600 playing.

## 11. Tests

**Totals**, all executed this phase:

| Suite | Result |
|---|---|
| Rust workspace (`cargo test --workspace`) | 168 passed, 0 failed |
| FFmpeg integration (`cutcap-media -- --ignored`) | 12 video + 4 audio passed |
| Real-device output test (`audio_output -- --ignored`) | passed |
| .NET Desktop tests | 57 passed |
| .NET Interop tests | 52 passed, against real FFmpeg and the real engine DLL |

The Rust workspace total breaks down as: contracts 6, core 11 + 7, engine 124
(7 ignored measurements or fixtures), media 20.

**New or changed coverage:**
- **Core:**
  - migration, link and stream-kind invariants;
  - group-atomic insert/edit/remove;
  - single-rounding sample mapping.
- **Media:** audio metadata parsing and limits; peak folding; the 4 audio
  integration tests above.
- **Engine:**
  - 7 linked-edit tests;
  - `ClockMap` and virtual-device output;
  - latency compensation;
  - 4 transport tests (exact sample per output frame across cuts/gaps/holes,
    starvation, unavailable sources, seek epochs);
  - 3 A/V playback tests (audio master, late video, the end stops the device,
    the paused in-flight period);
  - 3 waveform service tests (validation, batching, throttling, cancellation,
    stale drop);
  - ABI 9 round trip;
  - 2 persistence tests (linked pairs round-trip; schema-1 open/refusal leaves
    the file untouched).
- **Interop** (`AudioWaveformTests`):
  - linked placement and its stream identity;
  - move/trim/split/remove/undo through the ABI;
  - **waveform peaks equal min/max from a separate FFmpeg PCM decode**, bucket
    by bucket, and leave the document clean;
  - an honest audio view.
- **Desktop** (`WaveformTests`):
  - cache LRU, budget and token policy;
  - visible-first planning, cache skipping and scrolling;
  - trimmed clips only request their own chunks.

## 12. Native audio verification

`scripts/native-ui/av-ui.ps1` drives the real Debug app with real mouse and
keyboard input, sent only while CutCap owns the foreground:
- Source: a generated 20 s 1280×720 H.264 + 48 kHz AAC file with a white flash
  on the first frame of every second and a 10 ms 2 kHz click at every second.
- **Sound** is verified by `cutcap-audio-loopback`, a WASAPI loopback recorder
  (feature `audio-diagnostics`) that dates every packet by its
  performance-counter timestamp.
- **Picture** is verified by sampling the screen.

Final run (`docs/measurements/phase4b-native-scenario.txt`):

1. **Import.** The inspector lists "AAC · 48000 Hz · stereo · fltp".
2. **Drag onto Video 1.** Real OLE drag; 2 clips appear.
3. **Linked audio on Audio 1.**
4. **Real waveform.** 1712 light-blue peak pixels over navy.
5. **Play.** The loopback hears the clicks: rms 0.166, 7 clicks.
6. **Audible output.** The clicks are recorded at the endpoint: this is actual
   sound output.
7. **Pause and resume.**
   - The playhead stays at 00:05.739 for 0.7 s.
   - No click is heard during the pause.
   - Resume latency is 80 ms.
8. **Seek to 6 s.** Playback restarts with 107 ms start latency.
9. **Split at 10 s (Ctrl+B).** 4 clips.
10. **Trim the right pair's start to 11 s from the audio member.**
11. **Move the pair to 13 s by its video member.**
12. **Undo/redo the move.** Both members follow each time.
13. **Play across the cut from 8 s.** Clicks arrive at +1093 and +2089 ms (8 s,
    9 s), then nothing for the 10–13 s gap, then +6089, +7089, +8089 ms (source
    11 s, 12 s, 13 s at timeline 13, 14, 15 s): the gap is exact to ±4 ms.
14. **Sync across the cut.** Click − flash median 50 ms (start of a
    window, uncompensated; §13 has the steady values).
15. **Save, close and reopen.** The file holds two links with exact ticks
    (`video@9360000+6480000/in168960`), identical after reopen.
16. **Waveforms regenerate** after reopen (763 px), and reopened playback is
    audible (3 clicks).
17. **Missing media.** The source was renamed; all four clips show "Missing"
    and play silence.
18. **Relink.** The waveform is back (763 px).
19. **Phase 3D schema-1 project** (genuine file, two video clips): opens, plays,
    and its SHA-256 is unchanged by opening.
20. **Close during playback and waveform analysis.** Exits in 651 ms.
21. **No orphans.** 0 CutCap/FFmpeg/FFprobe processes remain; the endpoint is
    silent afterwards (rms 0), so the device stream was released.

The same scenario with `CUTCAP_AUDIO_LATENCY_MS=67` also passed
(`phase4b-native-scenario-compensated-67ms.txt`).

## 13. Drift and performance

`scripts/native-ui/av-bench-ui.ps1` produced, per source:
- placement;
- three 4 s playback windows from non-whole positions;
- a 60 s run on a 180 s 1080p source;
- a 24-clip timeline.

Files: Release in `docs/measurements/phase4b-av-release.json`, Debug in
`phase4b-av-debug.json`.

**Release build, uncompensated (default):**

| Source | Start (cold / warm) | Click − engine clock | Click − screen flash | Frames/s | Late | Underruns | App CPU | FFmpeg CPU |
|---|---|---|---|---|---|---|---|---|
| 720p30 | 345 / 103–113 ms | 51–67 ms | 26–41 ms | 29.7 | 0 | 0 | 0.46–0.72 cores | ≈0.25 cores |
| 1080p30 | 353 / 115 ms | 59–67 ms | 30–42 ms | 29.6–30.2 | 0 | 0 | 0.53–0.66 | ≈0.4 |
| 4K30 | 569 / 108–115 ms | 55–63 ms | 24–32 ms | 29.9–30.0 | 0 | 0 | 0.44–0.61 | ≈0.85 |
| 720p VFR (avg ≈23.4 fps) | 337 / 106–110 ms | 61–69 ms | 38 ms | 23.3–23.5 (source rate) | 0 | 0 | 0.41–0.62 | ≈0.25 |

How to read the table:
- **Click − engine clock** is when the endpoint mix carries a click, minus when
  the engine's clock said that sample plays (window medians). Within a window
  the spread is about ±0.5 ms.
- **Click − screen flash** is when the click reaches the endpoint, minus when a
  GDI capture first sees the flash (16.6 ms sample interval, window medians).
  Positive means sound after picture, at the loopback tap: the DAC adds more
  after that. So with the default setting, audio is late by roughly 25–40 ms
  plus the DAC.
- **Picture after clock.** The picture appears about 20–30 ms after its clock
  time (render, compositor, capture).

**Drift, 60 s of 1080p (Release):**
- 60 of 60 clicks; device vs system clock 1.3 ppm.
- **Click − engine clock** 67.3 ms median (66.5–67.8), slope **0.006 ms/min**:
  no drift between the audio clock and real output.
- **Click − flash** 38.0 ms median (35.7–40.0), slope 3.3 ms/min. Both runs
  (Debug 3.4 ms/min) show this. The clock itself does not drift, so this
  is the screen-presentation phase. It is consistent with the display refresh
  running about 55 ppm off the system clock, which would make it a sawtooth
  bounded by one refresh interval (16.7 ms). That interpretation was not proven
  with a longer run.
- 0 underruns, 0 late frames, buffer 390–400 ms, 0.51 app cores, 2 FFmpeg
  processes.

**Many clips** (24 clips, 12 linked pairs, Release): playing across the cut at
20 s gives clicks 999.89–1000.08 ms apart (a seamless cut), 0 underruns, 0 late
frames.

**Waveforms:**
- Engine analysis of a 20 s source (4 chunks, one batch) takes 88–101 ms in
  Release (154–265 ms Debug).
- The waveform is visible within 2.3 s of pressing "Add to timeline" as
  measured through UI Automation. That is an upper bound: the automation steps
  themselves take about 1.5 s.
- The cache had 2944 hits against 4 misses per source and holds 32 KiB.

**Render cost** (lanes record time with waveforms, Release):
- median 127–174 µs, p95 247–319 µs, with 198–398 waveform columns;
- 24 clips: median 342 µs, p95 498 µs;
- single outliers up to 14 ms on the first render or a GC.

**Memory and shutdown:**
- Working set 386–408 MiB (Avalonia/Skia, frames, filmstrips).
- Exit about 410 ms after the discard prompt; 0 leftover processes everywhere.

**Debug** numbers are in the Debug file: cold start 720–905 ms, about 0.8–1.0
app cores, otherwise the same sync behaviour.

## 14. Limitations

- **Single-track audio.** Only the first audio track is monitored. There is no
  mixing of several audio tracks, and no volume, pan, mute or solo (the lane
  speaker icon is still decorative).
- **Primary stream only.** Placement links only the primary stream. Other
  streams cannot be placed, and pairs cannot be unlinked or edited apart.
  Audio-only files cannot be imported.
- **Device latency is not measured at runtime.** The delay after the engine
  (51–69 ms here) is unreported by Windows, and CutCap does not compensate it
  by default. `CUTCAP_AUDIO_LATENCY_MS` exists, but there is no UI calibration
  yet.
- **Sync measured on one machine.** The loopback tap excludes DAC and speaker
  latency, and the screen capture includes compositor and capture delay; the
  physical acoustic-to-photon offset was not measured. The per-start offset
  varies by about 18 ms.
- **Cold start.** The first Play after launch takes 337–569 ms (Release) while
  FFmpeg loads.
- **Shared-mode output only.** No exclusive mode, no device selection UI, and
  no hot-switch to a new default device (a failure stops playback; the next
  Play falls back to the system clock).
- **No disk cache.** Waveforms are recomputed each session.
- **FFmpeg stays external** (PATH), as before.
- **Player header wording.** The player header still reads "single video
  track". That is true of the picture; the status line names the audio clock.

## 15. Dependencies and licensing

New Rust packages:
- `wasapi 0.27.0` (MIT, Windows only), the WASAPI bindings;
- `num-integer 0.1.47` (MIT OR Apache-2.0), pulled in by `wasapi`.

`wasapi` also uses `windows`/`windows-core 0.62.2` (MIT OR Apache-2.0), which
were already in the lock, and `log` (MIT OR Apache-2.0). `tempfile` (MIT OR
Apache-2.0) is a media dev-dependency only.

FFmpeg stays an external, user-installed tool that is **not bundled**. Decoding
audio through it changes nothing in ADR 006/008. Subprocess isolation is not
treated as resolving distribution obligations. No application-wide source
licence has been chosen on the owner's behalf. LICENSE_REVIEW.md is updated.

## 16. Commits

Branch `claude/phase-4b-audio-waveforms` from `origin/main` `0a2708c`:

| Commit | Subject |
|---|---|
| `2d56dae` | feat(audio): add linked audio clips, PCM decoding and audio-master playback |
| `5f63459` | feat(ui): draw real waveforms and linked audio clips in the timeline |
| `1ee2737` | test(audio): verify audible output, A/V offset and drift natively |
| `ba0ec14` | docs: report Phase 4B audio, synchronization and waveforms |
| (this commit) | docs: record the Phase 4B pull request |

**Clean-worktree checks.** Each of the first three commits was checked out in a
clean worktree and checked with:
- clippy `--workspace --all-targets -D warnings`;
- `cargo test --workspace`;
- the .NET solution build;
- `dotnet test`.

Results:

| Commit | Clippy | `cargo test` | .NET build | `dotnet test` |
|---|---|---|---|---|
| `2d56dae` | clean | 168 passed | 0 errors | Desktop 54, Interop 52 passed |
| `5f63459` | clean | 168 passed | 0 errors | Desktop 57, Interop 52 passed |
| `1ee2737` | clean | 168 passed | 0 errors | Desktop 57, Interop 52 passed |

The engine and interop changes share one commit: the ABI bump and the
interop tests that pin it must travel together. The loopback tool's `[[bin]]`
entry moved with the tool into the test commit.

**Excluded from the commits:**
- generated media, recordings (WAV/JSON) and screenshots (`.artifacts/`,
  git-ignored);
- build outputs and the publish folder;
- caches.

Measurement logs were copied into `docs/measurements` after checking that they
contain no user paths.

## 17. Pull request

- **PR:** [#5](https://github.com/ayzeeyan/cutcap/pull/5) "feat: add synchronized
  audio playback and real timeline waveforms (Phase 4B)": base `main`, head
  `claude/phase-4b-audio-waveforms`.
- **State at creation:** open, not a draft, auto-merge off, head `ba0ec14`.
- **Push:** `git push -u` without force; the remote head was verified equal to
  the local one, and `origin/main` is an ancestor.
- **For the owner:** left open for review. Neither `main` nor the feature branch
  is deleted.

## 18. Next milestone

**Phase 4C — audio mixing and track controls.** Mix all audio tracks with
per-track mute, solo and volume (lane header controls become real), and allow
unlinking and placing non-primary streams.

That should be followed by an output-latency calibration UI: a guided
loopback/click measurement that stores the per-device offset this phase had
to measure by hand.

Measure first, as in this phase. Disk caches for waveforms and filmstrips
remain optional follow-ups.

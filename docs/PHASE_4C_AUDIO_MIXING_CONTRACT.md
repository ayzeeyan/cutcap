# Phase 4C contract — multi-track mixing, track controls, independent audio

Binding for the Phase 4C implementation and its reviewers. Versions after this
phase: ABI/contract **10**, core schema **3**, envelope `format_version` **1**.
Decisions and alternatives: ADR 026. Measured results: the Phase 4C report.

## 1. Audit findings this contract answers

| Finding (Phase 4B code on `eb03725`) | Consequence |
|---|---|
| `TimelineIndex::monitored_audio` and `plan_for` select one track. | The plan becomes every audio track, in project order. |
| The feeder fills the whole 400 ms ring, so any control change would be up to 400 ms late. | The mix-ahead is bounded separately from the ring (§6). |
| `MediaSummary.metadata` is `VideoMetadata`; a source without video cannot be represented. | A source is a video source or an audio-only source (§9). No fabricated video fields. |
| `Session::submit` stops timeline playback for every command but a few, and `Engine::execute` renews the evaluation generation. | Track-control commands must be exempt from both (§5). |
| `VerifyMedia` compares saved and probed metadata exactly, so a pre-4B project whose source has audio would read as "changed". | Saved metadata without audio details matches on everything else (§10). |
| A linked audio clip stores `source_in` in the video clock; linked trims use the video range. | `source_in` is defined as a rational instant (§8). |
| `finished(slot)` can observe "done" just after `try_recv` saw nothing, losing the last chunk. | The mixer re-reads the queue after seeing a decoder finish. |

## 2. Track mixing rules

At output frame `k` of an epoch the mix is

```
mixed[k] = Σ over audio tracks t, in project order: pcm_t[k] × g_t[k]
```

- `pcm_t` is the stereo float32 PCM of the clip active on track `t` at `k`
  (§8), or silence when the track has no clip or the source has no sample there.
- All sources are decoded by FFmpeg to the device mix rate and interleaved
  stereo float32, the same representation the output path already uses. Mono
  reaches stereo by FFmpeg's −3 dB pan law; layouts above stereo use FFmpeg's
  default downmix.
- The sum order is ascending project track order and is deterministic.
- There is one output stream, one ring and one clock, whatever the track count.
- Clips never overlap on one track (existing core invariant, unchanged).

**Active tracks.** A track participates in frame `k` when one of its clips
covers `k`. It is **audible** by §4. An inaudible track contributes zero but its
clip stays in the project and its waveform stays visible.

## 3. Volume

- Stored as an integer `gain_cdb`, hundredths of a decibel: `-6000..=600`
  (−60.00 dB to +6.00 dB), default `0`. Out of range is rejected, never clamped
  silently.
- Linear amplitude gain is `10^(gain_cdb / 2000)` (amplitude, not power).
- −60 dB is a real, non-zero gain. Mute is a separate state.
- Rust owns the value. C# shows `gain_cdb` from the view; during a slider drag
  it sends previews, and Rust publishes the effective value back.

## 4. Mute and solo

Evaluated from the project state, in Rust:

```
any_solo = any audio track has solo
audible(t) = !t.mute && (!any_solo || t.solo)
```

- With no solo, every unmuted track is audible.
- With one or more solos, only soloed tracks are audible.
- **Mute wins over solo**: a muted, soloed track is silent (and still counts for
  `any_solo`, so it silences the others).
- Multiple solos are allowed.
- Mute and solo change `g_t` only. They never remove clips or change the clock.

## 5. Control changes during playback

- Commands `SetTrackGain`, `SetTrackMute` and `SetTrackSolo` do not stop
  playback, do not renew the evaluation generation and do not start an epoch.
  Undo and Redo of those entries behave the same way.
- Each applied snapshot pushes the controls to the transport; the mixer reads
  them once per block (§6).
- A gain change takes effect through a linear ramp of **10 ms** starting at the
  next block, so no control change produces a step. A new epoch starts at the
  target with no ramp, so seeks and starts stay sample-exact.
- Audible latency of a control is the mix-ahead plus the device buffer plus the
  device's own processing delay; it is measured, not assumed.

**History.** One history stack. An entry is either a clip change or a track
control change `{track, before, after}`.
- A volume drag is previews (`preview_track_gain`, no history, no revision)
  followed by one `commit_track_gain`. A commit equal to the stored value adds
  no entry.
- Each mute or solo toggle is one entry.
- Track creation remains outside history.

## 6. Mixer budgets

| Budget | Value | On excess |
|---|---|---|
| Audible tracks with overlapping clips (`MAX_MIX_TRACKS`) | 16 | At Play: playback fails with an explicit error naming the count and position. A control change during playback that exceeds it: playback continues, the extra tracks stay silent, and `mix.over_limit` and the detail text say so. |
| Decoder processes (`MAX_DECODERS`) | 24 (16 active + 8 prefetch) | Prefetch waits. |
| PCM queued per decoder | 24 chunks of at most 4096 frames | Back-pressure on that decoder only. |
| Decoded PCM in total | at most 24 × 24 × 4096 frames (about 18.9 MiB) | Follows from the two above. |
| Mix block | 10 ms of frames | Controls are read per block. |
| Mix-ahead on a device | 150 ms (`prefill` 120 ms), inside the 400 ms ring | The feeder waits. |
| Prefetch horizon | 1500 ms before a clip's first sample | No decoder for later clips. |
| Standby decoders for inaudible tracks | Only inside `MAX_MIX_TRACKS` after audible tracks | A track made audible later joins late (§7). |

Decoder threads and FFmpeg processes run at normal priority for playback;
waveform and thumbnail work stays below normal and throttled during playback.
The WASAPI render thread only copies from the ring: no file, no decoder, no
join, no allocation per period.

## 7. Scheduling inside one block

1. Retire decoders whose clip is behind `k`; start decoders for active clips
   and clips inside the horizon, audible first, in track order.
2. Bound the block by the next clip boundary, the extent and the ring room.
3. For each active clip:
   - **Exact start.** At the clip's first frame in this epoch an audible track
     waits for PCM, so a clip never starts late.
   - **Running.** A track already contributing waits for PCM too; if the wait
     outlasts the ring the output pads, the clock freezes and an underrun is
     counted, as in Phase 4B. Tracks stay sample-aligned.
   - **Late join.** A track that becomes audible mid-clip with no PCM yet never
     blocks the others. It contributes silence until its PCM covers `k`, then
     ramps in. Its first contributing sample is still the exact sample for `k`.
   - **Inaudible.** Discards PCM behind `k`, never blocks.
   - **Source hole, missing source, finished stream.** Silence for that track.
4. Sum in project order, clip, push.

**Clipping.** The final stereo sum is hard-clipped to `[-1, 1]`. There is no
limiter and no normalization. The view reports the number of clipped samples
and the peak, so clipping is visible and measurable.

## 8. Clip stream identity and sample boundaries

- `Clip.stream` names `video` or `audio{index}` and is persisted.
- `source_in` is an **exact rational instant on the file's container clock**.
  Its `timebase` is a representation, chosen once:

| Clip | `source_in` clock |
|---|---|
| Video, and linked audio | The video stream's clock (Phase 4B, unchanged). |
| Audio after Unlink | Unchanged value and clock: Unlink never rewrites timing. |
| Independently placed audio (audio-only source, chosen stream) | `1/sample_rate` of that stream, so edits land on whole samples when the project clock allows. |

- Output frame `k` of a clip plays source sample `k + offset`, with
  `offset = ceil((source_in + (origin − start)) × rate)` (one rounding, as in
  Phase 4B). Rate is the device rate; a stream at another rate is resampled by
  FFmpeg with `-copyts`, so its PCM PTS stays on the container clock.
- Start trims and splits move `source_in` by a whole number of source ticks
  (existing grid); nothing accumulates. Negative and non-zero stream start PTS
  are ordinary values on that clock.

## 9. Audio-only sources and stream placement

- `MediaSummary.metadata` is a `SourceMetadata`: `Video(VideoMetadata)` or
  `Audio(AudioOnlyMetadata)`. The audio variant has a container, a validated
  `audio` list and nothing about pictures. `MediaAsset.kind` is `Audio`.
- A file is audio-only when FFprobe reports no non-attached video stream
  (cover art does not count) and at least one decodable audio stream.
- Supported, each covered by a real FFmpeg test: `wav` with `pcm_s16le`,
  `pcm_s24le` or `pcm_f32le`; `mp3`; `mov,mp4,m4a…` with `aac`; `flac`;
  `matroska` with PCM (existing fixture). Anything else stays `Unsupported`.
- `PlaceAudioStream {media, stream, track?, position}` places one chosen stream
  as an independent clip on the given audio track or, with none, on the first
  audio track free for its whole interval. The stream must have validated
  details. Length is the stream's duration (or the source duration when the
  stream reports none). `PlaceClip`/`InsertClip` with an audio-only source use
  its primary stream on the given audio track.
- A video source dropped on an audio track is rejected; every stream other than
  linked primary audio is an explicit `PlaceAudioStream`. Files are never
  auto-expanded into one clip per stream.
- Waveform cache keys already include the stream index. Audio-only keys use the
  same form.
- The Source monitor shows an audio-only source as such and its transport stays
  disabled. Timeline playback of audio-only clips runs with a blank picture.
- Placing a stream selects the new clip. A click on the current source's tile
  returns the inspector to that source, without reloading the Source monitor.

## 10. Linking, unlinking, persistence

- `UnlinkClip {clip}`: removes the link from both members in one transaction and
  one undo step; timing, stream and `source_in` are untouched.
- `LinkClips {clip, partner}`: valid only for one unlinked video clip and one
  unlinked audio clip with the same media, start, duration and identical
  `source_in`, and an audio stream that exists in the source. A shared file alone
  is never enough. The view offers it only when an exact candidate exists.
- Moves, trims, splits and removal of an unlinked clip touch only that clip.
- Trims of an independent audio clip are bounded by its **own stream's** range,
  in the clip's clock; an existing clip that already reaches outside that range
  stays valid, it just cannot grow further.

**Schema 3** adds to each track `gain_cdb`, `mute` and `solo`, nested as `controls` (written only when
not default, audio tracks only). Migration, in memory on open:

| From | Result |
|---|---|
| Schema 1 | As in Phase 4B, then default controls. |
| Schema 2 | Default controls. A schema-2 document carrying non-default control fields is refused. |
| Schema 3 | Validated: range; video tracks must hold defaults. |
| Anything else | Rejected. |

Opening never writes; a refused file is untouched; saving writes schema 3.
Selection, previews, caches and device state are never written.

Saved source metadata without per-stream audio details (written before Phase 4B)
matches a fresh probe when everything else matches and the fingerprint matches;
the details are then adopted.

## 11. Clock and epochs

- One audio-master clock, as in Phase 4B: device position through the segment
  map. Intentional silence (gaps, muted tracks, tracks with no clip, an all-muted
  mix) is real timeline audio to the clock. Only frames the output had to pad
  are starvation.
- Every plan-changing event (seek, stop, edit, undo of an edit, open, new,
  relink, end) ends the epoch: decoders are cancelled and reaped, the output is
  flushed, and PCM from an old epoch is never pushed because the epoch is
  rechecked under the feed lock immediately before each push.
- Device failure stops playback with an error; the next Play uses the system
  clock, without sound, as in Phase 4B.
- The timeline's length is the later of the monitored picture's end and the last
  audio clip's end, for preview and for playback alike. A timeline holding only
  audio has a length and plays, with a blank picture.
- `CUTCAP_AUDIO_LATENCY_MS` stays at default 0; no measured device value is
  hardcoded.

## 12. ABI 10

New exports, all acceptance-only:

| Export | Meaning |
|---|---|
| `cutcap_session_place_audio_stream(h, media, len, u32 stream, track, len, i64 position, u32 unit)` | §9. `track_len` 0 means the first free audio track. |
| `cutcap_session_unlink_clip(h, clip, len)` | §10. |
| `cutcap_session_link_clips(h, clip, len, partner, len)` | §10. |
| `cutcap_session_set_track_mute(h, track, len, u32 muted)` | One undo step. |
| `cutcap_session_set_track_solo(h, track, len, u32 soloed)` | One undo step. |
| `cutcap_session_preview_track_gain(h, track, len, i32 cdb)` | Transient; live in the mixer; no history. |
| `cutcap_session_commit_track_gain(h, track, len, i32 cdb)` | One undo step; ends the preview. |
| `cutcap_session_cancel_track_gain(h)` | Ends the preview, restores the stored value. |

View additions:
- tracks: `gain_cdb`, `gain_text`, `muted`, `soloed`, `audible`;
- media: `audio_only`, `audio_streams` (`index`, `label`, `primary`);
- `editing`: `can_unlink`, `link_candidate`, link detail;
- `audio.mix`: mixed tracks, peak, clipped samples, over-limit count, block
  timing, decoder peak;
- `audio.output`: render-thread timing.

`HistoryStatus` adds `undo_track`/`redo_track`.

## 13. Errors and cancellation

- Rejected commands change nothing and arrive as status text.
- Decoder failure on one track silences that track and reports why; other
  tracks continue.
- Over-limit at Play is a playback failure with the explicit message of §6.
- Closing cancels every decoder, joins the feeder off the UI thread's blocking
  paths as in Phase 4B, stops the stream and releases the endpoint.

## 14. Out of scope

Automation, EQ, effects, crossfades, time-stretching, recording, export,
latency calibration UI, multi-selection.

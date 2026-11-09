# Phase 5A contract — first real MP4 export

Binding for the Phase 5A implementation and its reviewers. Versions after this
phase: ABI/contract **11**, core schema **3** (unchanged), envelope
`format_version` **1** (unchanged). Export writes nothing to the project document.
Decisions and alternatives: ADR 027. Measured results: the Phase 5A report.

## 1. Audit findings this contract answers

| Finding (Phase 4C code on `16fb04a`) | Consequence |
|---|---|
| `TimelineIndex` is immutable, shared by `Arc` and rebuilt on geometry changes. | The plan keeps that `Arc`; later edits cannot reach it. |
| Track controls do not renew the evaluation index, so its track copies can be stale. | Controls are copied from the project, never from the index (§3). |
| `audio_transport.rs` mixes its exact layout and gain math into device transport code. | The layout, gain and clipping move to `audio_mix.rs`, shared by both (§7). |
| `stream.rs` emits RGBA at preview size, bounded at 1280×720, as PAM. | Export uses an offline variant that emits exact-size YUV 4:2:0, sharing the packet framing (§6). |
| Preview silences a missing audio source and plays on. | Export refuses instead (§10). |
| The source fingerprint is a full SHA-256, checked at open or relink only. | Preflight recomputes it (§10). |
| Start trims and splits land on the source tick grid, not the frame grid. | A clip head usually falls between two frames (§5). |

## 2. Output preset: CutCap Standard MP4

| Property | Value |
|---|---|
| Container | MP4, `+faststart` |
| Video | H.264 by `libx264`, CRF 23, preset `veryfast` |
| Picture | 1280×720, square pixels, `yuv420p`, 30 fps constant |
| Color | SDR, BT.709 matrix, primaries and transfer tags, limited range |
| Audio | AAC (FFmpeg `aac`), 48 000 Hz, stereo, 192 kbps |

If either encoder is missing, Export is unavailable and says why. Another encoder
is never substituted.

## 3. Immutable plan

Starting an export asks the command owner for an `ExportPlan`, built on its own
thread without mutating anything. It owns:

- project id, name and document revision;
- the project timebase;
- the selected video track's stable `TrackId` and name (or none), and the names of
  occupied video tracks it omits;
- the `Arc<TimelineIndex>` holding every clip's identity, position, stream, link
  and source mapping;
- media details and expected fingerprints of the sources it uses;
- each audio track's **committed** `TrackControls` from the project. An uncommitted
  slider preview lives in the session and cannot reach the plan;
- the output grid (§4), output path and overwrite permission.

Not captured: playhead, frame caches, device latency compensation, WASAPI state.
Nothing is written to the project. Export does not change dirty state, undo history
or either playhead, and later edits, New or Open cannot affect a running export.

If several video tracks hold clips, only the selected one is rendered and the user
must acknowledge the omission before starting. There is no compositing.

## 4. Output grid

For content extent `E` (later of the selected track's end and the last audio clip's
end), project clock `c`, `fps = 30`, `rate = 48000`:

```
frames  = ceil(E * c * fps)          // exact rational, i128
samples = frames * rate / fps        // 1600 per frame, exact
```

- Video and audio have the same nominal duration, `frames / 30` s, at most one
  frame longer than `E`. Audio past `E` is silence.
- Frame `n` owns project tick `floor(n / (fps * c))`. Clips use `[start, end)`, and
  flooring makes that membership exact at any project clock, because `start` and
  `end` are integers.
- Sample `k` follows Phase 4C unchanged: `samples_until` (ceiling) for clip starts
  and ends, and one `source_sample_offset` per clip.
- `E == 0` is refused. Longer than `MAX_EXPORT_SECONDS` (14 400) is refused.

## 5. Video evaluation

For frame `n` at tick `t`, in order:

1. The selected track's clip with `start <= t < end`, or black for a gap.
2. Its exact source bounds: `map_clip_source` at the clip start (ceil-rescaled once).
3. Decoded frames in real PTS order, each mapped to its first presentation tick by
   `frame_timeline_time`. PTS outside the bounds is never accepted.
4. The latest frame with `position <= t`. The same decoded frame is reused until the
   next one is due; adjacent output frames do not decode or seek.

- **VFR.** Only actual PTS is used. Nominal frame rate is never used to derive a
  source time.
- **Cuts.** At a clip change the held frame is dropped before the next clip is
  consulted, so nothing crosses a cut.
- **Gaps** are black frames (Y 16, U 128, V 128), never a held frame.
- **Source tail.** After the last eligible frame, that frame is held to the clip end.
- **Clip head (a deliberate difference from preview).** Phase 3D shows blank until
  the first eligible PTS. On a live clock that is an invisible sub-frame blank, but
  in a CFR file it would be a permanent black flash. Because source-in points fall
  between frames, about half of all cuts would have one. Export therefore shows the
  first eligible frame when its position is within `HEAD_HOLD_MS` (100 ms) of `t`.
  Past that bound the head stays black. An earlier frame outside the clip's interval
  is still never used.
- **No eligible frame in a clip.** The clip is black and the result reports the count.
  A decoder failure is an export failure instead.

## 6. Picture conversion

Each clip's decoder produces finished 1280×720 YUV 4:2:0 frames:

- Rotation is applied by FFmpeg's autorotate, as in preview.
- The display aspect comes from `display_aspect` or dimensions × SAR, swapped for
  90°/270°.
- The picture is fitted inside the frame without distortion. Scaled dimensions are
  even, centred, and padded black. Landscape letterboxes; portrait pillarboxes.
- Matrix: `bt709` stays; `smpte170m`, `bt470bg` and `bt601` convert from BT.601.
  An untagged source goes through FFmpeg's default, exactly as the preview does, so
  the file looks like what the editor showed. Range follows the source. Output is
  BT.709 limited range, and tagged so. Primaries are not remapped.
- HDR and BT.2020 sources are refused by metadata validation, never tone-mapped.

## 7. Audio

Audio shares Phase 4C's exact layout (`layout`), audibility and gain (`track_mix`),
summing (`GainRamp::mix_into`), and clipping (`clip_mix`). Differences:

- Gain is static: a ramp is live-control smoothing only.
- Blocks wait for PCM without limit other than the decoder stall timeout. No late
  joins exist, because the mixer waits for each decoder.
- Inaudible tracks are not decoded.
- A decoder failure ends the export. A source hole or end of stream inside a clip is
  silence, counted in the result.
- `CUTCAP_AUDIO_LATENCY_MS` and WASAPI are never consulted.
- The sum is hard-clipped and counted. There is no normalization.

More than 16 audible overlapping tracks is refused by preflight.

## 8. Encoder transport

One FFmpeg process encodes. Its inputs:

- `pipe:0`: raw YUV 4:2:0 frames, written one at a time, blocking on the pipe;
- a scratch file of raw `f32le` stereo PCM, fully written before the encoder starts.

Timestamps follow from the inputs: frame `n` at `n/30`, sample `k` at `k/48000`, both
from zero. AAC priming is written as the MP4 edit list by FFmpeg.

**Why a scratch file.** Windows offers one standard input. A second raw stream would
need either a named pipe (`CreateNamedPipeW`: new handwritten `unsafe` or a new crate,
and an open-order deadlock to manage) or a loopback socket (any local process could
connect first). Muxing both into one pipe would need a hand-written container muxer.
Audio is small next to video and is produced at many times real time, so it is
written first. This also decodes audio and video in sequence rather than together,
which lowers peak process count. Raw video is never written to disk.

The scratch file is exactly `samples * 8` bytes (at most 5.5 GB at the 4-hour limit),
sits beside the output, is reserved up front, and is removed on every exit path.

## 9. Output file safety

- The destination must be a local absolute path ending in `.mp4`, in an existing
  folder, not a reserved device name, and not any source file of the project.
- An existing file is replaced only with explicit permission. Without it, the export
  is refused before work starts and again at publish.
- FFmpeg writes a unique temporary file in the destination folder, never the
  destination itself.
- After the encoder exits successfully, the temporary file is validated by FFprobe
  (codecs, 1280×720, `yuv420p`, 30 fps, exact frame count, 48 kHz stereo, duration),
  flushed, then renamed over the destination.
- On any failure or cancellation the temporary files are removed and an existing
  output stays intact.
- **Guarantees.** The rename is `MoveFileExW` within one volume, which replaces the
  name in one step on NTFS. Windows documents no general atomicity or power-loss
  guarantee for it. A crash of the whole machine can leave a temporary file behind.
  Host death ends the child processes through their job objects.
- Disk space: preflight reserves the scratch size plus a video estimate (8 Mbps +
  192 kbps, plus 64 MiB) in the destination folder, which also proves write permission.
  It is an estimate; a disk filling up later is still reported as a failure.

## 10. Preflight

Run on the worker before any decoder or encoder starts, in this order: plan shape and
duration; selected track; source availability; a fresh probe and full SHA-256 against
the expected fingerprint, for each source the plan uses; stream metadata and SDR;
source ranges; the 16-track limit; output path; encoder availability; disk reservation.
A missing or changed source blocks the export. Intentional gaps and silence do not.

## 11. Progress

States: `idle`, `validating`, `preparing` (mixing audio), `decoding` (encoder open,
awaiting the first picture), `encoding`, `finalizing`, `completed`, `cancelling`,
`cancelled`, `failed`.

```
progress = 0.10 * mixed_samples / samples
         + 0.88 * encoded_frames / frames
```

`encoded_frames` is FFmpeg's own `frame=` count of frames through the encoder, not
frames written to the pipe. `finalizing` holds at 0.98 or less. Only `completed` is
1.0. Wall time is never used for progress. Speed is encoded output seconds divided
by encoding time. A start returns on acceptance and is not completion.

## 12. Cancellation and shutdown

Cancel takes the job id, so a stale cancel is refused. It sets the state to
`cancelling` immediately, then: the token stops new work and wakes queues; decoders
are cancelled and joined; the encoder's watcher kills and reaps the child; temporary
files are removed; the state becomes `cancelled`. The UI thread never waits on this.
Session shutdown cancels the job and waits for the worker, bounded at 5 s.

## 13. Budgets

| Budget | Value | On excess |
|---|---|---|
| Export jobs per session | 1 | Refused while one runs |
| Duration | 14 400 s | Refused by preflight |
| Video decoders | 2 (active, next prepared 1 s ahead) | Fixed |
| Decoded frames per decoder | queue of 3, plus one held and one lookahead | Back-pressure |
| Audio decoders | 16 audible overlapping, 24 total | Refused by preflight, then fixed |
| PCM per decoder | 24 chunks of 4096 frames | Back-pressure |
| Encoder input | one frame per write, OS pipe | Back-pressure |
| Child memory | 1 GiB commit each, job-enforced | The child fails, and so does the export |
| Decoder stall | 20 s | Failure |
| Encoder stall | 120 s without an accepted frame | Failure |
| Finalization | 300 s | Failure |
| Child diagnostics | 64 KiB | Truncated |

All export children run below normal priority so the editor stays responsive.

## 14. ABI 11

| Export | Meaning |
|---|---|
| `cutcap_session_start_export(u64, u16* path, usize, u8* track, usize, u32 flags)` | Accepts a start. `track` length 0 means no video track. Flag 1: replacing an existing file is allowed. Flag 2: omitted occupied video tracks are acknowledged. |
| `cutcap_session_cancel_export(u64, u8* job, usize)` | Requests cancellation of that job. |

The view gains `export`: `available`, `unavailable_reason`, the per-snapshot `summary`
(preset text, video tracks with their duration, frames and blocking reason, and the
included audio track count), and `job` (id, state, progress, counts, speed, paths,
revision, track name, result figures, error). No pixels or samples cross the ABI.

## 15. Errors

Failures name the stage and a cause in plain language. Disk-full and permission
failures are reported as such. The project, its dirty state and any existing output
are untouched by a failed export.

## 16. Out of scope

Multi-track compositing, transitions, effects, keyframes, text, grading, HDR, hardware
encoding, other formats or presets, proxies, audio effects, latency calibration,
installers, cloud and AI features.

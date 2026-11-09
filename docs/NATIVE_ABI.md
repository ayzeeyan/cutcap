# CutCap native engine ABI, version 11

## Phase 5A additions (current authority)

Rust `ABI_VERSION`, managed `NativeMethods.AbiVersion` and `CONTRACT_VERSION` are
**11** (ADR 027). The project envelope stays 1 and the core schema stays 3. Every
ABI 10 export, status number, layout and handle rule is unchanged.

| New export | Semantics |
|---|---|
| `cutcap_session_start_export(u64, u16* path, usize, u8* track, usize, u32 flags)` | Accepts an export of the current project. `track` length 0 means no video track. Flag 1: replacing an existing file is allowed. Flag 2: omitted occupied video tracks are acknowledged. Acceptance is not completion. |
| `cutcap_session_cancel_export(u64, u8* job, usize)` | Asks that job to stop. Never waits. A job that is no longer current is refused. |

The view gains `export`: `available` (null until FFmpeg has been asked),
`unavailable_reason`, `summary` and `job`. `summary` has the preset text, each video
track's `id`, `name`, `clips`, `duration_text`, `frames` and `blocked` reason,
`default_track`, and the audio track counts. It is rebuilt once per snapshot. `job`
has `id`, `state`, `running`, `progress`, the frame and sample counts, `elapsed_text`,
`speed`, the output path, project revision, track name, `error` and `result`. No
pixels, samples or encoded data cross the ABI.

## Phase 4C additions (ABI 10, retained)

Rust `ABI_VERSION`, managed `NativeMethods.AbiVersion` and `CONTRACT_VERSION` are
**10** (ADR 026). Project envelope `format_version` remains 1; the core schema is
**3**. Every ABI 9 export, status number, layout and handle rule is unchanged; the
bump guards eight exports and new view fields.

| New export | Semantics |
|---|---|
| `cutcap_session_place_audio_stream(u64, u8* media, usize, u32 stream, u8* track, usize, i64 position, u32 unit)` | Place one decodable stream of a Ready source as an independent clip. `track` length 0 picks the first audio track free for the whole interval. One undo step. |
| `cutcap_session_unlink_clip(u64, u8* clip, usize)` | Remove the link of a linked pair. One undo step; no timing changes. |
| `cutcap_session_link_clips(u64, u8* clip, usize, u8* partner, usize)` | Link one unlinked video clip and one unlinked audio clip with identical media, start, duration and `source_in`. One undo step. |
| `cutcap_session_set_track_mute(u64, u8* track, usize, u32 muted)` | One undo step; playback continues. |
| `cutcap_session_set_track_solo(u64, u8* track, usize, u32 soloed)` | One undo step; playback continues. |
| `cutcap_session_preview_track_gain(u64, u8* track, usize, i32 cdb)` | Transient volume during a drag, −6000..=600 hundredths of a dB. Heard live and shown in the view; no history, no document change. |
| `cutcap_session_commit_track_gain(u64, u8* track, usize, i32 cdb)` | Ends a drag with one undo step. Equal to the stored value: no entry. |
| `cutcap_session_cancel_track_gain(u64)` | Ends a drag; the stored value applies again. |

All are acceptance-only and validated before anything is queued. Audio-only
sources use the existing `place_clip`/`insert_clip` on an audio track.

View additions:
- `tracks[]`: `gain_cdb` (effective, including a drag), `gain_text`, `muted`,
  `soloed`, `audible`.
- `media[]`: `audio_only`, `audio_streams[]` (`index`, `label`, `primary`).
- `inspector.badge`: `"Video"`, `"Audio"` or empty.
- `editing`: `can_unlink`, `link_candidate` (clip id of the one exact partner).
- `history`: `undo_track`, `redo_track`.
- `audio.mix`: `active_tracks`, `mixed_tracks`, `mixed_tracks_peak`,
  `decoders_peak`, `clipped_samples`, `peak`, `over_limit`, `blocks`,
  `block_us_last/max/total`, `mixed_ms`.
- `audio.output`: `period_us_last/max`, `periods` (render-thread timing).

## Phase 4B additions (ABI 9, retained)

Rust `ABI_VERSION`, managed `NativeMethods.AbiVersion` and `CONTRACT_VERSION`
are **9** (ADR 025). Project envelope `format_version` remains 1; the core
schema is **2** (PROJECT_FORMAT.md). Every ABI 8 export, status number, frame
and thumbnail layout and handle rule is unchanged; the bump guards three new
exports, new clip-row fields and the audio/waveform view sections.

| New export | Semantics |
|---|---|
| `cutcap_session_request_waveforms(u64, u8* json, usize len)` | Replace the waveform demand with a UTF-8 JSON array of `{token, media_id, audio_key, stream, chunk}` (≤128, ≤64 KiB, visible first). Nonblocking. Wants whose media is not Ready, whose `audio_key` is not current or whose stream/chunk is invalid are dropped; chunks no longer wanted leave the queue and an in-flight batch nobody wants is cancelled. |
| `cutcap_session_next_waveform(u64, CutCapWaveformInfo* out)` | Announce the next completed chunk whose key is still current, or `CUTCAP_NO_FRAME`. The pointer is validated before anything is consumed. |
| `cutcap_session_copy_waveform(u64, u64 token, f32* dst, usize len)` | Copy the announced chunk as `buckets` interleaved `(min, max)` floats (`len ≥ 2·buckets`) and release it. Another token or a second copy → `CUTCAP_STALE_FRAME`; a failed chunk → `CUTCAP_NO_FRAME`; a null/short destination → `INVALID_ARGUMENT`, chunk stays announced. |

`CutCapWaveformInfo` is 40 bytes, 8-byte aligned: `u64 token, i64 chunk, u32
rate, u32 channels, u32 buckets, u32 bucket_frames, u32 status, u32 reserved`
(status 0 ready, 1 failed). A chunk covers native-rate source samples
`[chunk·262144, (chunk+1)·262144)` in 1024 buckets of 256 frames, min/max over
all channels; an empty bucket has `min > max`. Peaks stay in Rust until copied.

View additions: clip rows add `stream` (`"video"`/`"audio"`), `audio_stream`,
`link_id`, `audio_key` (`"{media_id}:{path hash}:{stream}"` while Ready, else
empty), `audio_rate`, `source_in_samples` and `source_end_samples` (the exact
sample interval playback uses: one ceiling rounding of the shared `source_in`).
The view adds `audio` (state, clock source, output status: device, rate,
stream latency, latency compensation, buffered/played/padded ms, underruns,
error; decoders, source silence, detail) and `waveforms` (worker stats).
Timeline playback adds `clock_source`, `video_late`, `av_offset_ms`,
`av_offset_max_ms` and `av_samples`. Placement of a source with audio inserts a
linked video/audio pair; edits of either member apply to both (one undo step).

## Phase 4A additions (ABI 8, retained)

Rust `ABI_VERSION`, managed `NativeMethods.AbiVersion` and `CONTRACT_VERSION`
are **8** (ADR 024). Project envelope/core schema remain 1. Every ABI 7 export,
status number, frame layout and handle rule is unchanged; the bump guards the
three new exports and the new view fields.

| New export | Semantics |
|---|---|
| `cutcap_session_request_thumbnails(u64, u8* json, usize len)` | Replace the filmstrip demand with a UTF-8 JSON array of `{token, media_id, source_key, sample_us, height}` (≤128 wants, ≤64 KiB, visible first). Nonblocking acceptance. Wants whose media is unknown/not Ready or whose `source_key` is not the current key are dropped; a sample at/after the source end completes at once as no-frame without decoding. Samples no longer wanted are dropped from the queue, and an in-flight batch none of whose samples is still wanted is cancelled. |
| `cutcap_session_next_thumbnail(u64, CutCapThumbnailInfo* out)` | Announce the next completed thumbnail whose source key is still current (stale ones are discarded here), or `CUTCAP_NO_FRAME` when none waits. The pointer is validated before anything is consumed. Announcing again discards an announced, uncopied one. |
| `cutcap_session_copy_thumbnail(u64, u64 token, u8* dst, usize len, usize stride)` | Copy the announced thumbnail's RGBA8 rows (`stride ≥ width·4`) and release it. Another token → `CUTCAP_STALE_FRAME`; a no-frame/failed result → `CUTCAP_NO_FRAME` (released); a too-small/null destination → `INVALID_ARGUMENT` and the thumbnail stays announced. |

`CutCapThumbnailInfo` is 40 bytes, 8-byte aligned: `u64 token, i64
actual_ticks, u32 timebase_num, u32 timebase_den, u32 width, u32 height, u32
stride, u32 status` (0 ready, 1 no frame at/after the sample, 2 failed).
`actual_ticks` is the decoded frame's original PTS in the stream timebase (0
unless ready). Ownership: pixels stay in Rust until copied into caller memory;
no pointer, callback or handle escapes.

Sample mapping (Rust): `sample_us` → stream ticks with **ceiling** rounding,
clamped to `start_pts`; the thumbnail is the first decoded frame whose actual
PTS is ≥ that tick (the Phase 3C still policy), never a nominal-FPS frame
index. Heights are clamped to 16–216 px; the width follows the display aspect
(rotation and sample aspect applied, ≤384 px).

View additions: clip rows add `source_key` (`"{media_id}:{fnv1a64(path)}:{stream}"`
while the media is Ready, otherwise empty, so relinking changes it),
`source_in_ticks`/`source_end_ticks` (the clip's exact source interval,
end-exclusive, stream ticks), `source_in_us` (ceil) / `source_end_us` (floor)
and `thumb_aspect`. The session view adds `thumbnails { pending, in_flight,
waiting, decoded, no_frame, failed, batches, cancelled_batches, last_batch_ms,
last_process_id, throttled }`; it is republished whenever these change.
Thumbnail work never touches the project snapshot, history or document
revision. Tests: `crates/cutcap-ffi/src/thumbnail_tests.rs`,
`crates/cutcap-media/tests/ffmpeg_integration.rs` (thumbnail cases),
`tests/CutCap.Interop.Tests/ThumbnailTests.cs`.

## Phase 3E additions (ABI 7, retained)

Rust `ABI_VERSION`, managed `NativeMethods.AbiVersion` and `CONTRACT_VERSION`
were **7** (ADR 023). Project envelope/core schema remain 1. Every ABI 6 export,
status number, frame layout and handle rule is unchanged; the bump is required
because the frontend consumes new view fields and calls new exports, and it is
the guard that stops a stale ABI-6 DLL from failing on the first edit.

| New export | Semantics |
|---|---|
| `cutcap_session_place_clip(u64, u8* media, usize, u8* track, usize, i64 position, u32 unit)` | Enqueue PlaceClip: the whole Ready video source at `position` (drag-and-drop). |
| `cutcap_session_move_clip(u64, u8* clip, usize, u8* track, usize, i64 position, u32 unit, u32 anchor)` | Enqueue MoveClip: the clip's `anchor` edge (0 start, 1 end) lands at `position` on `track`. |
| `cutcap_session_trim_clip(u64, u8* clip, usize, u32 edge, i64 position, u32 unit)` | Enqueue TrimClip of the start (0) or end (1) boundary. |
| `cutcap_session_split_clip(u64, u8* clip, usize, i64 position, u32 unit)` | Enqueue SplitClip at an explicit position. |
| `cutcap_session_split_clip_at_playhead(u64, u8* clip, usize)` | Split at the Rust-owned timeline playhead (transport clock while playing, else the still-preview position). |

`unit` is 0 for exact project ticks and 1 for whole milliseconds (floored to
ticks by the owner); any other unit/edge/anchor, malformed or nil UUIDs and
null-with-length pointers return `INVALID_ARGUMENT`. All exports mean
acceptance only; engine rejections (overlap, audio track, source range, edge,
grid) arrive as view status and change nothing. Every accepted edit is one
history entry and, like ABI 6 structural edits, invalidates the timeline epoch:
playback stops, the playhead is kept, nothing auto-resumes.

View additions: clip rows add `end_ms`, `start_ticks`, `end_ticks`,
`min_start_ticks`/`min_start_ms` (earliest start a start trim can reach inside
the source, on its tick grid) and `max_end_ticks`/`max_end_ms` (latest end an
end trim can reach; null when the source end is unknown). The session view adds
`editing { split_enabled, split_detail, notice }`, computed by Rust from the
selected clip and the playhead each view; `notice` repeats the last edit's
source-tick alignment note. The wrapper adds PlaceClip/MoveClip/TrimClip/
SplitClip/SplitClipAtPlayhead with a `TimelinePosition` (ticks or ms) and
`ClipEdge`. Tests: `crates/cutcap-ffi/src/timeline_edit_session_tests.rs`,
`clip_edit_tests.rs`; `tests/CutCap.Interop.Tests/TimelineAdvancedEditingTests.cs`.

## Phase 3D additions (ABI 6, retained)

Rust ABI_VERSION, managed NativeMethods.AbiVersion and CONTRACT_VERSION are **6**.
Project envelope/core schema remain 1. All prior exports/status numbers and the
24-byte FrameInfo layout are retained. Added nonblocking exports (u64 session):
`cutcap_session_play_timeline`, `cutcap_session_pause_timeline`,
`cutcap_session_stop_timeline`. Managed PlayTimeline/ResumeTimeline/PauseTimeline/
StopTimeline mirror these. Play requires Timeline mode and a ready project; empty
tracks reach Ended at zero. Existing source Play/Stop keep Source-only meaning.

`timeline_playback` is a new small consumed view: state, desired_playing, epoch,
stable requested_ticks/text, clock_ticks/ms/text, offered_ticks/PTS, copied_ticks/
PTS/clip identity, decoded/offered/copied/dropped/stall/handoff/decoder-start
counters, buffered scheduler frames, last observed active/prepared process IDs,
error and request_to_ready_us. Copied ticks describe the offered frame's exact
mapped timeline deadline, not the advancing clock or physical display. Process
IDs are diagnostics and can name already reaped children. Counts include decoded
preroll/lookahead; they are not physical presented FPS.

`timeline_preview` continues supplying track/clip/media, extent, status and the
Rust clock position during continuous playback. Stopped seeks retain ABI 5 ceiling
semantics; active playback seeks use actual PTS intervals. Seek acceptance does
not prove readiness. Failed owner plan requests surface Failed asynchronously.
The source and timeline positions never copy into one another. Leaving either
mode pauses it. Restored cached images receive a fresh shared frame generation.

The owner returns a private immutable Arc<TimelineIndex> plan, not a serialized
clip vector, through the same bounded reliable bridge. One desired plan and one
in-flight query coalesce rapid seeks; two persistent workers bound active/next
clip decoding. No per-frame owner commands, project snapshots or timeline JSON.
See PHASE_3D_TIMELINE_PLAYBACK_CONTRACT.md for the full clock, VFR, half-open cut,
gap, handoff, cancellation and structural-mutation policy.

## Phase 3C additions (ABI 5, retained)

ABI_VERSION / NativeMethods.AbiVersion and CONTRACT_VERSION were **5** for Phase 3C. Format/core schema stay 1. Every ABI 4 export, numeric status and the 24-byte FrameInfo layout remains intact. New controls return acceptance only; no evaluation, file/process I/O or waits occur in the ABI call.

| Export | Meaning |
|---|---|
| set_preview_mode(u64, u32) | 0 Source, 1 Timeline; other values INVALID_ARGUMENT. Source pauses on leaving, retains its epoch/position/preroll, and does not auto-resume. |
| seek_timeline(u64, i64 ticks) | Exact nonnegative project ticks; switches to Timeline. |
| seek_timeline_ms(u64, i64 ms) | UI milliseconds, floor-rescaled in the owner to project ticks. |
| seek_timeline_text(u64, u8*, usize) | Existing source-style millisecond/timecode grammar or exact integer followed by `ticks`. |
| cancel_timeline_preview(u64) | Cancel ephemeral evaluation/decode, clear timeline image; no project mutation. |

All export names have the `cutcap_session_` prefix. New consumed view fields are `preview_mode` and `timeline_preview` (position ticks/timebase, display/input text, extent, monitored track, state/detail, active clip/media, exact requested/actual PTS, model generation and timing/copy diagnostics). `view.frame` always describes the visible mode only. Generations use one native counter across both modes and are renewed when a cached source image is restored; incompatible old copies are stale. Pixels still bypass JSON and MVVM. One frontend bitmap is reused.

Timeline seek goes through a bounded latest desired slot and at most one reliable owner evaluation. TimelineEvaluated returns a small correlated result, not a ProjectSnapshot; it never changes `timeline_generation` or rebuilds the separately fetched clip document. Source transport APIs retain Source-only meaning and Play/Stop reject while Timeline is monitored. Timeline Play uses the separate Phase 3D controls above. EngineSession exposes SetPreviewMode, SeekTimeline, SeekTimelineMilliseconds, SeekTimelineText and CancelTimelinePreview. See PHASE_3C_TIMELINE_PREVIEW_CONTRACT.md for exact half-open intervals, rational rounding, endpoint rejection and generation invalidation.

## Phase 3B additions (ABI 4, retained)

Rust `ABI_VERSION` and C# `NativeMethods.AbiVersion` are 4; CONTRACT_VERSION is 4 (ADR 020). The bump is required by AGENTS.md for new consumed view fields, and it is the guard that stops a stale ABI-3 DLL from passing the version check and failing on the first clip command. Every ABI 3 export, status number, frame layout and handle rule is unchanged.

| New export | Semantics |
|---|---|
| `cutcap_session_insert_clip(u64, u8* media, usize, u8* track, usize)` | Enqueue InsertClip (append-to-end policy, ENGINE_UI_CONTRACT.md). Malformed/nil UUIDs or null pointers → `INVALID_ARGUMENT`; engine rejections arrive as view status. |
| `cutcap_session_remove_clip(u64, u8* id, usize)` | Enqueue RemoveClip. |
| `cutcap_session_select_clip(u64, u8* id, usize)` | Transient clip selection; the inspector follows it. |
| `cutcap_session_undo(u64)` / `cutcap_session_redo(u64)` | Enqueue Undo/Redo; "Nothing to undo/redo." arrives as an error status. |
| `cutcap_session_timeline(u64, u8* buf, usize cap, usize* len)` | Copies the timeline document `{generation, clips}` with the same buffer protocol as `cutcap_session_view`. |

View additions: `timeline_generation` (u64; fetch the timeline document only when it differs from the last generation read) and `history` (`can_undo`, `can_redo`, `undo`, `redo` labels, `depth`). Clips are intentionally absent from the view: the view is re-serialized for each playback frame, the clip list only per edit. Clip timing crosses as floored display milliseconds plus Rust-formatted strings; exact ticks stay in Rust and appear in the inspector. The wrapper exposes InsertClip/RemoveClip/SelectClip/Undo/Redo/ReadTimeline (64 MiB ceiling, about 340 bytes per clip at the 100,000-clip core limit). Tests: `crates/cutcap-ffi/src/timeline_tests.rs` (ABI arguments, round trip, rejections) and `tests/CutCap.Interop.Tests/TimelineEditingTests.cs` (real DLL and FFmpeg).

## Phase 3A additions (ABI 3, retained)

Rust `ABI_VERSION` and C# `NativeMethods.AbiVersion` are 3; internal CONTRACT_VERSION is 3. Core schema remains 1; `.cutcap` envelope format is 1. All ABI 2 transport/frame layouts, status numbers and ownership rules below remain unchanged. The in-tree frontend refuses older/newer DLLs. Phase 2B's production transport is integrated, not a pending handoff.

| New export | Semantics |
|---|---|
| `cutcap_session_save_project(u64)` | Enqueue Save to the last successfully opened/saved path. With no path, terminal error requests Save As. |
| `cutcap_session_save_project_as(u64, u16* path, usize units)` | Enqueue Save As to an absolute `.cutcap` path. |
| `cutcap_session_open_project(u64, u16* path, usize units)` | Enqueue validated Open. Old project/media survives any failure. |
| `cutcap_session_relink_media(u64, u8* id, usize id_len, u16* path, usize units)` | Inspect original-content candidate on the single probe worker; owner alone changes the reference. |

Path input validates pointer/alignment/1..32767 units and rejects embedded NUL before use. IDs retain existing UUID rules. All command returns mean acceptance only; no function above performs file I/O, probing or waiting on the dispatcher. The wrapper exposes SaveProject/SaveProjectAs/OpenProject/RelinkMedia. Project file operations are serialized; queue-full and owner Busy errors remain observable.

Consumed view changes: `project.document` contains `path`, domain `revision`, `saved_revision`, derived `dirty`, `operation_id` (RequestId), `state` (idle/saving/opening/saved/opened/failed) and optional `error`. `project.saved` is true only when a file path exists and the current revision is clean. This domain revision is **not** SessionView.revision (presentation cache invalidation). Saving/Open emit nonterminal Updated snapshots and keep their RequestIds in `pending` through terminal completion/failure. The single frontend document workflow waits for the owner requests to drain, checks a new operation identity and terminal Saved status, and rechecks dirty state before replacement/close; accepted is never shown as saved.

Media rows add `availability` (checking/ready/missing/changed/unavailable), `detail` and resolved `path`. Saved metadata remains context, not readiness. Track rows add Rust-formatted `lane_label` so preserved clip data is not described as an empty track. No pixel array enters the view. Relink completion/failure appears in media availability/import feedback and increments the document revision only on a successful path change.

Only the owner mutates documents/revisions. One file worker owns its immutable snapshot. A bounded session slot retains verification results until the reliable command queue accepts them. Opening the same ProjectId still invalidates the prior source epoch/probe. See PROJECT_FORMAT.md for path/hash checks, bounded parsing, same-directory flush/replace, detached owned file tasks on uninterruptible OS calls and guarantees/limits. UI New/Open/close prompts for unsaved changes; file dialog Cancel leaves state intact.

Playback startup now waits for an actual successor PTS or EOF before anchoring the playing clock (ADR 019). Paused first frames remain immediate; true post-preroll starvation still freezes the clock in Buffering. No frame scheduling deadline or display-refresh assumption changed.

## ABI 2 foundation retained by ABI 3 and 4

`crates/cutcap-ffi` builds `cutcap_engine.dll` (Windows x64, `extern "C"`). The Avalonia frontend loads it through `src-dotnet/CutCap.Interop`. Sources of truth are abi.rs and NativeMethods.cs. ABI 3 is versioned independently of in-process contract 3, document format 1 and core schema 1.

## Shape

The frontend never receives Rust pointers, callbacks or project objects. It calls short, non-blocking functions on the UI thread and **pulls** state:

1. Commands (`new_project`, `add_track`, `select_track`, `clear_selection`, `select_media`, `import`, `cancel_import`, `seek`, `seek_text`, `cancel_preview`) queue work and return immediately.
2. A UI timer (16 ms) calls `cutcap_session_pump`, which drains at most 8 reliable engine events, polls the probe job and takes at most one decoded frame. It never waits.
3. When pump reports a change, the frontend reads the coalesced **view** (UTF-8 JSON) and, if `view.frame.generation` changed, copies the frame into its own bitmap.

No unmanaged-to-managed callbacks exist, so no delegate can be collected while native code holds it, and UI shutdown cannot race native frame delivery.

## Functions

All return `int32` status unless noted. Strings are explicit pointer + length, no terminator: UTF-8 bytes, except file paths, which are UTF-16 code units so every Windows path (including unpaired surrogates and long paths) round-trips exactly. Input text is limited to 64 KiB; paths to 32,767 units.

| Function | Notes |
|---|---|
| `uint32 cutcap_abi_version()` | Always callable. The frontend refuses to start on mismatch. |
| `cutcap_last_error(u8* buf, usize cap, usize* len)` | Message of the calling thread's last failed call. |
| `cutcap_session_create(u32 abi, u64* out_handle)` | Checks `abi`, starts the command worker and decoder, queues the first project. |
| `cutcap_session_destroy(u64)` | Cancels probe/decode, kills and reaps FFmpeg children, joins workers, frees. Second call → `INVALID_HANDLE`. |
| `cutcap_session_new_project(u64)` / `add_track(u64, u32 kind)` | `kind` 0 video, 1 audio. |
| `cutcap_session_select_track(u64, u8* id, usize)` / `select_media(...)` | UUID text from the view; nil or malformed → `INVALID_ARGUMENT`. |
| `cutcap_session_clear_selection(u64)` | |
| `cutcap_session_import(u64, u16* path, usize units)` | Starts the single cancellable probe; registration is committed by the project owner. |
| `cutcap_session_cancel_import(u64)` | |
| `cutcap_session_seek(u64, i64 ms)` | Relative source milliseconds; checked ceil rescale to the stream clock. |
| `cutcap_session_seek_text(u64, u8*, usize)` | `1500`, `1.5s`, `00:01.500`; the tested Rust grammar rejects ambiguous `1.5`. |
| `cutcap_session_cancel_preview(u64)` | Keeps the last valid frame. |
| `cutcap_session_play(u64)` | Accept Play/Resume for the selected source; INVALID_ARGUMENT if none is ready. Idempotent while playing. |
| `cutcap_session_pause(u64)` | Freeze the source clock; keep bounded preroll and last image. |
| `cutcap_session_stop(u64)` | Cancel persistent source decode and request the isolated zero-position frame. |
| `cutcap_session_pump(u64, u32* changed)` | `changed` = 1 when the view must be re-read. |
| `cutcap_session_view(u64, u8* buf, usize cap, usize* len)` | Writes `len` first; `cap < len` → `BUFFER_TOO_SMALL` without copying. The same revision is returned on retry. |
| `cutcap_session_frame_info(u64, CutCapFrameInfo*)` | `NO_FRAME` when nothing is presented. |
| `cutcap_session_copy_frame(u64, u64 generation, u8* dst, usize len, usize stride)` | Row-by-row copy into caller memory with any `stride >= width*4`. A replaced generation → `STALE_FRAME`. |

`CutCapFrameInfo` is `#[repr(C)]`, 24 bytes, 8-aligned: `u64 generation; u32 width; u32 height; u32 stride; u32 pixel_format`. `pixel_format` 1 = packed RGBA8, opaque alpha, top row first (Avalonia `PixelFormats.Rgba8888`, no conversion). Frames are at most 1280×720.

ABI 2 keeps this layout and existing function signatures/status numbers. New exports add source transport. Numeric Seek validates source readiness and the 0..24 h range; SeekText preserves grammar-error feedback before source-readiness validation. Successful copy is now a once-per-generation **delivery acknowledgment**, not a physical presentation claim. Reset/seek invalidates copying an old epoch immediately, even while its already copied bitmap remains visible. Caller lengths above isize::MAX are rejected before constructing a slice.

See [PHASE_2_PLAYBACK_CONTRACT.md](PHASE_2_PLAYBACK_CONTRACT.md) for state/seek/clock/EOF semantics. The managed wrapper exposes Play/Pause/Resume/Stop and caches LastReadView for diagnostics. Phase 2B integrated the production visual controls; Phase 3A adds document operations while preserving them.

Status codes: 0 OK, 1 INVALID_ARGUMENT, 2 INVALID_HANDLE, 3 ABI_MISMATCH, 4 BUFFER_TOO_SMALL, 5 NO_FRAME, 6 STALE_FRAME, 7 BUSY (bounded queue full), 8 FAILED, 9 PANIC.

## Ownership and lifetime rules

- **Sessions** are opaque `u64` keys in a process-wide registry (`Mutex<BTreeMap<u64, Arc<Mutex<Session>>>>`), not pointers. A forged, stale or destroyed key returns `INVALID_HANDLE`; Rust never dereferences caller-supplied session memory. `SessionSafeHandle` owns the key in .NET; SafeHandle reference counting keeps it alive across every in-flight P/Invoke, so `Dispose` cannot race a call.
- **Caller buffers** (text, paths, view, frame destination) are owned by the frontend. Rust reads or writes them only during the call, after checking null, alignment and every length; it never retains them.
- **Frames** are owned by Rust (`PresentedFrame`, replaced on each presentation). The frontend copies; it never holds a Rust pointer. `generation` ties metadata to pixels.
- **Panics** are caught at every export (`catch_unwind`); none unwinds across the ABI. A panic poisons only that session, whose later calls return `PANIC`. No .NET exception crosses into Rust (no callbacks).
- **Threads.** Call a session from one thread (the UI thread): `cutcap_last_error` is per thread. The session itself is mutex-guarded. Rust owns the command worker, the probe thread and the decoder thread; Avalonia objects are touched only on the dispatcher thread.
- **Shutdown.** The frontend stops its pump timer, then disposes the session. Destroy cancels the probe token and decoder, which kill and reap their FFmpeg/FFprobe children, then closes reliable results before requests and joins the command worker (the Phase 1 order). Measured: about 0.1 s, including during an active decode.

## View document

`SessionView` (`crates/cutcap-ffi/src/view.rs`, mirrored by `SessionView.cs`) carries the existing project/source/presentation view plus `playback`. Source.position_ms remains the requested seek position. Playback carries state/intent, epoch, requested/clock/offered/copied positions, exact offered/copied SourceTime, formatted strings, first-frame readiness latency, process ID, queue occupancy and decoded/offered/copied/dropped/stall counters. Frame gains actual SourceTime and epoch, correlated to its generation; no pixels enter JSON. Legacy metrics.frames_presented counts offers, not renders. Durations/rates/timecodes are formatted in Rust. Unknown fields are ignored, but any newly consumed field or signature/layout/status change requires a synchronized ABI revision.

## Versioning

Bump `ABI_VERSION` (Rust) and `NativeMethods.AbiVersion` (C#) together for any change to a signature, struct layout, status meaning or a view field the frontend reads. Document it here and in DECISIONS.md. The ABI is in-tree only: the DLL ships next to the frontend that was built with it, and the version check guards against a mismatched copy.

## Tests

- `crates/cutcap-ffi/src/tests.rs` (controlled backend, no FFmpeg): version mismatch, invalid/double-destroyed handles, error codes/messages, null/invalid pointers, Unicode wide paths, frame buffer/stride/generation checks, latest-seek supersession, bounded shutdown during probe/decode, failed probe, new-project reset, tracks.
- `tests/CutCap.Interop.Tests` (real DLL from .NET; media tests skip without FFmpeg): the same boundary from C#, plus real metadata, first frame pixels, Unicode paths, VFR 201 ms → PTS 700, rapid-seek stale rejection, corrupt/missing files and dispose during activity with child-process checks.

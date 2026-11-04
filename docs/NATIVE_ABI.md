# CutCap native engine ABI, version 3

## Phase 3A additions (current authority)

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

## ABI 2 foundation retained by ABI 3

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

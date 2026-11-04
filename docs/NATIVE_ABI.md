# CutCap native engine ABI, version 1

`crates/cutcap-ffi` builds `cutcap_engine.dll` (Windows x64, `extern "C"`, the platform's only x64 calling convention). The Avalonia frontend loads it through `src-dotnet/CutCap.Interop`. Source of truth: `crates/cutcap-ffi/src/abi.rs`; the C# mirror is `NativeMethods.cs`. This ABI is versioned separately from the in-process contract (`CONTRACT_VERSION` 2, unchanged) and from project schema 1.

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
| `cutcap_session_pump(u64, u32* changed)` | `changed` = 1 when the view must be re-read. |
| `cutcap_session_view(u64, u8* buf, usize cap, usize* len)` | Writes `len` first; `cap < len` → `BUFFER_TOO_SMALL` without copying. The same revision is returned on retry. |
| `cutcap_session_frame_info(u64, CutCapFrameInfo*)` | `NO_FRAME` when nothing is presented. |
| `cutcap_session_copy_frame(u64, u64 generation, u8* dst, usize len, usize stride)` | Row-by-row copy into caller memory with any `stride >= width*4`. A replaced generation → `STALE_FRAME`. |

`CutCapFrameInfo` is `#[repr(C)]`, 24 bytes, 8-aligned: `u64 generation; u32 width; u32 height; u32 stride; u32 pixel_format`. `pixel_format` 1 = packed RGBA8, opaque alpha, top row first (Avalonia `PixelFormats.Rgba8888`, no conversion). Frames are at most 1280×720.

Status codes: 0 OK, 1 INVALID_ARGUMENT, 2 INVALID_HANDLE, 3 ABI_MISMATCH, 4 BUFFER_TOO_SMALL, 5 NO_FRAME, 6 STALE_FRAME, 7 BUSY (bounded queue full), 8 FAILED, 9 PANIC.

## Ownership and lifetime rules

- **Sessions** are opaque `u64` keys in a process-wide registry (`Mutex<BTreeMap<u64, Arc<Mutex<Session>>>>`), not pointers. A forged, stale or destroyed key returns `INVALID_HANDLE`; Rust never dereferences caller-supplied session memory. `SessionSafeHandle` owns the key in .NET; SafeHandle reference counting keeps it alive across every in-flight P/Invoke, so `Dispose` cannot race a call.
- **Caller buffers** (text, paths, view, frame destination) are owned by the frontend. Rust reads or writes them only during the call, after checking null, alignment and every length; it never retains them.
- **Frames** are owned by Rust (`PresentedFrame`, replaced on each presentation). The frontend copies; it never holds a Rust pointer. `generation` ties metadata to pixels.
- **Panics** are caught at every export (`catch_unwind`); none unwinds across the ABI. A panic poisons only that session, whose later calls return `PANIC`. No .NET exception crosses into Rust (no callbacks).
- **Threads.** Call a session from one thread (the UI thread): `cutcap_last_error` is per thread. The session itself is mutex-guarded. Rust owns the command worker, the probe thread and the decoder thread; Avalonia objects are touched only on the dispatcher thread.
- **Shutdown.** The frontend stops its pump timer, then disposes the session. Destroy cancels the probe token and decoder, which kill and reap their FFmpeg/FFprobe children, then closes reliable results before requests and joins the command worker (the Phase 1 order). Measured: about 0.1 s, including during an active decode.

## View document

`SessionView` (`crates/cutcap-ffi/src/view.rs`, mirrored by `SessionView.cs`) carries `revision`, `engine_ready`, `project`, `status`, `pending`, `tracks`, `media`, `import`, `inspector` (kind/title/detail/sections), `source` (the source monitor; never the timeline playhead), `frame` (generation/size) and `metrics`. Durations, rates, rotations and timecodes are formatted in Rust, next to the exact rational arithmetic; the frontend renders them and keeps no project state. Unknown fields are ignored by the frontend, so adding a field is compatible; removing or retyping one requires `ABI_VERSION` + 1.

## Versioning

Bump `ABI_VERSION` (Rust) and `NativeMethods.AbiVersion` (C#) together for any change to a signature, struct layout, status meaning or a view field the frontend reads. Document it here and in DECISIONS.md. The ABI is in-tree only: the DLL ships next to the frontend that was built with it, and the version check guards against a mismatched copy.

## Tests

- `crates/cutcap-ffi/src/tests.rs` (controlled backend, no FFmpeg): version mismatch, invalid/double-destroyed handles, error codes/messages, null/invalid pointers, Unicode wide paths, frame buffer/stride/generation checks, latest-seek supersession, bounded shutdown during probe/decode, failed probe, new-project reset, tracks.
- `tests/CutCap.Interop.Tests` (real DLL from .NET; media tests skip without FFmpeg): the same boundary from C#, plus real metadata, first frame pixels, Unicode paths, VFR 201 ms → PTS 700, rapid-seek stale rejection, corrupt/missing files and dispose during activity with child-process checks.

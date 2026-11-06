# CutCap architecture — Avalonia frontend over the Rust engine

Phase 3C current addition: **ABI/contract 5**, exact single-video-track timeline preview. `core/timeline.rs` owns the sorted lookup index and rational source mapping; `ffi/engine.rs` answers small correlated evaluations without ProjectSnapshot clones; `ffi/timeline_preview.rs` owns one latest evaluation/decode/frame and presentation state. `ffi/media.rs` routes timeline work through the existing isolated decoder, pausing and retaining source transport independently. One session-wide frame counter and the original Avalonia FrameView/bitmap serve both modes. See PHASE_3C_TIMELINE_PREVIEW_CONTRACT.md. No timeline Play, audio or multi-track composition.

Since the Avalonia migration (docs/AVALONIA_MIGRATION_REPORT.md) the frontend is C#/.NET 10 + Avalonia 12; Rust keeps editing semantics, media processing, project state and time arithmetic. Phase 2B integrated production source transport. Phase 3A adds revisioned `.cutcap` persistence/relinking and startup preroll stabilization through ABI 3. Phase 3B adds Rust-owned clip insertion/removal/selection and bounded undo/redo through ABI 4 (ADR 020). The Slint frontend is retired; its last version is commit a3de692 on `claude/phase-2a-ui`.

## Workspace and ownership

```mermaid
flowchart LR
  Desktop[CutCap.Desktop: Avalonia views / view models / frame presentation] --> Interop[CutCap.Interop: P/Invoke, SafeHandle, view DTOs]
  Interop -- C ABI v5, cutcap_engine.dll --> Ffi[cutcap-ffi: session host / view mapping / ABI]
  Ffi --> Core[cutcap-core: validated project]
  Ffi --> Media[cutcap-media: replaceable media adapter]
  Ffi --> Contracts[cutcap-contracts: IDs / exact time / DTOs]
  Core --> Contracts
  Media --> Contracts
```

Core remains independent of GUI, codecs, GPU, filesystem and threading. Contracts are a leaf. `cutcap-ffi` owns the session (command worker, media host), derives display DTOs and is the only crate with `unsafe` (isolated in `abi.rs`). The .NET side holds no project state: every displayed value comes from the latest engine view, and every action is an engine call.

| Module | Responsibility |
|---|---|
| contracts/ids.rs, time.rs | Distinct non-nil UUID identities; exact source/project clocks; checked floor/ceil rescaling |
| contracts/media.rs, messages.rs, document.rs, timeline.rs | Validated metadata, commands, snapshots, source identity and document status; contract version 5 |
| core/project.rs | Schema 1 validated project graph and atomic mutations |
| core/timeline.rs | Per-video-track sorted clip index, half-open lookup, single-rounding exact source mapping and endpoint validation |
| ffi/timeline_preview.rs | Latest transient seek/evaluation, model/request generation gates, blank/unavailable/no-frame states and bounded frame ownership |
| media/backend.rs, probe.rs | MediaBackend contract, FFmpeg policy and bounded metadata parsing |
| media/process.rs, cancel.rs | Owned child processes, bounded pipe capture, timeout, cancellation and reaping |
| media/frame.rs, latest.rs | Validated opaque RGBA8; bounded latest-request/result mailbox |
| media/stream.rs, persistent.rs, clock.rs | Framed persistent decode, three-frame queue, latest epoch/reset and monotonic clock |
| ffi/playback.rs | Source transport, two-frame lookahead, due/drop/buffering/EOF and copied-position accounting |
| ffi/engine.rs, bridge.rs | Single project owner, document revisions/adoption and reliable bounded channels |
| ffi/persistence.rs | Bounded format 1 loading, same-directory synced temporary writes/replacement, source fingerprints and owned file tasks |
| ffi/media.rs | Probe job, latest decoder, ticket-gated presentation, latest frame (ported from media_ui/preview) |
| ffi/session.rs | Request bookkeeping and pump (ported from the Slint coordinator) |
| ffi/view.rs | View DTOs and exact-time formatting; tested source-time grammar |
| ffi/abi.rs | Versioned C ABI: handle registry, panic barrier, buffer validation (docs/NATIVE_ABI.md) |
| ffi/bin/gpu_info.rs | Optional DX12 adapter enumeration (`--features gpu-diagnostics`) |
| Interop/EngineSession.cs | Managed owner: ABI check, typed calls, view reads, frame copy into caller memory |
| Desktop/Services/EngineHost.cs | UI-thread pump (16 ms timer; plus one pump per compositor frame while playing, ADR 017), view/frame notifications, command rejection → status |
| Desktop/ViewModels/Transport.cs | Pure ABI 2 state → transport controls/readout mapping; throttled scrub-to-seek controller |
| Desktop/ViewModels | MVVM over the view; in-place list sync preserves item identity and focus |
| Desktop/Controls | IconView, FrameView (bitmap presenter), TimelineScale/Ruler/Lanes (custom-drawn), TimelineGeometry (clip layout, culling, hit-testing) |

## Lifecycle and data flow

App creates EngineHost, the main window and its view models, then starts the session: cutcap_session_create verifies ABI 5, starts command/isolated/persistent workers and queues NewProject (two real empty tracks). A 16 ms UI timer (and, while source playback is active, a `TopLevel.RequestAnimationFrame` callback once per compositor frame) drains at most eight reliable events, polls media work and selects at most one offered frame from bounded lookahead. Decode, filesystem/process I/O and waits stay off pump. Frame copying records delivery, not scanout. See PHASE_2_PLAYBACK_CONTRACT.md for transport and PROJECT_FORMAT.md for file operations. The window initially fits the current display's logical work area; user resize/maximize then retains normal Windows behavior.

Import opens Avalonia's native asynchronous picker (`StorageProvider`). The chosen path crosses the ABI as UTF-16. One probe thread canonicalizes/validates the file and invokes FFprobe while capturing its full source fingerprint under a Windows read-sharing lock. The owner validates and commits RegisterMedia; only its successful snapshot adds a library entry and starts preview. Reopened/relinked sources reuse this single worker, compare saved expectations and remain unavailable until verified.

Source selection is independent of timeline Track/Clip selection. Relative whole milliseconds are converted in Rust with checked rational ceil rescaling into source ticks, then added to start PTS. Source commands do not move the timeline playhead; only Rust timeline evaluation updates it. Every request carries `PreviewTicket { project_id, media_id, request_id }`. One active decode, one replaceable pending request and one replaceable outcome bound the backlog; the decoder and the session's presentation gate independently reject stale successes and errors.

Frame presentation: the session keeps the latest validated RGBA8 frame with a generation number. `FrameView` asks for the frame info and has the engine copy the pixels once, row by row, straight into a reused `WriteableBitmap` (RGBA8888, opaque, no conversion), then invalidates itself. Frames never pass through view-model property notifications. One full-frame copy per presentation; Avalonia's compositor may upload/copy further. No zero-copy claim.

Shutdown: after the window closes, stop pumping and dispose the session. Cancel decoders (kill/reap children), close reliable results before requests and join the cheap command owner. Source inspection waits at most 250 ms for completion; a task still in an uninterruptible OS/backend call detaches with only owned data/token, as can a cancelled file task. Normal tasks complete/join; detached tasks cannot adopt state or touch UI. PROJECT_FORMAT.md records the precise limits; universal termination of OS-stalled I/O is not claimed.

## Domain and compatibility

Project schema remains 1: version, ID, name, rational timebase, tracks, source references and clips. Mutations validate before commit; deserialization validates a complete temporary graph. Unknown fields/versions, invalid names/IDs/durations/endpoints, duplicates, missing references, kind mismatches, negative starts and same-track overlaps fail. Core limits are 128 tracks, 10,000 references, 100,000 clips; the application/document cap is 128 assets. File loading adds a 16 MiB byte budget and structural preflight before typed allocation. See PROJECT_FORMAT.md for precise limits.

No decoded buffers, native handles, runtime metadata caches, selections or layout are serialized. Envelope format 1 adds source metadata expectations/fingerprints outside Project, and re-probes after adoption. Schema 1 does not model editable per-stream audio. One file worker owns the captured snapshot; the project owner alone adopts documents and records revisions. Edits during saving remain dirty. Failed Open/Save preserves working state. Relative references, missing/changed states, relink, replacement guarantees and detached-task shutdown limits are specified in PROJECT_FORMAT.md. Contract 3 is an in-process protocol, separate from native ABI 3.

Timeline ticks use the project clock (default 1/720000 s), never preview frame rate. SourceTime is signed, distinct and carries its own rational timebase. Rescaling uses checked i128 intermediates and explicit floor/ceil. VFR selection uses real PTS, never nominal frame-rate indexing.

## Media and presentation boundary

MediaBackend accepts path or DecodeRequest plus Cancellation and returns typed probe/frame results. Future libavcodec/libavformat or hardware implementations can replace FfmpegBackend without changing core or importing codecs into the frontend. They must preserve cancellation, timestamp and buffer semantics.

The subprocess adapter is tested with external FFmpeg/FFprobe 9.0.1, invoked directly with argument arrays (Unicode/spaces tested), stdin null and no console. No executable is copied or bundled. The frame policy is **first original PTS at or after request**. One-second demux preroll with -copyts -noaccurate_seek -seek_timestamp 1 plus explicit PTS selection preserves nonzero starts. Automatic accurate-seek discard was disabled after the offset regression fixture exposed incorrect selection.

The first showinfo output supplies actual integer PTS and explicit filter timebase. A per-request unpredictable filter name/exact log prefix prevents media tags from impersonating timestamp records; a forged-title fixture tests this boundary. Missing PTS/no frame/invalid bytes fail explicitly. Requests after the last PTS may fail before nominal duration. CFR/VFR/offset fixtures establish tested behavior, not frame-accuracy guarantees for arbitrary codecs. Textual showinfo parsing is a compatibility risk requiring upgrade tests.

FFmpeg auto-orients quarter-turn matrices. DAR/SAR and rotation determine a square-pixel preview fit within 1280×720, without upscaling square-pixel sources. Rotated content and dimensions are tested. SDR converts through RGB24 to opaque RGBA8 (alpha discarded), top row first and packed width×4 stride. HDR/PQ/HLG/BT.2020, reflected matrices and non-quarter turns are rejected. Unknown SDR color metadata uses FFmpeg defaults; full color management and HDR grading are absent.

## Budgets

| Resource | Bound |
|---|---|
| Project request/result queues | 32 each; non-blocking submit and ≤8 events per 16 ms pump |
| Imports | One probe/picker, ≤128 registered metadata assets |
| Persistence | One captured file task; project bytes ≤16 MiB, structural preflight; one retained verification result; 1 MiB source-hash scratch buffer |
| Timeline preview | One desired seek + one in-flight owner query, existing latest decoder, one <=3,686,400-byte timeline image; cached source image may coexist, one frontend bitmap |
| Decode | One active, one pending, one completed outcome |
| Preview | ≤1280×720 RGBA8; ≤3,686,400 bytes/frame; exact stride/length/alpha |
| Input | Local nonempty regular file ≤32 GiB; path text ≤32,768 bytes |
| Source | Edges ≤4096, pixels ≤4096×2160, reported duration ≤24 h, ≤64 streams |
| Probe output/error | 1 MiB / 64 KiB; decode error output ≤1 MiB |
| Process | Probe 15 s / decode 20 s; 2 decoder threads, 1 filter/output thread; max single allocation 64 MiB |
| Inspection | 8 MiB probe input; 5 s analyzed media duration; file/pipe protocols |
| Diagnostics | ≤256 presentation samples; ≤128 probe timing records |

The previous session frame, an incoming decoded frame and the frontend bitmap can coexist (about three preview buffers, 11,059,200 bytes). Renderer caches and source/reference surfaces are additional. These budgets do not cap aggregate FFmpeg RSS or constitute an OS sandbox. File canonicalization/metadata calls run off UI but cannot be interrupted by the process timeout; pathological local filesystems, reparse/device paths and codec behavior need future hardening.

## UI and next rendering decision

The Avalonia shell (DESIGN.md) has a custom title bar, native caption buttons, an asset workspace, Source/Timeline monitor, inspector and full-width timeline. Panels use splitters/minimum constraints; container queries adapt labels. Append/remove/select/undo/redo and exact single-video-track timeline seeking are enabled. Audio/Text/Stickers/Effects/Transitions/Filters/Adjustment, clip inspector settings, Split, Export, continuous Timeline Play and track visibility/mute/lock remain honest/disabled. Native pickers provide persistence/relink; charcoal/cyan styling is preserved.

The timeline ruler and lanes are custom-drawn controls that draw only the viewport; `TimelineScale` keeps time ↔ pixel conversion independent of rendering and is unit-tested. Track headers are a list with keyboard selection through the engine. Clips are drawn by the lanes control itself (no control per clip): `TimelineGeometry` keeps per-track clip lists sorted by start, so the visible clips of the scroll viewport are found by binary search and pointer hits are resolved the same way. Clip rows come from the separately fetched timeline document (fetched only when `timeline_generation` changes), carry floored display milliseconds and Rust-formatted strings, and are never edited in C#. Missing/changed sources draw with a coral dashed outline and a "Missing ·" prefix.

## Clip editing and history (Phase 3B)

The command owner is the only editor. InsertClip appends the whole Ready source; RemoveClip leaves media untouched; selection is transient. `history.rs` keeps 200 exact edits with identity/order/revision restoration. Failed edits and selection never enter history; New/Open reset it. The Phase 3C evaluator uses the same exact Clip values and invalidates independently of undoable saved revisions. Source mapping and endpoint policy are in PHASE_3C_TIMELINE_PREVIEW_CONTRACT.md. Continuous timeline playback is the next milestone.

wgpu 30.0.1 remains an optional DX12 diagnostic. The CPU frame path is measured (docs/measurements, AVALONIA_MIGRATION_REPORT.md). GPU interop with Avalonia's compositor is deliberately not attempted until playback measurements show the CPU path is the bottleneck; any experiment must keep the CPU fallback.

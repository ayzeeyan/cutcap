# UI–engine and media contracts, version 3

## Phase 3A current contract

CONTRACT_VERSION and native ABI are 3. The Phase 2 source transport remains separate from timeline time and retains its pixel/time ownership. Project schema stays 1; `.cutcap` envelope format 1 is specified in PROJECT_FORMAT.md.

SaveProject { optional path } captures a complete validated Project/source-expectation snapshot and owner revision. OpenProject { path } reads/validates separately before owner adoption. Both emit nonterminal Updated snapshots, then exactly one Completed or FileFailed terminal while the connection is live. FileFailed includes the unchanged live snapshot plus error/status; it is not a successful mutation. Busy/invalid preconditions use Failed. The existing request/result queues remain capacity 32 and pump drain <=8. Exactly one file operation exists; source probes/decoders remain isolated.

ProjectSnapshot adds DocumentStatus and ordered MediaAvailability rows. Track summaries include per-track saved clip counts. Document dirty derives from owner revision versus successfully written revision, not command acceptance or UI edits. Selection/seek/playback/re-probe do not dirty the document. New/Open replacement rejects in-flight file operations; mutations reject while opening, and can continue while saving. Newer mutations cannot be marked saved by an older snapshot's completion.

RegisterMedia now carries an optional source fingerprint; real imports always capture the entire file's SHA-256/length under a Windows read-sharing lock while probing. Saved metadata/fingerprints become document expectations outside core. After Open, the single probe checks one reference at a time without blocking adoption or decoding on UI. VerifyMedia transfers the scoped result to the owner. A bounded one-result session slot retries reliable submission on queue saturation; frames never use that slot. Matching terminal RequestId, ProjectId/MediaId, cancelled/abandoned probe state and source generations prevent outdated work from changing a replacement, including reopening the same ProjectId.

Relink uses the same inspection path, requires original fingerprint and metadata, preserves media identity/order/clips, and changes the path atomically through core only after validation. Missing/changed/unavailable sources remain in the document/library and cannot preview until Ready. Failed candidates leave the original reference intact. Selection/layout remain transient. Filesystem and shutdown guarantees/limits are in PROJECT_FORMAT.md; no universal crash-safe/power-loss claim applies.

Source playback readiness is refined by ADR 019: playing/playing-seek preroll requires an actual successor frame or EOF before anchoring the first frame. Paused source requests still need one frame. No nominal frame interval or UI-delay workaround is used; actual starvation still enters Buffering and freezes the clock. The Phase 2 playback contract below otherwise remains binding.

## Retained media/command foundation (read with the additions above)

cutcap-contracts is the Rust source of truth, reached through ABI 3 in NATIVE_ABI.md. Source Play/Pause/Stop and continuous seek/clock/frame delivery are specified in PHASE_2_PLAYBACK_CONTRACT.md with the Phase 3A preroll amendment. They never move the project timeline playhead.

## Reliable commands

CommandEnvelope { request_id, project_id, command } transfers owned values. Fresh non-nil RequestIds must not be reused. All commands except NewProject require the active ProjectId. Stale scope/invalid operations preserve previous state. NewProject atomically replaces a valid project, creates two empty tracks and clears session selections/metadata; invalid replacement preserves the old project.

A session submit rejection (try_send Busy/Internal, surfaced over the ABI as BUSY/FAILED) means not accepted. Each accepted command produces one terminal Completed, Failed or FileFailed while transport stays live; Updated is nonterminal. Teardown may abandon requests; no cross-process durable delivery/deduplication is promised. Terminal pending IDs are removed and cleared on disconnection.

| Command | Current behavior |
|---|---|
| NewProject, GetSnapshot | In-memory replacement / snapshot |
| AddTrack, Select | Validated empty track creation / None, Track or Clip timeline selection |
| RegisterMedia { boxed MediaSummary } | Validate DTO again; atomically add core MediaAsset, cache metadata and select source; reject duplicate path/ID or >128 imports |
| SelectMedia { media_id } | Validate registered source; update selection independently of timeline selection |
| OpenProject, SaveProject | Validated background adoption / captured revision save, as specified above |
| VerifyMedia | Reliable source verification/relink result; owner validates original identity before mutation |
| ImportMedia { paths } | Batch operation Unsupported; UI import uses the single probe below |
| RemoveClip, Seek(TimelineTime), SetPlayback, Undo, Redo | Unsupported; source seek is separate |
| StartExport, Cancel { target } | Reserved Unsupported; media uses a concrete Cancellation token |

ProjectSnapshot owns ID/name/timebase, track summaries, counts, timeline SelectionState, bounded MediaSummary rows and optional source_media ID. It is a session view DTO, not persistence data. Metadata includes codec, coded size, pixel format, DAR/SAR, rotation, average/nominal rates, timebase/start/duration, estimate flag, container, video index/container ID, audio indices and available color fields. Unknown optional values remain None; invalid required values fail import. Decoded bytes never travel in reliable events.

EngineError carries Unsupported, InvalidInput, NotFound, StaleProject, Busy or Internal and a message. Failures appear in status/logs, never as success.

## Import

The Avalonia frontend owns the native, window-parented asynchronous file picker (StorageProvider); dialog Cancel returns no path and registers nothing. The chosen path crosses the ABI as UTF-16. The session owns one cancellable probe task; probe cancellation signals its token, and the session joins only after is_finished(), never in a UI call. 

MediaBackend::probe(path, cancellation) runs off UI and returns ProbeResult with owned canonical path, validated metadata and subprocess duration. No Project mutation occurs there. The session checks captured ProjectId/cancellation and submits RegisterMedia with an existing typed MediaId. Only the successful owner's snapshot adds a library row and requests its initial preview. New Project invalidates older dialog/probe work; another import waits for the sole probe to finish cancellation.

Core MediaAsset stores path/ID and kind; UI video import registers Video. Audio presence is metadata, not an editable per-stream reference. Runtime metadata is session-only; document expectations/fingerprints are separate and revalidated on reopening. No file watcher exists. A missing/replaced source at seek follows the existing typed failure path.

## Source frame requests

The replaceable MediaBackend trait imports no UI/core types. DecodeRequest owns:

- PreviewTicket { project_id, media_id, request_id }, using existing stable/request IDs.
- A MediaSummary and requested SourceTime in the selected stream's exact rational clock.
- Cancellation is independently passed by reference to the worker-owned token.

UI whole milliseconds are relative to source start. Checked ceil rescaling and checked start-PTS addition form the request. Slider doubles in the frontend are presentation only; the ABI takes integer milliseconds. SourceTime is distinct from TimelineTime; no nominal frame-rate multiplication occurs.

Policy: **first actual PTS at or after request**, not nearest/floor/current display interval. A reliable integer stream start_pts is required; a rounded decimal start_time is not presented as observed source PTS. Demux preroll plus explicit filtering is tested against CFR, VFR and positive-offset media. A time between VFR PTS 200 and 700 selects 700. After the last PTS, NoFrame is valid even before nominal duration; estimated duration and slider endpoint do not guarantee a frame.

DecodedFrame owns ticket, requested/actual SourceTime, validated RgbaFrame, subprocess duration and reaped child PID for diagnostics. Actual PTS/timebase come from the first emitted FFmpeg showinfo frame; requested time is never substituted. The filter is named with the fresh, unpredictable RequestId, and parsing accepts only its exact context prefix. Generic showinfo text in untrusted media tags is ignored; the CFR fixture deliberately carries a forged timestamp. Missing PTS fails. Actual must be at/after the request after explicit ceil rescaling. Further lookahead showinfo records are ignored. CLI log parsing must be retested on FFmpeg upgrades.

RgbaFrame private storage is top-down packed RGBA8, straight **opaque** alpha 255, width 1..1280, height 1..720, stride exactly width×4 and exact byte length ≤3,686,400. Constructors validate before presentation. FFmpeg handles SDR conversion/quarter-turn orientation; HDR, reflection/non-quarter turns and color-critical accuracy are unsupported.

## Latest results and cancellation

LatestDecoder holds one active request/token, one replaceable pending request and one completed DecodeOutcome { ticket, result }. Its small mutex/condition variable owns no Project or UI objects. Idle workers sleep. A new request cancels active work, replaces pending work, clears old completed data and installs the desired ticket.

The worker publishes only if ticket still matches desired, cancellation is false and shutdown has not begun. The session's presentation gate independently checks its expected ticket for **successes and failures**. New selection immediately invalidates presentation; queued selection snapshots start decoding only when they match the UI's latest desired media. Cancel clears pending/completed work and expected ticket, retains the last valid image and reports cancellation. Another seek remains possible.

This mailbox intentionally coalesces obsolete **ephemeral preview outcomes**, not project mutations/import registration/durable export terminal results. Those retain reliable paths. MediaError distinguishes Cancelled, Timeout, Invalid, Unsupported, Budget, Process(stage/code/detail), Io(stage/source), NoFrame and Worker. Probe terminal results stay in its JoinHandle until consumed. Current preview errors are surfaced; no invented progress percentage is shown. Changing source clears its old image; a failed seek retains the selected source's last valid frame.

## Threads and shutdown

- The Avalonia UI thread owns views, view models, the pump timer and the frame bitmap. It calls the session only through short non-blocking ABI functions and pulls the view/frame; no native callbacks exist.
- The project worker solely owns Engine/Project and session selections. Two channels each hold 32; pump drains ≤8 per 16 ms tick; worker result sends apply backpressure.
- One probe thread owns one FFprobe process. One decoder worker owns one FFmpeg process. Each process has two bounded pipe readers.
- Direct argument spawning uses stdin null/no console. Probe timeout 15 s, decode 20 s; cooperative condition-variable wake/check ≤10 ms. Error/cancel/timeout paths kill/reap before reader joins. This configuration launches no helper children.
- On close, stop timer, cancel/join media, drop reliable results before requests and join project worker. Tests cover saturated queues, active child termination/reaping and decoder shutdown.

See ARCHITECTURE.md for byte/source/input budgets and filesystem/OS limitations. These are not hard real-time cancellation or a hostile-codec sandbox. No media buffer or codec pointer crosses into core/contracts serialization.

## Future extensions

Source playback is implemented; audio and timeline evaluation remain future work. The UI pump is not the playback clock. Export terminal results will need a reliable job path separate from preview frames. Changing color/alpha, frame selection, queues, document schemas or public compatibility requires a decision and regression evidence.

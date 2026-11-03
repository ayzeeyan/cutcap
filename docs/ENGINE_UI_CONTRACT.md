# UI–engine and media contracts, version 2

cutcap-contracts is the Rust source of truth. CONTRACT_VERSION is 2; Project schema stays 1. This is an in-process typed protocol, not a plugin/network ABI. Phase 1 adds RegisterMedia, SelectMedia, metadata DTOs and media snapshot fields. Producer, consumer, tests and docs change together.

## Reliable commands

CommandEnvelope { request_id, project_id, command } transfers owned values. Fresh non-nil RequestIds must not be reused. All commands except NewProject require the active ProjectId. Stale scope/invalid operations preserve previous state. NewProject atomically replaces a valid project, creates two empty tracks and clears session selections/metadata; invalid replacement preserves the old project.

UI try_send rejection (Busy/Internal) means not accepted. Each accepted command produces exactly one terminal Completed(snapshot) or Failed(typed EngineError) while transport stays live. Teardown may abandon pending requests; no durable delivery/retry/deduplication promise exists. The coordinator removes terminal pending IDs and clears them on disconnection.

| Command | Current behavior |
|---|---|
| NewProject, GetSnapshot | In-memory replacement / snapshot |
| AddTrack, Select | Validated empty track creation / None, Track or Clip timeline selection |
| RegisterMedia { boxed MediaSummary } | Validate DTO again; atomically add core MediaAsset, cache metadata and select source; reject duplicate path/ID or >128 imports |
| SelectMedia { media_id } | Validate registered source; update selection independently of timeline selection |
| OpenProject, ImportMedia { paths } | Reserved/batch operations Unsupported; UI import uses app-owned probing below |
| RemoveClip, Seek(TimelineTime), SetPlayback, Undo, Redo | Unsupported; source seek is separate |
| StartExport, Cancel { target } | Reserved Unsupported; media uses a concrete Cancellation token |

ProjectSnapshot owns ID/name/timebase, track summaries, counts, timeline SelectionState, bounded MediaSummary rows and optional source_media ID. It is a session view DTO, not persistence data. Metadata includes codec, coded size, pixel format, DAR/SAR, rotation, average/nominal rates, timebase/start/duration, estimate flag, container, video index/container ID, audio indices and available color fields. Unknown optional values remain None; invalid required values fail import. Decoded bytes never travel in reliable events.

EngineError carries Unsupported, InvalidInput, NotFound, StaleProject, Busy or Internal and a message. Failures appear in status/logs, never as success.

## Import

The UI owns one asynchronous rfd Windows dialog future and one cancellable probe task. The dialog is parented using Slint's public raw-window-handle API. Native Cancel returns no path. Active probe cancellation signals its token; the UI joins only after is_finished(), never waits in a callback.

MediaBackend::probe(path, cancellation) runs off UI and returns ProbeResult with owned canonical path, validated metadata and subprocess duration. No Project mutation occurs there. The UI checks captured ProjectId/cancellation and submits RegisterMedia with an existing typed MediaId. Only the successful owner's snapshot adds a library row and requests its initial preview. New Project invalidates older dialog/probe work; another import waits for the sole probe to finish cancellation.

Core MediaAsset stores original path/ID and Video kind. Audio presence is metadata, not an editable per-stream reference. Metadata is session-only; no file watcher/persistence exists. A missing/replaced file at seek yields a typed failure. Future reopen must re-probe/fingerprint instead of trusting cached metadata.

## Source frame requests

The replaceable MediaBackend trait imports no Slint/core types. DecodeRequest owns:

- PreviewTicket { project_id, media_id, request_id }, using existing stable/request IDs.
- A MediaSummary and requested SourceTime in the selected stream's exact rational clock.
- Cancellation is independently passed by reference to the worker-owned token.

UI whole milliseconds are relative to source start. Checked ceil rescaling and checked start-PTS addition form the request. Slider floats are presentation only. SourceTime is distinct from TimelineTime; no nominal frame-rate multiplication occurs.

Policy: **first actual PTS at or after request**, not nearest/floor/current display interval. A reliable integer stream start_pts is required; a rounded decimal start_time is not presented as observed source PTS. Demux preroll plus explicit filtering is tested against CFR, VFR and positive-offset media. A time between VFR PTS 200 and 700 selects 700. After the last PTS, NoFrame is valid even before nominal duration; estimated duration and slider endpoint do not guarantee a frame.

DecodedFrame owns ticket, requested/actual SourceTime, validated RgbaFrame, subprocess duration and reaped child PID for diagnostics. Actual PTS/timebase come from the first emitted FFmpeg showinfo frame; requested time is never substituted. The filter is named with the fresh, unpredictable RequestId, and parsing accepts only its exact context prefix. Generic showinfo text in untrusted media tags is ignored; the CFR fixture deliberately carries a forged timestamp. Missing PTS fails. Actual must be at/after the request after explicit ceil rescaling. Further lookahead showinfo records are ignored. CLI log parsing must be retested on FFmpeg upgrades.

RgbaFrame private storage is top-down packed RGBA8, straight **opaque** alpha 255, width 1..1280, height 1..720, stride exactly width×4 and exact byte length ≤3,686,400. Constructors validate before presentation. FFmpeg handles SDR conversion/quarter-turn orientation; HDR, reflection/non-quarter turns and color-critical accuracy are unsupported.

## Latest results and cancellation

LatestDecoder holds one active request/token, one replaceable pending request and one completed DecodeOutcome { ticket, result }. Its small mutex/condition variable owns no Project or Slint objects. Idle workers sleep. A new request cancels active work, replaces pending work, clears old completed data and installs the desired ticket.

The worker publishes only if ticket still matches desired, cancellation is false and shutdown has not begun. The UI independently checks its presenter's expected ticket for **successes and failures**. New selection immediately invalidates presentation; queued selection snapshots start decoding only when they match the UI's latest desired media. Cancel clears pending/completed work and expected ticket, retains the last valid image and reports cancellation. Another seek remains possible.

This mailbox intentionally coalesces obsolete **ephemeral preview outcomes**, not project mutations/import registration/durable export terminal results. Those retain reliable paths. MediaError distinguishes Cancelled, Timeout, Invalid, Unsupported, Budget, Process(stage/code/detail), Io(stage/source), NoFrame and Worker. Probe terminal results stay in its JoinHandle until consumed. Current preview errors are surfaced; no invented progress percentage is shown. Changing source clears its old image; a failed seek retains the selected source's last valid frame.

## Threads and shutdown

- UI/main owns Slint components/models/weak upgrades, dialog polling and image creation/assignment. Rc<RefCell<...>> stays local.
- The project worker solely owns Engine/Project and session selections. Two channels each hold 32; UI drains ≤8 per 33 ms tick; worker result sends apply backpressure.
- One probe thread owns one FFprobe process. One decoder worker owns one FFmpeg process. Each process has two bounded pipe readers.
- Direct argument spawning uses stdin null/no console. Probe timeout 15 s, decode 20 s; cooperative condition-variable wake/check ≤10 ms. Error/cancel/timeout paths kill/reap before reader joins. This configuration launches no helper children.
- On close, stop timer, cancel/join media, drop reliable results before requests and join project worker. Tests cover saturated queues, active child termination/reaping and decoder shutdown.

See ARCHITECTURE.md for byte/source/input budgets and filesystem/OS limitations. These are not hard real-time cancellation or a hostile-codec sandbox. No media buffer or codec pointer crosses into core/contracts serialization.

## Future extensions

Playback needs persistent bounded decoding, real frame durations, presentation clock and audio policy; the 33 ms coordination timer is not a playback clock. Export terminal results need a reliable job path separate from the latest-preview mailbox. Persistence must bound bytes/allocations and migrate/validate before Project replacement. Changing color/alpha, frame selection, queues or public compatibility requires a decision and regression evidence.

# Source playback contract — ABI 2

Status: engine/interop implementation contract for the frontend owner to review. The owner will relay it to Claude; agreement from Claude is not claimed. This document defines the engine-facing interface before shared signatures change. Avalonia XAML, visual controls and styling remain frontend-owned. Project schema 1 and project-command contract 2 stay unchanged; source playback is independent of the timeline.

## Commands and acceptance

New nonblocking exports: `cutcap_session_play(u64)`, `cutcap_session_pause(u64)`, `cutcap_session_stop(u64)`. Managed methods: `EngineSession.Play()`, `Pause()`, `Resume()` (Play alias), `Stop()`. Existing Seek/SeekText, selection, cancellation and frame-copy signatures remain. ABI_VERSION and NativeMethods.AbiVersion increase together to 2.

OK means the session accepted a control transition / queued decoder work. It does **not** mean a frame decoded, a bitmap changed, playback reached a requested time or a display scanned out pixels. Source-not-ready is INVALID_ARGUMENT. Process/parser/decode errors arrive asynchronously in the view; queue/disconnection errors are immediate typed failures. Repeated Play/Pause are idempotent.

| Input | Behavior |
|---|---|
| Select/import source | Cancel prior transport/generations, clear old image, request initial isolated frame at source-relative zero; selected source is stopped |
| Play / Resume | Start a persistent decode epoch from current requested source position, or resume the existing paused epoch without relaunch; at Ended restart from zero |
| Pause | Freeze the monotonic source clock; keep current image and bounded decoder preroll. Pausing before first-frame readiness remains paused when it arrives |
| Seek while playing | Cancel/reset decoder, invalidate old epoch, retain last image, decode first PTS >= target; resume clock only after first new frame is ready |
| Seek while paused | Same reset, but present the first matching frame and remain paused |
| Seek while stopped | Preserve Phase 1 isolated-frame fallback and ceiling PTS policy |
| Stop | Cancel persistent decode, invalidate epoch, stop clock and request the zero-position isolated frame. Do not claim the zero frame is displayed before delivery |
| Cancel preview | Cancel both pending isolated work and source transport; keep last image, stop advancement and report cancellation |
| New project / source replacement | Invalidate all source-scoped work and delivery generations immediately; stale successes/errors cannot affect replacement |
| Decode/parser/process failure | Failed state, clock frozen, bounded typed error; retain last valid selected-source image and allow a new seek/Play |

## State and time

`SessionView.playback` is a new object. State is one of `stopped`, `starting`, `playing`, `paused`, `seeking`, `buffering`, `ended`, `failed`. It carries desired-playing intent, epoch/request identity, requested position, source clock position, last offered PTS, last successfully copied PTS, process ID and counters (decoded, offered, copied, dropped, buffer stalls).

All authoritative positions use integer source PTS and explicit rational timebase. Millisecond fields/strings are derived in Rust. A steady `Instant` clock anchors at the first matching decoded frame's actual PTS, never nominal fps. Pausing removes wall time from the clock. Resume preserves the paused source position. A reset/seek invalidates its old clock anchor. Video-only clock ownership is explicit; an audio master can replace it in a later contract.

The first matching frame follows the existing ceiling rule, including VFR. Subsequent frames use their actual PTS; nominal/average frame rate never generates timestamps. The newest frame due at the sampled clock wins; older due frames are counted as dropped. Future frames are held. A starved decoder holds the last image, enters Buffering and freezes the clock; recovery reanchors at the frozen source time, preserving VFR gaps.

EOF is distinct from Ended. EOF means decoding finished successfully. Ended requires all queued frames to become due and the final offered frame to be copied. It holds that final frame and reports its actual PTS. Last-frame duration is not guessed from fps, so position can precede reported container duration. No automatic looping, audio, timeline playback or export.

## Readiness versus delivery versus display

1. **Decoded:** a validated frame plus indexed PTS record is available on a bounded worker queue.
2. **Offered:** the scheduler chose a due frame and published a new frame generation for the frontend to copy.
3. **Copied:** `cutcap_session_copy_frame` successfully copied that exact generation to caller-owned memory. The engine records its PTS once; repeat copies do not inflate delivered-frame counts. This is a delivery acknowledgment, not evidence of physical display.
4. **Rendered / scanned out:** only the frontend/compositor can observe this. Engine metrics must never call decoded or copied FPS physical presented FPS. Claude should retain FrameView render instrumentation for actual presentation measurements.

`source.position_ms` retains its Phase 1 **requested position** meaning. Playback copied position and clock are separate fields. Claude must bind a playback display to the explicitly chosen copied/clock field, and must not run a frontend timer that invents media position. Existing FrameView can continue pulling by generation unchanged. Frame buffers stay outside JSON and view-model properties.

`CutCapFrameInfo` remains the 24-byte v1 layout. `view.frame` gains source PTS/timebase and epoch metadata, tied to the same generation. Stale copy requests remain STALE_FRAME; replacement clears old-generation access. SessionSafeHandle, registry-key sessions, one dispatcher-thread caller and no native callbacks remain mandatory.

## Decoder, queues and cleanup

One persistent worker owns at most one active process and one replaceable pending reset request. Decoded queue capacity is three frames (each <=3,686,400 bytes); scheduler lookahead is two. Reliable current-epoch completion/error is separate from pixels. Queue saturation applies backpressure. Pause does not grow memory or spin. Cancellation wakes blocked producers and terminates/reaps the child before joining readers. Windows children use a one-process kill-on-close job and 1 GiB commit cap. No ABI command/pump joins an active worker or performs filesystem I/O.

Selected backend experiment: PAM `image2pipe` has a bounded P7 header and exactly sized RGBA payload. A separate machine-readable FFmpeg `stats_mux_pre` record contains per-epoch nonce, packet index, integer PTS, rational timebase and encoded packet byte size. Only one video stream/PAM packet per image is allowed. Pair in strict index order and validate header+payload byte size against the record; missing/extra/malformed/mismatched records are failures, never guessed timing. Header/record/error bytes, dimensions, timestamps and stalls are bounded. Free-form showinfo is not used for continuous frames. Per-frame fallback remains Phase 1's adapter.

Native FFmpeg libraries would avoid transport copies but need a reviewed Windows DLL/build/ABI/distribution arrangement absent from this development environment. No FFmpeg executable/library is copied, downloaded or bundled. The external GPL-enabled 9.0.1 development runtime remains subject to LICENSE_REVIEW.md; subprocess use does not automatically clear licensing.

## Claude integration checklist

Integration status (2026-11-04, Phase 2B): Claude reviewed this contract and integrated it without changing it; see PHASE_2B_PLAYBACK_UI_REPORT.md for how each item below was implemented and tested. Two engine-side findings are reported there for the engine owner: offer timing against vsync, and a one-pump Buffering state (counted as a stall) at the start of every epoch.

- Review this contract before enabling visual Play/Pause/Stop. Keep unavailable playback UI disabled until wired to ABI 2 capability/state.
- Toggle Play/Pause from engine state and desired-playing intent; do not infer success from command return alone.
- Render distinct Starting/Seeking/Buffering/Ended/Failed feedback from typed state, not guessed percentages.
- Use requested position for the seek editor; use copied position or explicitly labelled clock for playback readout.
- Preserve engine-driven source/selection identity, scrubber feedback guard and timeline independence.
- Copy by generation into FrameView's bitmap; keep frames out of MVVM. Report render/presentation instrumentation independently of native copied counters.
- Stop the UI pump before disposing SafeHandle. Exercise pause/resume, rapid seek, source replacement and close during playback with the actual DLL.

The engine owner implements Rust, the ABI, managed wrapper/DTOs and tests. Visual frontend changes are a separate coordinated integration; no claimed Claude review or screen-presentation validation is fabricated.

# Timeline playback contract — Phase 3D / ABI 6

This extends the ABI 5 still evaluator, not the project format (envelope/schema 1).
Rust owns two independent video-only transports. No frontend clock evaluates clips
or invents PTS. Source playback retains its existing behavior.

## Commands and lifecycle

Timeline Play/Resume, Pause and Stop are nonblocking. Acceptance is neither decoded
readiness, copied-frame acknowledgment nor physical presentation. Timeline seeks
preserve playing intent. Pause removes paused wall time. Stop cancels decoding,
sets position zero and requests the existing isolated still at zero; the view must
not claim that image is ready until delivery. Play at Ended restarts at zero.
Leaving a mode pauses its transport; returning never auto-plays. Positions are
independent. Structural edits, Open/New, relink and effective monitored-track
changes stop and re-evaluate, invalidating the old epoch before further delivery.
Saving and source-only selection do not interrupt timeline playback.

## Evaluation and clock

The project owner provides an immutable, generation-tagged core timeline index
and bounded source descriptors to the session. Index building happens on the owner,
not the UI pump. Active/next lookup is binary. The policy remains selected video
track, otherwise selected clip's video track, otherwise first video track.

The monotonic clock advances integer project ticks by floor(elapsed nanoseconds /
project tick duration), checked, independent of source FPS. Clips are half-open
`[start,end)`. Timeline extent is the monitored track's actual end, not ruler padding.
At Ended the final valid image may remain as a labelled terminal image; it does not
belong to a successor clip or gap. Empty tracks end at zero.

Still seeks retain Phase 3C's ceiling policy. Continuous playback uses actual PTS
presentation intervals: an eligible frame covers its mapped deadline through the
next eligible deadline or exclusive clip end. The inverse deadline is
`clip.start + ceil((actual_pts - source_in) / project_tick_duration)`, combining
rationals before rounding. The clip's lower and exclusive upper source bounds are
ceil-rescaled once to original stream ticks. Never accept a frame outside them.
No nominal FPS is used. If no eligible preceding frame is available at a seek,
show blank until the first actual eligible PTS. If none exists in the clip, keep
the clip blank; this is distinct from process failure.

## Decoder handoff and bounds

Reuse the framed PAM + nonce/index/PTS/timebase/packet-size backend. Interval mode
retains at most the last eligible preroll frame at/before the request, then streams
successors. The ordinary source stream and isolated fallback keep ceiling behavior.
There are two persistent timeline workers: active and at most one prepared next
clip. Prepare within two timeline seconds of entry, including during gaps; never
start one process per frame. Each worker has three queued frames, and the scheduler
holds at most two lookahead frames per slot. Every frame is <=1280x720 RGBA8.
Existing source paused preroll is separately bounded and may coexist; all child
processes retain the existing Windows one-process/1 GiB job limit.

At a cut, invalidate the outgoing image before admitting the incoming clip. If
incoming preroll is unavailable, freeze at that exact boundary and enter Buffering.
Recovery reanchors at the frozen position. Gaps clear the image and advance without
decoding the gap. EOF is not Ended: hold the final eligible frame until clip end.
Startup requires real successor/EOF readiness, so it does not manufacture a stall.
Real mid-clip starvation freezes the clock; no guessed frame duration bridges it.

Project, model generation, transport epoch, monitored track, clip, media, request,
source clock and exclusive endpoint jointly gate delivery and terminal errors.
Current terminal errors bypass future queued pixels, including while paused;
successful EOF waits for the worker queue to drain after completion publication.
Cancellation replaces pending work and wakes bounded producers. No pixels enter
project events, JSON or view models. Shutdown cancels/reaps owned children before
joining media workers; no ABI command waits for a decoder.

## Presentation and diagnostics

ABI/contract 6 add timeline controls and a small `timeline_playback` view alongside
the existing still evaluation and source transport. Frame-copy layout is unchanged.
Clock position, offered timeline/source PTS and copied position are separate values.
Counters distinguish decoded, offered, copied, dropped and stalls. FrameView render
callbacks are application render observations, never physical scanout measurements.
High-frequency views do not resend/rebuild the clip document.

External development FFmpeg is not redistributed. See LICENSE_REVIEW.md. Seek
preroll relies on documented FFmpeg `-ss`, `-noaccurate_seek`, `-seek_timestamp` and
`-copyts` semantics, then validates indexed real PTS rather than trusting seek time:
[FFmpeg documentation](https://ffmpeg.org/ffmpeg.html).

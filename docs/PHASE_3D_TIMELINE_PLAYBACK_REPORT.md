# Phase 3D — Continuous video-only timeline playback

Date: 2026-11-06 (Asia/Singapore). Implemented in the existing Avalonia/Rust editor.
No frontend replacement, codec implementation, GPU interop, audio or export work.

## Baseline and scope

Verified local HEAD and origin `codex/phase-3c-timeline-preview` both pointed to
`43d859c4e60c05a19d9587f0f8670fe6ddc5517d`. The checkout was clean. Created
`codex/phase-3d-timeline-playback` directly from that commit. The unchanged baseline
passed all nine verify.ps1 stages: 96 Rust and 74 .NET tests, builds and publish.
No existing work was discarded and no dependency/toolchain upgrade was made.

The binding implementation contract is
[PHASE_3D_TIMELINE_PLAYBACK_CONTRACT.md](PHASE_3D_TIMELINE_PLAYBACK_CONTRACT.md).
ABI and contract are **6**; core schema and `.cutcap` envelope remain **1**.

## Implementation and changed modules

- `cutcap-core/src/timeline.rs`: binary next-clip lookup and checked inverse
  actual-PTS→timeline mapping, sharing the existing rational arithmetic/index.
- `cutcap-media/{backend,persistent,stream}.rs`: interval stream policy on the
  existing framed persistent adapter; ordinary source/isolated ceiling policies
  are preserved. No second decoder or per-frame process launch.
- `cutcap-ffi/timeline_plan.rs`, `timeline_playback.rs`: immutable owner plan,
  project clock, active/prepared slots, scheduling, gaps, EOF, errors and counters.
  `bridge.rs`, `engine.rs`, `session.rs`, `timeline_preview.rs`, `view.rs`, `abi.rs`
  integrate ownership, generation invalidation, controls, copying and view DTOs.
- `cutcap-contracts/messages.rs` and `CutCap.Interop/{NativeMethods,EngineSession,
  SessionView}.cs`: version 6 query/control/data contract on both sides.
- Desktop `EngineHost`, `PlayerViewModel`, `MainWindowViewModel`, PlayerPanel and
  TimelinePanel: mode-specific commands, glyphs/state, stable typed input and
  localized clock/playhead updates. Existing theme and bitmap path remain.
- Core, media, FFI/session/ABI, interop and desktop tests cover the new behavior.
  `TimelineTransportProbe.cs` and `summarize-timeline-playback.py` provide opt-in
  reproducible observations of the actual existing editor.
- AGENTS, CLAUDE, README, architecture, ABI, contracts, decisions, roadmap,
  development/setup and licensing documentation describe the final boundaries.

## Clock, timestamp and frame semantics

Rust owns a separate monotonic timeline clock. Wall time advances integer project
ticks with checked floor conversion. Pause removes paused wall time; buffering
freezes at a safe current position or exact cut; resume reanchors. No source FPS
or frontend timer creates a timeline timestamp.

Core lookup is `[start,end)`. Source mapping remains a single rational sum rounded
up once into original stream ticks. Inverse presentation deadlines are
`clip.start + ceil((actual_pts-source_in)/project_tick_duration)`, also one rational
rounding. Source lower and exclusive upper bounds are validated. Fractional final
PTS that map to the exclusive project end are not offered. Signed/nonzero source
PTS and different clocks are covered by deterministic tests.

Stopped still seeks keep the Phase 3C ceiling policy. Continuous seeks retain the
last eligible real PTS at/before target, then actual successors. A frame remains
eligible through the next PTS or clip end. No eligible initial predecessor means
blank until the first eligible PTS; no eligible frame in the clip means blank,
distinct from decode failure. No nominal FPS or fabricated PTS fills VFR intervals.

Source EOF does not end the timeline early: the last eligible image can hold to
clip end. Ended uses the monitored track's actual extent and retains its terminal
image, explicitly labelled Ended. Play at Ended restarts zero. Stop sets clock and
presentation position zero immediately, clears old permissions and requests the
existing zero still; readiness is not claimed at acceptance. Cancel freezes the
current resolved position rather than reverting to the last isolated still.

## Decoder continuity, handoff and bounded ownership

One active and at most one prepared-next timeline worker reuse PersistentDecoder.
Each worker has three queued frames, with two scheduler lookahead frames per slot.
Next-clip preparation begins within two timeline seconds of entry. Same-source
adjacent clips currently use separate prepared epochs too. There is no process
launch per video frame. The source transport retains its own existing bounded
paused preroll independently.

PAM packets remain paired with nonce/index/integer PTS/timebase/byte-size mux
records; every image and timestamp is validated. Interval preroll coalesces to one
owned predecessor. At a cut the outgoing image is cleared before incoming delivery.
A late prepared slot enters truthful Buffering at the exact cut; recovery resumes
there. Prepared errors do not prematurely interrupt the current clip: they fail
at entry. Gaps clear the image, advance normally and launch no gap decoder.

All work is scoped by project/model generation, transport epoch, monitored track,
clip, media and fresh decoder ticket, with original source-clock/range validation.
Frames/errors from superseded requests cannot replace current presentation. Pixels
never enter reliable project events, JSON or view models. Cancellation wakes full
queues and owned subprocesses are terminated/reaped on shutdown. Windows job
limits remain one process/1 GiB per child; this is not a hostile-codec sandbox.

The project owner builds/shares Arc<TimelineIndex>; seeks do not copy the clip
document or scan all clips. At most one owner plan is in flight plus one latest
desired seek. Source-only/same-monitor selection and Save preserve a timeline
epoch. Geometry edits, effective track changes, Open/New and relink stop/re-evaluate.
Seeking/playing are transient, never dirty state or undo steps.

## ABI and Avalonia

Added `play_timeline`, `pause_timeline`, `stop_timeline` exports and managed
PlayTimeline/ResumeTimeline/PauseTimeline/StopTimeline. Existing source controls
keep source semantics. `timeline_playback` distinguishes stable requested time,
clock, offered and copied frame ticks/PTS/clip identity, epoch, errors and counters.
FrameInfo remains 24 bytes. Frame epoch is the per-decoder request identity;
timeline_playback.epoch identifies the containing transport, spanning clip handoffs.
SafeHandle, registry keys, panic containment and frame-copy permissions remain.

The existing tabs, toolbar, ruler, slider and FrameView/WriteableBitmap are reused.
The visible mode determines which transport the commands/pump consume. Leaving a
mode pauses it without copying positions; returning never auto-plays. Only the
player/playhead receive continuous updates. Clip documents are fetched only on
their generation changing. Typed go-to text follows stable requested state rather
than every clock update. Native testing exposed stale wording and an incomplete
post-edit time-field update; both were corrected with regression coverage.

## Executed validation

Final commands, all exit 0:

| Command | Actual result |
|---|---|
| `./scripts/verify.ps1` | All nine stages passed |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `cargo test --workspace` | 111 passed; 15 explicit integration/performance/child fixtures ignored |
| `cargo build --workspace`, `cargo build --release` | Passed |
| `dotnet build CutCap.slnx -c Debug -nologo` | Passed, no warnings/errors |
| `dotnet test CutCap.slnx -c Debug -nologo --no-build` | 41 interop + 38 desktop passed; explicit layout benchmark not run |
| `dotnet build CutCap.slnx -c Release -nologo` | Passed, no warnings/errors |
| `dotnet publish apps/CutCap.Desktop/CutCap.Desktop.csproj -c Release -r win-x64 --self-contained true -o artifacts/publish/win-x64 -nologo` | Passed |
| `cargo test -p cutcap-media --test ffmpeg_integration -- --ignored --test-threads=1` | Nine real FFmpeg tests passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | Passed |
| `cargo run -p cutcap-ffi --features gpu-diagnostics --bin cutcap-gpu-info` | Intel integrated DX12 and Microsoft CPU DX12 enumerated |

Controlled-clock/readiness tests exercise adjacent/multiple cuts, gap advancement,
late handoff/freeze/recovery, genuine starvation versus startup, pause/resume,
mixed source clocks, negative/nonzero source offsets, EOF versus Ended, no eligible
frames, overflow and stale copy permissions. Session/ABI tests cover rapid seeks,
pending cancellation, immediate Stop zero, same-monitor/source selection, edits,
history and mode switching. Real DLL/FFmpeg tests cover two-source cuts, playing
and paused seeks, replay, persisted gaps, VFR short endpoints, missing/relinked
media, same-source offset cuts, Open during playback, tracks and bounded disposal.
Media interval PTS are compared to an independent FFprobe frame list, including
long-GOP/B-frame media; active child cancellation/reaping is checked.

Terminal decoder errors are consumed independently of future queued pixels, even
while paused. Successful EOF is observed only after terminal publication and a
fresh empty-worker-queue check; it cannot discard a concurrently published final
frame. A dedicated regression covers error-versus-EOF behavior with future pixels.

All previous Phase 3C tests remain. Failed assertions during development identified
test-fixture start-PTS omissions, an incorrect ceiling-still expectation at 2.9s,
and a genuine cancel-position regression; these were fixed, not suppressed. Clippy
findings were corrected without disabling warnings. No remote CI result is claimed.
Machine-local logs are ignored under `.artifacts/phase3d` and `.artifacts/verification`.

## Native Windows evidence

Windows desktop: Core Ultra 7 255H, Intel integrated graphics, 16 GB, 1920×1200 at
150%; Rust 1.99.1, .NET SDK 10.0.403, Avalonia 12.1.5, external FFmpeg/FFprobe 9.0.1.
The published self-contained editor was launched, not just an interop test host.

Performed through native controls: imported two 12-second 640×360 H.264/B-frame
test videos with distinct red/blue bands, appended both to Video 1, saved a real
`.cutcap`, switched to Timeline and observed automatic A→B playback. Paused B,
sought backward numerically into A, resumed across the cut again, stopped to zero
and replayed. Space worked immediately after selecting Video 1. Source playback
advanced while the timeline remained at 10.137 seconds; switching back retained
that paused timeline position. Tested normal/maximized windows and panel resizing.

A valid saved test document with a gap was opened: blank still, advancing blank
playback and automatic B entry were observed. A separate missing-reference test
document retained both clips, displayed Unavailable/Failed without an old image,
and recovered after original-content relinking through the native picker. Undo
during playback stopped/re-evaluated; Redo restored the appended clip; Undo returned
to Saved. Originals were untouched. The gap fixture was constructed as test data;
the UI does not claim gap insertion/dragging/trim support.

Closed while real playback/FFmpeg work was active. A fresh published editor then
opened the original two-source saved project; the unsaved-empty-project guard was
observed, the saved clips/references recovered, and Timeline playback reached Ended
on B. No editor/FFmpeg/FFprobe processes remained after close.

The existing diagnostic driver set the actual editor to its declared minimum
layout (captured window 963×631 logical image pixels), then held it for manual
inspection. Timeline playback crossed the 5,000-clip project at that size, with
both active/prepared FFmpeg children observed before closing during handoffs.
Cleanup again left no children. No other DPI, monitor or screen reader is claimed.

Genuine unedited application captures:

| Evidence | Capture |
|---|---|
| Playing A / automatic B / paused B | [A](screenshots/phase3d/timeline-clip-a.jpg), [B](screenshots/phase3d/timeline-clip-b.jpg), [paused](screenshots/phase3d/timeline-paused-b.jpg) |
| Independent paused position | [Timeline](screenshots/phase3d/independent-paused-timeline.jpg) |
| Blank clock interval / recovery | [Gap](screenshots/phase3d/playing-gap.jpg), [after gap](screenshots/phase3d/after-gap.jpg) |
| Missing / relinked | [Missing](screenshots/phase3d/missing-media.jpg), [relinked](screenshots/phase3d/relinked-preview.jpg) |
| Minimum layout / actual playback | [Layout](screenshots/phase3d/minimum-layout.jpg), [playing](screenshots/phase3d/minimum-playback.jpg) |
| Reopened project at Ended on B | [Recovered](screenshots/phase3d/reopened-ended.jpg) |

Exact-tick cut ownership is established by automated tests; interactive ruler
clicks use approximate pixel coordinates. Genuine starvation is deterministically
injected in tests; the representative native performance runs did not stall.

## Performance observations

[Sanitized data](measurements/phase3d-timeline-playback.json) contains three Release
episodes for each source/project: 720p, 1080p and 4K 30fps H.264 with 150-frame GOPs
and B-frames; sparse real VFR; and 5,000 valid one-second clips (first ten timeline
seconds, ten handoffs). Preview is bounded to 1280×720. Production EngineHost,
FrameView and timeline drawing were used. No concurrent build/test was running.
Resource sampling every 100ms includes the host and observed FFmpeg/FFprobe;
child CPU is an observed lower bound. Percentiles use nearest rank, pooled by source.

| Source/project (3 episodes) | Play→first DrawImage median/max ms | Render gap p50/p95/p99 ms | Cross-cut render gap median/max ms | Seek→render median/max ms | Peak host+children working MiB |
|---|---:|---:|---:|---:|---:|
| 720p, 2 clips | 130.5 / 131.2 | 33.4 / 43.4 / 50.1 | 37.8 / 41.2 | 221.9 / 360.3 | 523.4 |
| 1080p, 2 clips | 154.8 / 156.0 | 33.4 / 41.5 / 45.9 | 33.5 / 33.5 | 259.9 / 415.6 | 566.3 |
| 4K, 2 clips | 222.8 / 223.2 | 33.3 / 42.6 / 46.8 | 35.0 / 43.6 | 487.8 / 825.9 | 764.3 |
| VFR, 2 clips | 99.7 / 116.8 | 499.7 / 803.6 / 823.8 | 102.2 / 104.5 | 95.4 / 99.2 | 348.0 |
| 5,000 clips, 720p | 119.2 / 122.5 | 33.3 / 48.9 / 57.0 | 31.5 / 51.4 | 161.8 / 232.7 | 550.6 |

Cross-cut gap means last outgoing→first incoming **DrawImage**, including a normal
frame interval; it is not decoder downtime or scanout. VFR's long intervals follow
its actual sparse PTS. Small CFR runs decoded/offered/copied 300 frames each; drawn
generations were 299–300. Native dropped/stall counters were zero in all 15 runs,
but some copied generations were not separately drawn. Large-project runs offered
301 frames (the last sampled clock crossed 10s), copied 300–301, drew 297–300 and
started 12 bounded epochs including preroll. No smooth/locked 30/60 FPS claim follows.

Small CFR decoded delivery rates under bounded backpressure were approximately
29.3–29.6 frames/s including startup, separate from their drawn-generation counts.
An independent unthrottled maximum decoder-throughput benchmark was not performed.

Observed machine CPU median was 10.4%, 10.6%, 13.8%, 8.2%, 11.1% respectively on
16 logical processors, including diagnostics and observed child CPU. Host working
peaks stayed roughly 341–443 MB across the batch; this is a short repeated run,
not a long-duration leak proof. Maximum native RGBA frame is 3,686,400 bytes.
Frame-copy medians were ~0.4–0.6ms for HD previews; 4K-source copy p95 ~1.04ms.
The diagnostic dispatcher interval p95 was 27.3–34.4ms, max 54.6ms. Large timeline
sampled draw p95 was 2.217ms, max 17.667ms, versus small-project ~0.13ms medians.
View size stayed <=4,241 bytes; the 5,000-clip document was 1,668,914 bytes, fetched
zero times during playback. No per-frame clip model rebuild was observed.

A separate three-session real-ABI cleanup measurement started both active/prepared
children, superseded them by seeking and copied the replacement frame in
128.5–157.0ms (median 156.8ms). Disposing the playing replacement epoch took
6.15–7.37ms (median 6.17ms); both old child handles had exited and no replacement
survived disposal. These are native command→copy/dispose observations, not GUI
render/scanout or universal cancellation bounds. [Data](measurements/phase3d-timeline-cleanup.json).

A separate Release core measurement (2,000 seeks per project, 2/5k/50k clips) found
cached evaluation median 300ns, p95 500/400/800ns, first index+evaluation
31/433/4,279us and responses 1,222–1,234 bytes. This is evaluator cost, not decode
or screen latency. CPU-frame presentation is sufficient for this milestone;
4K seek preroll/restart cost and large-project redraw tails deserve profiling
before a GPU/bitmap rewrite. Same-source decoder reuse is not implemented.

Reproduce with the existing generated playback fixtures, Release PlaybackProbe
`--timeline-play`, then `scripts/summarize-timeline-playback.py`. The initial pilot
was repeated after fixing a diagnostic that could count a cached still immediately
after Play acceptance; only the corrected 15-episode batch is reported above.
Later presentation-field/tooltip corrections did not change decoder deadlines,
bitmap path or these transport timing policies.

## Licensing, limitations and next step

No new dependency, codec binding, FFmpeg executable/library copy or packaging rule.
The external installed GPL-enabled static FFmpeg/FFprobe 9.0.1 remains an authorized
development runtime. Subprocess use is not automatic licensing clearance; product
distribution and application license remain owner decisions (LICENSE_REVIEW.md).

Single monitored video track, SDR CPU preview only; no audio, compositing, export,
trim/split/move, effects or vsync/GPU rewrite. One next slot cannot prewarm every
possible rapid edit/seek; late preparation truthfully buffers. OS/codec cancellation
is not hard real time or a hostile-file sandbox. Long-duration leaks, other codecs,
HDR, other DPI/monitors, screen readers, arbitrary storage conditions and physical
scanout are unverified. No remote CI execution is claimed.

Recommended next milestone: exact trim/split operations with bounded undo/history
and the same evaluator/transport invalidation; separately profile 4K preroll and
consider proxies or measured decoder reuse before native-library/GPU changes.

## Git evidence

Implementation commit hashes and acknowledged origin push are appended after
the verified feature commits are created. No force push or main merge is used.

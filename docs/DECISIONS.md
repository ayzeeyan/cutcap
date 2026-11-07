# Architecture decisions

## ADR 025 — Linked audio clips, schema 2, WASAPI audio-master clock, waveforms and ABI 9 (Phase 4B)

**Stream identity and links (schema 2).** A clip names the stream it presents
(`stream: video | audio{index}`); a placed source with audio becomes a linked
pair (`link`) of one video and one audio clip with identical media, start,
duration and `source_in`. Core validates kind and link invariants at the end of
every `replace_clips` transaction, so no edit can leave half a pair. Rejected:
an audio offset field per clip (a second time value to keep consistent with
every trim/split), and implicit "audio follows video" without stored links (old
files could not say which clips belong together). The audio member shares the
video member's `source_in`; audio maps it to samples with **one** ceiling
rounding (`ceil((source_in + audio_start − video_start)·rate)`), so trims and
splits never accumulate error and the picture and sound of a cut agree on the
same instant.

**Migration.** Schema 1 → 2 happens in memory on open: every schema-1 clip is a
video clip. A schema-1 clip on an audio track, or stream/link fields in a
schema-1 document, are refused (`Migration`), never guessed. Opening never
writes; a refused file is untouched; saving writes schema 2. The envelope stays
`format_version` 1 (its shape is unchanged).

**Placement policy.** Placement links the primary audio stream (default
disposition, else lowest index) on the first audio track free for the whole
interval, as part of the same undo step; no free track rejects the whole
placement. Track creation stays outside history, so placement never creates a
track.

**Decoding.** One FFmpeg process per audio span (`-copyts`, keyframe seek
0.5 s early, `aresample` to the device rate, float stereo, mux stats giving
packet PTS = source sample index), bounded queues, cancellation that reaps the
child. Waveform analysis is a separate native-rate, one-thread, below-normal
pass (≤4 chunks of 262 144 frames per process, 256-frame min/max buckets).
Rejected: linking FFmpeg libraries (ADR 006/008), decoding PCM in C#.

**Output and clock.** WASAPI shared, event driven, float stereo at the mix rate
via `wasapi 0.27.0`; a 400 ms ring with 120 ms prefill. **The audio device is
the master clock**: the device position (`IAudioClock`, sampled after each
write, ≤40 ms extrapolation, never past written frames) mapped through segments
that separate real timeline frames from padding. Starvation stops the clock;
late video never does; paused state reports the frozen position (a real
endpoint plays its in-flight period after Stop). No device or
`CUTCAP_AUDIO=off` → system clock with an explicit note; device failure stops
playback with an error. Only the first audio track is monitored (no mixer yet).
Rejected: driving audio from the video timer (would not be audio-master and
would drift), exclusive mode (blocks other apps, needs format negotiation).

**Unreported output latency.** Measured on the test machine: the endpoint mix
carries a sample 51–69 ms after the WASAPI position says it plays
(`GetStreamLatency` = 0). No API reports this processing delay, so it is an
explicit setting, `CUTCAP_AUDIO_LATENCY_MS` (0–500, default 0), that holds the
clock back. A per-device calibration UI is future work.

**Waveforms (ABI 9).** A low-priority service mirrors filmstrips: demand
replaces demand (≤128), consecutive chunks batch, unwanted batches cancel,
playback throttles to single chunks with 200 ms pauses, results keyed by
`audio_key` (source identity + stream) drop when stale. C# keeps a 16 MiB LRU of
(min, max) peaks and plans visible chunks first; Render draws cached peaks only.
Peaks never enter the Project, history or files.

## ADR 024 — Source-grid filmstrips, batched low-priority thumbnails and ABI 8 (Phase 4A)

Timeline clips show real decoded frames. **Timing stays in Rust:** clip rows
carry the exact source interval and a `source_key`; Rust maps each requested
sample (µs) to stream ticks with ceiling rounding and selects the first decoded
frame whose *actual* PTS is ≥ that tick, so VFR, offset and signed timestamps
behave like the Phase 3C still preview and nominal FPS is never used. C# only
lays out cells and draws a decoded image when its actual PTS lies inside the
clip's `[source_in, source_end)`.

**Fixed source grid.** Cells are placed on multiples of one cell's source
duration at the current zoom, plus a leading partial cell at the in-point.
Alternatives rejected: per-clip even spacing (every trim, split or move would
request all-new samples and nothing would be shared between split parts), and
per-frame sampling (unbounded work at high zoom). The grid makes scrolling,
moves and splits cache hits; a zoom change requests a new grid.

**Extraction.** One FFmpeg process per batch of ≤6 samples from one source:
one keyframe-seeked input per sample (`-noaccurate_seek -ss t−1s`, `-copyts`),
a `select=gte(pts,T)` branch with a `showinfo` tag for the actual PTS, and
`trim=end_frame=1`, concatenated into raw RGBA at the thumbnail size. This
reuses one process launch and decoder setup per batch without a persistent
decode protocol (ADR 017 keeps free-form showinfo out of *continuous*
playback; here each branch emits at most one frame and its tag is matched by a
per-batch nonce, so a forged tag in metadata is ignored). The process runs at
below-normal priority with one thread inside the existing job-object limits.
Rejected: one process per thumbnail (launch cost dominates: ~6× the
processes), a persistent seeking decoder (a protocol change owned by the
media engine), and embedding FFmpeg libraries (licensing/packaging, ADR 006/008).

**Scheduling and staleness.** One worker thread; demand replaces demand;
unwanted queued samples are dropped and an in-flight batch nobody wants is
cancelled (child killed and reaped). Playback throttles thumbnails to batches
of 2 with 150 ms pauses. Results carry their `source_key`; announcing discards
any whose key is no longer current, and a project change clears the service.
Relinking changes the key, so no stale image is ever drawn for another file.

**Cache.** C# keeps an LRU by pixel bytes (48 MiB default,
`CUTCAP_THUMBNAIL_CACHE_MB` to override), keyed by (source key, sample,
height); one `WriteableBitmap` per decoded thumbnail, disposed on eviction.
Negative results are remembered. There is no disk cache in this phase: it
would add invalidation, privacy (paths) and cleanup policy for a cache that
rebuilds in seconds from a local file.

**ABI 8** adds `request_thumbnails`, `next_thumbnail` and `copy_thumbnail`
(pull-based, copy into caller memory, no callbacks) and the clip/view fields
above. Schema/envelope stay 1: thumbnails are never persisted. Lanes grow from
50 to 56 DIP so a 43 px strip fits beside the name band.

## ADR 023 — Exact clip move/trim/split, grouped history and ABI 7 (Phase 3E)

Keep schema/envelope 1: a clip's identity, track, start, duration and integer
`source_in` already describe every result of a move, trim or split. The one
representability limit is that `source_in` is an integer number of stream
ticks while edits are expressed in project ticks. Rather than adding a
fractional in-point (a schema migration) or rounding per frame, edits that move
`source_in` (start trim, split) are aligned to the exact grid on which the
offset is a whole number of stream ticks: `source_step = den(project_tick /
stream_tick)`, e.g. 375 project ticks (0.52 ms) for 1/15360 s, 720 (1 ms) for
1/1000 s, 8 for 1/90000 s. Splits round to the nearest grid point; start trims
round up toward the retained part so they never overlap a preceding clip or
move before a snap target. The adjustment is at most one stream tick, reported
in `edit_notice`, and never accumulates: the retained frames keep exactly the
mapping they had (tested over twenty consecutive off-grid splits, VFR, nonzero
and negative start PTS, three stream clocks, with real FFmpeg frames).

Edits are atomic core transactions (`Project::replace_clips`, all-or-nothing
with exact rollback) recorded as `ClipChange` values, which generalizes the
Phase 3B insert/remove history without changing its revision semantics. A
gesture is one transaction because the frontend previews locally and commits
once on release; the authoritative project never mutates per pointer move.
Structural edits reuse the Phase 3D invalidation path (new evaluation
generation → transport cancelled, playhead kept, no auto-resume).

ABI/contract 7 add five exports and consumed clip/editing fields. The frontend
never derives project time: it compares and selects exact tick values the view
supplies (clip edges, trim limits, playhead) and otherwise commits whole
milliseconds. Library drag-and-drop uses an in-process data format carrying
only the media identity, name, duration and readiness.

## ADR 022 — Independent timeline interval transport and ABI 6 (Phase 3D)

Keep schema/envelope 1. Share an immutable owner-built core index through a
private host event so timeline polling performs binary active/next lookup without
UI model construction, C# evaluation or per-frame project serialization. The
source and timeline clocks/epochs/positions remain independent. ABI/contract 6
add explicit timeline transport controls and its consumed diagnostics, preserving
the 24-byte frame-copy layout and single WriteableBitmap.

Reuse the existing persistent PAM/indexed-PTS decoder with an interval mode:
retain at most the last eligible preroll frame <= request, then actual successors.
Still/source seek ceiling semantics stay unchanged. Inverse source deadlines ceil
one rational difference; wall-clock advancement floors to project ticks. Neither
uses nominal FPS. Two bounded timeline worker slots prepare the next clip within
two seconds of entry, including gaps; source paused preroll remains separately
bounded. This uses existing subprocess custody/licensing, with no new dependency
or codec implementation. Native libraries and seamless same-source decoder reuse
remain possible behind this boundary, but require a separate measured change.

At cuts clear outgoing permissions immediately. If incoming preroll is late,
freeze at that exact boundary, truthfully Buffering; do not run clock on an old
image. Gaps are blank clock intervals; source EOF holds the last eligible frame
through clip end; monitored extent controls Ended and replay. Structural edits,
relink and replacement stop/re-evaluate; no unsafe speculative continuation.
No scheduler/vsync deadline, GPU path, audio or compositing change is made.

Evidence and limits: PHASE_3D_TIMELINE_PLAYBACK_REPORT.md. FrameView callbacks
measure application DrawImage generations only, never physical scanout.

## ADR 021 — Indexed single-track timeline evaluation and ABI 5 (Phase 3C)

Core keeps a derived per-video-track sorted index rebuilt after geometry edits/Open, not on seeks. Owner evaluation uses binary lookup on `[start,end)` and returns a small result, with a transient generation UUID separate from undoable saved revisions. No ProjectSnapshot/clip serialization occurs per seek. Selected-clip view lookup is cached too, removing the remaining per-view clip scan. Indexes, monitor mode/position and preview results remain outside project schema/envelope 1.

Map source_in plus clip-local time as exact rationals before one ceiling conversion to original stream ticks. Exclusively bound the clip's source endpoint and require exact rescaling of decoded PTS to the stream clock. A ceiling frame beyond that endpoint is NoFrame, not another clip or a decoder error; gaps are Blank and do not launch decoding. This intentionally preserves Phase 1's ceiling policy, including its sparse-VFR/short-clip no-frame cases.

Reuse the one isolated latest decoder for timeline and source still frames, with one desired owner evaluation and one in-flight query. Leaving Source pauses its persistent transport and retains position/epoch/preroll; returning restores the cached source frame under a renewed shared generation, without auto-play. A bounded native source image and timeline image may coexist; no second frontend bitmap exists. Pending mutations, source availability/relink, track changes, mode switches and project adoption invalidate request/model generations independently. Current-epoch real frames alone can be copied.

ABI/contract 5 add mode/exact-tick/millisecond/text/cancel controls and consumed monitor DTOs; signatures/status/frame layout from ABI 4 remain intact. Ruler/slider coordinates are presentation inputs; Rust rescales and owns the resulting timeline position. C# never locates an active clip or maps its source PTS. Continuous timeline Play stays disabled. This is a video-only monitored-track slice, not compositing or a new media architecture.

## ADR 020 — Rust-owned clip editing, bounded undo/redo and ABI 4 (Phase 3B)

The command owner gains InsertClip { media_id, track_id } next to the existing RemoveClip, Select, Undo and Redo commands. Insertion is an explicit append-to-end policy: the whole verified, Ready video source is placed on a video track starting where that track's last clip ends; duration is the source's reported `duration_ticks` rescaled (floor) from the stream clock to the project clock, source_in is the stream's start PTS. Sources without a known positive duration, shorter than one project tick, not Ready, of the wrong kind, on a missing track, or whose end would overflow are rejected before mutation. Core validates overlap/kinds/limits as before; it only gains exact index restoration, removal returning the former index, and the append point. No new dependency, no schema change: `.cutcap` format 1/core schema 1 already persist clips.

History lives in the owner (`crates/cutcap-ffi/src/history.rs`), bounded to 200 entries, each holding the exact clip value, its index and the document revisions before/after the edit. Undo restores the exact clip at its index (identity, timing, source mapping and order), and returns the document to the `before` revision only if nothing else changed since; otherwise it takes a fresh revision. Revisions are now unique per session (`next_revision` only grows), so a restored revision is never confused with a different state: Save → Insert → Undo is clean, Save → Insert → Add track → Undo stays dirty. A divergent edit clears redo. Failed commands and selection never enter history or change the revision; New and Open reset it. Track creation and relinking are not undoable in this phase and are documented as such.

The view JSON is re-serialized for every playback frame, so clips are **not** part of it. A separate timeline document (`cutcap_session_timeline`, `{generation, clips}`) is rebuilt at most once per project snapshot and fetched by the frontend only when the view's `timeline_generation` changes. Measured at 5,000 clips: timeline document 1.68 MB built in 6.7 ms once per edit, view 2.5 KB in 0.1 ms per frame (docs/measurements/phase3b-timeline.json). Clip rows carry floored display milliseconds for drawing and Rust-formatted strings as the authoritative readouts; C# never derives project time.

New exports (insert/remove/select clip, undo, redo, timeline) and consumed view fields (`timeline_generation`, `history`) bump ABI 3 → 4 and CONTRACT_VERSION 3 → 4, per AGENTS.md. The bump is also the only guard against a stale ABI-3 DLL next to a 3B frontend, which would otherwise pass the version check and fail on the first clip command. Frame layout, status numbers, handle semantics and every ABI 3 export are unchanged.

The Avalonia timeline draws clips in the existing single custom lanes visual with per-track sorted layouts, binary-search viewport culling and hit-testing; no control per clip, no thumbnails or waveforms (none exist). Missing sources draw a coral dashed outline (no purple). Ctrl+Z, Ctrl+Y/Ctrl+Shift+Z and Delete are taken in the window's tunnel handler except while a TextBox has focus. Drag, trim, split, move, timeline playback and composition remain future work.

## ADR 018 — Validated project documents, revisioned file operations and ABI 3 (Phase 3A)

Keep core schema 1; `.cutcap` envelope format 1 stores the complete Project plus per-source metadata expectations/full SHA-256 fingerprints. Runtime selection, playback, images, errors and layout remain transient. Core gains only an atomic source-reference mutation. A single background file operation owns a captured validated snapshot; the command owner keeps processing edits and records the saved snapshot revision only on successful replacement. Open validates byte/structure/graph limits before adoption and preserves current state on failure. Reopening the same ProjectId also resets source work. See PROJECT_FORMAT.md for path, format, integrity and bounded-shutdown guarantees/limits.

Use pinned tempfile 3.27.0 (MIT OR Apache-2.0) for exclusively owned same-directory temporary files and replacement, and sha2 0.11.1 (MIT OR Apache-2.0, Rust >=1.88) for complete source hashes. Inspected tempfile Windows code uses MoveFileExW(REPLACE_EXISTING), no copy fallback. Sync temp contents before rename, never delete/truncate the destination first. This avoids new handwritten unsafe; directory/power-loss durability and concurrent external-writer conflict handling remain unproven. Local NTFS fault/lock/process-kill tests establish the narrower claims.

After Open, reuse the existing single cancellable probe to compare current source content and metadata asynchronously. Missing/changed/unavailable assets remain registered. Relink accepts the original verified content, preserves identities/order/clips, and dirties the document only when its path changes. Reliable verification results wait in one bounded session slot if the owner queue is full; they do not share the frame mailbox. Source expectations are document data outside the codec-independent Project, not a trusted runtime cache.

ABI/contract 3 add Save/Save As/Open/Relink and consumed document/availability/track-label fields; producer, consumer, tests and docs change together. The existing frame layout, transport calls, opaque handles and status numbers remain intact. Sol owns both sides for this milestone (owner states Claude is not active). The frontend adds native file dialogs, terminal-result-based dirty feedback and Save/Discard/Cancel replacement/close protection, preserving the workspace design. Startup window sizing now fits the current display's logical work area: native inspection reproduced the old 1440×900 logical window hiding its title/project controls at 150% scaling.

## ADR 019 — Preroll readiness precedes source clock anchoring (Phase 3A)

The first PAM packet could be offered before a real successor reached lookahead. The next pump treated that empty queue as starvation, creating the reported Playing → Buffering → Playing startup blip. For playing/playing-seek epochs, wait in Starting/Seeking for two actual timestamped frames or confirmed EOF before anchoring/offering the first. A paused preview still needs only one frame; resuming that initial paused frame waits for real lookahead. No nominal-fps deadline, UI indicator delay, vsync assumption or scheduling-deadline change is added. Genuine exhaustion after preroll still enters Buffering, increments stalls and freezes the source clock; deterministic VFR/EOF/pause/starvation tests cover both behaviors.

## ADR 001 — Three crates (accepted, Phase 0)
`cutcap-contracts` is the leaf: IDs, time values, command/event DTOs and boundary errors. `cutcap-core` depends on contracts and owns validated project state. `cutcap` depends on both and owns the window, worker lifecycle and adapters. No engine crate full of stubs, async runtime or plugin system.

## ADR 002 — Exact ticks, explicit timebase (accepted)
Timeline positions are signed integer ticks in the project's rational seconds-per-tick timebase. Positive durations and checked endpoints reject invalid clips. Default is 1/720000 second: common integer and 30000/1001 frame rates map exactly. This is a storage clock, not the preview rate. Source timestamps carry their own rational timebase and a distinct type. VFR will use decoded presentation timestamps; never infer source time from frame index. Other rates require explicit conversion/rounding at a future media adapter.

## ADR 003 — Single project owner and bounded messages (accepted; drain owner moved by ADR 013)
A standard-library worker owns the model. UI submission uses nonblocking `try_send`; events are drained in bounded batches by a Slint timer on the event-loop thread. No worker accesses a Slint component. Cheap operations only in Phase 0. Decode/export will require cancellable execution separate from the command owner. See ENGINE_UI_CONTRACT.md.

## ADR 004 — Independent preview presentation (Slint details superseded by ADR 012/015)
Slint 1.18.1 uses winit + software rendering for the shell. Only the app's preview adapter writes viewer properties. There is no assumption that Slint's internal renderer is a video renderer. Optional wgpu 30.0.1 diagnostics enumerate DX12 adapters without creating a device or sharing textures. No unstable Slint wgpu/winit APIs are enabled. FFmpeg is not a Phase 0 dependency. Phase 1 follows the owner's later CPU-first instruction: use supported CPU frames and measure before considering GPU integration (ADR 010). The originally proposed Phase 1 GPU comparison is deferred until playback measurements demonstrate a need.

## ADR 005 — Toolchain and dependency policy (accepted; Slint pin removed by ADR 012)
Pin Rust 1.99.1, MSVC target, rustfmt and Clippy in rust-toolchain.toml; manifests declare Rust 1.99 as the supported baseline. Commit Cargo.lock. Slint runtime/build versions match exactly. Other dependency ranges allow compatible updates only through reviewed lockfile changes. CI uses the pinned toolchain, not a floating latest action. Upgrade compiler and Slint deliberately, run all checks and a Windows launch, and record results. Four compiler jobs limit peak memory on 16 GB hardware; no measured runtime performance claims yet.

## ADR 006 — Licensing (provisional distribution path; Slint route no longer applies, see ADR 012)
No application license has been chosen; packages are non-publishable. Review the Slint royalty-free desktop route with built-in AboutSlint attribution. The installed FFmpeg has `--enable-gpl --enable-version3`; do not bundle or link it by accident. LGPL-compatible shared builds, obligations, codec patents and the final application license must be resolved before distribution. See LICENSE_REVIEW.md.

## ADR 007 — Honest editor shell (accepted)
Real empty tracks, selection, track creation and new-project commands establish the model/UI round trip. View-only zoom, panel splitters, scrolling, workspace reset and About are presentation state. Unsupported media operations remain disabled with explanations. No sample clips or simulated transport.

Initial placement uses the public Slint window API at physical (32,16) on the primary display. Native maximize/restore is available. Persistent/per-monitor workspace geometry is deferred; avoid unstable backend access just to implement saved layouts in Phase 0.

Use Slint logical-key bindings for shortcuts. In 1.18, the framework accepts Escape in its popup handling path even when no popup is open, before application bindings. Clear selection uses Ctrl+Shift+A; Escape remains popup dismissal. No internal framework override is used.

## ADR 008 — Independent subprocess media adapter (accepted, Phase 1)

Add cutcap-media for real FFprobe parsing, controlled FFmpeg frame decoding, process ownership and tests. It depends on contracts, never Slint/core. One concrete MediaBackend trait is justified by tested behavior and a future libavcodec/hardware replacement. Native FFmpeg bindings would add ABI/build/licensing complexity without helping this request-based milestone. External FFmpeg/FFprobe 9.0.1 are development runtime dependencies; no binary is bundled/downloaded/packaged. Argument spawning, bounded pipes, timeouts, cooperative cancellation and kill/reap ownership are mandatory. See LICENSE_REVIEW.md for distribution questions; a subprocess is not automatic license clearance.

## ADR 009 — Contract 2 and metadata session cache (accepted, Phase 1)

Add RegisterMedia/SelectMedia and real metadata/library fields in snapshots. Only the project worker registers MediaAsset; probing/decoding run separately. Source selection does not replace timeline Track/Clip selection. Keep schema 1 unchanged: paths/IDs are serialized, metadata is a re-probable session cache, and no buffers enter Project. The app cap is 128 imported assets. Persistence must later decide fingerprinting, re-probing and explicit per-stream references, particularly for audio. Reserved ImportMedia batch/timeline Seek/Cancel command shapes remain Unsupported; the implemented app media workflow has its own concrete request/token contract.

## ADR 010 — CPU still frames and latest requests (accepted, Phase 1)

One active decode, one replaceable pending request and one replaceable outcome bound backlog. Project/media/request identity gates successes and errors in worker and presenter. Opaque top-down packed RGBA8 is limited to 1280×720 / 3,686,400 bytes and copied once into Slint's supported SharedPixelBuffer on UI. No internal Slint renderer/GPU APIs or unsafe code. The old frame can remain while seeking; changing source clears it. Durable project/import/export terminal outcomes must never use this ephemeral mailbox. Performance evidence and measurement limits are in PHASE_1_REPORT.md; request-based success is not playback pacing evidence.

## ADR 011 — Explicit ceiling PTS selection (accepted with compatibility risk)

Select first decoded original PTS >= requested stream PTS. Requests use signed integers/rational clocks with checked floor/ceil conversion, not frame-rate indexing. FFmpeg preserves copyts, demux-seeks with one-second preroll and noaccurate_seek, then applies explicit PTS filtering. The positive-offset regression exposed automatic accurate_seek discard selecting no frame; explicit filtering fixes it. showinfo provides original PTS/timebase, with first record matched to the first raw output. Missing PTS is failure. Text parsing is tied to the tested FFmpeg 9.0.1 CLI and must be retested on upgrades. Near-end requests after the last PTS may return NoFrame; there is no fabricated fallback or arbitrary-media frame-accuracy promise.

Quarter-turn orientation/aspect is applied before bounded square-pixel presentation. Only SDR conversion is supported; HDR/reflections/non-quarter turns are rejected. Alpha is discarded to opaque. A future color-managed adapter may change these semantics only with explicit compatibility/tests/docs.

Timestamp records must match the uniquely named showinfo filter for the fresh RequestId, not arbitrary stderr containing the word showinfo. A forged media title reproduced false PTS 999999 instead of 0 with the original broad parser; exact per-request context matching fixes it. Both a parser unit test and the real CFR/Unicode/offset fixtures enforce this untrusted-metadata boundary. Integer start_pts is required; a decimal fallback is not claimed as observed PTS.

## ADR 012 — Avalonia frontend over the Rust engine (accepted, owner-directed migration)

The owner directed a migration from Slint to Avalonia UI (C#/.NET), keeping Rust for editing semantics, media processing, project state and future compositing. Avalonia 12.1.5 on .NET 10 (SDK 10.0.401 pinned by global.json), Fluent theme with CutCap tokens, Inter typeface (Avalonia.Fonts.Inter), CommunityToolkit.Mvvm 8.4.2 for source-generated observable properties and commands, compiled bindings by default. No WPF/WinUI/web layers. The C# frontend must not become a second engine: it holds no project state and derives every value from the engine view. The Slint app (apps/cutcap) was removed after native parity checks; it remains in history at a3de692. Slint, slint-build and rfd left the workspace; the DX12 diagnostic moved to `cutcap-ffi --bin cutcap-gpu-info`. The Slint `--benchmark-preview` CLI was replaced by an opt-in UI trace (CUTCAP_TRACE_PREVIEW=1) and scripts/native-ui/bench.ps1.

## ADR 013 — Pull-based, versioned C ABI with a handle registry (accepted)

`crates/cutcap-ffi` builds cutcap_engine.dll (cdylib + rlib). The Phase 1 command owner and bridge moved there unchanged; media_ui/preview/coordinator logic became a frontend-independent `Session`. The ABI (docs/NATIVE_ABI.md) uses u64 registry keys instead of pointers (stale/forged/double-destroyed handles return INVALID_HANDLE), catches panics at every export, validates every caller buffer, and has no callbacks: a 16 ms UI timer pumps and pulls a coalesced JSON view plus frames by generation. JSON is acceptable for the small, low-frequency view (≤ a few KB; capped at 8 MiB on read); frames never use it. Unsafe code is confined to abi.rs with SAFETY comments; workspace `unsafe_code = "deny"` still applies elsewhere, and core/contracts/media remain `forbid`. ABI_VERSION 1; contract version 2 and schema 1 unchanged.

## ADR 014 — Display formatting stays next to exact time (accepted)

Durations, frame rates, rotations, PTS captions and inspector rows are formatted in Rust (`cutcap-ffi/src/view.rs`, moved from the Slint presentation mapping with its tests) and sent as strings with the raw values the UI needs (milliseconds for the scrubber). The frontend never re-derives source time from floating point. Go-to text is parsed by the same tested Rust grammar via `seek_text`. Library filter and name search are presentation state in C#.

## ADR 015 — CPU frame copy into a reused WriteableBitmap (accepted)

The engine copies each presented RGBA8 frame once, row by row, into a locked Avalonia WriteableBitmap (PixelFormats.Rgba8888, opaque) that is reused while the size is unchanged; the frame is drawn with uniform fit. Measured copy cost for a 1280×720 frame: ~0.3 ms (first frame ~1 ms) versus 1.0–2.4 ms for Slint's SharedPixelBuffer path. No DX12/texture sharing with Avalonia's compositor is attempted before playback exists and measurements show the CPU path is the bottleneck.

## ADR 016 — Windows packaging (accepted for development distribution)

Release publishing is self-contained win-x64 with ReadyToRun (measured: window on screen 570–890 ms vs 1.0–1.2 s without, ~40 MB lower startup working set). Single-file is not used. Native AOT was evaluated and is not enabled: NuGet audit reports GHSA-p3jp-9v83-76w9 (high) for Microsoft.DotNet.ILCompiler 10.0.12 shipped with SDK 10.0.401, and the advisory is not suppressed; re-evaluate after an SDK update. FFmpeg/FFprobe are not bundled (ADR 006/008). No installer or public release.

## ADR 017 — Persistent framed source transport and ABI 2 (accepted engine contract; frontend handoff pending)

Continuous source decoding uses one FFmpeg process per playback/seek epoch, not one per frame. PAM supplies bounded, self-framed RGBA8 packets; official stats_mux_pre records supply nonce/index/integer PTS/timebase/packet size. Strict index and size pairing rejects mismatches, missing timestamps and non-increasing PTS. Free-form showinfo is not a continuous-playback protocol. Native libraries were evaluated but this installation has only the external static CLI; shipping native libraries would require a reviewed reproducible DLL/ABI/license setup. The MediaBackend stream operation keeps future implementations isolated and the Phase 1 single-frame route remains available.

Three decoded frames and two scheduler lookahead frames bound native queues. Instant is the video-only clock master, with actual PTS and explicit pause/buffering/seek resets. Newest available due frame wins; skipped due frames are counted. EOF does not invent last-frame duration. ABI 2 adds Play/Pause/Stop, exact frame epoch/time and transport view, and records delivery on successful copy. FrameInfo stays 24 bytes, no native callbacks/pointers escape, and source requested position remains distinct from copied/clock positions. See PHASE_2_PLAYBACK_CONTRACT.md. The owner relays the contract to Claude; visual bindings are not claimed complete.

Windows children receive a 1 GiB commit limit, one-process cap and kill-on-close job using pinned win-custody 0.1.2 (MIT OR Apache-2.0). Its narrow Job implementation was inspected; no handwritten unsafe was added outside the established ABI. Assignment occurs immediately after spawn, so the tiny launch window is not claimed sandboxed. Nested-job assignment failures stop/reap the child and surface an error. This is resource/lifetime containment, not a file/network security sandbox.

The owner authorized only a diagnostics hook in FrameView.Render and a driver opening the actual existing Avalonia app. It records distinct drawn generations separately from decoded/copied counters; physical scanout is not measured. No XAML, styles or production Play bindings are changed.

## ADR 017 — Playback UI bound to ABI 2 state; render-loop pumping during playback (accepted, Phase 2B)

The source transport reads only engine state: the Play/Pause glyph comes from `desired_playing` and the state; the readout comes from the copied frame's time (otherwise the requested position, labelled); the go-to field follows the requested position. There is no frontend clock. The library, inspector and timeline are re-applied only when their own view data changes. While playback is active, EngineHost also pumps from `TopLevel.RequestAnimationFrame` (once per compositor frame), because the 16 ms DispatcherTimer is quantized by the ~15.6 ms Windows timer: measured render gaps over 40 ms fell from ~30–35 % to ~7–16 % (PHASE_2B_PLAYBACK_UI_REPORT.md). The timer stays the idle path; no busy loop. `CUTCAP_PUMP_MODE=timer` restores timer-only pumping for A/B diagnostics. A vsync-aligned offer deadline would remove the remaining one-vsync outliers but is an engine change, left to the engine owner.

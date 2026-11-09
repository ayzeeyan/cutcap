# Phase 5A — First real MP4 export

Status: implemented and verified on one Windows machine. Not merged. ABI and contract
**11**, core schema **3** and project envelope `format_version` **1**, both unchanged.
The binding rules are in `PHASE_5A_EXPORT_CONTRACT.md`, and ADR 027 records the
decisions and the alternatives rejected.

## 1. Starting repository revision

- **Baseline.** Verified `origin/main` at `16fb04ae8f3346c91844793632769028349166e8`,
  the Phase 4C merge (PR #6). Branch `claude/phase-5a-mp4-export` was cut from it.
- **Confirmed there.** ABI/contract 10, schema 3, envelope 1, FFmpeg/FFprobe 9.0.1
  (external, with `libx264` and `aac`).
- **Baseline gates.** Rust 220 passed and 28 ignored, Desktop tests 63, Interop tests 57.
- **Test machine.** Intel Core Ultra 7 255H (16 logical cores), integrated graphics,
  15.4 GiB usable memory, Windows 11 (10.0.26300), 150% scaling.
- `docs/PRODUCT.md`, named by the task, does not exist. The other named documents were
  read.

## 2. Pre-existing CI fix and evidence

`FiveThousandClipsPlanOnlyTheViewport` failed the post-merge run at 52 ms against a
50 ms limit. The limit timed one cold call: locally the first call took 14.4 ms and a
warm call has a median of 26 µs, about 550 times less. The test was timing the JIT and
the runner, not culling.

The planner now reports how many clips it examined. The test asserts that count (40 to
44, the viewport's reach), and a new test asserts that the count is identical for 5 000
and 100 000 clips. Neither depends on a clock. Removing the culling made the test fail
(2 034 examined, against 40 to 44), so it still guards the behaviour. The timing lives on
as an opt-in warm measurement that reports a distribution and cannot fail on a duration.
The Desktop suite then passed 15 consecutive runs.

The other Desktop timing check (`SnapAndOverlapLookups...`) is a mean over 10 000
lookups with a 1 ms limit, far from this failure mode, and was left unchanged.

## 3. Export architecture and immutable snapshot rules

```
Export click ─► PrepareExport ─► command owner builds an immutable ExportPlan
                                      │
              ExportHost job thread ◄─┘
                preflight ─► mix audio to scratch PCM ─► open one encoder
                ─► evaluate each frame ─► finish ─► FFprobe ─► flush ─► rename
```

- The plan owns the existing immutable `Arc<TimelineIndex>`, the project timebase and
  revision, the selected `TrackId`, expected source fingerprints, the audio clips and the
  committed track controls (`ffi/export/plan.rs`).
- Controls come from the project, not the index, because controls do not renew the
  index. An uncommitted slider preview lives in the session and cannot reach the plan.
- A running job is unaffected by later edits, New or Open. Tests move and remove clips,
  change controls and replace the project mid-export, and the output is unchanged.
- Export changes neither dirty state, revision, history nor a playhead. Nothing is added
  to the project document.
- `audio_mix.rs` was factored out of `audio_transport.rs` unchanged, so live playback and
  export share the same layout, gain and clipping code.

## 4. Source video evaluation

For output frame `n` at project tick `t`: the selected track's clip with
`start <= t < end`; its exact source bounds; decoded frames in real PTS order mapped by
`frame_timeline_time`; and the latest frame with `position <= t`.

- **No frame crosses a cut.** The held frame belongs to the clip's reader, which is
  dropped at the change.
- **Gaps are black frames**, never a held frame.
- **Source tail.** The last eligible frame holds to the clip end.
- **Clip head.** Trims land on source ticks, so a head usually falls between two frames.
  Phase 3D's blank until the first eligible PTS is invisible live but would be a permanent
  black flash on about half of all cuts in a file. Export shows the first eligible frame
  when it is within 100 ms. Past that, the head is black. A frame before the in-point is
  never used.
- **No eligible frame at all** is black and reported. A decoder failure fails the export.

Each decoder runs through a new `render_frames` backend path with the same framing,
timestamp validation, stall detection and process ownership as continuous preview, but
producing finished 1280×720 YUV 4:2:0. The fit is computed exactly in Rust
(`VideoMetadata::fit_within`): display aspect including SAR and rotation, even sizes,
centred padding. Two decoders at most run: the active clip and the next one, within 1 s.

## 5. VFR/CFR conversion

The output is constant 30 fps. The input is never assumed constant: only real PTS values
are compared with each output instant. A frame is reused without decoding until the next
one is due, so a slow source decodes less often than it is shown. Tests compare every
output frame with independent rational arithmetic at four source clocks.

## 6. Offline audio mixing

Every audible track is mixed in project order into 48 kHz stereo `f32`, in blocks of up to
4096 sample frames that stop at each clip boundary, into a scratch file. Each clip's
position and source offset come from Phase 4C's `layout`, including its one ceiling
rounding. Gain is static and uses the committed document value. The final sum is
hard-clipped and counted; there is no normalization. Muted and non-soloed tracks are not
decoded. A decoder failure fails the export. A hole in a source is silence and is counted.
`CUTCAP_AUDIO_LATENCY_MS` and WASAPI are never consulted.

## 7. Exact A/V timestamp policy

- `frames = ceil(extent × 30)`, with extent the later of the selected track's end and the
  last audible clip's end, in exact `i128` arithmetic.
- `samples = frames × 1600`, so both streams have the same nominal length, at most one
  frame beyond the content.
- Frame `n` owns project tick `floor(n / (30 × clock))`, which keeps `[start, end)`
  membership exact at any project clock.
- Frame `n` is presented at `n/30` and sample `k` at `k/48000`, both from zero. FFmpeg
  writes AAC priming as the MP4 edit list, and independent decoding honours it.

The measured result, in §14: every click lands within 4 ms of its exact time, picture and
sound agree within one frame, and drift across the file is under 2 ms.

## 8. Encoder process architecture

One FFmpeg process runs per export: raw frames on standard input, one at a time, blocking
on the pipe, and the mix from the scratch file. A second raw stream would need a named pipe
(new `unsafe` or a new crate, plus an open-order deadlock), a loopback socket (any local
process could connect first), or a hand-written muxer (ADR 027). Raw video never reaches
disk. Audio and video decoders never run together.

Arguments are passed with `Command::arg` and no shell. The process is held in a
kill-on-close job with a 1 GiB commit limit. A watcher thread can end it while a write is
blocked, which is how cancellation and the 120 s stall limit work. Progress and diagnostics
come from its status stream, with diagnostics capped at 64 KiB.

`libx264` and `aac` are checked at session start and again in preflight. Another encoder
is never substituted.

## 9. Output file safety

The destination must be a local absolute `.mp4` in an existing folder, not a reserved
device name and not any source of the project. An existing file is replaced only with
explicit permission (the picker's own prompt, valid for one start). FFmpeg writes a unique
temporary file beside the destination.

After a clean exit the file is validated by FFprobe (codecs, size, pixel format, frame
rate, exact frame count, audio format, duration), flushed, and only then renamed. Failure
or cancellation removes every temporary file and leaves an existing output untouched.

Disk space and write permission are checked by reserving the scratch size plus an
estimate. Disk-full and permission failures are reported as such.

**What is and is not guaranteed.** The rename is `MoveFileExW` within one volume, which
replaces the name in one step on NTFS. Windows documents no general atomicity or power-loss
guarantee for it. A whole-machine crash can leave a temporary file behind. If the host dies,
its child processes end with their job objects.

## 10. Cancellation and progress

States: `validating`, `preparing`, `decoding`, `encoding`, `finalizing`, `cancelling`,
`completed`, `cancelled`, `failed`.

`progress = 0.10 × mixed_samples/samples + 0.88 × encoded_frames/frames`, where
`encoded_frames` is FFmpeg's own count. Frames merely written to the pipe do not count.
`finalizing` holds at 0.98 or less, and only `completed` is 1.0. Nothing is derived from
wall time.

Cancel names its job, so a stale request is refused. The token stops new work and wakes
queues, the watcher kills the encoder, decoders are joined and files removed. The UI never
waits. Closing while exporting asks first, and session shutdown waits at most 5 s.

## 11. Native ABI changes

`ABI_VERSION`, `NativeMethods.AbiVersion` and `CONTRACT_VERSION` are 11. Every ABI 10
export, status number, layout and handle rule is unchanged. New:
`cutcap_session_start_export` and `cutcap_session_cancel_export`, both acceptance-only, and
the coalesced `export` view. No pixels or samples cross the ABI.

## 12. Avalonia Export dialog

The Export button is enabled once FFmpeg has confirmed both encoders, explains itself
otherwise, and shows `Exporting N%` during a job. The dialog states the file, the video
track, the preset, length, heard audio tracks and the captured revision. It requires a
check when other video tracks hold clips, and says when the picture will be black. It is
not modal, so the editor stays usable. Every figure comes from the engine's view.

## 13. Automated tests

Every figure below comes from a run on the final tree.

| Gate | Result |
|---|---|
| `scripts/verify.ps1` (fmt, clippy, tests, Debug and Release builds, .NET tests, self-contained win-x64 publish) | All 9 stages passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Clean |
| `cargo test --workspace --all-features --locked` | **278 passed, 0 failed, 38 ignored** (baseline 220 and 28) |
| .NET Interop tests | 62 passed (baseline 57), with and without an audio device |
| Avalonia desktop tests | 77 passed (baseline 63), with and without an audio device |
| Real FFmpeg suite (ignored by default, run with `--ignored`) | 19 passed |
| Real export media oracles (`export_media.rs`) | 9 passed |
| Real mixed sources and real WASAPI output | 3 passed |
| Engine suite repeated with no audio device | 30 runs, 0 failures |
| `git diff --check`, LF endings, `Cargo.lock` | Clean, no new dependency |

By crate: contracts 11, core 6 + 15 + 7, engine 213, media 26.

`crates/cutcap-ffi/src/export/tests.rs` runs the whole job against a controlled backend
whose every frame and sample identifies its source, comparing each with independent
arithmetic. It covers:

- one clip, adjacent clips, cuts on and between output frames, moved clips, gaps and a
  leading gap;
- start and end trims, splits, nonzero source in-points, nonzero and negative source PTS;
- VFR on four source clocks, and clip heads inside and beyond the 100 ms hold;
- a clip with no eligible frame, and a source tail;
- audio-only timelines, audio longer and shorter than the video, linked audio;
- gain, mute, solo, exact sample placement, clipping and source holes;
- more than 16 audible tracks, track selection and acknowledgment, empty and over-length
  timelines;
- missing and changed sources, a missing encoder, Unicode paths and bad destinations;
- replacing an existing file, encoder failures including disk-full and permission,
  failed validation, cancellation, and temporary-file cleanup;
- a large project, undo and redo, and export after saving and reopening;
- through the session: an immutable snapshot under edits, project replacement, a preview
  gain that must not export, stale job rejection, and shutdown during export;
- progress accounting that cannot reach 1.0 early.

`ExportTests.cs` covers the real ABI, including a real export, cancellation and disposing
during an export. `ExportViewModelTests.cs` covers the dialog's rules.

## 14. Independent MP4 validation

`crates/cutcap-ffi/tests/export_media.rs` runs real exports through the real session and
checks the files with separately invoked FFmpeg and FFprobe:

| Test | What is verified independently |
|---|---|
| Edited multi-track timeline | H.264 High, 1280×720, `yuv420p`, 30/1, exact frame count, BT.709 tags, AAC 48 kHz stereo, duration; the picture at 8 timestamps across trims, a split and two gaps; four tones present and absent over exact intervals, including an independent clip and a muted track; a later undo changing a later export |
| Gain | Measured amplitude ratio within 3% of −12 dB |
| A/V sync | Through a trim and a gap: 7 clicks each within 4 ms of the exact time, picture and sound within one frame, drift under 2 ms |
| VFR and offset sources | Real source times at five sample points; a stream starting at PTS 5000 |
| Long GOP with B-frames | A trim 3 s into a 150-frame GOP matches a separate decode of the source |
| Picture shapes | 4:3 pillarbox, wide letterbox, portrait, anamorphic PAL filling 16:9, rotation metadata |
| Colors | Lime from untagged, BT.709-tagged and BT.601-tagged sources stays within 6 of the source |
| Cancellation | Encoder gone, no leftovers, existing file intact, session can export again |

**Defects these checks found, all fixed.**

| Found by | Defect | Fix |
|---|---|---|
| Hand-run FFprobe | FFmpeg 9 left primaries and transfer untagged when set as output options | `setparams` on the frames |
| Interop test | The finished path was shown as `\\?\C:\…` | Canonical form used only for checks |
| Native oracle | Untagged HD sources were read as BT.709, so colors differed from the preview | Untagged sources follow the preview's path |

## 15. Actual Windows interaction tests

`scripts/native-ui/export-ui.ps1` drives the real app with real pointer and keyboard input,
with CutCap verified as the foreground window before every action. Sources are generated.
Expected values come from the positions actually saved in the project file. Full log:
`docs/measurements/phase5a-native-scenario.txt`.

| Step | Result |
|---|---|
| Import two videos and audio-only files | Pass |
| Place, trim start, place after a gap, split, move | Pass; saved as 2–8, 11–13, 15–19 s |
| Independent audio on Audio 2 at −5.5 dB, muted Audio 3 | Pass |
| Save, open Export, choose path, start | Pass; 19 s timeline in 3.2–3.4 s (Debug) |
| FFprobe | H.264, 1280×720, `yuv420p`, 30 fps, 570 frames, 19.000 s, BT.709, AAC 48 kHz stereo |
| Decoded picture at 9 timestamps and 2 gaps | All correct scene, gaps black |
| Decoded audio | 400 and 800 Hz at their clips (0.25), independent clip at −5.5 dB (0.133), silent before it, muted track absent |
| Windows Media Foundation | Opened headlessly: 19.000 s, 1280×720, position advanced during play |
| Unmute, export again | New file has the 1600 Hz track; first file byte-identical |
| Start over the first file, cancel | Cancelled in 710 ms; old file byte-identical; no temporary files; no FFmpeg left |
| Close during an export (long source) | Prompted; 2 FFmpeg running before; exited in 2.8 s, nothing left, no partial file |
| Schema 1, 2 and 3 projects | Each opens and exports (720, 660, 570 frames); none rewritten |

"Where practical" for the ordinary player: the inbox player's own window was not launched,
because the test may not capture other applications. The same Windows media stack was
exercised through `Windows.Media.Playback.MediaPlayer` with no window.

## 16. Export performance

Release engine on the machine in §1, through the session API. Sources are synthetic, so
bitrates and speeds will differ for real footage. Full data:
`docs/measurements/phase5a-export-bench.json`.

| Scenario | Output | Encoded fps | Encode × real time | Whole job × | Audio mix × | Start to first frame, ms | Decoder starts | Output waits at cuts, mean / max ms | Child memory, MiB | Output MiB |
|---|---|---|---|---|---|---|---|---|---|---|
| 30 s, 6 clips, 1080p | 900 frames | 231 | 7.7 | 6.5 | 72 | 1120 | 6 | 65 / 141 | 395 | 5.4 |
| 60 s, 12 clips, 1080p | 1800 | 257 | 8.6 | 7.4 | 74 | 1491 | 12 | 54 / 124 | 419 | 10.9 |
| 180 s, one clip, 1080p | 5400 | 288 | 9.6 | 9.3 | 701 | 870 | 1 | 90 | 386 | 32.3 |
| VFR 720p | 601 | 374 | 12.5 | 10.5 | 221 | 627 | 1 | 90 | 365 | 4.4 |
| 4K source | 300 | 159 | 5.3 | 4.5 | 127 | 719 | 1 | 146 | 523 | 1.3 |
| 8 audio tracks | 1200 | 343 | 11.4 | 9.8 | 168 | 956 | 1 | 117 | 379 | 5.8 |
| 240 cuts in 60 s | 1800 | 74 | 2.5 | 2.1 | 16 | 4764 | 240 | 70 / 181 | 472 | 10.8 |

- The output is the bottleneck, so decoder throughput is not separately limiting; decoded
  frame counts match what the encoder consumed, and repeats were not decoded (250 of 601
  in the VFR case).
- The host's working set stayed under 50 MiB. Child processes peaked at 5.
- Children used 4.2 to 6.6 cores on average. In the 240-cut run decoders were too short
  for the 200 ms sampler to measure, so its CPU figure is not meaningful.
- Temporary disk is 0.37 MiB per second of output, exactly `samples × 8`; raw video is
  never written.
- Output was about 1.5 Mbps on this synthetic material.
- Cancelling took 37 ms and session shutdown during encoding 42 ms.

**UI responsiveness**, from WM_NULL round trips to the Release app's UI thread during a
180 s export that took 19.9 s:

| | Median | p95 | p99 | Max | Unanswered |
|---|---|---|---|---|---|
| Idle | 0.19 ms | 0.83 ms | 1.34 ms | 1.55 ms | 0 of 160 |
| During export | 0.11 ms | 0.58 ms | 1.46 ms | 5.31 ms | 0 of 412 |

The app used 0.55 cores and at most 378 MiB during that export. These results hold for
this machine. No general speed is promised.

## 17. Known limitations

- **One preset**, 720p30. There is no 1080p, frame-rate choice or quality control yet.
- **One video track.** No compositing; other occupied tracks are left out, after an
  acknowledgment.
- **Export differs from preview at clip heads**, by up to 100 ms, by design (§4).
- **Untagged sources** use FFmpeg's default matrix, matching the preview. Footage that was
  really BT.709 but untagged is slightly off in both. Primaries are not remapped.
- **HDR and BT.2020 sources are refused**, not tone-mapped.
- **No resume.** A failed or cancelled export starts again from the beginning.
- **The disk check is an estimate.** A disk that fills later is reported as a failure.
- **Many short clips are slower** (2.5× real time at 240 clips per minute), since each cut
  starts a decoder.
- **Export and playback together were not measured.** Export children run below normal
  priority, so playback is favoured, and export slows.
- **The Windows player check** used the media stack without its window (§15).
- **Measured on one machine**, with synthetic sources.
- **FFmpeg stays external**, and must include `libx264` and `aac`.

## 18. Licensing and distribution issues

No crate, DLL, codec or font was added. The items below remain open for the owner; see
`LICENSE_REVIEW.md`. None is resolved by having produced a working MP4.

- `libx264` is GPL. The development FFmpeg is GPL-enabled and stays external and unbundled.
  Bundling it would bring GPL obligations the undecided application license would need to
  accommodate.
- Invoking FFmpeg as a subprocess does not by itself settle distribution questions.
- H.264 and AAC are subject to patent licensing programs. Whether distributing an encoder,
  or content made with one, needs a license has not been reviewed.
- No application license was chosen, and CutCap is not claimed to be ready for commercial
  or public distribution.

## 19. Commit hashes

On branch `claude/phase-5a-mp4-export`, on top of `16fb04a`:

| Commit | Subject |
|---|---|
| `147d8ce` | test(ci): stabilize viewport filmstrip regression |
| `698bb74` | feat(export): add deterministic timeline render plan |
| `36d4b00` | feat(export): encode H264 video and mixed AAC audio |
| `7583597` | feat(ui): enable real MP4 export with progress and cancellation |
| `ec64d1b` | test(export): verify encoded timeline against independent media oracles |
| `d616719` | docs: report Phase 5A MP4 export |

A final commit, "docs: record the Phase 5A pull request", adds this section and §20.

Each commit was checked on its own in a clean worktree, with `--all-features --locked` and
no audio device, as CI runs it:

| Commit | Format, clippy | `cargo test` | .NET build | `dotnet test` |
|---|---|---|---|---|
| `147d8ce` | clean | 220 passed | 0 errors | Desktop 64, Interop 57 passed |
| `698bb74` | clean | 231 passed | 0 errors | Desktop 64, Interop 57 passed |
| `36d4b00` | clean | 234 passed | 0 errors | Desktop 64, Interop 57 passed |
| `7583597` | clean | 234 passed | 0 errors | Desktop 64, Interop 57 passed |
| `ec64d1b` | clean | 278 passed | 0 errors | Desktop 77, Interop 62 passed |
| `d616719` | clean | 278 passed | 0 errors | Desktop 77, Interop 62 passed |

The ABI bump and the interop layer travel together in `36d4b00`, as they must.

**Excluded from the commits:** generated media, recordings, screenshots and exported files
(`.artifacts/`, git-ignored), build outputs and the publish folder, and caches. Measurement
logs were copied into `docs/measurements` after checking that they contain no user paths.

## 20. GitHub PR URL and number

- **PR:** [#7](https://github.com/ayzeeyan/cutcap/pull/7) "feat: export edited timelines to
  real H.264/AAC MP4 files (Phase 5A)": base `main`, head `claude/phase-5a-mp4-export`.
- **State at creation:** open, not a draft, auto-merge off, head `d616719`.
- **Push:** `git push -u` without force. The remote head was verified equal to the local
  one, and `origin/main` (`16fb04a`) is an ancestor.
- **CI:** the GitHub workflow was still running when this was written. Its result is on the
  pull request. The runner has no FFmpeg, so real-media tests are skipped or ignored there.
- **For the owner:** left open for review. Neither `main` nor the feature branch is deleted.

## 21. Recommended Phase 5B

**A 1080p preset and a quality choice, measured first.** The 720p pipeline now has
headroom on this machine (about 9× real time for a single 1080p source), so a 1080p preset
is the natural next step and the cost should be measured the same way. The choice of
resolution and quality can be added at that point, still as a small fixed list. Output
latency calibration remains a separate, small follow-up for preview.

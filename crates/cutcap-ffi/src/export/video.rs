//! Offline picture evaluation: which frame every output instant shows.
//!
//! Timing comes only from the plan's clips and each decoded frame's real
//! timestamp (PHASE_5A_EXPORT_CONTRACT.md §5). At most two decoders run: the
//! active clip's and, when the next clip is close, that one's.
use super::{Fail, Progress, plan::ExportPlan};
use cutcap_contracts::*;
use cutcap_core::{
    Clip, EvaluationError, SourceMapping, frame_position, frame_timeline_time, head_hold_ticks,
    map_clip_source,
};
use cutcap_media::{
    Cancellation, FrameEncoder, MediaBackend, MediaError, PreviewTicket, RenderFrame, RenderRequest,
};
use std::{
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Decoded frames waiting per decoder, beyond the held and next frames.
const FRAME_QUEUE: usize = 3;
/// A clip's decoder is started this long before the clip is due.
const PREPARE_SECONDS: i128 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PictureStats {
    /// Frames decoded by FFmpeg and accepted.
    pub decoded: u64,
    /// Output frames that repeated the previous decoded frame without decoding.
    pub reused: u64,
    /// Output frames in a timeline gap.
    pub gap_frames: u64,
    /// Output frames inside a clip with no eligible picture.
    pub no_picture_frames: u64,
    /// Output frames that showed a clip's first frame ahead of its time.
    pub head_frames: u64,
    pub decoder_starts: u64,
    pub clip_changes: u64,
    /// The longest and total time output waited for a clip's first picture.
    pub cut_wait_max_us: u64,
    pub cut_wait_total_us: u64,
}

struct Timed {
    position: TimelineTime,
    frame: RenderFrame,
}

/// One clip's decoder and its position in that clip's frames.
struct ClipReader {
    clip: Clip,
    bounds: SourceMapping,
    ticket: PreviewTicket,
    rx: Receiver<RenderFrame>,
    cancel: Cancellation,
    done: Arc<Mutex<Option<Result<(), MediaError>>>>,
    worker: Option<JoinHandle<()>>,
    held: Option<Timed>,
    next: Option<Timed>,
    eof: bool,
    decoded: u64,
    /// Output has already used the frame in `held` once.
    held_used: bool,
}

impl ClipReader {
    fn open(clip: Clip, plan: &ExportPlan, backend: &Arc<dyn MediaBackend>) -> Result<Self, Fail> {
        let media = plan
            .sources
            .get(&clip.media_id)
            .map(|source| source.media.clone())
            .ok_or_else(|| Fail::Message("A video clip's source is not in the plan.".into()))?;
        let video = media
            .metadata
            .video()
            .ok_or_else(|| Fail::Message("A video clip's source has no picture.".into()))?;
        let bounds = map_clip_source(&clip, clip.start, plan.timebase, video.timebase)
            .map_err(|e| Fail::Message(format!("A clip cannot be mapped to its source: {e}")))?;
        let ticket = PreviewTicket {
            project_id: plan.project,
            media_id: clip.media_id,
            request_id: RequestId::new(),
        };
        let request = RenderRequest {
            ticket,
            media,
            start: bounds.requested,
            preset: plan.preset,
        };
        let (tx, rx) = mpsc::sync_channel(FRAME_QUEUE);
        let cancel = Cancellation::default();
        let done = Arc::new(Mutex::new(None));
        let worker = {
            let (backend, cancel, done) = (backend.clone(), cancel.clone(), done.clone());
            thread::Builder::new()
                .name("cutcap-export-decoder".into())
                .spawn(move || {
                    let result = backend.render_frames(&request, &cancel, &mut |frame| {
                        tx.send(frame).map_err(|_| MediaError::Cancelled)
                    });
                    *done.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
                })
                .map_err(|e| Fail::io("Starting a video decoder", &e))?
        };
        Ok(Self {
            clip,
            bounds,
            ticket,
            rx,
            cancel,
            done,
            worker: Some(worker),
            held: None,
            next: None,
            eof: false,
            decoded: 0,
            held_used: false,
        })
    }

    /// The decoder has stopped: `Ok` means its stream simply ended.
    fn outcome(&self) -> Option<Result<(), Fail>> {
        let done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        match done.as_ref()? {
            Ok(()) | Err(MediaError::NoFrame) => Some(Ok(())),
            Err(MediaError::Cancelled) => Some(Err(Fail::Cancelled)),
            Err(error) => Some(Err(Fail::media("Video decoding", error))),
        }
    }

    /// Accepts one decoded frame, or ends the clip's stream at its source end.
    fn accept(&mut self, frame: RenderFrame, clock: Timebase) -> Result<(), Fail> {
        if frame.ticket != self.ticket || frame.actual.timebase != self.bounds.requested.timebase {
            return Err(Fail::Message(
                "A decoded frame does not belong to its clip.".into(),
            ));
        }
        if frame.actual.ticks >= self.bounds.end_exclusive.ticks {
            self.finish_stream();
            return Ok(());
        }
        let eligible = self
            .bounds
            .accepts(frame.actual)
            .map_err(|e| Fail::Message(e.to_string()))?;
        if !eligible {
            return Err(Fail::Message(
                "A decoded frame lies outside its clip's source range.".into(),
            ));
        }
        match frame_timeline_time(&self.clip, frame.actual, clock) {
            Ok(position) => {
                self.decoded += 1;
                self.next = Some(Timed { position, frame });
            }
            // Rounding can put the final fractional source tick at the clip end.
            Err(EvaluationError::OutsideClip) => self.finish_stream(),
            Err(error) => return Err(Fail::Message(error.to_string())),
        }
        Ok(())
    }

    fn finish_stream(&mut self) {
        self.eof = true;
        self.cancel.cancel();
    }

    /// Blocks until the next frame is known, or the stream has ended.
    fn fill_next(&mut self, clock: Timebase, cancel: &Cancellation) -> Result<(), Fail> {
        while self.next.is_none() && !self.eof {
            if cancel.is_cancelled() {
                return Err(Fail::Cancelled);
            }
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(frame) => self.accept(frame, clock)?,
                Err(RecvTimeoutError::Timeout) => {
                    // A decoder publishes its result after its last frame, so
                    // read the result first and the queue again after it.
                    if let Some(outcome) = self.outcome() {
                        match self.rx.try_recv() {
                            Ok(frame) => self.accept(frame, clock)?,
                            Err(_) => {
                                outcome?;
                                self.eof = true;
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.outcome().unwrap_or(Ok(()))?;
                    self.eof = true;
                }
            }
        }
        Ok(())
    }

    /// Moves to the latest frame due at `t`, reading exactly as far as that
    /// requires: one frame past it, or the end of the stream.
    fn advance(
        &mut self,
        t: TimelineTime,
        clock: Timebase,
        cancel: &Cancellation,
    ) -> Result<(), Fail> {
        loop {
            self.fill_next(clock, cancel)?;
            match self.next.as_ref() {
                Some(next) if next.position <= t => {
                    self.held = self.next.take();
                    self.held_used = false;
                }
                _ => return Ok(()),
            }
        }
    }
}

impl Drop for ClipReader {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Dropping the receiver unblocks a decoder waiting to send.
        let (_tx, rx) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.rx, rx));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Renders every output frame of the plan into `encoder`.
pub fn render(
    plan: &ExportPlan,
    backend: &Arc<dyn MediaBackend>,
    encoder: &mut dyn FrameEncoder,
    progress: &mut Progress<'_>,
    cancel: &Cancellation,
) -> Result<PictureStats, Fail> {
    let clock = plan.timebase;
    let fps = plan.preset.fps;
    let black = black_frame(plan.preset);
    let head = head_hold_ticks(clock).map_err(|e| Fail::Message(e.to_string()))?;
    let prepare_ticks =
        PREPARE_SECONDS * i128::from(clock.denominator()) / i128::from(clock.numerator());
    let mut stats = PictureStats::default();
    let mut active: Option<ClipReader> = None;
    let mut prepared: Option<ClipReader> = None;
    for n in 0..plan.grid.frames {
        if cancel.is_cancelled() {
            return Err(Fail::Cancelled);
        }
        let t = frame_position(n, clock, fps).map_err(|e| Fail::Message(e.to_string()))?;
        let clip = match plan.video_track {
            Some(track) => plan
                .index
                .active(plan.project, track, t)
                .map_err(|e| Fail::Message(e.to_string()))?
                .cloned(),
            None => None,
        };
        // Nothing crosses a cut: the old clip's held frame goes with its reader.
        if active.as_ref().map(|r| r.clip.id) != clip.as_ref().map(|c| c.id) {
            stats.decoded += active.take().map_or(0, |r| r.decoded);
            if let Some(clip) = clip {
                stats.clip_changes += 1;
                active = Some(match prepared.take().filter(|r| r.clip.id == clip.id) {
                    Some(reader) => reader,
                    None => {
                        stats.decoder_starts += 1;
                        ClipReader::open(clip, plan, backend)?
                    }
                });
            }
        }
        let next = plan
            .video_track
            .and_then(|track| plan.index.next(track, t))
            .filter(|c| i128::from(c.start.0) - i128::from(t.0) <= prepare_ticks)
            .cloned();
        if let Some(next) = next
            && prepared.as_ref().is_none_or(|r| r.clip.id != next.id)
        {
            stats.decoded += prepared.take().map_or(0, |r| r.decoded);
            stats.decoder_starts += 1;
            prepared = Some(ClipReader::open(next, plan, backend)?);
        }
        let (picture, repeated): (&[u8], bool) = match active.as_mut() {
            None => {
                stats.gap_frames += 1;
                (&black, false)
            }
            Some(reader) => {
                let began = Instant::now();
                let waiting = reader.held.is_none();
                reader.advance(t, clock, cancel)?;
                if waiting {
                    let waited = began.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
                    stats.cut_wait_total_us += waited;
                    stats.cut_wait_max_us = stats.cut_wait_max_us.max(waited);
                }
                match (&reader.held, &reader.next) {
                    (Some(held), _) => {
                        let repeated = reader.held_used;
                        reader.held_used = true;
                        (held.frame.planes.as_slice(), repeated)
                    }
                    // Before the clip's first eligible frame (contract §5).
                    (None, Some(next)) if next.position.0.saturating_sub(t.0) <= head => {
                        stats.head_frames += 1;
                        (next.frame.planes.as_slice(), false)
                    }
                    (None, _) => {
                        stats.no_picture_frames += 1;
                        (&black, false)
                    }
                }
            }
        };
        if repeated {
            stats.reused += 1;
        }
        encoder.write_frame(picture).map_err(|e| match e {
            MediaError::Cancelled => Fail::Cancelled,
            other => Fail::media("Encoding", &other),
        })?;
        progress.submitted(n + 1, encoder.encoded());
    }
    stats.decoded += active
        .iter()
        .chain(prepared.iter())
        .map(|r| r.decoded)
        .sum::<u64>();
    Ok(stats)
}

fn black_frame(preset: ExportPreset) -> Vec<u8> {
    let luma = (preset.width as usize) * (preset.height as usize);
    let mut frame = vec![128u8; preset.frame_bytes()];
    frame[..luma].fill(16);
    frame
}

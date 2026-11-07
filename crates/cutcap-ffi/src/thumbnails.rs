//! Low-priority, bounded filmstrip thumbnail worker (Phase 4A).
//!
//! The frontend sends its whole current demand (visible cells first, then a
//! small overscan) each time the viewport settles; the demand **replaces** the
//! previous one, so scrolled-away samples never reach the decoder and an
//! in-flight batch whose samples are no longer wanted is cancelled. One worker
//! thread owns at most one FFmpeg batch at a time (up to `MAX_BATCH` samples
//! of one source, two while playback is active, with a pause between batches).
//! Completed results wait in a bounded queue until the frontend copies them;
//! nothing here touches the project, the document revision or history.
use cutcap_contracts::{MediaSummary, SourceTime};
use cutcap_media::{
    Cancellation, MAX_BATCH, MediaBackend, MediaError, Thumbnail, ThumbnailRequest,
};
use serde::Serialize;
use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

/// Largest accepted demand; anything beyond the viewport is speculative.
pub const MAX_DEMAND: usize = 128;
/// Completed results waiting for the frontend before the worker pauses. The
/// check precedes each batch, so at most MAX_WAITING + MAX_BATCH are held.
pub const MAX_WAITING: usize = 64;
const THROTTLED_BATCH: usize = 2;
const THROTTLED_PAUSE: Duration = Duration::from_millis(150);

#[derive(Debug, Clone)]
pub struct Job {
    pub token: u64,
    pub media: MediaSummary,
    /// Identity of the verified source the frontend saw (stale-result gate).
    pub source_key: String,
    pub position: SourceTime,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ready,
    NoFrame,
    Failed,
}

#[derive(Debug)]
pub struct Done {
    pub token: u64,
    pub source_key: String,
    pub status: Status,
    pub thumbnail: Option<Thumbnail>,
}

/// Diagnostics for the view and measurements; never project data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ThumbnailStats {
    pub pending: usize,
    pub in_flight: usize,
    pub waiting: usize,
    pub decoded: u64,
    pub no_frame: u64,
    pub failed: u64,
    pub batches: u64,
    pub cancelled_batches: u64,
    pub last_batch_ms: Option<u64>,
    pub last_process_id: Option<u32>,
    pub throttled: bool,
}

#[derive(Default)]
struct State {
    pending: VecDeque<Job>,
    in_flight: Vec<u64>,
    cancel: Option<Cancellation>,
    done: VecDeque<Done>,
    throttled: bool,
    shutdown: bool,
    stats: ThumbnailStats,
}

pub struct ThumbnailService {
    shared: Arc<(Mutex<State>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ThumbnailService {
    pub fn start(backend: Arc<dyn MediaBackend>) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let inner = shared.clone();
        let worker = thread::Builder::new()
            .name("cutcap-thumbnails".into())
            .spawn(move || run(&inner, backend.as_ref()))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Replaces the demand. Duplicates, already-waiting tokens and tokens in
    /// flight are not queued twice; an in-flight batch nobody wants is cancelled.
    pub fn set_demand(&self, jobs: Vec<Job>) {
        let mut state = self.state();
        let waiting: HashSet<u64> = state.done.iter().map(|d| d.token).collect();
        let mut seen = HashSet::new();
        let wanted: VecDeque<Job> = jobs
            .into_iter()
            .take(MAX_DEMAND)
            .filter(|j| seen.insert(j.token))
            .collect();
        if !state.in_flight.is_empty()
            && !state.in_flight.iter().any(|t| seen.contains(t))
            && let Some(cancel) = &state.cancel
        {
            cancel.cancel();
        }
        let in_flight: HashSet<u64> = state.in_flight.iter().copied().collect();
        state.pending = wanted
            .into_iter()
            .filter(|j| !waiting.contains(&j.token) && !in_flight.contains(&j.token))
            .collect();
        state.stats.pending = state.pending.len();
        self.shared.1.notify_all();
    }

    /// Results that need no decode (outside the source range) skip the worker.
    pub fn complete_now(&self, token: u64, source_key: String, status: Status) {
        let mut state = self.state();
        if state.done.len() < MAX_WAITING {
            state.done.push_back(Done {
                token,
                source_key,
                status,
                thumbnail: None,
            });
            state.stats.waiting = state.done.len();
        }
    }

    pub fn take(&self) -> Option<Done> {
        let mut state = self.state();
        let done = state.done.pop_front();
        state.stats.waiting = state.done.len();
        self.shared.1.notify_all();
        done
    }

    /// Playback active: smaller batches and a pause between them.
    pub fn set_throttled(&self, throttled: bool) {
        let mut state = self.state();
        if state.throttled != throttled {
            state.throttled = throttled;
            state.stats.throttled = throttled;
            self.shared.1.notify_all();
        }
    }

    /// Project replacement: drop everything, cancel the running batch.
    pub fn clear(&self) {
        let mut state = self.state();
        state.pending.clear();
        state.done.clear();
        if let Some(cancel) = &state.cancel {
            cancel.cancel();
        }
        state.in_flight.clear();
        state.stats.pending = 0;
        state.stats.waiting = 0;
    }

    pub fn stats(&self) -> ThumbnailStats {
        self.state().stats.clone()
    }

    /// Cancels the running FFmpeg batch (it is killed and reaped by the media
    /// process owner) and joins the worker.
    pub fn shutdown(&mut self) {
        {
            let mut state = self.state();
            state.shutdown = true;
            if let Some(cancel) = &state.cancel {
                cancel.cancel();
            }
            self.shared.1.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _joined = worker.join();
        }
    }
}

impl Drop for ThumbnailService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(shared: &(Mutex<State>, Condvar), backend: &dyn MediaBackend) {
    let (lock, wake) = shared;
    loop {
        let (batch, cancel, media, height, key) = {
            let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
            while !state.shutdown && (state.pending.is_empty() || state.done.len() >= MAX_WAITING) {
                state = wake.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            if state.shutdown {
                return;
            }
            let limit = if state.throttled {
                THROTTLED_BATCH
            } else {
                MAX_BATCH
            };
            let first = state.pending.pop_front().expect("nonempty");
            let mut batch = vec![first];
            // Same source and size only; keep the frontend's priority order.
            let mut index = 0;
            while batch.len() < limit && index < state.pending.len() {
                let same = state.pending[index].source_key == batch[0].source_key
                    && state.pending[index].height == batch[0].height;
                if same {
                    let job = state.pending.remove(index).expect("index");
                    batch.push(job);
                } else {
                    index += 1;
                }
            }
            let cancel = Cancellation::default();
            state.in_flight = batch.iter().map(|j| j.token).collect();
            state.cancel = Some(cancel.clone());
            state.stats.pending = state.pending.len();
            state.stats.in_flight = batch.len();
            let media = batch[0].media.clone();
            let height = batch[0].height;
            let key = batch[0].source_key.clone();
            (batch, cancel, media, height, key)
        };
        let requests: Vec<ThumbnailRequest> = batch
            .iter()
            .map(|j| ThumbnailRequest {
                token: j.token,
                position: j.position,
            })
            .collect();
        let started = Instant::now();
        let outcome = backend.thumbnails(&media, height, &requests, &cancel);
        let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
        state.in_flight.clear();
        state.cancel = None;
        state.stats.in_flight = 0;
        state.stats.batches += 1;
        state.stats.last_batch_ms =
            Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        let throttled = state.throttled;
        if cancel.is_cancelled() {
            state.stats.cancelled_batches += 1;
            continue;
        }
        match outcome {
            Err(MediaError::Cancelled) => state.stats.cancelled_batches += 1,
            Err(_) => {
                // A whole-batch failure marks each sample failed (no endless retry).
                for job in &batch {
                    state.stats.failed += 1;
                    state.done.push_back(Done {
                        token: job.token,
                        source_key: key.clone(),
                        status: Status::Failed,
                        thumbnail: None,
                    });
                }
            }
            Ok(results) => {
                for (job, result) in batch.iter().zip(results) {
                    let (status, thumbnail) = match result {
                        Ok(t) => {
                            state.stats.decoded += 1;
                            (Status::Ready, Some(t))
                        }
                        Err(MediaError::NoFrame) => {
                            state.stats.no_frame += 1;
                            (Status::NoFrame, None)
                        }
                        Err(_) => {
                            state.stats.failed += 1;
                            (Status::Failed, None)
                        }
                    };
                    if let Some(t) = &thumbnail {
                        state.stats.last_process_id = Some(t.process_id);
                    }
                    state.done.push_back(Done {
                        token: job.token,
                        source_key: key.clone(),
                        status,
                        thumbnail,
                    });
                }
            }
        }
        state.stats.waiting = state.done.len();
        if throttled && !state.shutdown {
            let _ = wake.wait_timeout(state, THROTTLED_PAUSE);
        }
    }
}

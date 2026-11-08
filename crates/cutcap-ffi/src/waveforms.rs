//! Low-priority, bounded waveform analysis worker (Phase 4B).
//!
//! Mirrors the Phase 4A thumbnail worker. The frontend sends its whole
//! current demand (chunks of visible audio clips first, then a small
//! overscan); the demand replaces the previous one and an in-flight batch
//! nobody wants is cancelled. One worker thread owns at most one FFmpeg
//! process at a time, analysing up to `MAX_PEAK_CHUNKS` consecutive chunks of
//! one stream at native rate and below-normal priority (one chunk with a
//! pause while playback runs, so analysis never competes with the audio
//! output). Results wait in a bounded queue; nothing here touches the project.
use cutcap_contracts::MediaSummary;
use cutcap_media::{Cancellation, MAX_PEAK_CHUNKS, MediaBackend, MediaError, Peaks};
use serde::Serialize;
use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

pub const MAX_DEMAND: usize = 128;
pub const MAX_WAITING: usize = 32;
const THROTTLED_PAUSE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct Job {
    pub token: u64,
    pub media: MediaSummary,
    /// Identity of the verified source and stream the frontend saw.
    pub audio_key: String,
    pub stream: u32,
    pub chunk: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ready,
    Failed,
}

#[derive(Debug)]
pub struct Done {
    pub token: u64,
    pub audio_key: String,
    pub status: Status,
    pub peaks: Option<Peaks>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WaveformStats {
    pub pending: usize,
    pub in_flight: usize,
    pub waiting: usize,
    pub analysed: u64,
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
    stats: WaveformStats,
}

pub struct WaveformService {
    shared: Arc<(Mutex<State>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
}

impl WaveformService {
    pub fn start(backend: Arc<dyn MediaBackend>) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let inner = shared.clone();
        let worker = thread::Builder::new()
            .name("cutcap-waveforms".into())
            .spawn(move || run(&inner, backend.as_ref()))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Replaces the demand (see the thumbnail worker for the same rules).
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

    pub fn take(&self) -> Option<Done> {
        let mut state = self.state();
        let done = state.done.pop_front();
        state.stats.waiting = state.done.len();
        self.shared.1.notify_all();
        done
    }

    /// Playback active: one chunk per batch and a pause between batches.
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

    pub fn stats(&self) -> WaveformStats {
        self.state().stats.clone()
    }

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

impl Drop for WaveformService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(shared: &(Mutex<State>, Condvar), backend: &dyn MediaBackend) {
    let (lock, wake) = shared;
    loop {
        let (batch, cancel) = {
            let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
            while !state.shutdown && (state.pending.is_empty() || state.done.len() >= MAX_WAITING) {
                state = wake.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            if state.shutdown {
                return;
            }
            let limit = if state.throttled { 1 } else { MAX_PEAK_CHUNKS };
            let first = state.pending.pop_front().expect("nonempty");
            let mut batch = vec![first];
            // Consecutive chunks of the same stream, in demand order.
            while batch.len() < limit {
                let last = batch.last().expect("nonempty");
                let Some(index) = state.pending.iter().position(|j| {
                    j.audio_key == last.audio_key
                        && j.stream == last.stream
                        && j.chunk == last.chunk + 1
                }) else {
                    break;
                };
                let job = state.pending.remove(index).expect("index");
                batch.push(job);
            }
            let cancel = Cancellation::default();
            state.in_flight = batch.iter().map(|j| j.token).collect();
            state.cancel = Some(cancel.clone());
            state.stats.pending = state.pending.len();
            state.stats.in_flight = batch.len();
            (batch, cancel)
        };
        let started = Instant::now();
        let head = &batch[0];
        let outcome =
            backend.audio_peaks(&head.media, head.stream, head.chunk, batch.len(), &cancel);
        let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
        state.in_flight.clear();
        state.cancel = None;
        state.stats.in_flight = 0;
        state.stats.batches += 1;
        state.stats.last_batch_ms =
            Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        let throttled = state.throttled;
        if cancel.is_cancelled() || matches!(outcome, Err(MediaError::Cancelled)) {
            state.stats.cancelled_batches += 1;
            continue;
        }
        match outcome {
            Ok(peaks) => {
                for (job, peaks) in batch.iter().zip(peaks) {
                    state.stats.analysed += 1;
                    state.stats.last_process_id = Some(peaks.process_id).filter(|p| *p != 0);
                    state.done.push_back(Done {
                        token: job.token,
                        audio_key: job.audio_key.clone(),
                        status: Status::Ready,
                        peaks: Some(peaks),
                    });
                }
            }
            Err(_) => {
                // One failure marks the batch failed (no endless retry).
                for job in &batch {
                    state.stats.failed += 1;
                    state.done.push_back(Done {
                        token: job.token,
                        audio_key: job.audio_key.clone(),
                        status: Status::Failed,
                        peaks: None,
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

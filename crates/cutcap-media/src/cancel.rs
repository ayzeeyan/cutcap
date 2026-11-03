use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct Cancellation(Arc<State>);

#[derive(Default)]
struct State {
    cancelled: AtomicBool,
    wait: Mutex<()>,
    changed: Condvar,
}

impl Cancellation {
    pub fn cancel(&self) {
        // Pair the predicate update with the wait mutex: cancellation between a
        // predicate check and sleeping must not lose its notification.
        let _guard = self
            .0
            .wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0.cancelled.store(true, Ordering::Release);
        self.0.changed.notify_all();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    /// Timed condition-variable wait, not a spin loop. Cancellation wakes promptly.
    pub(crate) fn wait(&self, duration: Duration) {
        if self.is_cancelled() {
            return;
        }
        let lock = self
            .0
            .wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _wait_result = self
            .0
            .changed
            .wait_timeout_while(lock, duration, |_| !self.is_cancelled());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Instant};
    #[test]
    fn cancellation_wakes_all_waiters_and_is_sticky() {
        let token = Cancellation::default();
        let (ready, started) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..8 {
            let (cancel, ready, done) = (token.clone(), ready.clone(), done.clone());
            workers.push(thread::spawn(move || {
                ready.send(()).unwrap();
                cancel.wait(Duration::from_secs(5));
                done.send(cancel.is_cancelled()).unwrap();
            }));
        }
        for _ in 0..8 {
            started.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        token.cancel();
        for _ in 0..8 {
            assert!(finished.recv_timeout(Duration::from_secs(1)).unwrap());
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let now = Instant::now();
        token.wait(Duration::from_secs(5));
        assert!(now.elapsed() < Duration::from_secs(1));
    }
}

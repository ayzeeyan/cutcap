use crate::{Cancellation, DecodeRequest, DecodedFrame, MediaBackend, MediaError, PreviewTicket};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

struct State {
    desired: Option<PreviewTicket>,
    pending: Option<DecodeRequest>,
    completed: Option<DecodeOutcome>,
    active: Option<Cancellation>,
    stopped: bool,
}
struct Mailbox {
    state: Mutex<State>,
    changed: Condvar,
}

/// One active decode, one replaceable pending request, one replaceable result.
/// Buffers never enter the reliable project-event channel.
pub struct LatestDecoder {
    mailbox: Arc<Mailbox>,
    worker: Option<JoinHandle<()>>,
}

/// Successes and failures carry the same correlation identity.
pub struct DecodeOutcome {
    pub ticket: PreviewTicket,
    pub result: Result<DecodedFrame, MediaError>,
}

impl LatestDecoder {
    pub fn start(backend: Arc<dyn MediaBackend>) -> Result<Self, std::io::Error> {
        let mailbox = Arc::new(Mailbox {
            state: Mutex::new(State {
                desired: None,
                pending: None,
                completed: None,
                active: None,
                stopped: false,
            }),
            changed: Condvar::new(),
        });
        let shared = mailbox.clone();
        let worker = thread::Builder::new()
            .name("cutcap-frame-decoder".into())
            .spawn(move || {
                loop {
                    let (request, token) = {
                        let Ok(state) = shared.state.lock() else {
                            return;
                        };
                        let Ok(mut state) = shared
                            .changed
                            .wait_while(state, |s| s.pending.is_none() && !s.stopped)
                        else {
                            return;
                        };
                        if state.stopped {
                            return;
                        }
                        let Some(request) = state.pending.take() else {
                            continue;
                        };
                        let token = Cancellation::default();
                        state.active = Some(token.clone());
                        (request, token)
                    };
                    let ticket = request.ticket;
                    let result = backend.decode(&request, &token);
                    let Ok(mut state) = shared.state.lock() else {
                        return;
                    };
                    state.active = None;
                    if !state.stopped && state.desired == Some(ticket) && !token.is_cancelled() {
                        state.completed = Some(DecodeOutcome { ticket, result });
                    }
                }
            })?;
        Ok(Self {
            mailbox,
            worker: Some(worker),
        })
    }
    pub fn request(&self, request: DecodeRequest) -> Result<(), MediaError> {
        let mut state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
        if state.stopped || self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(MediaError::Worker);
        }
        if let Some(token) = &state.active {
            token.cancel();
        }
        state.desired = Some(request.ticket);
        state.completed = None;
        state.pending = Some(request);
        self.mailbox.changed.notify_one();
        Ok(())
    }
    pub fn cancel(&self) -> Result<(), MediaError> {
        let mut state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
        if let Some(token) = &state.active {
            token.cancel();
        }
        state.desired = None;
        state.pending = None;
        state.completed = None;
        Ok(())
    }
    pub fn take(&self) -> Result<Option<DecodeOutcome>, MediaError> {
        let mut state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
        if !state.stopped && self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(MediaError::Worker);
        }
        Ok(state.completed.take())
    }
    pub fn shutdown(&mut self) -> Result<(), MediaError> {
        {
            let mut state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
            state.stopped = true;
            state.pending = None;
            state.completed = None;
            if let Some(token) = &state.active {
                token.cancel();
            }
            self.mailbox.changed.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| MediaError::Worker)?;
        }
        Ok(())
    }
}
impl Drop for LatestDecoder {
    fn drop(&mut self) {
        let _shutdown_result = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProbeResult, RgbaFrame};
    use cutcap_contracts::*;
    use std::{
        path::Path,
        sync::mpsc,
        time::{Duration, Instant},
    };

    struct Controlled {
        started: mpsc::Sender<(PreviewTicket, Cancellation)>,
        release: Mutex<mpsc::Receiver<()>>,
        cooperative: bool,
    }
    impl MediaBackend for Controlled {
        fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
            Err(MediaError::Unsupported("test".into()))
        }
        fn decode(
            &self,
            request: &DecodeRequest,
            cancel: &Cancellation,
        ) -> Result<DecodedFrame, MediaError> {
            self.started.send((request.ticket, cancel.clone())).unwrap();
            if self.cooperative {
                cancel.wait(Duration::from_secs(2));
                return Err(MediaError::Cancelled);
            }
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            // Deliberately ignore cancellation; the mailbox must reject stale success/error.
            if request.position.ticks == 0 {
                return Err(MediaError::NoFrame);
            }
            Ok(DecodedFrame {
                ticket: request.ticket,
                requested: request.position,
                actual: request.position,
                image: RgbaFrame::new(1, 1, 4, vec![255, 0, 0, 255]).unwrap(),
                decode_us: 1,
                process_id: 0,
            })
        }
    }
    fn request(ticks: i64) -> DecodeRequest {
        let metadata =
            crate::parse_probe(&serde_json::to_vec(&crate::probe::tests::fixture()).unwrap())
                .unwrap();
        let media = MediaSummary {
            id: MediaId::new(),
            path: "fixture.mp4".into(),
            metadata,
        };
        DecodeRequest {
            ticket: PreviewTicket {
                project_id: ProjectId::new(),
                media_id: media.id,
                request_id: RequestId::new(),
            },
            position: SourceTime {
                ticks,
                timebase: media.metadata.timebase,
            },
            media,
        }
    }
    fn ready(decoder: &LatestDecoder) -> DecodeOutcome {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(outcome) = decoder.take().unwrap() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "decoder did not complete");
            thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn latest_pending_request_replaces_backlog_and_rejects_stale_errors() {
        let (started, events) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut decoder = LatestDecoder::start(Arc::new(Controlled {
            started,
            release: Mutex::new(gate),
            cooperative: false,
        }))
        .unwrap();
        decoder.request(request(0)).unwrap();
        let (_, old_token) = events.recv_timeout(Duration::from_secs(2)).unwrap();
        decoder.request(request(1)).unwrap();
        let newest = request(2);
        let ticket = newest.ticket;
        decoder.request(newest).unwrap();
        assert!(old_token.is_cancelled());
        release.send(()).unwrap();
        let (next, _) = events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(next, ticket);
        assert!(decoder.take().unwrap().is_none());
        release.send(()).unwrap();
        let outcome = ready(&decoder);
        assert_eq!(outcome.ticket, ticket);
        assert_eq!(outcome.result.unwrap().actual.ticks, 2);
        assert!(decoder.take().unwrap().is_none());
        decoder.shutdown().unwrap();
    }
    #[test]
    fn explicit_cancel_discards_late_success_and_clears_pending_results() {
        let (started, events) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut decoder = LatestDecoder::start(Arc::new(Controlled {
            started,
            release: Mutex::new(gate),
            cooperative: false,
        }))
        .unwrap();
        decoder.request(request(1)).unwrap();
        let (_, token) = events.recv_timeout(Duration::from_secs(2)).unwrap();
        decoder.cancel().unwrap();
        assert!(token.is_cancelled());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while decoder.mailbox.state.lock().unwrap().active.is_some() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(decoder.take().unwrap().is_none());
        decoder.shutdown().unwrap();
    }
    #[test]
    fn shutdown_wakes_active_work_and_joins_worker() {
        let (started, events) = mpsc::channel();
        let (_release, gate) = mpsc::channel();
        let mut decoder = LatestDecoder::start(Arc::new(Controlled {
            started,
            release: Mutex::new(gate),
            cooperative: true,
        }))
        .unwrap();
        decoder.request(request(1)).unwrap();
        let (_, token) = events.recv_timeout(Duration::from_secs(2)).unwrap();
        let now = Instant::now();
        decoder.shutdown().unwrap();
        assert!(token.is_cancelled());
        assert!(now.elapsed() < Duration::from_secs(1));
        assert!(decoder.request(request(2)).is_err());
    }
}

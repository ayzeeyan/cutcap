use crate::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

pub const PLAYBACK_QUEUE_FRAMES: usize = 3;
struct State {
    desired: Option<PreviewTicket>,
    pending: Option<DecodeRequest>,
    active: Option<Cancellation>,
    frames: VecDeque<DecodedFrame>,
    terminal: Option<Result<(), MediaError>>,
    decoded: u64,
    pid: Option<u32>,
    stopped: bool,
}
struct Mailbox {
    state: Mutex<State>,
    changed: Condvar,
}
pub struct PersistentDecoder {
    mailbox: Arc<Mailbox>,
    worker: Option<JoinHandle<()>>,
}
pub struct DecodeProgress {
    pub decoded: u64,
    pub process_id: Option<u32>,
    pub buffered: usize,
}
impl PersistentDecoder {
    pub fn start(backend: Arc<dyn MediaBackend>) -> Result<Self, std::io::Error> {
        let mailbox = Arc::new(Mailbox {
            state: Mutex::new(State {
                desired: None,
                pending: None,
                active: None,
                frames: VecDeque::new(),
                terminal: None,
                decoded: 0,
                pid: None,
                stopped: false,
            }),
            changed: Condvar::new(),
        });
        let shared = mailbox.clone();
        let worker = thread::Builder::new()
            .name("cutcap-continuous-decoder".into())
            .spawn(move || {
                loop {
                    let (request, cancel) = {
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
                    let result = backend.stream(&request, &cancel, &mut |frame| {
                        if frame.ticket != ticket {
                            return Err(MediaError::Invalid(
                                "continuous frame identity mismatch".into(),
                            ));
                        }
                        let mut state = shared.state.lock().map_err(|_| MediaError::Worker)?;
                        while state.frames.len() >= PLAYBACK_QUEUE_FRAMES
                            && !state.stopped
                            && state.desired == Some(ticket)
                            && !cancel.is_cancelled()
                        {
                            state = shared
                                .changed
                                .wait_timeout(state, std::time::Duration::from_millis(20))
                                .map_err(|_| MediaError::Worker)?
                                .0;
                        }
                        if state.stopped || state.desired != Some(ticket) || cancel.is_cancelled() {
                            return Err(MediaError::Cancelled);
                        }
                        state.decoded = state.decoded.saturating_add(1);
                        state.pid = Some(frame.process_id);
                        state.frames.push_back(frame);
                        Ok(())
                    });
                    let Ok(mut state) = shared.state.lock() else {
                        return;
                    };
                    state.active = None;
                    if !state.stopped && state.desired == Some(ticket) {
                        state.terminal = Some(result);
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
        state.pending = Some(request);
        state.frames.clear();
        state.terminal = None;
        state.decoded = 0;
        state.pid = None;
        self.mailbox.changed.notify_all();
        Ok(())
    }
    pub fn cancel(&self) -> Result<(), MediaError> {
        let mut state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
        if let Some(token) = &state.active {
            token.cancel();
        }
        state.desired = None;
        state.pending = None;
        state.frames.clear();
        state.terminal = None;
        state.pid = None;
        self.mailbox.changed.notify_all();
        Ok(())
    }
    pub fn take(&self) -> Result<Option<DecodedFrame>, MediaError> {
        let mut state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
        if !state.stopped && self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(MediaError::Worker);
        }
        let frame = state.frames.pop_front();
        self.mailbox.changed.notify_all();
        Ok(frame)
    }
    pub fn terminal(&self) -> Result<Option<Result<(), MediaError>>, MediaError> {
        Ok(self
            .mailbox
            .state
            .lock()
            .map_err(|_| MediaError::Worker)?
            .terminal
            .take())
    }
    pub fn progress(&self) -> Result<DecodeProgress, MediaError> {
        let state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
        Ok(DecodeProgress {
            decoded: state.decoded,
            process_id: state.pid,
            buffered: state.frames.len(),
        })
    }
    pub fn shutdown(&mut self) -> Result<(), MediaError> {
        self.cancel()?;
        {
            let mut state = self.mailbox.state.lock().map_err(|_| MediaError::Worker)?;
            state.stopped = true;
            self.mailbox.changed.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| MediaError::Worker)?;
        }
        Ok(())
    }
}
impl Drop for PersistentDecoder {
    fn drop(&mut self) {
        let _result = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutcap_contracts::*;
    use std::{
        path::Path,
        time::{Duration, Instant},
    };
    struct Producer;
    impl MediaBackend for Producer {
        fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
            Err(MediaError::Worker)
        }
        fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
            Err(MediaError::Worker)
        }
        fn stream(
            &self,
            r: &DecodeRequest,
            _: &Cancellation,
            emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
        ) -> Result<(), MediaError> {
            for index in 0..100 {
                emit(DecodedFrame {
                    ticket: r.ticket,
                    requested: r.position,
                    actual: SourceTime {
                        ticks: r.position.ticks + index,
                        timebase: r.position.timebase,
                    },
                    image: RgbaFrame::new(1, 1, 4, vec![0, 0, 0, 255])?,
                    decode_us: 1,
                    process_id: 42,
                })?;
            }
            Ok(())
        }
    }
    fn request() -> DecodeRequest {
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
            position: media.metadata.start(),
            media,
        }
    }
    #[test]
    fn full_queue_applies_backpressure_and_supersession_shutdown_wake_it() {
        let mut decoder = PersistentDecoder::start(Arc::new(Producer)).unwrap();
        decoder.request(request()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while decoder.progress().unwrap().buffered < PLAYBACK_QUEUE_FRAMES {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            decoder.progress().unwrap().decoded,
            PLAYBACK_QUEUE_FRAMES as u64
        );
        let new = request();
        let ticket = new.ticket;
        decoder.request(new).unwrap();
        loop {
            if let Some(frame) = decoder.take().unwrap() {
                assert_eq!(frame.ticket, ticket);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        let now = Instant::now();
        decoder.shutdown().unwrap();
        assert!(now.elapsed() < Duration::from_secs(1));
    }
}

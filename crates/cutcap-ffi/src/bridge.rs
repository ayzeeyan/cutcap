use std::{
    cell::{Cell, RefCell},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::Duration,
};

use cutcap_contracts::{CommandEnvelope, EngineError, EngineEvent, ErrorCode};

use crate::engine::Engine;

pub const QUEUE_CAPACITY: usize = 32;
pub const EVENTS_PER_TICK: usize = 8;

/// UI-thread owner. Channel endpoints and the worker have one explicit lifecycle.
pub struct EngineBridge {
    commands: RefCell<Option<SyncSender<CommandEnvelope>>>,
    events: RefCell<Option<Receiver<EngineEvent>>>,
    worker: RefCell<Option<JoinHandle<()>>>,
    disconnected: Cell<bool>,
}

impl EngineBridge {
    pub fn start() -> Result<Self, std::io::Error> {
        let (commands, requests) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (results, events) = mpsc::sync_channel(QUEUE_CAPACITY);
        let worker = thread::Builder::new()
            .name("cutcap-command-owner".into())
            .spawn(move || {
                let mut engine = Engine::default();
                loop {
                    // File work owns its snapshot on another thread. The command
                    // owner remains available for edits while a save is running.
                    let command = if engine.file_busy() {
                        requests.recv_timeout(Duration::from_millis(10))
                    } else {
                        requests
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    };
                    match command {
                        Ok(command) => {
                            if results.send(engine.handle(command)).is_err() {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if let Some(event) = engine.poll_file()
                        && results.send(event).is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            commands: RefCell::new(Some(commands)),
            events: RefCell::new(Some(events)),
            worker: RefCell::new(Some(worker)),
            disconnected: Cell::new(false),
        })
    }

    pub fn submit(&self, command: CommandEnvelope) -> Result<(), EngineError> {
        let commands = self.commands.borrow();
        let sender = commands.as_ref().ok_or_else(disconnected_error)?;
        match sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(EngineError::new(
                ErrorCode::Busy,
                "Command queue is full. Wait for pending actions, then try again.",
            )),
            Err(TrySendError::Disconnected(_)) => Err(disconnected_error()),
        }
    }

    pub fn drain(&self, mut receive: impl FnMut(EngineEvent)) -> Result<(), EngineError> {
        let events = self.events.borrow();
        let receiver = events.as_ref().ok_or_else(disconnected_error)?;
        for _ in 0..EVENTS_PER_TICK {
            match receiver.try_recv() {
                Ok(event) => receive(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.disconnected.replace(true) {
                        return Err(disconnected_error());
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    /// Call only after the UI event loop stops. Drop the event receiver first to
    /// unblock a worker waiting on a full event queue, then close requests/join.
    pub fn shutdown(&self) -> Result<(), EngineError> {
        self.events.borrow_mut().take();
        self.commands.borrow_mut().take();
        if let Some(worker) = self.worker.borrow_mut().take() {
            worker.join().map_err(|_| {
                EngineError::new(
                    ErrorCode::Internal,
                    "The command worker terminated unexpectedly.",
                )
            })?;
        }
        Ok(())
    }
}

fn disconnected_error() -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "The engine connection closed. Restart CutCap.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutcap_contracts::{Command, RequestId};
    use std::time::Duration;

    #[test]
    fn actual_worker_returns_correlated_event_and_shuts_down() {
        let bridge = EngineBridge::start().unwrap();
        let request_id = RequestId::new();
        bridge
            .submit(CommandEnvelope {
                request_id,
                project_id: None,
                command: Command::NewProject {
                    name: "Worker test".into(),
                },
            })
            .unwrap();
        let event = bridge
            .events
            .borrow()
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(matches!(event, EngineEvent::Completed { request_id: id, .. } if id == request_id));
        bridge.shutdown().unwrap();
        assert!(
            bridge
                .submit(CommandEnvelope {
                    request_id: RequestId::new(),
                    project_id: None,
                    command: Command::GetSnapshot
                })
                .is_err()
        );
    }

    #[test]
    fn saturated_channels_apply_backpressure_and_shutdown_without_deadlock() {
        let bridge = EngineBridge::start().unwrap();
        let mut full = false;
        for _ in 0..(QUEUE_CAPACITY * 4) {
            let result = bridge.submit(CommandEnvelope {
                request_id: RequestId::new(),
                project_id: None,
                command: Command::NewProject {
                    name: "Bounded".into(),
                },
            });
            if let Err(error) = result {
                assert_eq!(error.code, ErrorCode::Busy);
                full = true;
                break;
            }
        }
        assert!(full);
        bridge.shutdown().unwrap();
    }
}

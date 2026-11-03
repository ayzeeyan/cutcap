//! Developer-only driver of the real import/selection/presentation workflow.
//! Timing ends at UI image assignment, not monitor scanout.
use crate::preview::PresentationMeasurement;
use serde::Serialize;
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

pub enum Action {
    Import(PathBuf),
    Seek(i32),
}

pub struct Run {
    inputs: VecDeque<PathBuf>,
    positions: VecDeque<i32>,
    previous_samples: usize,
    waiting: Option<Instant>,
    ready_at: Instant,
    pub complete: bool,
    pub error: Option<String>,
}
impl Run {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            inputs: ["720p.mp4", "1080p.mp4", "4k.mp4"]
                .map(|name| directory.join(name))
                .into(),
            positions: VecDeque::new(),
            previous_samples: 0,
            waiting: None,
            ready_at: Instant::now(),
            complete: false,
            error: None,
        }
    }
    pub fn advance(&mut self, samples: usize) -> Option<Action> {
        if self.complete {
            return None;
        }
        if let Some(started) = self.waiting {
            if samples > self.previous_samples {
                self.waiting = None;
                // Give the real renderer event-loop opportunities between requests.
                self.ready_at = Instant::now() + Duration::from_millis(100);
            } else {
                if started.elapsed() > Duration::from_secs(30) {
                    self.error = Some(
                        "No preview completed within 30 seconds; inspect the application log."
                            .into(),
                    );
                    self.complete = true;
                }
                return None;
            }
        }
        if Instant::now() < self.ready_at {
            return None;
        }
        let action = if let Some(position) = self.positions.pop_front() {
            Action::Seek(position)
        } else if let Some(path) = self.inputs.pop_front() {
            // Selection itself requests 0 ms; these are the remaining nine samples.
            self.positions = [250, 500, 750, 1000, 1250, 1500, 2000, 2500, 2800].into();
            Action::Import(path)
        } else {
            self.complete = true;
            return None;
        };
        self.previous_samples = samples;
        self.waiting = Some(Instant::now());
        Some(action)
    }
}

#[derive(Serialize)]
pub struct Report<'a> {
    pub method: &'static str,
    pub app_version: &'static str,
    pub max_ui_tick_gap_us: u64,
    pub completed: bool,
    pub error: Option<&'a str>,
    pub samples: &'a [PresentationMeasurement],
}

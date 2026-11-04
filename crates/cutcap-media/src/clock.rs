use std::time::Instant;

/// Source-relative nanoseconds, never preview-frame indexes. The master can be
/// replaced by an audio clock later without changing timestamped frame queues.
#[derive(Default)]
pub struct PresentationClock {
    position: u64,
    anchor: Option<Instant>,
}
impl PresentationClock {
    pub fn position(&self, now: Instant) -> u64 {
        self.position.saturating_add(self.anchor.map_or(0, |a| {
            now.saturating_duration_since(a)
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64
        }))
    }
    pub fn start(&mut self, position: u64, now: Instant) {
        self.position = position;
        self.anchor = Some(now);
    }
    pub fn pause(&mut self, now: Instant) {
        self.position = self.position(now);
        self.anchor = None;
    }
    pub fn freeze_at(&mut self, position: u64) {
        self.position = position;
        self.anchor = None;
    }
    pub fn resume(&mut self, now: Instant) {
        if self.anchor.is_none() {
            self.anchor = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn monotonic_clock_excludes_paused_wall_time() {
        let now = Instant::now();
        let mut clock = PresentationClock::default();
        clock.start(700_000_000, now);
        clock.pause(now + Duration::from_millis(100));
        assert_eq!(clock.position(now + Duration::from_secs(8)), 800_000_000);
        clock.resume(now + Duration::from_secs(9));
        assert_eq!(
            clock.position(now + Duration::from_millis(9200)),
            1_000_000_000
        );
        clock.freeze_at(200_000_000);
        assert_eq!(clock.position(now + Duration::from_secs(20)), 200_000_000);
    }
}

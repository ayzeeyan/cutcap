//! Exact output-grid arithmetic for offline export. No I/O, floats or codecs.
//!
//! See PHASE_5A_EXPORT_CONTRACT.md §4. Video and audio share one nominal
//! duration of `frames / fps`; both are derived from the same frame count.
use cutcap_contracts::{
    ExportPreset, HEAD_HOLD_MS, MAX_EXPORT_SECONDS, TimeError, Timebase, TimelineTime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GridError {
    #[error("The timeline is empty, so there is nothing to export.")]
    Empty,
    #[error("The timeline is longer than the four hours one export may render.")]
    TooLong,
    #[error(transparent)]
    Time(#[from] TimeError),
}

/// How many output frames and audio sample frames an export has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputGrid {
    pub frames: u64,
    pub samples: u64,
}

/// The grid covering `extent`: the first whole frame count at or past it.
pub fn output_grid(
    extent: TimelineTime,
    clock: Timebase,
    preset: ExportPreset,
) -> Result<OutputGrid, GridError> {
    if extent.0 <= 0 {
        return Err(GridError::Empty);
    }
    let (ticks, numerator, denominator) = (
        i128::from(extent.0),
        i128::from(clock.numerator()),
        i128::from(clock.denominator()),
    );
    let scaled = ticks.checked_mul(numerator).ok_or(TimeError::Overflow)?;
    if scaled > i128::from(MAX_EXPORT_SECONDS) * denominator {
        return Err(GridError::TooLong);
    }
    let frames =
        u64::try_from((scaled * i128::from(preset.fps) + denominator - 1).div_euclid(denominator))
            .map_err(|_| TimeError::Overflow)?;
    let samples = frames
        .checked_mul(u64::from(preset.sample_rate))
        .ok_or(TimeError::Overflow)?
        / u64::from(preset.fps);
    Ok(OutputGrid { frames, samples })
}

/// The project tick that owns output frame `frame`. Flooring makes `[start,
/// end)` membership exact even when a frame does not land on a whole tick,
/// because clip boundaries are whole ticks.
pub fn frame_position(frame: u64, clock: Timebase, fps: u32) -> Result<TimelineTime, TimeError> {
    let ticks = i128::from(frame)
        .checked_mul(i128::from(clock.denominator()))
        .ok_or(TimeError::Overflow)?
        / (i128::from(fps) * i128::from(clock.numerator()));
    i64::try_from(ticks)
        .map(TimelineTime)
        .map_err(|_| TimeError::Overflow)
}

/// [`HEAD_HOLD_MS`] in project ticks (ceiling).
pub fn head_hold_ticks(clock: Timebase) -> Result<i64, TimeError> {
    let numerator = i128::from(HEAD_HOLD_MS) * i128::from(clock.denominator());
    let denominator = 1000 * i128::from(clock.numerator());
    i64::try_from((numerator + denominator - 1) / denominator).map_err(|_| TimeError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESET: ExportPreset = ExportPreset::STANDARD;

    fn grid(ticks: i64, clock: Timebase) -> Result<OutputGrid, GridError> {
        output_grid(TimelineTime(ticks), clock, PRESET)
    }

    #[test]
    fn whole_seconds_are_exact_on_every_common_clock() {
        for clock in [
            Timebase::DEFAULT_TIMELINE,
            Timebase::new(1, 48_000).unwrap(),
            Timebase::new(1, 90_000).unwrap(),
            Timebase::new(1001, 30_000).unwrap(),
        ] {
            let one_second = i64::from(clock.denominator()) / i64::from(clock.numerator());
            let found = grid(one_second * 10, clock).unwrap();
            // 1001/30000 has no whole-second tick count: 10 s floors to 299 ticks.
            if clock.numerator() == 1 {
                assert_eq!(
                    found,
                    OutputGrid {
                        frames: 300,
                        samples: 480_000
                    }
                );
            } else {
                assert_eq!(found.samples, found.frames * 1600);
            }
        }
    }

    #[test]
    fn a_fraction_of_a_frame_rounds_up_so_the_last_content_is_shown() {
        let clock = Timebase::DEFAULT_TIMELINE;
        assert_eq!(grid(24_000, clock).unwrap().frames, 1);
        assert_eq!(grid(24_001, clock).unwrap().frames, 2);
        assert_eq!(grid(23_999, clock).unwrap().frames, 1);
        assert_eq!(grid(1, clock).unwrap().frames, 1);
        assert_eq!(grid(24_001, clock).unwrap().samples, 3200);
    }

    #[test]
    fn empty_and_excessive_timelines_are_refused() {
        let clock = Timebase::DEFAULT_TIMELINE;
        assert_eq!(grid(0, clock), Err(GridError::Empty));
        assert_eq!(grid(-5, clock), Err(GridError::Empty));
        let limit = MAX_EXPORT_SECONDS as i64 * 720_000;
        assert_eq!(grid(limit, clock).unwrap().frames, 432_000);
        assert_eq!(grid(limit + 1, clock), Err(GridError::TooLong));
        assert_eq!(grid(i64::MAX, clock), Err(GridError::TooLong));
    }

    #[test]
    fn frame_positions_are_exact_and_floor_between_ticks() {
        let clock = Timebase::DEFAULT_TIMELINE;
        assert_eq!(frame_position(0, clock, 30).unwrap(), TimelineTime(0));
        assert_eq!(frame_position(1, clock, 30).unwrap(), TimelineTime(24_000));
        assert_eq!(
            frame_position(431_999, clock, 30).unwrap(),
            TimelineTime(431_999 * 24_000)
        );
        // 1 ms ticks: 33.33.. floors to 33, 66.66.. to 66, and 100 is exact.
        let coarse = Timebase::new(1, 1000).unwrap();
        for (frame, tick) in [(1, 33), (2, 66), (3, 100)] {
            assert_eq!(
                frame_position(frame, coarse, 30).unwrap(),
                TimelineTime(tick)
            );
        }
        assert!(frame_position(u64::MAX, Timebase::new(1, u32::MAX).unwrap(), 1).is_err());
    }

    #[test]
    fn the_head_hold_is_a_ceiling_in_ticks() {
        assert_eq!(head_hold_ticks(Timebase::DEFAULT_TIMELINE).unwrap(), 72_000);
        assert_eq!(head_hold_ticks(Timebase::new(1, 3).unwrap()).unwrap(), 1);
        assert_eq!(
            head_hold_ticks(Timebase::new(1, 1000).unwrap()).unwrap(),
            100
        );
    }

    #[test]
    fn the_preset_has_a_whole_number_of_samples_per_frame() {
        assert_eq!(PRESET.sample_rate % PRESET.fps, 0);
        assert_eq!(PRESET.frame_bytes(), 1280 * 720 + 2 * (640 * 360));
    }
}

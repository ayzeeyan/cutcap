use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Serialize};

/// A rational number of seconds per tick, independent of any preview frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timebase {
    numerator: NonZeroU32,
    denominator: NonZeroU32,
}

impl Timebase {
    pub const DEFAULT_TIMELINE: Self = Self {
        numerator: NonZeroU32::MIN,
        denominator: NonZeroU32::new(720_000).expect("nonzero constant"),
    };

    pub fn new(numerator: u32, denominator: u32) -> Result<Self, TimeError> {
        Ok(Self {
            numerator: NonZeroU32::new(numerator).ok_or(TimeError::ZeroTimebase)?,
            denominator: NonZeroU32::new(denominator).ok_or(TimeError::ZeroTimebase)?,
        })
    }

    pub const fn numerator(self) -> u32 {
        self.numerator.get()
    }
    pub const fn denominator(self) -> u32 {
        self.denominator.get()
    }
}

/// Ticks in the containing project's timebase. Pixels never belong in this type.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimelineTime(pub i64);

impl TimelineTime {
    pub const ZERO: Self = Self(0);

    pub fn checked_add(self, duration: TimelineDuration) -> Result<Self, TimeError> {
        let ticks = i64::try_from(duration.ticks()).map_err(|_| TimeError::Overflow)?;
        self.0
            .checked_add(ticks)
            .map(Self)
            .ok_or(TimeError::Overflow)
    }
}

/// Positive tick duration; checked deserialization uses the same constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct TimelineDuration(NonZeroU64);

impl TimelineDuration {
    pub fn new(ticks: u64) -> Result<Self, TimeError> {
        if ticks > i64::MAX as u64 {
            return Err(TimeError::Overflow);
        }
        NonZeroU64::new(ticks)
            .map(Self)
            .ok_or(TimeError::ZeroDuration)
    }

    pub const fn ticks(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for TimelineDuration {
    type Error = TimeError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<TimelineDuration> for u64 {
    fn from(value: TimelineDuration) -> Self {
        value.ticks()
    }
}

/// A demuxer's presentation timestamp. Negative source PTS are valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTime {
    pub ticks: i64,
    pub timebase: Timebase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("timebase numerator and denominator must be nonzero")]
    ZeroTimebase,
    #[error("clip duration must be positive")]
    ZeroDuration,
    #[error("timeline arithmetic overflow")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_oversized_durations_on_every_path() {
        assert_eq!(TimelineDuration::new(0), Err(TimeError::ZeroDuration));
        assert_eq!(TimelineDuration::new(u64::MAX), Err(TimeError::Overflow));
        assert!(serde_json::from_str::<TimelineDuration>("0").is_err());
        assert!(serde_json::from_str::<TimelineDuration>("18446744073709551615").is_err());
    }

    #[test]
    fn rejects_overflow_and_invalid_timebases() {
        let one = TimelineDuration::new(1).unwrap();
        assert_eq!(
            TimelineTime(i64::MAX).checked_add(one),
            Err(TimeError::Overflow)
        );
        assert_eq!(TimelineTime(4).checked_add(one), Ok(TimelineTime(5)));
        assert!(Timebase::new(0, 24).is_err());
        assert!(Timebase::new(1, 0).is_err());
        assert!(serde_json::from_str::<Timebase>(r#"{"numerator":1,"denominator":0}"#).is_err());
    }

    #[test]
    fn fractional_frame_rate_has_exact_default_ticks() {
        // 30000/1001 fps: 720000 * 1001 / 30000 = 24024, without floats.
        let timebase = Timebase::DEFAULT_TIMELINE;
        assert_eq!(timebase.denominator() as u64 * 1001, 24_024 * 30_000);
        let source = SourceTime {
            ticks: -90,
            timebase: Timebase::new(1, 90_000).unwrap(),
        };
        assert_eq!(
            serde_json::from_str::<SourceTime>(&serde_json::to_string(&source).unwrap()).unwrap(),
            source
        );
    }
}

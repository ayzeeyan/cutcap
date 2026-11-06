//! Immutable owner-built transport input. Not a serialized project or UI model.
use cutcap_contracts::*;
use cutcap_core::TimelineIndex;
use std::{collections::BTreeMap, sync::Arc};

pub struct TimelinePlan {
    pub project: ProjectId,
    pub generation: RequestId,
    pub timebase: Timebase,
    pub track: Option<MonitoredTrack>,
    pub extent: TimelineTime,
    pub position: TimelineTime,
    pub index: Arc<TimelineIndex>,
    pub media: BTreeMap<MediaId, MediaSummary>,
    pub availability: BTreeMap<MediaId, MediaAvailability>,
}

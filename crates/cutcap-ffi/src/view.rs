//! Display-ready view DTOs derived from engine snapshots.
//!
//! Time and rational values are formatted here, next to the exact arithmetic,
//! so the frontend never re-derives durations from floating point. The
//! frontend owns layout, interaction and styling; it renders these strings.
use cutcap_contracts::*;
use serde::Serialize;

/// The whole frontend view, serialized as one JSON document per revision.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub revision: u64,
    pub engine_ready: bool,
    pub project: Option<ProjectView>,
    pub status: StatusView,
    pub pending: u32,
    pub tracks: Vec<TrackView>,
    pub media: Vec<MediaView>,
    pub import: ImportView,
    pub inspector: InspectorView,
    pub source: SourceView,
    pub frame: Option<FrameView>,
    pub metrics: PreviewMetrics,
    pub playback: crate::playback::PlaybackView,
    /// Changes whenever the timeline document (`cutcap_session_timeline`)
    /// would change. Clips are not in this view: it is re-serialized for every
    /// playback frame, while the clip list changes only on commands.
    pub timeline_generation: u64,
    pub history: HistoryStatus,
    pub preview_mode: PreviewMode,
    pub timeline_preview: crate::timeline_preview::TimelinePreviewView,
}

/// The clip list, serialized separately from [`SessionView`] and fetched by the
/// frontend only when `timeline_generation` changes.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineView {
    pub generation: u64,
    /// Timeline clips in project order; timing derived from exact ticks.
    pub clips: Vec<ClipView>,
}

/// One timeline clip for display. `start_ms`/`duration_ms` are floored from the
/// exact project ticks for drawing; the strings are the authoritative readouts.
#[derive(Debug, Clone, Serialize)]
pub struct ClipView {
    pub id: String,
    pub track_id: String,
    pub track_index: usize,
    pub media_id: String,
    pub name: String,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub start_text: String,
    pub duration_text: String,
    pub end_text: String,
    /// Ready, missing, changed, checking or unavailable source.
    pub availability: &'static str,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    /// True only when this exact document revision has completed saving/opening.
    pub saved: bool,
    pub clip_count: usize,
    pub document: DocumentStatus,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StatusView {
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackView {
    pub id: String,
    pub name: String,
    /// "video" or "audio".
    pub kind: &'static str,
    /// Per-family number shown in the header ("V1", "A2").
    pub badge: String,
    pub selected: bool,
    pub lane_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaView {
    pub id: String,
    pub name: String,
    pub resolution: String,
    pub format: String,
    pub duration: String,
    pub duration_ms: Option<i64>,
    pub estimated: bool,
    pub has_audio: bool,
    /// The source shown in the viewer; independent of timeline selection.
    pub selected: bool,
    pub availability: Availability,
    pub detail: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportView {
    pub busy: bool,
    pub error: bool,
    pub message: String,
}

impl Default for ImportView {
    fn default() -> Self {
        Self {
            busy: false,
            error: false,
            message: IMPORT_IDLE.into(),
        }
    }
}

pub const IMPORT_IDLE: &str = "Import a local video.";

#[derive(Debug, Clone, Serialize)]
pub struct InspectorView {
    /// "project", "track" or "source".
    pub kind: &'static str,
    pub title: String,
    pub detail: String,
    pub sections: Vec<SectionView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionView {
    pub title: String,
    pub rows: Vec<RowView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RowView {
    pub label: String,
    pub value: String,
}

/// Source monitor state. Every position is a source request; the timeline
/// playhead is not part of this view.
#[derive(Debug, Clone, Serialize)]
pub struct SourceView {
    pub ready: bool,
    pub title: String,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub duration_estimated: bool,
    pub busy: bool,
    pub has_frame: bool,
    pub status: String,
    pub detail: String,
    /// Actual displayed frame: relative time and original PTS.
    pub caption: String,
}

impl Default for SourceView {
    fn default() -> Self {
        Self {
            ready: false,
            title: "No source selected".into(),
            position_ms: 0,
            duration_ms: 0,
            duration_estimated: false,
            busy: false,
            has_frame: false,
            status: "Select a video to preview".into(),
            detail: "Import a local video into the media library.".into(),
            caption: String::new(),
        }
    }
}

/// Metadata of the latest presented frame. Pixels are copied separately.
#[derive(Debug, Clone, Serialize)]
pub struct FrameView {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub actual: SourceTime,
    pub epoch: String,
}

/// Last measured preview timings, for diagnostics and the migration report.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PreviewMetrics {
    pub probe_us: Option<u64>,
    pub decode_us: Option<u64>,
    /// Seek request accepted → frame ready for the frontend (excludes UI copy/paint).
    pub request_to_ready_us: Option<u64>,
    pub frames_presented: u64,
}

pub fn file_name(asset: &MediaSummary) -> String {
    asset
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub fn media_view(asset: &MediaSummary, selected: bool) -> MediaView {
    let info = &asset.metadata;
    let mut format = codec_label(&info.codec);
    if let Some(rate) = info.average_rate.or(info.nominal_rate) {
        format.push_str(", ");
        format.push_str(&frame_rate(rate));
    }
    let duration = duration_ms(info);
    MediaView {
        id: asset.id.to_string(),
        name: file_name(asset),
        resolution: format!("{} × {}", info.width, info.height),
        format,
        duration: duration.map_or_else(|| "--:--".to_owned(), short_duration),
        duration_ms: duration,
        estimated: info.duration_is_estimate,
        has_audio: !info.audio_streams.is_empty(),
        selected,
        availability: Availability::Ready,
        detail: "Source verified".into(),
        path: asset.path.to_string_lossy().into_owned(),
    }
}

pub fn media_view_with_state(
    asset: Option<&MediaSummary>,
    state: &MediaAvailability,
    selected: bool,
) -> MediaView {
    let mut view = asset
        .map(|asset| media_view(asset, selected))
        .unwrap_or_else(|| MediaView {
            id: state.id.to_string(),
            name: state
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            resolution: "—".into(),
            format: "Metadata unavailable".into(),
            duration: "--:--".into(),
            duration_ms: None,
            estimated: false,
            has_audio: false,
            selected,
            availability: state.state,
            detail: state.detail.clone(),
            path: state.path.to_string_lossy().into_owned(),
        });
    view.availability = state.state;
    view.detail = state.detail.clone();
    view.path = state.path.to_string_lossy().into_owned();
    if state.state != Availability::Ready {
        view.format = format!(
            "{} · {}",
            match state.state {
                Availability::Checking => "Checking",
                Availability::Missing => "Missing media",
                Availability::Changed => "Source changed",
                _ => "Unavailable",
            },
            view.format
        );
    }
    view
}

/// Whole milliseconds (floor) of a project-clock tick count, for display only.
pub fn timeline_ms(ticks: i64, timebase: Timebase) -> i64 {
    Timebase::new(1, 1000)
        .ok()
        .and_then(|ms| {
            SourceTime { ticks, timebase }
                .rescale(ms, Rounding::Floor)
                .ok()
        })
        .map_or(0, |time| time.ticks)
}

fn availability_name(state: Availability) -> &'static str {
    match state {
        Availability::Checking => "checking",
        Availability::Ready => "ready",
        Availability::Missing => "missing",
        Availability::Changed => "changed",
        Availability::Unavailable => "unavailable",
    }
}

pub fn clip_name(snapshot: &ProjectSnapshot, media: MediaId) -> String {
    snapshot
        .media_state
        .iter()
        .find(|state| state.id == media)
        .map_or_else(
            || "Unknown source".to_owned(),
            |state| {
                state
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            },
        )
}

/// Clips with display timing. O(clips + tracks).
pub fn clip_views(snapshot: &ProjectSnapshot) -> Vec<ClipView> {
    let tb = snapshot.timebase;
    snapshot
        .clips
        .iter()
        .map(|clip| {
            let start = clip.start.0;
            let duration = i64::try_from(clip.duration.ticks()).unwrap_or(i64::MAX);
            let start_ms = timeline_ms(start, tb);
            let duration_ms = timeline_ms(duration, tb);
            let end_ms = timeline_ms(start.saturating_add(duration), tb);
            ClipView {
                id: clip.id.to_string(),
                track_id: clip.track_id.to_string(),
                track_index: snapshot
                    .tracks
                    .iter()
                    .position(|track| track.id == clip.track_id)
                    .unwrap_or(0),
                media_id: clip.media_id.to_string(),
                name: clip_name(snapshot, clip.media_id),
                start_ms,
                duration_ms,
                start_text: precise_duration(start_ms),
                duration_text: precise_duration(duration_ms),
                end_text: precise_duration(end_ms),
                availability: snapshot
                    .media_state
                    .iter()
                    .find(|state| state.id == clip.media_id)
                    .map_or("unavailable", |state| availability_name(state.state)),
                selected: snapshot.selection == SelectionState::Clip(clip.id),
            }
        })
        .collect()
}

/// Inspector rows for a selected clip: timeline placement and source mapping.
pub fn clip_sections(snapshot: &ProjectSnapshot, clip: &ClipSummary) -> Vec<SectionView> {
    let tb = snapshot.timebase;
    let start = clip.start.0;
    let duration = i64::try_from(clip.duration.ticks()).unwrap_or(i64::MAX);
    let track = snapshot.tracks.iter().find(|t| t.id == clip.track_id);
    let state = snapshot.media_state.iter().find(|s| s.id == clip.media_id);
    vec![
        section(
            "Timeline",
            vec![
                field("Track", track.map_or_else(String::new, |t| t.name.clone())),
                field("Start", precise_duration(timeline_ms(start, tb))),
                field("Duration", precise_duration(timeline_ms(duration, tb))),
                field(
                    "End",
                    precise_duration(timeline_ms(start.saturating_add(duration), tb)),
                ),
                field("Ticks", format!("{start} + {duration} @ {}", clock(tb))),
            ],
        ),
        section(
            "Source",
            vec![
                field("File", clip_name(snapshot, clip.media_id)),
                field(
                    "Source in",
                    format!(
                        "PTS {} @ {}/{}",
                        clip.source_in.ticks,
                        clip.source_in.timebase.numerator(),
                        clip.source_in.timebase.denominator()
                    ),
                ),
                field(
                    "Status",
                    state.map_or_else(|| "Unknown".to_owned(), |s| s.detail.clone()),
                ),
            ],
        ),
    ]
}

/// Track headers; badges number each family in order ("V1", "A1", "V2").
pub fn track_views(snapshot: &ProjectSnapshot) -> Vec<TrackView> {
    let (mut video, mut audio) = (0, 0);
    snapshot
        .tracks
        .iter()
        .map(|track| {
            let (kind, badge) = match track.kind {
                TrackKind::Video => {
                    video += 1;
                    ("video", format!("V{video}"))
                }
                TrackKind::Audio => {
                    audio += 1;
                    ("audio", format!("A{audio}"))
                }
            };
            TrackView {
                id: track.id.to_string(),
                name: track.name.clone(),
                kind,
                badge,
                selected: snapshot.selection == SelectionState::Track(track.id),
                // Tracks with clips draw the clips themselves instead of a label.
                lane_label: if track.clip_count == 0 {
                    format!("Empty {kind} track")
                } else {
                    String::new()
                },
            }
        })
        .collect()
}

fn section(title: &str, rows: Vec<RowView>) -> SectionView {
    SectionView {
        title: title.into(),
        rows,
    }
}

fn field(label: &str, value: impl Into<String>) -> RowView {
    RowView {
        label: label.into(),
        value: value.into(),
    }
}

fn clock(timebase: Timebase) -> String {
    format!("{}/{} s", timebase.numerator(), timebase.denominator())
}

pub fn project_sections(snapshot: &ProjectSnapshot) -> Vec<SectionView> {
    let video = snapshot
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .count();
    let audio = snapshot.tracks.len() - video;
    vec![
        section(
            "Timeline",
            vec![
                field("Tracks", format!("{video} video, {audio} audio")),
                field(
                    "Clips",
                    if snapshot.clip_count == 0 {
                        "None yet".into()
                    } else {
                        snapshot.clip_count.to_string()
                    },
                ),
                field("Clock", format!("{} per tick", clock(snapshot.timebase))),
            ],
        ),
        section(
            "Media",
            vec![
                field(
                    "Sources",
                    match snapshot.media_count {
                        0 => "None imported".to_owned(),
                        1 => "1 video".to_owned(),
                        count => format!("{count} videos"),
                    },
                ),
                field(
                    "Storage",
                    if snapshot.document.dirty {
                        "Unsaved changes"
                    } else {
                        "Saved project"
                    },
                ),
            ],
        ),
    ]
}

pub fn track_sections(snapshot: &ProjectSnapshot, track: &TrackSummary) -> Vec<SectionView> {
    let position = snapshot
        .tracks
        .iter()
        .position(|candidate| candidate.id == track.id)
        .map_or_else(String::new, |index| {
            format!("{} of {}", index + 1, snapshot.tracks.len())
        });
    vec![
        section(
            "Track",
            vec![
                field(
                    "Type",
                    match track.kind {
                        TrackKind::Video => "Video",
                        TrackKind::Audio => "Audio",
                    },
                ),
                field("Order", position),
                field("Clips", track.clip_count.to_string()),
            ],
        ),
        section(
            "Timeline",
            vec![field(
                "Clock",
                format!("{} per tick", clock(snapshot.timebase)),
            )],
        ),
    ]
}

pub fn source_sections(asset: &MediaSummary) -> Vec<SectionView> {
    let info = &asset.metadata;
    let unknown = || "Not reported".to_owned();
    let video = vec![
        field("Resolution", format!("{} × {}", info.width, info.height)),
        field(
            "Aspect",
            info.display_aspect
                .map_or_else(unknown, |r| format!("{}:{}", r.numerator, r.denominator)),
        ),
        field(
            "Frame rate",
            info.average_rate.map_or_else(unknown, frame_rate),
        ),
        field("Codec", codec_label(&info.codec)),
        field(
            "Pixel format",
            info.pixel_format.clone().unwrap_or_else(unknown),
        ),
        field(
            "Rotation",
            match info.rotation {
                0 => "None".to_owned(),
                degrees => format!("{degrees}° counter-clockwise"),
            },
        ),
    ];
    let mut timing = vec![
        field(
            "Duration",
            match duration_ms(info) {
                None => unknown(),
                Some(ms) if info.duration_is_estimate => {
                    format!("{} (container estimate)", precise_duration(ms))
                }
                Some(ms) => precise_duration(ms),
            },
        ),
        field("Start PTS", info.start_pts.to_string()),
        field("Timebase", clock(info.timebase)),
    ];
    if info.nominal_rate.is_some() && info.nominal_rate != info.average_rate {
        timing.push(field(
            "Nominal rate",
            info.nominal_rate.map_or_else(unknown, frame_rate),
        ));
    }
    let file = vec![
        field("Container", info.container.clone()),
        field(
            "Audio",
            match info.audio_streams.as_slice() {
                [] => "None".to_owned(),
                [index] => format!("1 stream (#{index})"),
                streams => format!(
                    "{} streams ({})",
                    streams.len(),
                    streams
                        .iter()
                        .map(|index| format!("#{index}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
        ),
        field(
            "Video stream",
            match &info.stream_id {
                Some(id) => format!("#{}, ID {id}", info.stream_index),
                None => format!("#{}", info.stream_index),
            },
        ),
        field(
            "Folder",
            asset
                .path
                .parent()
                .map_or_else(unknown, |folder| display_path(&folder.to_string_lossy())),
        ),
    ];
    let mut sections = vec![
        section("Video", video),
        section("Timing", timing),
        section("File", file),
    ];
    let color: Vec<&str> = [&info.color_space, &info.color_transfer, &info.color_range]
        .into_iter()
        .filter_map(|value| value.as_deref())
        .collect();
    let mut technical = Vec::new();
    if let Some(sar) = info.sample_aspect {
        technical.push(field(
            "Sample aspect",
            format!("{}:{}", sar.numerator, sar.denominator),
        ));
    }
    if !color.is_empty() {
        technical.push(field("Color", color.join(", ")));
    }
    if !technical.is_empty() {
        sections.push(section("Technical", technical));
    }
    sections
}

/// Canonical paths carry the Windows verbatim prefix (`\\?\C:\...`). Keep it in
/// the project, but show the familiar form. UNC verbatim paths become `\\server\...`.
pub fn display_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.to_owned()
    }
}

/// Source duration in whole milliseconds (floor), when the probe reported one.
pub fn duration_ms(info: &VideoMetadata) -> Option<i64> {
    let ticks = info.duration_ticks?;
    SourceTime {
        ticks,
        timebase: info.timebase,
    }
    .rescale(Timebase::new(1, 1000).ok()?, Rounding::Floor)
    .ok()
    .map(|time| time.ticks)
}

/// FFprobe codec names mapped to familiar labels; unknown names are shown as reported.
pub fn codec_label(codec: &str) -> String {
    match codec {
        "h264" => "H.264".into(),
        "hevc" => "HEVC".into(),
        "av1" => "AV1".into(),
        "vp8" => "VP8".into(),
        "vp9" => "VP9".into(),
        "mpeg2video" => "MPEG-2".into(),
        "mpeg4" => "MPEG-4".into(),
        "prores" => "ProRes".into(),
        "dnxhd" => "DNxHD".into(),
        "ffv1" => "FFV1".into(),
        "mjpeg" => "Motion JPEG".into(),
        other => other.into(),
    }
}

/// Exact rational rate rendered with at most three decimals, e.g. 30000/1001 → "29.97 fps".
pub fn frame_rate(rate: Ratio) -> String {
    let numerator = u64::from(rate.numerator);
    let denominator = u64::from(rate.denominator.max(1));
    let thousandths = (numerator * 1000 + denominator / 2) / denominator;
    let whole = thousandths / 1000;
    let fraction = format!("{:03}", thousandths % 1000);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("{whole} fps")
    } else {
        format!("{whole}.{fraction} fps")
    }
}

/// Library length: "2.5 s" under a minute, then "M:SS" or "H:MM:SS".
pub fn short_duration(ms: i64) -> String {
    let ms = ms.max(0);
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{}.{} s", seconds, (ms % 1000) / 100)
    } else if seconds < 3600 {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    }
}

/// Inspector length in the viewer's MM:SS.mmm notation.
pub fn precise_duration(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}.{:03}",
        ms / 60_000,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// Parses the viewer's go-to field into relative source milliseconds.
///
/// Accepted: whole milliseconds (`1500`), seconds with a unit (`1.5s`) and
/// `M:SS(.mmm)` or `H:MM:SS(.mmm)`. A bare decimal such as `1.5` is rejected
/// because it is ambiguous between seconds and milliseconds.
pub fn parse_source_time(text: &str) -> Result<i32, &'static str> {
    const MESSAGE: &str = "Enter a source time such as 1500, 1.5s or 00:01.500.";
    let text = text.trim();
    if text.is_empty() {
        return Err(MESSAGE);
    }
    let total: Option<i64> = if let Some(seconds) = text.strip_suffix('s') {
        parse_seconds(seconds.trim())
    } else if text.contains(':') {
        let parts: Vec<&str> = text.split(':').collect();
        let (hours, minutes, seconds) = match parts.as_slice() {
            [minutes, seconds] => (Some(0), parse_whole(minutes), parse_seconds(seconds)),
            [hours, minutes, seconds] => (
                parse_whole(hours),
                parse_whole(minutes).filter(|m| *m < 60),
                parse_seconds(seconds),
            ),
            _ => (None, None, None),
        };
        match (hours, minutes, seconds) {
            (Some(h), Some(m), Some(s)) if s < 60_000 => h
                .checked_mul(3_600_000)
                .and_then(|h| h.checked_add(m.checked_mul(60_000)?))
                .and_then(|total| total.checked_add(s)),
            _ => None,
        }
    } else {
        parse_whole(text)
    };
    total.and_then(|ms| i32::try_from(ms).ok()).ok_or(MESSAGE)
}

fn parse_whole(text: &str) -> Option<i64> {
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_seconds(text: &str) -> Option<i64> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole = if whole.is_empty() && !fraction.is_empty() {
        0
    } else {
        parse_whole(whole)?
    };
    let fraction = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<3}").parse::<i64>().ok()?
    };
    whole.checked_mul(1000)?.checked_add(fraction)
}

/// MM:SS.mmm of a source time relative to its own clock, floored to milliseconds.
pub fn format_time(time: SourceTime) -> String {
    let Ok(clock) = Timebase::new(1, 1000) else {
        return "unavailable".into();
    };
    let Ok(time) = time.rescale(clock, Rounding::Floor) else {
        return "unavailable".into();
    };
    precise_duration(time.ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_time_accepts_milliseconds_seconds_and_timecode() {
        assert_eq!(parse_source_time("1500"), Ok(1500));
        assert_eq!(parse_source_time(" 201 "), Ok(201));
        assert_eq!(parse_source_time("1.5s"), Ok(1500));
        assert_eq!(parse_source_time("2 s"), Ok(2000));
        assert_eq!(parse_source_time("0:01.5"), Ok(1500));
        assert_eq!(parse_source_time("00:01.500"), Ok(1500));
        assert_eq!(parse_source_time("1:02:03.250"), Ok(3_723_250));
        assert_eq!(parse_source_time("75:00"), Ok(4_500_000));
    }

    #[test]
    fn source_time_rejects_ambiguous_or_invalid_text() {
        for text in [
            "",
            "1.5",
            "-1",
            "abc",
            "1:2:3:4",
            "0:60",
            "1:60:00",
            "0:01.5000",
            "1e3",
            "s",
            "9999999999",
        ] {
            assert!(parse_source_time(text).is_err(), "{text:?} should fail");
        }
    }

    #[test]
    fn rates_and_durations_are_readable() {
        let rate = |numerator, denominator| Ratio {
            numerator,
            denominator,
        };
        assert_eq!(frame_rate(rate(30000, 1001)), "29.97 fps");
        assert_eq!(frame_rate(rate(24000, 1001)), "23.976 fps");
        assert_eq!(frame_rate(rate(4, 1)), "4 fps");
        assert_eq!(frame_rate(rate(25, 2)), "12.5 fps");
        assert_eq!(short_duration(3000), "3.0 s");
        assert_eq!(short_duration(400), "0.4 s");
        assert_eq!(short_duration(83_999), "1:23");
        assert_eq!(short_duration(3_723_000), "1:02:03");
        assert_eq!(precise_duration(3000), "00:03.000");
        assert_eq!(precise_duration(4_500_250), "75:00.250");
    }

    #[test]
    fn verbatim_prefixes_are_hidden_from_displayed_paths() {
        assert_eq!(display_path(r"\\?\C:\Users\编辑"), r"C:\Users\编辑");
        assert_eq!(
            display_path(r"\\?\UNC\server\share\clips"),
            r"\\server\share\clips"
        );
        assert_eq!(display_path(r"D:\clips"), r"D:\clips");
    }

    #[test]
    fn negative_and_offset_source_times_format_from_their_own_clock() {
        let timebase = Timebase::new(1, 90_000).unwrap();
        assert_eq!(
            format_time(SourceTime {
                ticks: 135_000,
                timebase
            }),
            "00:01.500"
        );
        assert_eq!(
            format_time(SourceTime {
                ticks: -90_000,
                timebase
            }),
            "00:00.000"
        );
    }
}

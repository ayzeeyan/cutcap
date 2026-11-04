//! UI-owned formatting of snapshot DTOs into Slint view rows. Pure text
//! mapping only: no project mutation, media work or engine semantics.
use cutcap_contracts::*;
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::{InfoRow, InfoSection, MediaRow, TrackRow};

/// Updates rows in place so repeater items (and their keyboard focus) survive.
pub fn sync_rows<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    rows: Vec<T>,
    install: impl FnOnce(ModelRc<T>),
) {
    if let Some(model) = current.as_any().downcast_ref::<VecModel<T>>() {
        for (index, row) in rows.iter().enumerate() {
            if index < model.row_count() {
                if model.row_data(index).as_ref() != Some(row) {
                    model.set_row_data(index, row.clone());
                }
            } else {
                model.push(row.clone());
            }
        }
        while model.row_count() > rows.len() {
            model.remove(model.row_count() - 1);
        }
    } else {
        install(ModelRc::new(VecModel::from(rows)));
    }
}

/// An inspector group before it becomes a Slint `InfoSection`.
pub struct Section {
    title: SharedString,
    rows: Vec<InfoRow>,
}

fn install_section(section: Section) -> InfoSection {
    InfoSection {
        title: section.title,
        rows: ModelRc::new(VecModel::from(section.rows)),
    }
}

/// Like `sync_rows`, but sections with an unchanged title keep their item
/// (and its collapsed state) and only their rows are updated.
pub fn sync_sections(
    current: ModelRc<InfoSection>,
    sections: Vec<Section>,
    install: impl FnOnce(ModelRc<InfoSection>),
) {
    let Some(model) = current.as_any().downcast_ref::<VecModel<InfoSection>>() else {
        install(ModelRc::new(VecModel::from(
            sections
                .into_iter()
                .map(install_section)
                .collect::<Vec<_>>(),
        )));
        return;
    };
    let count = sections.len();
    for (index, section) in sections.into_iter().enumerate() {
        match model.row_data(index) {
            Some(existing) if existing.title == section.title => {
                let title = existing.title.clone();
                sync_rows(existing.rows, section.rows, |rows| {
                    model.set_row_data(index, InfoSection { title, rows });
                });
            }
            Some(_) => model.set_row_data(index, install_section(section)),
            None => model.push(install_section(section)),
        }
    }
    while model.row_count() > count {
        model.remove(model.row_count() - 1);
    }
}

pub fn file_name(asset: &MediaSummary) -> SharedString {
    asset
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
        .into()
}

/// Library view chosen in the media panel: collection 0 all, 1 with audio,
/// 2 silent, narrowed by the name search.
pub fn passes_filter(asset: &MediaSummary, filter: i32, query: &str) -> bool {
    let has_audio = !asset.metadata.audio_streams.is_empty();
    let in_collection = match filter {
        1 => has_audio,
        2 => !has_audio,
        _ => true,
    };
    in_collection && matches_query(&file_name(asset), query)
}

/// Case-insensitive substring match on the file name; blank queries match all.
pub fn matches_query(name: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

pub fn media_row(asset: &MediaSummary, source_index: usize, selected: bool) -> MediaRow {
    let info = &asset.metadata;
    let mut format = codec_label(&info.codec);
    if let Some(rate) = info.average_rate.or(info.nominal_rate) {
        format.push_str(", ");
        format.push_str(&frame_rate(rate));
    }
    MediaRow {
        source_index: i32::try_from(source_index).unwrap_or(i32::MAX),
        name: file_name(asset),
        resolution: format!("{} × {}", info.width, info.height).into(),
        format: format.into(),
        duration: duration_ms(info)
            .map_or_else(|| "--:--".to_owned(), short_duration)
            .into(),
        estimated: info.duration_is_estimate,
        has_audio: !info.audio_streams.is_empty(),
        selected,
    }
}

/// Track header rows; badges number each family in order ("V1", "A1", "V2").
pub fn track_list(snapshot: &ProjectSnapshot) -> Vec<TrackRow> {
    let (mut video, mut audio) = (0, 0);
    snapshot
        .tracks
        .iter()
        .map(|track| {
            let (kind, badge) = match track.kind {
                TrackKind::Video => {
                    video += 1;
                    ("VIDEO", format!("V{video}"))
                }
                TrackKind::Audio => {
                    audio += 1;
                    ("AUDIO", format!("A{audio}"))
                }
            };
            TrackRow {
                name: track.name.clone().into(),
                kind: kind.into(),
                badge: badge.into(),
                selected: snapshot.selection == SelectionState::Track(track.id),
            }
        })
        .collect()
}

fn section(title: &str, rows: Vec<InfoRow>) -> Section {
    Section {
        title: title.into(),
        rows,
    }
}

fn field(label: &str, value: impl Into<SharedString>) -> InfoRow {
    InfoRow {
        label: label.into(),
        value: value.into(),
    }
}

fn clock(timebase: Timebase) -> String {
    format!("{}/{} s", timebase.numerator(), timebase.denominator())
}

pub fn project_rows(snapshot: &ProjectSnapshot) -> Vec<Section> {
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
                field("Clips", "None yet"),
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
                field("Storage", "In memory, not saved"),
            ],
        ),
    ]
}

pub fn track_rows(snapshot: &ProjectSnapshot, track: &TrackSummary) -> Vec<Section> {
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
                field("Clips", "Empty"),
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

pub fn source_rows(asset: &MediaSummary) -> Vec<Section> {
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
                .map_or_else(unknown, |folder| folder.to_string_lossy().into_owned()),
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
/// colon timecode (`0:01.5`, `00:01.500`, `1:02:03.250`). Fractions are
/// limited to milliseconds; the caller validates the source range.
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
    fn media_search_matches_names_case_insensitively() {
        assert!(matches_query("colors-cfr.mkv", ""));
        assert!(matches_query("colors-cfr.mkv", "   "));
        assert!(matches_query("colors-cfr.mkv", "CFR"));
        assert!(matches_query("Ünïcode clip.mp4", "ünï"));
        assert!(matches_query("audio-video.mp4", " video "));
        assert!(!matches_query("colors-cfr.mkv", "vfr"));
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
}

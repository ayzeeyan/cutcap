use crate::MediaError;
use cutcap_contracts::{Ratio, Rounding, SourceTime, Timebase, VideoMetadata};
use serde_json::Value;

pub const MAX_PROBE_BYTES: usize = 1024 * 1024;

/// Parse decimal seconds exactly; reject exponents, NaN, infinities and huge inputs.
pub(crate) fn decimal_time(text: &str, timebase: Timebase) -> Result<i64, MediaError> {
    if text.len() > 40 {
        return Err(invalid("invalid decimal timestamp"));
    }
    let negative = text.starts_with('-');
    let text = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || !fraction.bytes().all(|c| c.is_ascii_digit())
        || fraction.len() > 9
    {
        return Err(invalid("invalid decimal timestamp"));
    }
    let scale = 10_i128.pow(fraction.len() as u32);
    let whole: i128 = whole.parse().map_err(|_| invalid("timestamp overflow"))?;
    let frac: i128 = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse()
            .map_err(|_| invalid("timestamp overflow"))?
    };
    let num = whole
        .checked_mul(scale)
        .and_then(|n| n.checked_add(frac))
        .and_then(|n| n.checked_mul(i128::from(timebase.denominator())))
        .ok_or_else(|| invalid("timestamp overflow"))?;
    let denominator = scale * i128::from(timebase.numerator());
    let signed = if negative { -num } else { num };
    i64::try_from(signed.div_euclid(denominator)).map_err(|_| invalid("timestamp overflow"))
}

fn invalid(message: &str) -> MediaError {
    MediaError::Invalid(message.into())
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && *v != "N/A" && *v != "unknown")
}
fn integer(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}
fn ratio(raw: Option<&str>) -> Result<Option<Ratio>, MediaError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if matches!(raw, "0/0" | "0:1" | "0/1") {
        return Ok(None);
    }
    let (n, d) = raw
        .split_once('/')
        .or_else(|| raw.split_once(':'))
        .ok_or_else(|| invalid("malformed ratio"))?;
    let value = Ratio {
        numerator: n.parse().map_err(|_| invalid("malformed ratio"))?,
        denominator: d.parse().map_err(|_| invalid("malformed ratio"))?,
    };
    value.validate().map_err(|e| invalid(&e.to_string()))?;
    Ok(Some(value))
}

fn angle(raw: &str) -> Result<i64, MediaError> {
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if raw.len() > 32 || !fraction.bytes().all(|c| c == b'0') {
        return Err(MediaError::Unsupported("non-orthogonal rotation".into()));
    }
    whole
        .parse()
        .map_err(|_| invalid("invalid display rotation"))
}

pub fn parse_probe(bytes: &[u8]) -> Result<VideoMetadata, MediaError> {
    if bytes.len() > MAX_PROBE_BYTES {
        return Err(MediaError::Budget("FFprobe JSON exceeds 1 MiB"));
    }
    let root: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("FFprobe returned malformed JSON"))?;
    let streams = root["streams"]
        .as_array()
        .ok_or_else(|| invalid("missing streams"))?;
    if streams.is_empty() || streams.len() > 64 {
        return Err(MediaError::Budget("stream count must be 1–64"));
    }
    let video = streams
        .iter()
        .find(|s| {
            text(s, "codec_type") == Some("video")
                && s["disposition"]["attached_pic"].as_i64() != Some(1)
        })
        .ok_or_else(|| MediaError::Unsupported("no non-attached video stream".into()))?;
    let (n, d) = text(video, "time_base")
        .and_then(|t| t.split_once('/'))
        .ok_or_else(|| invalid("missing video timebase"))?;
    let timebase = Timebase::new(
        n.parse().map_err(|_| invalid("invalid timebase"))?,
        d.parse().map_err(|_| invalid("invalid timebase"))?,
    )
    .map_err(|_| invalid("invalid timebase"))?;
    let start_pts = integer(video, "start_pts").ok_or_else(|| {
        MediaError::Unsupported("video has no reliable integer source start PTS".into())
    })?;
    let duration_is_estimate = integer(video, "duration_ts").is_none()
        && text(video, "duration").is_none()
        && text(&root["format"], "duration").is_some();
    let duration_ticks = if let Some(ticks) = integer(video, "duration_ts") {
        Some(ticks)
    } else if let Some(seconds) = text(video, "duration") {
        Some(decimal_time(seconds, timebase)?)
    } else if let Some(seconds) = text(&root["format"], "duration") {
        let reported = decimal_time(seconds, timebase)?;
        // Matroska's reported duration includes a positive timestamp offset.
        // Container fallback is explicitly an estimate, especially with A/V.
        let offset = if text(&root["format"], "format_name").is_some_and(|s| s.contains("matroska"))
        {
            text(&root["format"], "start_time")
                .map(|s| decimal_time(s, timebase))
                .transpose()?
                .unwrap_or(0)
                .max(0)
        } else {
            0
        };
        Some(
            reported
                .checked_sub(offset)
                .ok_or_else(|| invalid("invalid container duration"))?,
        )
    } else {
        None
    };
    let mut rotation = 0_i64;
    if let Some(value) = video["tags"]["rotate"].as_str() {
        rotation = angle(value)?;
    }
    if let Some(side_data) = video["side_data_list"].as_array() {
        if side_data.len() > 32 {
            return Err(MediaError::Budget("too many display side-data records"));
        }
        for side in side_data {
            if let Some(value) = side.get("rotation") {
                let raw = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                rotation = angle(&raw)?;
            }
            if let Some(matrix) = side.get("displaymatrix").and_then(Value::as_str) {
                let values: Vec<i64> = matrix
                    .lines()
                    .filter_map(|line| line.split_once(':').map(|(_, data)| data))
                    .flat_map(str::split_whitespace)
                    .filter_map(|v| v.parse().ok())
                    .collect();
                if values.len() != 9
                    || i128::from(values[0]) * i128::from(values[4])
                        - i128::from(values[1]) * i128::from(values[3])
                        < 0
                {
                    return Err(MediaError::Unsupported(
                        "reflected or malformed display matrix".into(),
                    ));
                }
                // Only an unscaled orthogonal rotation is implemented. Do not
                // silently ignore skew, perspective, translation or singular matrices.
                const UNIT: i128 = 65536;
                let [a, b, u, c, d, v, x, y, w] = values.as_slice() else {
                    return Err(invalid("invalid display matrix"));
                };
                if [*a, *b, *c, *d].iter().any(|n| n.unsigned_abs() > 65536)
                    || i128::from(*a).pow(2) + i128::from(*b).pow(2) != UNIT * UNIT
                    || i128::from(*c).pow(2) + i128::from(*d).pow(2) != UNIT * UNIT
                    || i128::from(*a) * i128::from(*c) + i128::from(*b) * i128::from(*d) != 0
                    || [*u, *v, *x, *y] != [0, 0, 0, 0]
                    || *w != 1_i64 << 30
                {
                    return Err(MediaError::Unsupported(
                        "scaled, skewed or malformed display matrix".into(),
                    ));
                }
            }
        }
    }
    let rotation = rotation.rem_euclid(360);
    if rotation % 90 != 0 {
        return Err(MediaError::Unsupported("non-orthogonal rotation".into()));
    }
    let metadata = VideoMetadata {
        codec: text(video, "codec_name")
            .ok_or_else(|| MediaError::Unsupported("unknown video codec".into()))?
            .into(),
        width: integer(video, "width")
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| invalid("invalid video width"))?,
        height: integer(video, "height")
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| invalid("invalid video height"))?,
        pixel_format: text(video, "pix_fmt").map(str::to_owned),
        sample_aspect: ratio(text(video, "sample_aspect_ratio"))?,
        display_aspect: ratio(text(video, "display_aspect_ratio"))?,
        rotation: rotation as u16,
        average_rate: ratio(text(video, "avg_frame_rate"))?,
        nominal_rate: ratio(text(video, "r_frame_rate"))?,
        timebase,
        start_pts,
        duration_ticks,
        duration_is_estimate,
        container: text(&root["format"], "format_name")
            .ok_or_else(|| invalid("missing container format"))?
            .into(),
        stream_index: integer(video, "index")
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| invalid("invalid stream index"))?,
        stream_id: text(video, "id").map(str::to_owned),
        audio_streams: streams
            .iter()
            .filter(|s| text(s, "codec_type") == Some("audio"))
            .map(|s| {
                integer(s, "index")
                    .and_then(|i| u32::try_from(i).ok())
                    .ok_or_else(|| invalid("invalid audio stream index"))
            })
            .collect::<Result<_, _>>()?,
        color_space: text(video, "color_space").map(str::to_owned),
        color_transfer: text(video, "color_transfer").map(str::to_owned),
        color_range: text(video, "color_range").map(str::to_owned),
    };
    metadata.validate().map_err(|e| invalid(&e.to_string()))?;
    // Validate duration conversion now, never on the UI thread.
    if let Some(duration) = metadata.duration_ticks {
        SourceTime {
            ticks: duration,
            timebase,
        }
        .rescale(
            Timebase::new(1, 1000).map_err(|_| invalid("clock"))?,
            Rounding::Ceil,
        )
        .map_err(|_| invalid("duration overflow"))?;
    }
    Ok(metadata)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture() -> Value {
        serde_json::json!({"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
            "width":1920,"height":1080,"pix_fmt":"yuv420p","time_base":"1/90000",
            "start_pts":-9000,"duration_ts":270000,"avg_frame_rate":"30000/1001",
            "r_frame_rate":"30000/1001","sample_aspect_ratio":"1:1","display_aspect_ratio":"16:9"},
            {"index":1,"codec_type":"audio"}],"format":{"format_name":"mov,mp4","duration":"3.000000"}})
    }
    fn parse(value: &Value) -> Result<VideoMetadata, MediaError> {
        parse_probe(&serde_json::to_vec(value).unwrap())
    }
    #[test]
    fn authentic_fields_and_missing_optional_values_are_explicit() {
        let metadata = parse(&fixture()).unwrap();
        assert_eq!(metadata.start_pts, -9000);
        assert_eq!(metadata.duration_ticks, Some(270000));
        assert!(!metadata.duration_is_estimate);
        assert_eq!(metadata.audio_streams, vec![1]);
        assert_eq!(metadata.preview_dimensions().unwrap(), (1280, 720));
        assert_eq!(metadata.average_rate.unwrap().denominator, 1001);
        let mut value = fixture();
        value["streams"][0]
            .as_object_mut()
            .unwrap()
            .remove("duration_ts");
        value["format"].as_object_mut().unwrap().remove("duration");
        value["streams"][0]["avg_frame_rate"] = "0/0".into();
        let metadata = parse(&value).unwrap();
        assert_eq!(metadata.duration_ticks, None);
        assert!(!metadata.duration_is_estimate);
        assert_eq!(metadata.average_rate, None);
    }
    #[test]
    fn untrusted_metadata_limits_and_unsupported_streams_are_rejected() {
        for (field, value) in [
            ("width", 0_i64),
            ("width", 4097),
            ("height", 4097),
            ("duration_ts", -1),
            ("duration_ts", 90000 * 86401),
        ] {
            let mut root = fixture();
            root["streams"][0][field] = value.into();
            assert!(parse(&root).is_err(), "accepted invalid {field}");
        }
        for (field, value) in [
            ("time_base", "1/0"),
            ("avg_frame_rate", "garbage"),
            ("codec_name", "bad\ncodec"),
            ("color_transfer", "smpte2084"),
        ] {
            let mut root = fixture();
            root["streams"][0][field] = value.into();
            assert!(parse(&root).is_err());
        }
        let mut oversized = fixture();
        oversized["streams"][0]["width"] = 4096.into();
        oversized["streams"][0]["height"] = 2161.into();
        assert!(parse(&oversized).is_err());
        let mut root = fixture();
        root["streams"][0]["codec_type"] = "audio".into();
        assert!(matches!(parse(&root), Err(MediaError::Unsupported(_))));
        root = fixture();
        root["streams"][0]["disposition"] = serde_json::json!({"attached_pic":1});
        assert!(matches!(parse(&root), Err(MediaError::Unsupported(_))));
        root = fixture();
        root["streams"][0]
            .as_object_mut()
            .unwrap()
            .remove("start_pts");
        root["streams"][0]["start_time"] = "0.000011".into();
        assert!(parse(&root).is_err());
        assert!(parse_probe(b"{truncated").is_err());
        assert!(matches!(
            parse_probe(&vec![b' '; MAX_PROBE_BYTES + 1]),
            Err(MediaError::Budget(_))
        ));
    }
    #[test]
    fn exact_decimals_rotation_and_aspect_do_not_use_nominal_frame_indexes() {
        let clock = Timebase::new(1, 90000).unwrap();
        assert_eq!(decimal_time("1.001", clock).unwrap(), 90090);
        assert_eq!(decimal_time("-0.000001", clock).unwrap(), -1);
        for bad in [
            "NaN",
            "inf",
            "1e10",
            "1.1234567890",
            "9999999999999999999999999999999999999999",
        ] {
            assert!(decimal_time(bad, clock).is_err());
        }
        let mut root = fixture();
        root["streams"][0]["side_data_list"] = serde_json::json!([{"rotation":90}]);
        assert_eq!(
            parse(&root).unwrap().preview_dimensions().unwrap(),
            (405, 720)
        );
        root["streams"][0]["side_data_list"] = serde_json::json!([{"rotation":89.9}]);
        assert!(parse(&root).is_err());
        root["streams"][0]["side_data_list"] = serde_json::json!([{"rotation":0,"displaymatrix":"0: 65536 0 0\n1: 0 -65536 0\n2: 0 0 1073741824"}]);
        assert!(parse(&root).is_err());
        root["streams"][0]["side_data_list"] = serde_json::json!([{"rotation":0,"displaymatrix":"0: 0 0 0\n1: 0 0 0\n2: 0 0 1073741824"}]);
        assert!(parse(&root).is_err());
    }
}

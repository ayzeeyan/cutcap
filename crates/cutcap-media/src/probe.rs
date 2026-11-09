use crate::MediaError;
use cutcap_contracts::{
    AudioOnlyMetadata, AudioStreamMetadata, Ratio, Rounding, SourceMetadata, SourceTime, Timebase,
    VideoMetadata,
};
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

fn timebase_of(stream: &Value) -> Option<Timebase> {
    let (n, d) = text(stream, "time_base")?.split_once('/')?;
    Timebase::new(n.parse().ok()?, d.parse().ok()?).ok()
}

/// Details of one audio stream, or `None` when FFprobe did not report what
/// decoding needs (rate, channels, clock, integer start). Such a stream stays
/// listed in `audio_streams` but is not offered for linked placement.
fn audio_details(stream: &Value) -> Result<Option<AudioStreamMetadata>, MediaError> {
    let Some(index) = integer(stream, "index").and_then(|i| u32::try_from(i).ok()) else {
        return Err(invalid("invalid audio stream index"));
    };
    let (Some(timebase), Some(start_pts), Some(codec), Some(sample_rate), Some(channels)) = (
        timebase_of(stream),
        integer(stream, "start_pts"),
        text(stream, "codec_name"),
        integer(stream, "sample_rate").and_then(|r| u32::try_from(r).ok()),
        integer(stream, "channels").and_then(|c| u16::try_from(c).ok()),
    ) else {
        return Ok(None);
    };
    let duration_ticks = match integer(stream, "duration_ts") {
        Some(ticks) => Some(ticks),
        None => text(stream, "duration")
            .map(|s| decimal_time(s, timebase))
            .transpose()?,
    };
    let audio = AudioStreamMetadata {
        stream_index: index,
        stream_id: text(stream, "id").map(str::to_owned),
        codec: codec.into(),
        timebase,
        start_pts,
        duration_is_estimate: duration_ticks.is_none(),
        duration_ticks,
        sample_rate,
        channels,
        channel_layout: text(stream, "channel_layout").map(str::to_owned),
        sample_format: text(stream, "sample_fmt").map(str::to_owned),
        initial_padding: integer(stream, "initial_padding")
            .and_then(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0),
        default: stream["disposition"]["default"].as_i64() == Some(1),
    };
    Ok(audio.validate().is_ok().then_some(audio))
}

/// Whether this audio-only container/codec pair is one CutCap has verified
/// against real files. Extensions are never trusted: FFprobe's names decide.
pub fn audio_only_supported(container: &str, codec: &str) -> bool {
    container.split(',').any(|name| match name {
        "wav" => matches!(codec, "pcm_s16le" | "pcm_s24le" | "pcm_f32le"),
        "mp3" => codec == "mp3",
        "flac" => codec == "flac",
        "mov" | "mp4" | "m4a" => codec == "aac",
        "matroska" => codec == "pcm_s16le",
        _ => false,
    })
}

/// A file with a real picture stream is a video source; one with only cover
/// art or no video at all is an audio-only source. Nothing is fabricated for
/// the missing picture.
pub fn parse_source_probe(bytes: &[u8]) -> Result<SourceMetadata, MediaError> {
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
    let has_picture = streams.iter().any(|s| {
        text(s, "codec_type") == Some("video")
            && s["disposition"]["attached_pic"].as_i64() != Some(1)
    });
    if has_picture {
        return parse_probe(bytes).map(Into::into);
    }
    let container =
        text(&root["format"], "format_name").ok_or_else(|| invalid("missing container format"))?;
    let mut audio = Vec::new();
    for stream in streams
        .iter()
        .filter(|s| text(s, "codec_type") == Some("audio"))
    {
        // WAV has no timestamps: by format its first sample is at zero.
        let mut stream = stream.clone();
        if container.split(',').any(|name| name == "wav") && integer(&stream, "start_pts").is_none()
        {
            stream["start_pts"] = 0.into();
        }
        let Some(mut details) = audio_details(&stream)? else {
            continue;
        };
        if details.duration_ticks.is_none()
            && let Some(seconds) = text(&root["format"], "duration")
        {
            // Matroska reports durations only on the container: an estimate.
            let reported = decimal_time(seconds, details.timebase)?;
            details.duration_ticks = Some(
                reported
                    .checked_sub(details.start_pts.max(0))
                    .ok_or_else(|| invalid("invalid container duration"))?,
            );
            details.duration_is_estimate = true;
        }
        if details.validate().is_ok() {
            audio.push(details);
        }
    }
    if audio.is_empty() {
        return Err(MediaError::Unsupported(
            "no video stream and no decodable audio stream".into(),
        ));
    }
    let metadata = AudioOnlyMetadata {
        container: container.into(),
        audio,
    };
    metadata.validate().map_err(|e| invalid(&e.to_string()))?;
    Ok(SourceMetadata::Audio(metadata))
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
        audio: streams
            .iter()
            .filter(|s| text(s, "codec_type") == Some("audio"))
            .map(audio_details)
            .filter_map(Result::transpose)
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
    /// A stereo AAC stream on its own clock, starting 21 ms after the video.
    fn audio_stream(index: i64, default: i64) -> Value {
        serde_json::json!({"index":index,"codec_type":"audio","codec_name":"aac","sample_rate":"44100",
            "channels":2,"channel_layout":"stereo","sample_fmt":"fltp","initial_padding":1024,
            "time_base":"1/44100","start_pts":926,"duration_ts":132300,"id":"0x2",
            "disposition":{"default":default}})
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
    fn audio_streams_keep_their_own_clock_and_the_primary_follows_the_default_flag() {
        let mut root = fixture();
        root["streams"][1] = audio_stream(1, 0);
        root["streams"]
            .as_array_mut()
            .unwrap()
            .push(audio_stream(2, 1));
        root["streams"][2]["sample_rate"] = "48000".into();
        root["streams"][2]["channels"] = 1.into();
        root["streams"][2]["time_base"] = "1/48000".into();
        root["streams"][2]["start_pts"] = (-1024).into();
        root["streams"][2]
            .as_object_mut()
            .unwrap()
            .remove("duration_ts");
        root["streams"][2]["duration"] = "2.5".into();
        let m = parse(&root).unwrap();
        assert_eq!(m.audio_streams, vec![1, 2]);
        assert_eq!(m.audio.len(), 2);
        let first = m.audio_stream(1).unwrap();
        assert_eq!(first.timebase, Timebase::new(1, 44100).unwrap());
        assert_ne!(first.timebase, m.timebase, "audio keeps its own clock");
        assert_eq!((first.start_pts, first.duration_ticks), (926, Some(132300)));
        assert_eq!((first.sample_rate, first.channels), (44100, 2));
        assert_eq!(first.initial_padding, Some(1024));
        let primary = m.primary_audio().unwrap();
        assert_eq!(
            primary.stream_index, 2,
            "the default-disposition stream wins"
        );
        assert_eq!(
            (primary.start_pts, primary.duration_ticks),
            (-1024, Some(120000))
        );
        assert_eq!(primary.channels, 1);
        // Without a default flag the lowest index is primary.
        root["streams"][2]["disposition"]["default"] = 0.into();
        assert_eq!(
            parse(&root).unwrap().primary_audio().unwrap().stream_index,
            1
        );
        // A stream missing decoding facts stays listed but is not decodable.
        root["streams"][1]
            .as_object_mut()
            .unwrap()
            .remove("sample_rate");
        let m = parse(&root).unwrap();
        assert_eq!(m.audio_streams, vec![1, 2]);
        assert_eq!(m.audio.len(), 1);
        assert_eq!(m.primary_audio().unwrap().stream_index, 2);
        // Out-of-budget channel counts are not decodable either.
        root["streams"][2]["channels"] = 12.into();
        assert!(parse(&root).unwrap().primary_audio().is_none());
        // Video-only sources are unchanged.
        let mut silent = fixture();
        silent["streams"].as_array_mut().unwrap().pop();
        let m = parse(&silent).unwrap();
        assert!(m.audio_streams.is_empty() && m.audio.is_empty() && m.primary_audio().is_none());
    }
    #[test]
    fn metadata_saved_before_audio_details_still_deserializes() {
        let m = parse(&fixture()).unwrap();
        let mut json = serde_json::to_value(&m).unwrap();
        json.as_object_mut().unwrap().remove("audio");
        let old: VideoMetadata = serde_json::from_value(json).unwrap();
        assert!(old.audio.is_empty());
        assert_eq!(old.audio_streams, vec![1]);
    }
    #[test]
    fn metadata_saved_before_audio_details_matches_a_fresh_probe_of_the_same_file() {
        let mut root = fixture();
        root["streams"][1] = audio_stream(1, 1);
        let fresh: SourceMetadata = parse(&root).unwrap().into();
        let mut old = fresh.clone();
        old.video_mut().unwrap().audio.clear();
        assert!(
            old.matches_probe(&fresh),
            "details were simply not saved yet"
        );
        assert!(fresh.matches_probe(&fresh));
        // Anything else that differs is still a changed source.
        let mut other = old.clone();
        other.video_mut().unwrap().width = 1280;
        assert!(!other.matches_probe(&fresh));
        // Saved details that disagree are not excused.
        let mut wrong = fresh.clone();
        wrong.video_mut().unwrap().audio[0].sample_rate = 8000;
        assert!(!wrong.matches_probe(&fresh));
    }
    fn parse_source(value: &Value) -> Result<SourceMetadata, MediaError> {
        parse_source_probe(&serde_json::to_vec(value).unwrap())
    }
    #[test]
    fn audio_only_files_carry_no_invented_picture_and_cover_art_is_not_video() {
        let mp3 = serde_json::json!({"streams":[
            {"index":0,"codec_type":"audio","codec_name":"mp3","sample_rate":"44100","channels":2,
             "channel_layout":"stereo","sample_fmt":"fltp","time_base":"1/14112000","start_pts":353600,
             "duration_ts":141120000,"disposition":{"default":1}},
            {"index":1,"codec_type":"video","codec_name":"mjpeg","width":300,"height":300,
             "time_base":"1/90000","start_pts":0,"disposition":{"attached_pic":1}}],
            "format":{"format_name":"mp3","duration":"10.0"}});
        let parsed = parse_source(&mp3).unwrap();
        assert!(parsed.is_audio_only() && parsed.video().is_none());
        assert_eq!(parsed.container(), "mp3");
        let stream = parsed.primary_audio().unwrap();
        assert_eq!((stream.stream_index, stream.start_pts), (0, 353600));
        assert_eq!(stream.duration_ticks, Some(141120000));
        assert!(!stream.duration_is_estimate);
        // The on-disk form round-trips and cannot be mistaken for a video source.
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(!json.contains("width"));
        assert_eq!(
            serde_json::from_str::<SourceMetadata>(&json).unwrap(),
            parsed
        );

        // A container-only duration is kept and marked as an estimate.
        let mut mkv = mp3.clone();
        mkv["streams"][0]
            .as_object_mut()
            .unwrap()
            .remove("duration_ts");
        mkv["streams"][0]["time_base"] = "1/1000".into();
        mkv["streams"][0]["start_pts"] = 0.into();
        let parsed = parse_source(&mkv).unwrap();
        let stream = parsed.primary_audio().unwrap();
        assert_eq!(stream.duration_ticks, Some(10_000));
        assert!(stream.duration_is_estimate);

        // A real picture stream still makes it a video source.
        let parsed = parse_source(&fixture()).unwrap();
        assert!(!parsed.is_audio_only() && parsed.video().is_some());

        // Neither picture nor decodable audio: unsupported, as before.
        let mut nothing = fixture();
        nothing["streams"][0]["codec_type"] = "subtitle".into();
        assert!(matches!(
            parse_source(&nothing),
            Err(MediaError::Unsupported(_))
        ));
    }
    #[test]
    fn only_verified_audio_only_formats_are_accepted() {
        for (container, codec) in [
            ("wav", "pcm_s16le"),
            ("wav", "pcm_s24le"),
            ("wav", "pcm_f32le"),
            ("mp3", "mp3"),
            ("flac", "flac"),
            ("mov,mp4,m4a,3gp,3g2,mj2", "aac"),
            ("matroska,webm", "pcm_s16le"),
        ] {
            assert!(
                audio_only_supported(container, codec),
                "{container} {codec}"
            );
        }
        for (container, codec) in [
            ("wav", "pcm_mulaw"),
            ("mp3", "aac"),
            ("ogg", "vorbis"),
            ("mov,mp4,m4a,3gp,3g2,mj2", "alac"),
            ("matroska,webm", "opus"),
        ] {
            assert!(
                !audio_only_supported(container, codec),
                "{container} {codec}"
            );
        }
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

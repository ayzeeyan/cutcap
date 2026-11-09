//! Explicit release-only measurement, separate from correctness tests.
use super::*;

#[test]
#[ignore = "performance measurement: run explicitly with --release --ignored --nocapture"]
fn indexed_evaluation_cost_and_response_size() {
    let metadata=cutcap_media::parse_probe(br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"1/1000","start_pts":0,"duration_ts":3000}],"format":{"format_name":"matroska"}}"#).unwrap();
    for count in [2usize, 5000, 50_000] {
        let mut project = Project::new("Measure", Timebase::DEFAULT_TIMELINE).unwrap();
        let track = TrackId::new();
        let media = MediaId::new();
        project
            .add_track(Track {
                id: track,
                name: "Video 1".into(),
                kind: TrackKind::Video,
                controls: Default::default(),
            })
            .unwrap();
        project
            .add_media(cutcap_core::MediaAsset {
                id: media,
                path: "owned-fixture.mkv".into(),
                kind: TrackKind::Video,
            })
            .unwrap();
        let mut data = serde_json::to_value(project).unwrap();
        data["clips"]=serde_json::Value::Array((0..count).map(|i|serde_json::json!({
            "id":ClipId::new(),"media_id":media,"track_id":track,"start":i as i64*720_000,
            "duration":720_000,"source_in":{"ticks":0,"timebase":{"numerator":1,"denominator":1000}}
        })).collect());
        let project: Project = serde_json::from_value(data).unwrap();
        let project_id = project.id();
        let mut engine = Engine {
            project: Some(project),
            ..Engine::default()
        };
        engine.media_details.insert(
            media,
            MediaSummary {
                id: media,
                path: "owned-fixture.mkv".into(),
                metadata: metadata.clone().into(),
            },
        );
        engine.availability.insert(
            media,
            MediaAvailability {
                id: media,
                path: "owned-fixture.mkv".into(),
                state: Availability::Ready,
                detail: "Verified fixture".into(),
            },
        );
        let first = std::time::Instant::now();
        engine
            .evaluate(Some(project_id), TimelineSeek::Ticks(TimelineTime(0)), None)
            .unwrap();
        let build_us = first.elapsed().as_micros();
        let mut samples = Vec::with_capacity(2000);
        let mut bytes = 0;
        for i in 0..2000usize {
            let started = std::time::Instant::now();
            let result = engine
                .evaluate(
                    Some(project_id),
                    TimelineSeek::Ticks(TimelineTime((i % count) as i64 * 720_000 + 180_000)),
                    None,
                )
                .unwrap();
            samples.push(started.elapsed().as_nanos());
            bytes = serde_json::to_vec(&result).unwrap().len();
        }
        samples.sort_unstable();
        println!(
            "{}",
            serde_json::json!({"clips":count,"samples":2000,"index_and_first_evaluation_us":build_us,
            "evaluation_p50_ns":samples[1000],"evaluation_p95_ns":samples[1900],"evaluation_max_ns":samples[1999],"response_bytes":bytes})
        );
    }
}

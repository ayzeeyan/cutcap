//! Unpaced decoder + framed-pipe throughput, separate from presentation tests.
use cutcap_contracts::*;
use cutcap_media::*;
use std::time::Instant;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = FfmpegBackend::default();
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Pass a generated media path")?;
    let probe = backend.probe(std::path::Path::new(&path), &Cancellation::default())?;
    let media = MediaSummary {
        id: MediaId::new(),
        path: probe.path,
        metadata: probe.metadata,
    };
    let request = DecodeRequest {
        ticket: PreviewTicket {
            project_id: ProjectId::new(),
            media_id: media.id,
            request_id: RequestId::new(),
        },
        position: media.metadata.video().unwrap().start(),
        media,
    };
    let (mut frames, mut bytes, mut pid, mut first, mut last) = (0_u64, 0_u64, 0_u32, None, None);
    let started = Instant::now();
    backend.stream(&request, &Cancellation::default(), &mut |frame| {
        frames += 1;
        bytes += frame.image.pixels().len() as u64;
        pid = frame.process_id;
        first.get_or_insert(frame.actual);
        last = Some(frame.actual);
        Ok(())
    })?;
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({"method":"unpaced decode + PAM pipe + validation; not presented FPS","frames":frames,"seconds":elapsed,"fps":frames as f64/elapsed,"bytes":bytes,"pid":pid,"first_pts":first,"last_pts":last})
    );
    Ok(())
}

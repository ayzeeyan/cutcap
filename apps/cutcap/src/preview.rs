//! Replaceable CPU presentation adapter; called only on the Slint UI thread.
use crate::AppWindow;
use cutcap_contracts::{MediaSummary, Rounding, SourceTime, Timebase};
use cutcap_media::{DecodedFrame, PreviewTicket};
use serde::Serialize;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct PresentationMeasurement {
    pub source_width: u32,
    pub source_height: u32,
    pub preview_width: u32,
    pub preview_height: u32,
    pub frame_bytes: usize,
    pub requested_pts: i64,
    pub actual_pts: i64,
    pub timebase_numerator: u32,
    pub timebase_denominator: u32,
    pub decode_us: u64,
    pub copy_us: u64,
    pub request_to_ui_us: u64,
    pub import_to_ui_us: Option<u64>,
    pub probe_us: Option<u64>,
}

#[derive(Default)]
pub struct Presenter {
    ticket: Option<PreviewTicket>,
    queued: Option<Instant>,
}

impl Presenter {
    pub fn accepts(&self, ticket: PreviewTicket) -> bool {
        self.ticket == Some(ticket)
    }
    pub fn reset(&mut self, window: &AppWindow) {
        self.ticket = None;
        self.queued = None;
        window.set_preview_frame(Image::default());
        window.set_has_frame(false);
        window.set_preview_busy(false);
        window.set_source_ready(false);
        window.set_source_title("No source selected".into());
        window.set_preview_status("Select a video to preview".into());
        window.set_preview_detail("Import a local video into the media library.".into());
        window.set_frame_caption("".into());
        window.set_source_position_ms(0);
        window.set_source_duration_ms(0);
    }
    pub fn cancel(&mut self, window: &AppWindow) {
        self.ticket = None;
        self.queued = None;
        window.set_preview_busy(false);
        window.set_preview_status("Preview cancelled".into());
        window.set_preview_detail("Request another source position to continue.".into());
        window.set_status_text(
            "Frame request cancelled; the last displayed frame is retained.".into(),
        );
        window.set_status_error(false);
    }
    pub fn begin(&mut self, window: &AppWindow, ticket: PreviewTicket) {
        self.ticket = Some(ticket);
        self.queued = Some(Instant::now());
        window.set_preview_busy(true);
        window.set_preview_status("Decoding source frame…".into());
        window.set_preview_detail("A newer seek replaces this request.".into());
    }
    pub fn fail(&mut self, window: &AppWindow, message: &str) {
        self.ticket = None;
        self.queued = None;
        window.set_preview_busy(false);
        window.set_preview_status("Preview unavailable at this position".into());
        window.set_preview_detail(message.into());
        window.set_status_text(message.into());
        window.set_status_error(true);
    }
    pub fn present(
        &mut self,
        window: &AppWindow,
        media: &MediaSummary,
        decoded: DecodedFrame,
        probe_us: Option<u64>,
        import_started: Option<Instant>,
    ) -> Option<PresentationMeasurement> {
        // Independent UI gate protects even a result taken just before a new selection.
        if self.ticket != Some(decoded.ticket) || media.id != decoded.ticket.media_id {
            return None;
        }
        let queued = self.queued.take()?;
        let relative = media
            .metadata
            .start()
            .rescale(decoded.actual.timebase, Rounding::Floor)
            .ok()
            .and_then(|origin| decoded.actual.ticks.checked_sub(origin.ticks))
            .map(|ticks| SourceTime {
                ticks,
                timebase: decoded.actual.timebase,
            });
        let Some(relative) = relative else {
            self.fail(window, "Decoded source timestamp cannot be represented.");
            return None;
        };
        let started = Instant::now();
        // RgbaFrame has already validated dimensions, packed stride, length and opaque alpha.
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            decoded.image.pixels(),
            decoded.image.width(),
            decoded.image.height(),
        );
        window.set_preview_frame(Image::from_rgba8(pixels));
        window.set_preview_aspect(decoded.image.width() as f32 / decoded.image.height() as f32);
        window.set_has_frame(true);
        window.set_preview_busy(false);
        window.set_preview_status("Source frame decoded".into());
        window.set_preview_detail("".into());
        window.set_frame_caption(
            format!(
                "Frame +{} · PTS {} @ {}/{}",
                format_time(relative),
                decoded.actual.ticks,
                decoded.actual.timebase.numerator(),
                decoded.actual.timebase.denominator()
            )
            .into(),
        );
        let measurement = PresentationMeasurement {
            source_width: media.metadata.width,
            source_height: media.metadata.height,
            preview_width: decoded.image.width(),
            preview_height: decoded.image.height(),
            frame_bytes: decoded.image.pixels().len(),
            requested_pts: decoded.requested.ticks,
            actual_pts: decoded.actual.ticks,
            timebase_numerator: decoded.actual.timebase.numerator(),
            timebase_denominator: decoded.actual.timebase.denominator(),
            decode_us: decoded.decode_us,
            copy_us: micros(started),
            request_to_ui_us: micros(queued),
            import_to_ui_us: import_started.map(micros),
            probe_us,
        };
        tracing::info!(request = %decoded.ticket.request_id, decode_us=measurement.decode_us, copy_us=measurement.copy_us,
            request_to_ui_us=measurement.request_to_ui_us, bytes=measurement.frame_bytes, pts=measurement.actual_pts, child_pid=decoded.process_id,"preview presented");
        self.ticket = None;
        Some(measurement)
    }
}

pub fn micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
pub fn format_time(time: SourceTime) -> String {
    let clock = Timebase::new(1, 1000).expect("nonzero constant");
    let Ok(time) = time.rescale(clock, Rounding::Floor) else {
        return "unavailable".into();
    };
    let ms = time.ticks.max(0);
    format!(
        "{:02}:{:02}.{:03}",
        ms / 60_000,
        (ms / 1000) % 60,
        ms % 1000
    )
}

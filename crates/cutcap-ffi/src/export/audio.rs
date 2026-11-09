//! Offline mix: every audible track, in project order, to a scratch PCM file.
//!
//! The arithmetic is Phase 4C's, from `audio_mix`. What differs is the
//! transport: no device, no clock, no ramps, and the mixer simply waits for
//! its decoders, so every sample is computed at its exact position.
use super::{Fail, Progress, plan::ExportPlan};
use crate::audio_mix::*;
use cutcap_contracts::*;
use cutcap_media::{Cancellation, MediaBackend};
use std::{
    fs::File,
    io::{BufWriter, Write},
    sync::Arc,
    time::{Duration, Instant},
};

/// The most frames summed in one step; each step stops at clip boundaries.
const BLOCK_FRAMES: usize = 4096;
/// A clip's decoder starts this long before its first sample.
const PREFETCH_SECONDS: u64 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MixStats {
    pub clipped_samples: u64,
    pub peak: f32,
    /// Frames inside a clip where the source had nothing to play.
    pub source_silence_frames: u64,
    pub decoders_peak: usize,
    pub decoder_starts: u64,
    pub mix_us: u64,
}

/// Which spans are mixed and at what gain, in project order of their tracks.
pub(super) fn gains(plan: &ExportPlan, tracks: &[TrackId]) -> Vec<f32> {
    let mix = track_mix(&plan.controls);
    tracks
        .iter()
        .map(|id| {
            mix.iter()
                .find(|m| m.id == *id)
                .map_or(1.0, |m| if m.audible { m.gain } else { 0.0 })
        })
        .collect()
}

/// Mixes `plan.grid.samples` frames into `file` as raw interleaved `f32le`.
pub fn mix_to_file(
    plan: &ExportPlan,
    backend: &Arc<dyn MediaBackend>,
    file: &mut File,
    progress: &mut Progress<'_>,
    cancel: &Cancellation,
) -> Result<MixStats, Fail> {
    let rate = plan.preset.sample_rate;
    let total = plan.grid.samples;
    let (spans, _, tracks) = layout(&plan.audio, TimelineTime::ZERO, rate);
    let gains = gains(plan, &tracks);
    let horizon = u64::from(rate) * PREFETCH_SECONDS;
    let mut stats = MixStats::default();
    let mut slots: Vec<Slot> = Vec::new();
    let mut first = 0;
    let mut mix = vec![0.0f32; BLOCK_FRAMES * 2];
    let mut bytes = vec![0u8; BLOCK_FRAMES * 8];
    let mut writer = BufWriter::with_capacity(1 << 20, file);
    let mut k = 0u64;
    let began = Instant::now();
    while k < total {
        if cancel.is_cancelled() {
            return Err(Fail::Cancelled);
        }
        slots.retain(|slot| spans[slot.span].s1 > k);
        while first < spans.len() && spans[first].s1 <= k {
            first += 1;
        }
        for (offset, span) in spans[first..].iter().enumerate() {
            let index = first + offset;
            if span.s0 > k + horizon {
                break;
            }
            if span.s1 > k && !slots.iter().any(|s| s.span == index) && slots.len() < MAX_DECODERS {
                let slot = start_slot(
                    backend,
                    &plan.audio,
                    span,
                    index,
                    k.max(span.s0),
                    rate,
                    true,
                )
                .ok_or_else(|| Fail::Message("An audio decoder could not start.".into()))?;
                slots.push(slot);
                stats.decoder_starts += 1;
                stats.decoders_peak = stats.decoders_peak.max(slots.len());
            }
        }
        // The clips under frame k, in project track order; the block stops at
        // the next boundary, so each covers it entirely.
        let mut frames = BLOCK_FRAMES.min((total - k) as usize);
        let mut active: Vec<usize> = Vec::with_capacity(MAX_MIX_TRACKS);
        for (offset, span) in spans[first..].iter().enumerate() {
            if span.s0 > k {
                frames = frames.min((span.s0 - k) as usize);
                break;
            }
            if span.s1 > k {
                frames = frames.min((span.s1 - k) as usize);
                active.push(first + offset);
            }
        }
        active.sort_by_key(|index| spans[*index].track);
        // Each active clip says what it has here; the block shrinks to match.
        let mut pcm: Vec<(usize, i64)> = Vec::with_capacity(active.len());
        let mut silent = false;
        for span_index in &active {
            let span = spans[*span_index];
            let needed = k as i64 + span.offset;
            let slot = slots
                .iter()
                .position(|s| s.span == *span_index)
                .ok_or_else(|| Fail::Message("An audio clip has no decoder.".into()))?;
            loop {
                match slots[slot].peek(needed, frames) {
                    Run::Pcm(run) => {
                        frames = frames.min(run);
                        pcm.push((slot, needed));
                    }
                    Run::Silent(run) => {
                        frames = frames.min(run);
                        silent = true;
                    }
                    Run::End(None) => silent = true,
                    Run::End(Some(error)) => {
                        return Err(Fail::Message(format!("Audio decoding failed: {error}")));
                    }
                    Run::Wait => {
                        if cancel.is_cancelled() {
                            return Err(Fail::Cancelled);
                        }
                        slots[slot].wait_chunk(Duration::from_millis(20));
                        continue;
                    }
                }
                break;
            }
        }
        let out = &mut mix[..frames * 2];
        out.fill(0.0);
        for (slot, needed) in &pcm {
            let track = spans[slots[*slot].span].track;
            GainRamp::at(gains[track]).mix_into(slots[*slot].samples(*needed, frames), out);
        }
        let (clipped, peak) = clip_mix(out);
        stats.clipped_samples += clipped;
        stats.peak = stats.peak.max(peak);
        if silent {
            stats.source_silence_frames += frames as u64;
        }
        for (target, sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(out.iter()) {
            *target = sample.to_le_bytes();
        }
        writer
            .write_all(&bytes[..frames * 8])
            .map_err(|e| Fail::io("Writing the audio mix", &e))?;
        k += frames as u64;
        progress.mixed(k);
    }
    writer
        .flush()
        .map_err(|e| Fail::io("Writing the audio mix", &e))?;
    stats.mix_us = began.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    Ok(stats)
}

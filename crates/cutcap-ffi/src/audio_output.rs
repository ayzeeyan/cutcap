//! Phase 4B audio output and the audio-master clock.
//!
//! A bounded ring of interleaved stereo `f32` frames sits between the feeder
//! (decoders) and the output. The output records which device frames carried
//! timeline audio and which carried silence it had to insert itself (starved
//! or finished): a [`Segment`] map from device frames to timeline frames.
//! The clock is the device's own playback position (WASAPI `IAudioClock`,
//! sampled on the render thread right after each write, then extrapolated by
//! at most [`MAX_EXTRAPOLATION_MS`] and never past what was written) mapped
//! through that segment map. So timeline time advances only while real
//! timeline audio is actually being played, and stops while the output pads.
//!
//! WASAPI's position already subtracts the latency the driver reports, but
//! not processing delay the endpoint adds after the engine (enhancement APOs).
//! On the Phase 4B test machine (Realtek, `GetStreamLatency` = 0) the mixed
//! output reached the loopback tap 66.6 ms after the position said it played.
//! That delay cannot be queried, so it is an explicit, measured setting:
//! `CUTCAP_AUDIO_LATENCY_MS` (0-500, default 0) holds the clock back by that
//! much, which delays the picture to match the sound.
//!
//! Threads: the WASAPI render thread owns every COM object; the pump thread
//! only locks the shared state briefly (copying samples or reading the
//! clock). The render thread never waits on FFmpeg or the filesystem and
//! allocates nothing per period after start-up.
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const CHANNELS: usize = 2;
/// Ring capacity: 400 ms of audio (small: a seek flushes it).
const RING_MS: u64 = 400;
/// Frames the ring must hold before the device starts (start latency budget).
pub const PREFILL_MS: u64 = 120;
/// Clock extrapolation between device position samples.
pub const MAX_EXTRAPOLATION_MS: u64 = 40;
/// Upper bound for the output latency compensation setting.
pub const MAX_COMPENSATION_MS: u64 = 500;

/// `CUTCAP_AUDIO_LATENCY_MS`: whole milliseconds, clamped to the bound;
/// anything unparsable means none.
pub fn compensation_ms_from(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(0, |ms| ms.min(MAX_COMPENSATION_MS))
}

/// A run of device frames: real timeline frames, or padding silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub device_start: u64,
    pub timeline_start: u64,
    pub frames: u64,
    pub real: bool,
}

/// Exact device-frame to timeline-frame mapping (pure; unit-tested).
#[derive(Debug, Default, Clone)]
pub struct ClockMap {
    segments: VecDeque<Segment>,
    device_written: u64,
    timeline_written: u64,
}

impl ClockMap {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    #[cfg(test)]
    pub fn device_written(&self) -> u64 {
        self.device_written
    }
    #[cfg(test)]
    pub fn timeline_written(&self) -> u64 {
        self.timeline_written
    }
    /// Records `frames` written to the device, real or padding.
    pub fn record(&mut self, frames: u64, real: bool) {
        if frames == 0 {
            return;
        }
        match self.segments.back_mut() {
            Some(last) if last.real == real => last.frames += frames,
            _ => self.segments.push_back(Segment {
                device_start: self.device_written,
                timeline_start: self.timeline_written,
                frames,
                real,
            }),
        }
        self.device_written += frames;
        if real {
            self.timeline_written += frames;
        }
    }
    /// Timeline frames played when the device has played `device` frames.
    pub fn timeline_at(&self, device: u64) -> u64 {
        let device = device.min(self.device_written);
        let index = self.segments.partition_point(|s| s.device_start <= device);
        match index.checked_sub(1).and_then(|i| self.segments.get(i)) {
            None => 0,
            Some(s) if s.real => s.timeline_start + (device - s.device_start).min(s.frames),
            Some(s) => s.timeline_start,
        }
    }
    /// Drops segments the device has fully played (bounded memory).
    pub fn prune(&mut self, played: u64) {
        while self.segments.len() > 1
            && self
                .segments
                .get(1)
                .is_some_and(|next| next.device_start <= played)
        {
            self.segments.pop_front();
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct OutputStatus {
    /// unavailable, idle, prefill, playing, paused, failed.
    pub state: &'static str,
    pub device: String,
    pub rate: u32,
    pub stream_latency_ms: f64,
    /// Output latency compensation applied to the clock (setting, not measured here).
    pub latency_compensation_ms: f64,
    pub buffered_ms: f64,
    pub underruns: u64,
    pub padded_ms: f64,
    pub played_ms: f64,
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    ring: VecDeque<f32>,
    map: ClockMap,
    /// Device frames played at `anchor`, as last sampled from the device clock.
    anchor: Option<(u64, Instant)>,
    run: bool,
    started: bool,
    reset: bool,
    finished: bool,
    shutdown: bool,
    underruns: u64,
    padded: u64,
    latency_ms: f64,
    error: Option<String>,
    /// Virtual device (tests): every frame it played, padding included.
    #[cfg(test)]
    tap: Vec<f32>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    rate: u32,
    device: String,
    capacity: usize,
    /// Device frames the clock is held back by (output latency compensation).
    compensation: u64,
}

/// Handle to one output (WASAPI device or a virtual test device).
pub struct AudioOutput {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    virtual_device: bool,
}

impl AudioOutput {
    fn with(rate: u32, device: String, virtual_device: bool, compensation_ms: u64) -> Self {
        let capacity = (u64::from(rate) * RING_MS / 1000) as usize * CHANNELS;
        let state = State {
            ring: VecDeque::with_capacity(capacity),
            ..State::default()
        };
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
                wake: Condvar::new(),
                rate,
                device,
                capacity,
                compensation: u64::from(rate) * compensation_ms.min(MAX_COMPENSATION_MS) / 1000,
            }),
            worker: None,
            virtual_device,
        }
    }

    /// A device whose consumption tests drive with [`AudioOutput::consume`].
    #[cfg(test)]
    pub fn virtual_device(rate: u32) -> Self {
        Self::with(rate, "Virtual test output".into(), true, 0)
    }

    /// A virtual device whose clock is compensated by `ms`.
    #[cfg(test)]
    pub fn virtual_device_compensated(rate: u32, ms: u64) -> Self {
        Self::with(rate, "Virtual test output".into(), true, ms)
    }

    /// Opens the default Windows render endpoint (shared mode, event driven,
    /// float stereo at the endpoint's mix rate). `Err` when there is none.
    #[cfg(windows)]
    pub fn open_default() -> Result<Self, String> {
        wasapi_thread::open()
    }
    #[cfg(not(windows))]
    pub fn open_default() -> Result<Self, String> {
        Err("Audio output is implemented for Windows only.".into())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub fn rate(&self) -> u32 {
        self.shared.rate
    }
    /// Free ring space in frames.
    pub fn space(&self) -> usize {
        let state = self.state();
        (self.shared.capacity - state.ring.len()) / CHANNELS
    }
    #[cfg(test)]
    pub fn buffered_frames(&self) -> usize {
        self.state().ring.len() / CHANNELS
    }
    /// Appends whole frames (interleaved stereo); returns frames accepted.
    pub fn push(&self, samples: &[f32]) -> usize {
        let mut state = self.state();
        let room = (self.shared.capacity - state.ring.len()) / CHANNELS;
        let frames = (samples.len() / CHANNELS).min(room);
        state.ring.extend(&samples[..frames * CHANNELS]);
        frames
    }
    /// Appends `frames` of real timeline silence (a gap between clips).
    pub fn push_silence(&self, frames: usize) -> usize {
        let mut state = self.state();
        let room = (self.shared.capacity - state.ring.len()) / CHANNELS;
        let frames = frames.min(room);
        state
            .ring
            .extend(std::iter::repeat_n(0.0, frames * CHANNELS));
        frames
    }
    /// Whether enough audio is queued to start without an immediate underrun.
    pub fn prefilled(&self) -> bool {
        let state = self.state();
        state.finished
            || state.ring.len() / CHANNELS
                >= (u64::from(self.shared.rate) * PREFILL_MS / 1000) as usize
    }
    /// Starts or resumes device playback.
    pub fn start(&self) {
        self.state().run = true;
        self.shared.wake.notify_all();
    }
    /// Pauses device playback; buffered audio and the position are kept.
    pub fn pause(&self) {
        let mut state = self.state();
        state.run = false;
        self.shared.wake.notify_all();
    }
    /// Flushes everything and restarts the device position at zero.
    pub fn reset(&self) {
        let mut state = self.state();
        state.ring.clear();
        state.map.clear();
        state.anchor = None;
        state.run = false;
        state.finished = false;
        state.underruns = 0;
        state.padded = 0;
        state.reset = true;
        #[cfg(test)]
        state.tap.clear();
        if self.virtual_device {
            state.reset = false;
            state.started = false;
        }
        self.shared.wake.notify_all();
    }
    /// No more timeline audio will come: pad silently, not as an underrun.
    pub fn finish(&self) {
        self.state().finished = true;
    }
    pub fn failed(&self) -> Option<String> {
        self.state().error.clone()
    }
    /// Timeline frames played (since the last reset) at `now`, or `None`
    /// before the device has reported a position.
    pub fn clock(&self, now: Instant) -> Option<u64> {
        let state = self.state();
        let (played, at) = state.anchor?;
        let mut device = played;
        if state.run && state.started {
            let elapsed = now.saturating_duration_since(at);
            let limit = u64::from(self.shared.rate) * MAX_EXTRAPOLATION_MS / 1000;
            let extra = (elapsed.as_micros() as u64).saturating_mul(u64::from(self.shared.rate))
                / 1_000_000;
            device += extra.min(limit);
        }
        Some(
            state
                .map
                .timeline_at(device.saturating_sub(self.shared.compensation)),
        )
    }
    pub fn status(&self) -> OutputStatus {
        let state = self.state();
        let rate = f64::from(self.shared.rate.max(1));
        let ms = |frames: u64| frames as f64 * 1000.0 / rate;
        OutputStatus {
            state: if state.error.is_some() {
                "failed"
            } else if state.run && state.started {
                "playing"
            } else if state.run {
                "prefill"
            } else if state.anchor.is_some() {
                "paused"
            } else {
                "idle"
            },
            device: self.shared.device.clone(),
            rate: self.shared.rate,
            stream_latency_ms: state.latency_ms,
            latency_compensation_ms: self.shared.compensation as f64 * 1000.0 / rate,
            buffered_ms: ms((state.ring.len() / CHANNELS) as u64),
            underruns: state.underruns,
            padded_ms: ms(state.padded),
            played_ms: ms(state.anchor.map_or(0, |(p, _)| state.map.timeline_at(p))),
            error: state.error.clone(),
        }
    }

    #[cfg(test)]
    /// Virtual device only: the device consumes `frames` now (real frames from
    /// the ring, padding for the rest) and reports them as played.
    pub fn consume(&self, frames: usize, now: Instant) {
        if self.state().run {
            self.consume_in_flight(frames, now);
        }
    }

    /// Virtual device (tests): plays `frames` even after a pause, as a real
    /// endpoint does with the period already handed to it.
    #[cfg(test)]
    pub fn consume_in_flight(&self, frames: usize, now: Instant) {
        assert!(
            self.virtual_device,
            "consume drives the virtual device only"
        );
        let mut state = self.state();
        state.started = true;
        #[cfg(test)]
        {
            let real = (state.ring.len() / CHANNELS).min(frames);
            let played: Vec<f32> = state.ring.iter().take(real * CHANNELS).copied().collect();
            state.tap.extend(played);
            state
                .tap
                .extend(std::iter::repeat_n(0.0, (frames - real) * CHANNELS));
        }
        take(&mut state, frames, None);
        let played = state.map.device_written();
        state.anchor = Some((played, now));
    }

    /// Virtual device (tests): everything it has played since the last call.
    #[cfg(test)]
    pub fn take_tap(&self) -> Vec<f32> {
        std::mem::take(&mut self.state().tap)
    }

    pub fn shutdown(&mut self) {
        {
            let mut state = self.state();
            state.shutdown = true;
            state.run = false;
        }
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Moves up to `frames` from the ring into `out` (when given), padding the
/// rest with silence; records the segment map and underruns.
fn take(state: &mut State, frames: usize, out: Option<&mut Vec<u8>>) {
    let real = (state.ring.len() / CHANNELS).min(frames);
    let pad = frames - real;
    if let Some(out) = out {
        out.clear();
        for sample in state.ring.drain(..real * CHANNELS) {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out.resize(frames * CHANNELS * 4, 0);
    } else {
        state.ring.drain(..real * CHANNELS);
    }
    state.map.record(real as u64, true);
    state.map.record(pad as u64, false);
    if pad > 0 {
        state.padded += pad as u64;
        if !state.finished {
            state.underruns += 1;
        }
    }
}

#[cfg(windows)]
mod wasapi_thread {
    use super::*;
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

    pub(super) fn open() -> Result<AudioOutput, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("cutcap-audio-output".into())
            .spawn(move || render(tx))
            .map_err(|e| format!("Start audio output thread: {e}"))?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((output, start))) => {
                let mut output: AudioOutput = output;
                let _ = start.send(());
                output.worker = Some(worker);
                Ok(output)
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => Err("The audio device did not respond.".into()),
        }
    }

    type Opened = Result<(AudioOutput, std::sync::mpsc::Sender<()>), String>;

    fn render(tx: std::sync::mpsc::Sender<Opened>) {
        let fail = |e: wasapi::WasapiError| format!("Audio output: {e}");
        let setup = (|| {
            initialize_mta()
                .ok()
                .map_err(|e| format!("Audio output COM: {e}"))?;
            let device = DeviceEnumerator::new()
                .map_err(fail)?
                .get_default_device(Direction::Render)
                .map_err(|_| "No audio output device is available.".to_owned())?;
            let name = device
                .get_friendlyname()
                .unwrap_or_else(|_| "Default output".into());
            let mut client = device.get_iaudioclient().map_err(fail)?;
            let rate = client.get_mixformat().map_err(fail)?.get_samplespersec();
            let format = WaveFormat::new(32, 32, SampleType::Float, rate as usize, CHANNELS, None);
            let (period, _) = client.get_device_period().map_err(fail)?;
            client
                .initialize_client(
                    &format,
                    Direction::Render,
                    StreamMode::EventsShared {
                        autoconvert: true,
                        buffer_duration_hns: period * 2,
                    },
                    None,
                )
                .map_err(fail)?;
            let event = client.set_get_eventhandle().map_err(fail)?;
            let render = client.get_audiorenderclient().map_err(fail)?;
            let clock = client.get_audioclock().map_err(fail)?;
            let frequency = clock.get_frequency().map_err(fail)?.max(1);
            let latency_ms = client
                .get_stream_latency()
                .map(|hns| hns as f64 / 10_000.0)
                .unwrap_or(0.0);
            Ok((
                name, rate, client, event, render, clock, frequency, latency_ms,
            ))
        })();
        let (name, rate, client, event, render, clock, frequency, latency_ms) = match setup {
            Ok(parts) => parts,
            Err(error) => {
                let _ = tx.send(Err(error));
                return;
            }
        };
        let compensation =
            compensation_ms_from(std::env::var("CUTCAP_AUDIO_LATENCY_MS").ok().as_deref());
        let output = AudioOutput::with(rate, name, false, compensation);
        output.state().latency_ms = latency_ms;
        let shared = output.shared.clone();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        if tx.send(Ok((output, start_tx))).is_err() || start_rx.recv().is_err() {
            return;
        }
        let mut bytes: Vec<u8> = Vec::with_capacity(rate as usize * CHANNELS * 4);
        let lock = || shared.state.lock().unwrap_or_else(|p| p.into_inner());
        let result: Result<(), String> = (|| {
            loop {
                let mut state = lock();
                if state.shutdown {
                    let _ = client.stop_stream();
                    return Ok(());
                }
                if state.reset {
                    state.reset = false;
                    if state.started {
                        client.stop_stream().map_err(fail)?;
                        state.started = false;
                    }
                    client.reset_stream().map_err(fail)?;
                }
                if !state.run {
                    if state.started {
                        client.stop_stream().map_err(fail)?;
                        state.started = false;
                    }
                    let _unused = shared
                        .wake
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap_or_else(|p| p.into_inner());
                    continue;
                }
                if !state.started {
                    // Fill the device buffer before starting so playback begins with audio.
                    // The transport starts only after prefill; resume needs any audio.
                    if !state.finished && state.ring.is_empty() {
                        let _unused = shared
                            .wake
                            .wait_timeout(state, Duration::from_millis(5))
                            .unwrap_or_else(|p| p.into_inner());
                        continue;
                    }
                    let space = client.get_available_space_in_frames().map_err(fail)? as usize;
                    take(&mut state, space, Some(&mut bytes));
                    drop(state);
                    render.write_to_device(&bytes, None).map_err(fail)?;
                    client.start_stream().map_err(fail)?;
                    let mut state = lock();
                    state.started = true;
                    continue;
                }
                drop(state);
                if event.wait_for_event(200).is_err() {
                    // Shared-mode events stop when the stream is stopped; re-check state.
                    continue;
                }
                let space = client.get_available_space_in_frames().map_err(fail)? as usize;
                let mut state = lock();
                if !state.started || state.reset {
                    continue;
                }
                take(&mut state, space, Some(&mut bytes));
                drop(state);
                render.write_to_device(&bytes, None).map_err(fail)?;
                let (position, _qpc) = clock.get_position().map_err(fail)?;
                let now = Instant::now();
                let played =
                    (u128::from(position) * u128::from(rate) / u128::from(frequency)) as u64;
                let mut state = lock();
                if !state.reset {
                    state.anchor = Some((played, now));
                    state.map.prune(played);
                }
            }
        })();
        if let Err(error) = result {
            let mut state = lock();
            state.error = Some(error);
            state.run = false;
            state.started = false;
            let _ = client.stop_stream();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the default render endpoint by WASAPI loopback (mono channel 0).
    #[cfg(windows)]
    pub(crate) fn loopback(seconds: f64) -> std::thread::JoinHandle<(u32, Vec<f32>, Instant)> {
        std::thread::spawn(move || {
            use wasapi::*;
            initialize_mta().ok().unwrap();
            let device = DeviceEnumerator::new()
                .unwrap()
                .get_default_device(Direction::Render)
                .unwrap();
            let mut client = device.get_iaudioclient().unwrap();
            let format = client.get_mixformat().unwrap();
            let (rate, channels) = (format.get_samplespersec(), format.get_nchannels() as usize);
            let float = WaveFormat::new(32, 32, SampleType::Float, rate as usize, channels, None);
            client
                .initialize_client(
                    &float,
                    Direction::Capture,
                    StreamMode::EventsShared {
                        autoconvert: true,
                        buffer_duration_hns: 200_000,
                    },
                    None,
                )
                .unwrap();
            let event = client.set_get_eventhandle().unwrap();
            let capture = client.get_audiocaptureclient().unwrap();
            let mut bytes = std::collections::VecDeque::new();
            client.start_stream().unwrap();
            let started = Instant::now();
            while started.elapsed().as_secs_f64() < seconds {
                let _ = event.wait_for_event(100);
                capture.read_from_device_to_deque(&mut bytes).unwrap();
            }
            client.stop_stream().unwrap();
            let bytes: Vec<u8> = bytes.into_iter().collect();
            let samples = bytes
                .chunks_exact(4 * channels)
                .map(|f| f32::from_le_bytes([f[0], f[1], f[2], f[3]]))
                .collect();
            (rate, samples, started)
        })
    }

    /// Goertzel power of `frequency` relative to total power.
    pub(crate) fn tone_ratio(samples: &[f32], rate: u32, frequency: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * frequency / f64::from(rate);
        let (mut s1, mut s2, mut total) = (0.0f64, 0.0f64, 0.0f64);
        for &x in samples {
            let s0 = f64::from(x) + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s0;
            total += f64::from(x) * f64::from(x);
        }
        let power = s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2;
        if total == 0.0 {
            0.0
        } else {
            power / (total * samples.len() as f64 / 2.0)
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "plays a quiet 1 s tone on the default Windows output and records it by loopback"]
    fn real_device_plays_audio_heard_by_loopback_and_the_clock_follows_the_device() {
        let recording = loopback(2.5);
        std::thread::sleep(Duration::from_millis(300));
        let output = AudioOutput::open_default().expect("an audio output endpoint");
        let rate = output.rate();
        let total = rate as usize; // one second
        let tone: Vec<f32> = (0..total)
            .flat_map(|n| {
                let v = (0.05
                    * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / f64::from(rate)).sin())
                    as f32;
                [v, v]
            })
            .collect();
        let mut sent = output.push(&tone);
        assert!(output.prefilled());
        let started = Instant::now();
        output.start();
        let mut samples = Vec::new();
        while started.elapsed() < Duration::from_millis(1600) {
            if sent < total {
                sent += output.push(&tone[sent * 2..]);
                if sent == total {
                    output.finish();
                }
            }
            if let Some(frames) = output.clock(Instant::now()) {
                samples.push((started.elapsed().as_secs_f64(), frames));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = output.status();
        drop(output);
        let (loop_rate, captured, _) = recording.join().unwrap();
        // The clock is monotonic, reaches the whole second and never runs ahead of wall time.
        assert!(
            samples.windows(2).all(|w| w[1].1 >= w[0].1),
            "monotonic clock"
        );
        let last = samples.last().unwrap().1;
        assert_eq!(
            last, total as u64,
            "all real frames played, then padding freezes it"
        );
        let lead = samples
            .iter()
            .map(|(t, f)| *f as f64 / f64::from(rate) - t)
            .fold(f64::MIN, f64::max);
        assert!(
            lead < 0.01,
            "the audio clock never leads wall time ({lead:.4} s)"
        );
        // The tone was actually rendered by the endpoint.
        // Analyse only the span that carries sound: a pure tone gives ~1.0.
        let first = captured
            .iter()
            .position(|x| x.abs() > 0.005)
            .expect("sound was captured");
        let last = captured.iter().rposition(|x| x.abs() > 0.005).unwrap();
        let ratio = tone_ratio(&captured[first..=last], loop_rate, 440.0);
        let heard_ms = (last - first) as f64 * 1000.0 / f64::from(loop_rate);
        assert!(
            (900.0..1100.0).contains(&heard_ms),
            "about one second of tone was heard ({heard_ms:.0} ms)"
        );
        println!(
            "device '{}' rate {rate} latency {:.1} ms underruns {} padded {:.1} ms; loopback heard {heard_ms:.0} ms of tone @ {loop_rate}, 440 Hz ratio {ratio:.3}",
            status.device, status.stream_latency_ms, status.underruns, status.padded_ms
        );
        assert!(
            ratio > 0.5,
            "the loopback recording contains the 440 Hz tone (ratio {ratio:.3})"
        );
        assert_eq!(
            status.underruns, 0,
            "no starvation while the ring had audio"
        );
    }

    #[test]
    fn latency_compensation_holds_the_clock_back_and_parses_the_setting() {
        assert_eq!(compensation_ms_from(None), 0);
        assert_eq!(compensation_ms_from(Some(" 67 ")), 67);
        assert_eq!(compensation_ms_from(Some("-5")), 0);
        assert_eq!(compensation_ms_from(Some("9000")), MAX_COMPENSATION_MS);
        let plain = AudioOutput::virtual_device(48_000);
        let held = AudioOutput::virtual_device_compensated(48_000, 50);
        for output in [&plain, &held] {
            output.push(&vec![0.25; 48_000 * CHANNELS]);
            output.start();
        }
        let now = Instant::now();
        plain.consume(9_600, now);
        held.consume(9_600, now);
        assert_eq!(plain.clock(now), Some(9_600));
        assert_eq!(held.clock(now), Some(9_600 - 2_400), "50 ms at 48 kHz");
        assert_eq!(held.status().latency_compensation_ms, 50.0);
        let early = AudioOutput::virtual_device_compensated(48_000, 50);
        early.push(&vec![0.25; 4_800 * CHANNELS]);
        early.start();
        early.consume(960, now);
        assert_eq!(early.clock(now), Some(0), "never before the first frame");
    }

    #[test]
    fn clock_map_advances_only_through_real_frames_and_clamps_to_written() {
        let mut map = ClockMap::default();
        assert_eq!(map.timeline_at(10), 0);
        map.record(100, true);
        map.record(30, false); // the output had to pad: timeline frozen
        map.record(50, true);
        assert_eq!((map.device_written(), map.timeline_written()), (180, 150));
        assert_eq!(map.timeline_at(0), 0);
        assert_eq!(map.timeline_at(99), 99);
        assert_eq!(map.timeline_at(100), 100);
        assert_eq!(
            map.timeline_at(120),
            100,
            "padding does not advance the timeline"
        );
        assert_eq!(map.timeline_at(130), 100);
        assert_eq!(map.timeline_at(140), 110);
        assert_eq!(map.timeline_at(10_000), 150, "never past what was written");
        map.prune(140);
        assert_eq!(
            map.timeline_at(140),
            110,
            "pruning keeps the current segment"
        );
        assert_eq!(map.timeline_at(179), 149);
    }

    #[test]
    fn virtual_output_reports_played_timeline_frames_and_counts_underruns() {
        let output = AudioOutput::virtual_device(48_000);
        let now = Instant::now();
        assert_eq!(
            output.clock(now),
            None,
            "no position before the device plays"
        );
        assert_eq!(output.push(&vec![0.5; 2 * 1000]), 1000);
        assert!(
            !output.prefilled(),
            "120 ms = 5760 frames are needed to start"
        );
        output.push_silence(5000);
        assert!(output.prefilled());
        output.start();
        output.consume(4800, now);
        assert_eq!(output.clock(now), Some(4800));
        output.consume(2000, now); // 1200 real + 800 padded
        assert_eq!(output.clock(now), Some(6000));
        let status = output.status();
        assert_eq!(status.underruns, 1);
        assert!((status.padded_ms - 800.0 / 48.0).abs() < 1e-9);
        output.finish();
        output.consume(480, now);
        assert_eq!(
            output.status().underruns,
            1,
            "padding after the end is not an underrun"
        );
        output.pause();
        output.consume(480, now);
        assert_eq!(
            output.clock(now),
            Some(6000),
            "paused devices consume nothing"
        );
        output.reset();
        assert_eq!(output.clock(now), None);
        assert_eq!(output.buffered_frames(), 0);
        // The ring is bounded (400 ms).
        let pushed = output.push(&vec![0.0; 2 * 48_000]);
        assert_eq!(pushed, 19_200);
        assert_eq!(output.space(), 0);
    }
}

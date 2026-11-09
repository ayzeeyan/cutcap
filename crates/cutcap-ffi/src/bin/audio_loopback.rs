//! Native verification tool (feature `audio-diagnostics`): records the default
//! Windows output by WASAPI loopback and reports what was actually rendered.
//!
//! `cutcap-audio-loopback <out.wav> <seconds> [click_hz]` writes a 16-bit WAV
//! of the first channel and prints one JSON line: the device rate, the RMS
//! level, and the wall-clock time (Unix ms) of every detected tone burst
//! ("click") at `click_hz` (default 2000). Each capture packet carries the
//! performance-counter time of its first frame; the counter is mapped to Unix
//! time by bracketing an `IAudioClock` reading with wall-clock reads (the
//! bracket width is reported), so a click is dated to about a millisecond. The
//! older read-time envelope estimate is reported for comparison. Loopback taps
//! the endpoint mix: speaker/DAC latency after the mix is not included.
//!
//! `--tones 400,800,1200` adds the amplitude of each frequency per 10 ms
//! window (Goertzel over whole cycles when the frequency is a multiple of
//! 100 Hz, so the tones do not leak into each other).
#[cfg(windows)]
fn main() {
    use std::{
        collections::VecDeque,
        io::Write,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use wasapi::*;

    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .expect("usage: cutcap-audio-loopback <out.wav> <seconds> [click_hz]");
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let click_hz: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2000.0);
    let tones: Vec<u32> = args
        .iter()
        .position(|a| a == "--tones")
        .and_then(|i| args.get(i + 1))
        .map(|list| list.split(',').filter_map(|t| t.parse().ok()).collect())
        .unwrap_or_default();

    initialize_mta().ok().expect("COM");
    let device = DeviceEnumerator::new()
        .and_then(|e| e.get_default_device(Direction::Render))
        .expect("an audio output endpoint");
    let name = device.get_friendlyname().unwrap_or_default();
    let mut client = device.get_iaudioclient().expect("audio client");
    let mix = client.get_mixformat().expect("mix format");
    let (rate, channels) = (mix.get_samplespersec(), mix.get_nchannels() as usize);
    let float = WaveFormat::new(32, 32, SampleType::Float, rate as usize, channels, None);
    client
        .initialize_client(
            &float,
            Direction::Capture,
            StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 100_000,
            },
            None,
        )
        .expect("loopback client");
    let event = client.set_get_eventhandle().expect("event");
    let capture = client.get_audiocaptureclient().expect("capture client");
    // The stream clock reports a performance-counter reading (100 ns units) with
    // each position: bracketing it with wall-clock reads maps the counter to Unix
    // time, and every capture packet carries the counter time of its first frame.
    let clock = client.get_audioclock().expect("audio clock");
    let wall = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    };
    let mut best_map = (f64::INFINITY, 0.0f64); // (bracket width ms, unix_ms - qpc_ms)
    let sync = |best: &mut (f64, f64)| {
        let before = wall();
        if let Ok((_, qpc)) = clock.get_position() {
            let after = wall();
            if after - before < best.0 {
                *best = (
                    after - before,
                    (before + after) / 2.0 - qpc as f64 / 10_000.0,
                );
            }
        }
    };
    let mut bytes = VecDeque::new();
    let mut mono: Vec<f32> = Vec::new();
    let mut offsets_ms: Vec<f64> = Vec::new();
    // (first mono frame of the packet, packet counter time in ms)
    let mut packets: Vec<(usize, f64)> = Vec::new();
    client.start_stream().expect("start loopback");
    let started = Instant::now();
    let frame_bytes = 4 * channels;
    while started.elapsed() < Duration::from_secs_f64(seconds) {
        let _ = event.wait_for_event(20);
        sync(&mut best_map);
        while capture.get_next_packet_size().ok().flatten().unwrap_or(0) > 0 {
            let first = mono.len() + bytes.len() / frame_bytes;
            let info = capture
                .read_from_device_to_deque(&mut bytes)
                .expect("read loopback");
            if info.timestamp != 0 && !info.flags.timestamp_error {
                packets.push((first, info.timestamp as f64 / 10_000.0));
            }
        }
        let now = wall();
        while bytes.len() >= frame_bytes {
            let frame: Vec<u8> = bytes.drain(..frame_bytes).collect();
            mono.push(f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]));
        }
        // Frame `n` was rendered no later than `now`: offset = now - n / rate.
        offsets_ms.push(now - mono.len() as f64 * 1000.0 / f64::from(rate));
    }
    client.stop_stream().ok();
    // Lower envelope: the earliest bound is the best estimate of frame 0's time.
    let envelope_origin = offsets_ms.iter().copied().fold(f64::INFINITY, f64::min);
    // Counter-dated time of a mono frame: from the last packet starting at or before it.
    let dated = |frame: usize| -> Option<f64> {
        let at = packets.partition_point(|(first, _)| *first <= frame);
        let (first, qpc_ms) = packets.get(at.checked_sub(1)?)?;
        Some(qpc_ms + best_map.1 + (frame - first) as f64 * 1000.0 / f64::from(rate))
    };
    let origin = dated(0).unwrap_or(envelope_origin);

    // Tone-burst onsets: Goertzel power at `click_hz` over 2 ms windows.
    let window = (f64::from(rate) * 0.002) as usize;
    let w = 2.0 * std::f64::consts::PI * click_hz / f64::from(rate);
    let mut clicks = Vec::new();
    let mut last_click = -1.0e9;
    for (index, block) in mono.chunks(window).enumerate() {
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in block {
            let s0 = f64::from(x) + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt() * 2.0 / block.len() as f64;
        let t = (index * window) as f64 * 1000.0 / f64::from(rate);
        // Calibrated on recorded playback: clicks reach ~0.8, the tone stays below 0.08.
        if power > 0.3 && t - last_click > 500.0 {
            clicks.push(dated(index * window).unwrap_or(envelope_origin + t));
            last_click = t;
        }
    }
    let rms =
        (mono.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / mono.len().max(1) as f64).sqrt();

    // Tone amplitudes per 10 ms window, dated like the clicks.
    let tone_window = (f64::from(rate) * 0.010) as usize;
    let level = |hz: u32| -> Vec<String> {
        let w = 2.0 * std::f64::consts::PI * f64::from(hz) / f64::from(rate);
        mono.chunks_exact(tone_window)
            .map(|block| {
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for &x in block {
                    let s0 = f64::from(x) + 2.0 * w.cos() * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                let power = (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2)
                    .max(0.0)
                    .sqrt();
                format!("{:.5}", power * 2.0 / block.len() as f64)
            })
            .collect()
    };
    let tone_json = tones
        .iter()
        .map(|hz| format!("\"{hz}\":[{}]", level(*hz).join(",")))
        .collect::<Vec<_>>()
        .join(",");
    let tone_times = (0..mono.len() / tone_window.max(1))
        .step_by(1)
        .map(|i| {
            format!(
                "{:.1}",
                dated(i * tone_window).unwrap_or(envelope_origin + i as f64 * 10.0)
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    // 16-bit mono WAV of the first channel.
    let mut file = std::fs::File::create(out).expect("create wav");
    let data = (mono.len() * 2) as u32;
    let mut header = Vec::new();
    header.extend(b"RIFF");
    header.extend((36 + data).to_le_bytes());
    header.extend(b"WAVEfmt ");
    header.extend(16u32.to_le_bytes());
    header.extend(1u16.to_le_bytes());
    header.extend(1u16.to_le_bytes());
    header.extend(rate.to_le_bytes());
    header.extend((rate * 2).to_le_bytes());
    header.extend(2u16.to_le_bytes());
    header.extend(16u16.to_le_bytes());
    header.extend(b"data");
    header.extend(data.to_le_bytes());
    file.write_all(&header).expect("write wav");
    let samples: Vec<u8> = mono
        .iter()
        .flat_map(|x| ((x.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    file.write_all(&samples).expect("write wav");
    println!(
        "{{\"device\":{:?},\"rate\":{rate},\"frames\":{},\"rms\":{rms:.5},\"origin_unix_ms\":{origin:.3},\"envelope_origin_unix_ms\":{envelope_origin:.3},\"packets\":{},\"clock_map_bracket_ms\":{:.3},\"clicks_unix_ms\":[{}],\"tone_times_unix_ms\":[{}],\"tones\":{{{}}}}}",
        name,
        mono.len(),
        packets.len(),
        best_map.0,
        clicks
            .iter()
            .map(|c| format!("{c:.3}"))
            .collect::<Vec<_>>()
            .join(","),
        if tones.is_empty() {
            String::new()
        } else {
            tone_times
        },
        tone_json,
    );
}

#[cfg(not(windows))]
fn main() {
    eprintln!("cutcap-audio-loopback requires Windows.");
}

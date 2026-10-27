//! Microphone to speaker, with the capture and playback buffer delays printed
//! once a second.
//!
//! ```sh
//! cargo run -p ss-audio --example loopback -- 300
//! ```
//!
//! The argument is the number of seconds. The default is 5. A five-minute run
//! is the manual glitch check for T-010. The printed delays are device-buffer
//! estimates from `Stream::now()`, not an acoustic round trip.

use std::{
    env,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use rtrb::Producer;
use ss_audio::device::{DeviceManager, GraphConfig};

fn main() -> ExitCode {
    let seconds = env::args()
        .nth(1)
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(5);
    let manager = DeviceManager::new();
    let config = GraphConfig::MONO;
    let (capture, mut consumer) = match manager.open_capture(None, config) {
        Ok(open) => open,
        Err(error) => {
            eprintln!("capture failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let (playback, mut producer) = match manager.open_playback(None, config) {
        Ok(open) => open,
        Err(error) => {
            eprintln!("playback failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "loopback for {seconds}s, capture {} at {} Hz, playback {} at {} Hz",
        capture.device_id(),
        capture.device_rate(),
        playback.device_id(),
        playback.device_rate()
    );
    let frame = config.samples_per_frame();
    let started = Instant::now();
    let mut reported = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        if capture.is_lost() || playback.is_lost() {
            eprintln!("device lost; reopen with DeviceManager::open_capture / open_playback");
            return ExitCode::FAILURE;
        }
        match consumer.read_chunk(frame) {
            Ok(chunk) => {
                let (first, second) = chunk.as_slices();
                if !write_frame(&mut producer, first, second) {
                    eprintln!("playback ring overran");
                }
                chunk.commit_all();
            }
            Err(_) => thread::sleep(Duration::from_millis(2)),
        }
        if reported.elapsed() >= Duration::from_secs(1) {
            println!(
                "capture delay {:?}, playback delay {:?}, callbacks {}, underruns {}, overruns {}",
                capture.capture_delay(),
                playback.playback_delay(),
                capture.counters().callbacks() + playback.counters().callbacks(),
                playback.counters().underruns(),
                capture.counters().overruns(),
            );
            reported = Instant::now();
        }
    }
    ExitCode::SUCCESS
}

fn write_frame(producer: &mut Producer<f32>, first: &[f32], second: &[f32]) -> bool {
    for sample in first.iter().chain(second) {
        if producer.push(*sample).is_err() {
            return false;
        }
    }
    true
}

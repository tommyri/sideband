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
//!
//! Both streams follow the host default. A lost device, or a new default
//! (AirPods connecting, for example), drops that stream and opens it again
//! without restarting, with the same retry policy as a voice call
//! ([`ss_audio::device::Recovery`]). Each reopen is printed. Use headphones:
//! with built-in speakers and microphone this loop can feed back.

use std::{
    env,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use rtrb::{Consumer, Producer};
use ss_audio::device::{
    CaptureStream, DeviceEvent, DeviceId, DeviceManager, Direction, Error, GraphConfig,
    PlaybackStream, Recovery,
};

struct Side<S, R> {
    open: Option<(S, R)>,
    recovery: Recovery,
    reopens: u32,
}

impl<S: Opened, R> Side<S, R> {
    fn new(direction: Direction, open: (S, R)) -> Self {
        let now = Instant::now();
        let mut recovery = Recovery::new(direction, None, now);
        recovery.opened(open.0.device(), now);
        Self {
            open: Some(open),
            recovery,
            reopens: 0,
        }
    }

    /// The host error callback flagged the stream before the poll did.
    fn check_lost(&mut self) {
        if self.open.as_ref().is_some_and(|(stream, _)| stream.lost()) {
            self.open = None;
            self.recovery.closed(Instant::now());
        }
    }
}

fn main() -> ExitCode {
    let seconds = env::args()
        .nth(1)
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(5);
    let manager = DeviceManager::new();
    let config = GraphConfig::MONO;
    let mut capture = match manager.open_capture(None, config) {
        Ok(open) => Side::new(Direction::Capture, open),
        Err(error) => {
            eprintln!("capture failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut playback = match manager.open_playback(None, config) {
        Ok(open) => Side::new(Direction::Playback, open),
        Err(error) => {
            eprintln!("playback failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let (Some((input, _)), Some((output, _))) = (&capture.open, &playback.open) {
        println!(
            "loopback for {seconds}s, capture {} at {} Hz, playback {} at {} Hz",
            input.device_id(),
            input.device_rate(),
            output.device_id(),
            output.device_rate()
        );
    }
    let frame = config.samples_per_frame();
    let started = Instant::now();
    let mut reported = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        for event in manager.poll() {
            mark_reopens(&event, &mut capture, &mut playback);
        }
        capture.check_lost();
        playback.check_lost();
        if let Err(error) = reopen(&mut capture, "capture", |id| {
            manager.open_capture(id, config)
        }) {
            eprintln!("capture failed: {error}");
            return ExitCode::FAILURE;
        }
        if let Err(error) = reopen(&mut playback, "playback", |id| {
            manager.open_playback(id, config)
        }) {
            eprintln!("playback failed: {error}");
            return ExitCode::FAILURE;
        }
        if !forward(&mut capture, &mut playback, frame) {
            thread::sleep(Duration::from_millis(2));
        }
        if reported.elapsed() >= Duration::from_secs(1) {
            report(&capture, &playback);
            reported = Instant::now();
        }
    }
    println!(
        "done: capture reopened {} times, playback reopened {} times",
        capture.reopens, playback.reopens
    );
    ExitCode::SUCCESS
}

fn mark_reopens(
    event: &DeviceEvent,
    capture: &mut Side<CaptureStream, Consumer<f32>>,
    playback: &mut Side<PlaybackStream, Producer<f32>>,
) {
    println!("device event: {event:?}");
    if let DeviceEvent::SilentInput { access, .. } = event {
        println!(
            "capture is all zeros (microphone access {access:?}); check System Settings > \
             Privacy & Security > Microphone"
        );
    }
    let now = Instant::now();
    if capture.recovery.observe(event, now) {
        capture.open = None;
    }
    if playback.recovery.observe(event, now) {
        playback.open = None;
    }
}

/// Open a side on the current default when [`Recovery`] says it is due. The
/// old stream, if any, is dropped only once the new one is open. A missing
/// device is retried; a refused permission ends the example.
fn reopen<S, R>(
    side: &mut Side<S, R>,
    label: &str,
    open: impl FnOnce(Option<&DeviceId>) -> Result<(S, R), Error>,
) -> Result<(), Error>
where
    S: Opened,
{
    let Some(attempt) = side.recovery.attempt(Instant::now()) else {
        return Ok(());
    };
    let began = Instant::now();
    match open(attempt.device.as_ref()) {
        Ok(opened) => {
            println!(
                "reopened {label} on {} at {} Hz in {:?}",
                opened.0.device(),
                opened.0.rate(),
                began.elapsed()
            );
            side.recovery.opened(opened.0.device(), Instant::now());
            side.open = Some(opened);
            side.reopens += 1;
            Ok(())
        }
        Err(Error::PermissionDenied) => Err(Error::PermissionDenied),
        Err(error) => {
            println!("{label} did not reopen yet: {error}");
            side.recovery.failed(Instant::now());
            Ok(())
        }
    }
}

trait Opened {
    fn device(&self) -> DeviceId;
    fn rate(&self) -> u32;
    fn lost(&self) -> bool;
}

impl Opened for CaptureStream {
    fn device(&self) -> DeviceId {
        self.device_id().clone()
    }
    fn rate(&self) -> u32 {
        self.device_rate()
    }
    fn lost(&self) -> bool {
        self.is_lost()
    }
}

impl Opened for PlaybackStream {
    fn device(&self) -> DeviceId {
        self.device_id().clone()
    }
    fn rate(&self) -> u32 {
        self.device_rate()
    }
    fn lost(&self) -> bool {
        self.is_lost()
    }
}

/// Move one 10 ms frame from capture to playback. `false` when none was ready.
fn forward(
    capture: &mut Side<CaptureStream, Consumer<f32>>,
    playback: &mut Side<PlaybackStream, Producer<f32>>,
    frame: usize,
) -> bool {
    let Some((_, consumer)) = capture.open.as_mut() else {
        return false;
    };
    let Ok(chunk) = consumer.read_chunk(frame) else {
        return false;
    };
    if let Some((_, producer)) = playback.open.as_mut() {
        let (first, second) = chunk.as_slices();
        if !write_frame(producer, first, second) {
            eprintln!("playback ring overran");
        }
    }
    chunk.commit_all();
    true
}

fn report(
    capture: &Side<CaptureStream, Consumer<f32>>,
    playback: &Side<PlaybackStream, Producer<f32>>,
) {
    let input = capture.open.as_ref().map(|(stream, _)| stream);
    let output = playback.open.as_ref().map(|(stream, _)| stream);
    println!(
        "capture delay {:?}, playback delay {:?}, callbacks {}, underruns {}, overruns {}",
        input.and_then(CaptureStream::capture_delay),
        output.and_then(PlaybackStream::playback_delay),
        input.map_or(0, |stream| stream.counters().callbacks())
            + output.map_or(0, |stream| stream.counters().callbacks()),
        output.map_or(0, |stream| stream.counters().underruns()),
        input.map_or(0, |stream| stream.counters().overruns()),
    );
}

fn write_frame(producer: &mut Producer<f32>, first: &[f32], second: &[f32]) -> bool {
    for sample in first.iter().chain(second) {
        if producer.push(*sample).is_err() {
            return false;
        }
    }
    true
}

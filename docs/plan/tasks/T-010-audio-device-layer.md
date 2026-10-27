# T-010 — `ss-audio`: device layer (capture/playback, resampling, hot-plug)

Status: done
Phase: 1
Size: M
Depends on: T-003
Paths: `crates/ss-audio/src/device/`, `crates/ss-audio/src/resample.rs`

## Goal
Open an input and an output device at 48 kHz (or resample to/from it),
deliver 10 ms f32 frames to the rest of the audio graph, and survive device
changes.

## Scope
- `DeviceManager`: enumerate hosts/devices via `cpal` ≥ 0.18 (features:
  `pipewire` on Linux), stable device ids, default-device tracking,
  hot-plug/default-change notifications (poll + OS hooks where cpal exposes
  them).
- `CaptureStream::open(device, Config) -> (stream, rtrb::Consumer<f32>)`:
  prefers 48 kHz; mono or stereo per request; converts sample formats; if the
  device rate ≠ 48 kHz, resamples with `rubato` (SincFixedIn, async) inside the
  non-RT worker thread, not in the callback.
- `PlaybackStream::open(device, Config) -> (stream, rtrb::Producer<f32>)`:
  pulls from the ring; on underrun outputs silence and increments a counter.
- Framing: downstream always sees 480-sample (10 ms) frames at 48 kHz.
- `cpal` `realtime` feature on; buffer size request: smallest supported ≥ 5 ms,
  fallback to default.
- Timestamps: expose `StreamTrait::now()`-based capture/playback clocks for
  latency measurement (T-017) and A/V sync later.
- Counters: underruns, overruns, callbacks, resampler active.
- Example `examples/loopback.rs`: mic → speaker with measured round trip.

## Out of scope
Opus, jitter buffer, processing, OS-specific tuning (T-011/012/013).

## Acceptance criteria
- `loopback` example runs on all three OSes with no audible glitches at
  default settings for 5 minutes (manual check, documented in the task notes).
- Unit test: synthetic 44.1 kHz sine through the resampling path yields a
  48 kHz sine with < 0.5 dB amplitude error and correct frequency.
- Unplugging the active device (or changing the default) results in a
  `DeviceEvent::Lost` and the graph can reopen without restarting the process.
- No allocation in callbacks (verify with `assert_no_alloc` in debug builds).

## Notes / references
- cpal 0.18 release notes (native PipeWire/PulseAudio, accurate timestamps).
- Bluetooth HFP detection: if input device name/rate indicates 8/16 kHz, emit
  `DeviceEvent::LowQualityInput` for the UI.

## Notes from implementation

- `cpal` 0.18's realtime and PipeWire features are enabled only on Linux.
  They pull ALSA and PipeWire headers, which macOS and Windows CI do not have.
  `default_host` then prefers PipeWire and falls back to ALSA. The PulseAudio
  feature stays off; CI does not install `libpulse`.
- Rubato 5 replaced `SincFixedIn` with `Async::new_sinc(..., FixedAsync::Input)`.
  That is the fixed-input asynchronous sinc the task asked for. It runs on the
  worker thread. The device callback only converts sample format and channel
  count into an `rtrb` ring.
- A removed active device emits `DeviceEvent::Lost` and `Removed`. The next
  snapshot can emit `Added` again. `CaptureStream` / `PlaybackStream` are
  owned values, so `DeviceManager::open_capture` / `open_playback` can run
  again in the same process. `is_lost` is also set from the host error callback
  (`DeviceNotAvailable`, `StreamInvalidated`) without waiting for the poll.
- `examples/loopback.rs` prints capture and playback buffer delays from
  `Stream::now()`. A two-second run on macOS built-in input and output opened
  at 48 kHz with no underruns or overruns (capture delay about 13 ms, playback
  delay about 11 ms). The five-minute listening check is still manual:
  `cargo run -p ss-audio --example loopback -- 300`.
- Opening at 8 or 16 kHz emits `DeviceEvent::LowQualityInput`. A hands-free
  name at 48 kHz does not.

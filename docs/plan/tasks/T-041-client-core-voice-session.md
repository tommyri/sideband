# T-041 — `ss-client-core`: voice session orchestration

Status: done
Phase: 3
Size: M
Depends on: T-040, T-010, T-015, T-025
Paths: `crates/ss-client-core/src/voice/`

## Goal
Joining a voice channel builds the whole audio graph, wires it to the
connection's datagrams, and tears it down cleanly; the UI only flips switches.

## Scope
- `VoiceSession::start(client, channel_id, AudioConfig)`: open devices
  (T-010), create encoder for the channel's profile (T-014), `TrackReceiver`s
  on `TrackAdded`, mixer → playback; datagram send/receive tasks.
- Gating: `TalkGate` with modes `PushToTalk`, `VoiceActivation`, `OpenMic`;
  external `set_ptt(bool)` input (from `ss-input`), VAD from T-052; TALK_START/
  TALK_END flags; configurable release delay (default 150 ms).
- Self mute/deafen: mute stops sending (and tells server), deafen stops
  playback and sending.
- Per-user volume (0–200 %) and local mute persisted via `Storage`.
- Device changes mid-call: reopen without leaving the channel.
- UI sound injection API (`play_cue(Cue::Join)`), with cues provided as
  embedded Opus/WAV assets (T-055 supplies the pack).
- `VoiceStats` snapshot every 500 ms: per-speaker jitter buffer delay,
  loss, PLC count; own encoder bitrate, RTT, underruns → `ClientEvent::VoiceStats`.
- Encoder adaptation hook: lower bitrate within profile range when loss > 8 %
  for 5 s; restore after 30 s clean.

- From T-053: wire `ss_audio::processing::Chain` and the `render_tap` into
  the capture worker (push each mixer frame before the playback ring; drain
  on capture), feed `EchoPath` from measured device delays, and surface
  `Monitor` stats in `VoiceStats`. From T-013: reopen capture on
  `DeviceEvent::requires_reopen` (capture does not follow a new default on
  macOS); handle `SilentInput` and `PermissionDenied` as UI states.

## Out of scope
Implementing the processing stages (T-053), raw input (T-047–T-050).

## Acceptance criteria
- Integration test with two in-process clients and the server: A sends a
  synthetic tone (injected instead of mic via `AudioSource::Test`), B's mixer
  output contains the tone; mute stops it; deafen silences B's output.
- Leaving and rejoining 50 times leaks no threads/streams.

## Notes from implementation
- Added a reconnect-safe datagram bridge to `Client`; `VoiceSession` keeps the
  audio graph alive while the connection worker replaces QUIC sessions and
  remaps the server-assigned track ids after `Resynced`.
- The session owns a dedicated audio worker with device reopen handling,
  PTT/VAD/open-mic gating, mute/deafen, per-user persisted gain/local mute,
  processing-chain render taps and echo delay, UI cue injection, 500 ms stats,
  and the loss-driven bitrate hook.
- Added an in-process server integration test using `AudioSource::Test` and
  `AudioSource::Silent`; it verifies audible routing, mute, deafen, and 50
  join/leave cycles. Cue tones are generated in-core for now; T-055 can replace
  them with the final sound pack without changing the API.
- `cargo fmt` and workspace clippy pass. At task completion, the full workspace
  test suite needed to skip the macOS headless
  `AVAudioApplication::sharedInstance()` null test. T-054 subsequently fixed
  that headless path, so the current workspace no longer needs the skip.

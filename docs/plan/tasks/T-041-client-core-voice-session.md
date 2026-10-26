# T-041 — `ss-client-core`: voice session orchestration

Status: todo
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

## Out of scope
Processing chain (T-053), raw input (T-047–T-050).

## Acceptance criteria
- Integration test with two in-process clients and the server: A sends a
  synthetic tone (injected instead of mic via `AudioSource::Test`), B's mixer
  output contains the tone; mute stops it; deafen silences B's output.
- Leaving and rejoining 50 times leaks no threads/streams.

## Notes from implementation

# T-017 — `ss-cli`: headless client with latency measurement and impairment modes

Status: done
Phase: 1
Size: M
Depends on: T-010, T-014, T-015, T-016
Paths: `crates/ss-cli/src/`

## Goal
A terminal client that can actually be used for a voice call between two
machines, measures mouth-to-ear latency, and simulates bad networks. It stays
useful later for load testing and debugging.

## Scope
- `ss-cli connect <host:port> --name X --channel N [--profile voice|music]`:
  pin prompt (TOFU), join, full audio graph (T-010 + T-014 + T-015), PTT via
  spacebar in the terminal or `--vad` (simple energy gate) or `--open-mic`.
- Live stats line: RTT, loss, jitter, buffer delay per speaker, underruns,
  bitrate.
- `--measure-latency`: plays a periodic click into the *send* path and
  cross-correlates it in the *receive* path of a second instance looped back
  through the server, printing mouth-to-ear latency using cpal timestamps.
- `--impair loss=5,jitter=30,reorder=1`: applies loss/jitter to outgoing
  datagrams in-process (so no `tc` needed).
- `--bots N`: spawn N synthetic clients sending pre-recorded Opus (for T-062).
- `--record out.wav` of the mixed output for A/B listening.

## Out of scope
Text chat, GUI.

## Acceptance criteria
- Two people on two machines can talk via a VPS-hosted T-016 server.
- `--measure-latency` reports a stable number (documented) on a LAN and via
  the VPS.
- `--impair loss=5` is audibly acceptable (PLC/FEC working); stats confirm.

## Notes from implementation

- The voice graph lives in `ss-cli`. `ss-client-core` stays a link-only
  dependency so later client tasks are not coupled to this spike binary.
  `ss-server` is a dev-dependency used by the in-process forward test only.
- `--channel` accepts a ULID or a decimal integer. An integer is written into
  the last eight bytes of a 16-byte id so two processes agree without a ULID.
- Unix terminals usually report only key presses. Space toggles talk until a
  key-release event is seen, then it becomes hold-to-talk. `q`, Esc, and
  Ctrl-C quit. A non-tty with the default gate refuses to start; pass
  `--open-mic` or `--vad`.
- `--accept-pin` stores the first certificate fingerprint without a prompt.
  A later fingerprint change still stops the process. `--duration SECS` exits
  and prints the latency median. Both are extra flags so a measurement can
  run unattended.
- `--measure-latency` runs the second client in this process (`{name}-ear`).
  It joins the same channel, plays the mix, and does not capture or inject
  clicks. Mouth-to-ear is `(heard − sent) + playback delay + capture delay`.
  Clicks are a 10 ms 1 kHz burst every 500 ms. The correlator searches the
  newest 200 ms of the downmix.
- On this Mac, a release build against a local T-016 server
  (`127.0.0.1`, built-in microphone and speakers, both native 48 kHz) reported
  a median of 84.4 ms over 15 clicks. Samples sat between 80.0 and 85.0 ms.
  Capture delay was about 12–13 ms, playback delay about 8–13 ms, the jitter
  buffer stayed at 40 ms, and RTT was about 0.3 ms. A debug build stalls the
  audio thread in the correlator and inflates the number; measure with
  `--release`. LAN and VPS runs were not done.
- `--impair` is deterministic from the sequence number and applies only to the
  interactive client, not to `--bots`. A second client listening to
  `--impair loss=5` reported about 4% loss, a rising PLC count, and one
  speaker, with the buffer still at 40 ms. Whether that is pleasant to listen
  to was not judged by ear. Bots send a 440 Hz tone and are not impaired.
- The spike server advertises mono 48 kHz Opus for every profile. Both clients
  must pass the same `--profile` or the decoder duration will not match.
- `--record` writes 16-bit stereo 48 kHz WAV of the local mix.

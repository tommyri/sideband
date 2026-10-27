# Voice

The target is TeamSpeak 3 quality at TeamSpeak latency, on a server that
never decodes the audio. Discord uses the same codec and sounds worse
because it caps the rate and forces processing. This document is the encoder
contract. The datagram header is in `docs/plan/02-protocol.md` §4.

`VoiceProfile::encoder_params` in `ss-core` returns the numbers below. Bitrate is decimal: 80 kb/s means 80_000 bits per
second.

## Signal

| Parameter | Value | Why |
|---|---|---|
| Sample rate | 48 kHz | Opus native rate. Device I/O resamples to this before the encoder. |
| Frame, `voice` and `music` | 20 ms (960 samples) | The usual Opus frame. One datagram per frame. |
| Frame, `low_latency` | 10 ms (480 samples) | Shorter algorithmic delay. Costs a few kilobits of efficiency. |
| Channels | 1 for speech, 2 for music | Discord is mono only. Music mode is the stereo path TeamSpeak calls Opus Music. |
| Application | Per profile, below | `VOIP` tunes for speech. `AUDIO` tunes for music. `RESTRICTED_LOWDELAY` disables look-ahead. |
| Complexity | 10 | Encoder and decoder. Desktop CPUs have the budget. |
| VBR | On | Opus is designed for variable rate. |
| DTX | Off | Silence is gated by push-to-talk or the voice-activity detector before the encoder. DTX and the gate both dropping silence is how the tail of a word gets clipped. |
| In-band FEC | On | The next packet can reconstruct a single lost frame. |
| Expected loss | 5% on `voice` only | `OPUS_SET_PACKET_LOSS_PERC`. `music` and `low_latency` leave the control unset; FEC is still enabled. |

## Profiles

A voice channel stores one profile and an optional bitrate. An absent bitrate
means the default. A bitrate outside the range is rejected.

| Profile | `OPUS_APPLICATION_*` | Default | Range | Channels | Frame | FEC | Expected loss |
|---|---|---|---|---|---|---|---|
| `voice` | `VOIP` | 80 kb/s | 48–96 | 1 | 20 ms | on | 5% |
| `music` | `AUDIO` | 128 kb/s | 96–192 | 2 | 20 ms | on | unset |
| `low_latency` | `RESTRICTED_LOWDELAY` | 64 kb/s | 64 only | 1 | 10 ms | on | unset |

`voice` at 80 kb/s is the default. It sits above Discord's unboosted 64 kb/s
cap, in the range treated as matching or beating TeamSpeak 3 Opus Voice
quality 10. `music` at 128 kb/s stereo is the channel for someone playing a
track. `low_latency` spends a frame of efficiency to save 10 ms. It is an
option, and the default stays `voice`.

T-014 sets these through the libopus control interface (`OPUS_SET_BITRATE`,
`OPUS_SET_APPLICATION`, `OPUS_SET_VBR`, `OPUS_SET_COMPLEXITY`,
`OPUS_SET_INBAND_FEC`, `OPUS_SET_PACKET_LOSS_PERC`, `OPUS_SET_DTX`). T-041
lowers the channel bitrate inside the profile range when loss stays above 8%
for 5 seconds, and restores the configured rate after 30 seconds clean. The
rate stays inside the profile range.

## Decoder

| Control | Setting | Why |
|---|---|---|
| Deep packet-loss concealment | On | Ships in libopus 1.5 and 1.6. It is decoder-side, roughly 1% CPU, and it does not change the bitstream. A peer on an older libopus still decodes the stream. |
| DRED | Off, behind the `dred` build flag | Deep REDundancy (Opus 1.5, revised in 1.6) puts up to about a second of redundant speech in each packet for roughly 12–32 kb/s extra. It repairs burst loss that in-band FEC cannot. The IETF extension is not finished, and a 1.5 payload is invisible to a 1.6 decoder (the payload is skipped, not mis-decoded). That makes it safe to experiment with and unsafe to turn on for everyone. |
| Bandwidth extension (BWE) | Off | The 1.6 neural bandwidth extender rebuilds 8–20 kHz from wideband speech. The `voice` profile is already fullband, so BWE would not add content. |
| Opus HD (96 kHz, up to 2 Mb/s) | Not a voice profile | Experimental extension for rates past 20 kHz. Irrelevant for speech. A future music experiment can use it without a protocol change, because the server does not parse the payload. |

T-014 pins `opusic-c` 1.6.1, resolved through `opusic-sys` 0.7.6 to bundled
libopus 1.6.1. Upstream defaults do **not** compile Deep PLC. We enable
`opusic-c/osce`, which includes Deep PLC and the low-rate LACE/NoLACE
enhancers, and explicitly set decoder complexity 10. BWE stays disabled by
CTL even though OSCE compiles it. A private native bridge supplies the
decoder complexity control missing from the binding without DRED, plus
packet framing validation and DRED offsets. The `ss-audio/dred` feature
additionally compiles DRED; the encoder still starts with redundancy off.
Its duration control enables 0–1000 ms in 10 ms steps.

## What we are not using

Neural codecs aimed at very low bitrates are the wrong tool on a home
connection.

- EnCodec at 24 kb/s scored below Opus in the Interspeech 2024 listening
  tests. Its operating range is about 1.5–24 kb/s.
- Opus 1.6 with NoLACE and bandwidth extension beats EnCodec from 9 kb/s up.
  The voice profile runs far above that region.
- DAC can sound transparent near 8 kb/s and adds on the order of 190 ms of
  delay because the network is non-causal.
- Lyra V2 runs at 3.2, 6, or 9.2 kb/s with a 20 ms frame. It is a low-rate
  speech codec, and fullband Opus at 80 kb/s is the quality target here.
- ZipCodec (2026) reaches 0.80 kb/s at 160 ms theoretical latency with 842M
  parameters. That is a speech-model token codec.

At 64–128 kb/s, Opus is effectively transparent for speech. The useful
machine-learning work is inside Opus: Deep PLC, DRED, and the low-rate
enhancers. Replacing the codec would spend latency to solve a bitrate
problem we do not have.

## Path through the client

```
capture device
  → 48 kHz, mono or stereo (rubato only when the device rate is not 48 kHz)
  → optional chain: echo cancellation, noise suppression, automatic gain
  → gate (push-to-talk, voice activation, or open mic)
  → Opus encode, one frame
  → datagram header + payload

datagram
  → one jitter buffer and one decoder per track
  → per-user gain
  → mix, soft limiter
  → device rate, if needed
  → playback device
```

The real-time callback does not allocate, lock, log, or await. Frames cross
to the rest of the process through `rtrb` rings.

The gate, not the encoder, decides when packets flow. The first packet of a
burst sets `TALK_START`. The last flushed packet sets `TALK_END`. A receiver
shows the speaker as talking from `TALK_START` until `TALK_END` or until
200 ms pass with no packet.

Voice activation keeps a two-frame pre-roll so the start of a word is not
cut off, and it holds the gate open for a hangover (default 300 ms) after
the level drops.

## Processing chain

Every stage is off unless the user enables it. With all of them off, the
samples that reach the encoder are the samples that left the resampler.

Order when enabled: echo cancellation, then noise suppression, then
automatic gain. The echo canceller needs a reference from the mix, taken
before the playback ring, delayed by the measured device latency.

| Stage | Implementation | Added latency | When to use it |
|---|---|---|---|
| Echo cancellation | `sonora` AEC3 | A few milliseconds plus the filter | Speakers, or a microphone that hears the room |
| Noise suppression | RNNoise (10 ms frames) or DeepFilterNet3 (10 ms frames, low-latency model) | See the model | Fans, mechanical keyboards |
| Automatic gain | `sonora` AGC2 | Small | A microphone whose level wanders |
| macOS alternative | `AUVoiceIO` | Apple's voice-processing I/O unit | One switch. It replaces the three stages above and follows the system's mic mode (Standard, Voice Isolation, Wide Spectrum). |

GTCRN (ICASSP 2024) is the strongest small noise model we looked at, and it
is not in the first chain. It needs an integration we have not done. It is
listed in the backlog.

Each enabled stage must finish well under the frame duration. T-053 budgets
3 ms for the chain per 10 ms frame, and under 1.5 ms per stage.

## Devices

`cpal` 0.18 selects PipeWire, then PulseAudio, then ALSA on Linux. Windows
uses WASAPI shared mode. macOS uses CoreAudio. The per-OS reasons are
decision D7 in [decisions.md](decisions.md).

The capture and playback streams expose `cpal`'s hardware timestamps
(`StreamTrait::now` in 0.18). Mouth-to-ear measurement and, later, lining a
screen share up with a voice both use that clock.

A device that disappears, or a new default device, reopens the stream
without leaving the voice channel.

## Jitter buffer

One buffer per track. Packets are ordered by the datagram sequence number.
The target delay starts at 40 ms and tracks the 95th percentile of
inter-arrival deviation over the last 2 seconds, clamped to 20–200 ms. The
buffer grows or shrinks at a talk-burst boundary or in decoded silence, by
inserting a concealment frame or dropping a frame. Time-stretching inside a
word is deferred; it is in the backlog.

Recovery decisions happen before audio is released to the playback ring. A
missing slot uses the following packet's in-band FEC when that packet is
already buffered; otherwise it uses PLC. Already-committed slots cannot be
replaced or replayed when originals arrive late. Late forward observations
still inform the jitter estimate. With `dred`, buffered redundancy can recover
several missing slots before release; those slots are then committed together.

T-015 bounds each track to 64 queued packets and estimates the 95th percentile
from up to 200 forward inter-arrival deviations in a rolling two-second window.
The estimate adds a 20 ms margin and rounds up to the profile frame duration.
A new burst reanchors playout; a whole-frame capture-clock gap can identify a
boundary when flags are lost. After a long pause with no recent observations,
the estimate restarts at 40 ms. Inside a burst, only decoded PCM below
−60 dBFS permits delay expansion/contraction. Contraction decodes the discarded
packet to preserve codec prediction state. Sequence and sample timestamps wrap.

The playback worker emits 10 ms stereo quanta. Per-track and per-user linear
gains range from 0 to 4; master gain includes UI sounds. A linked stereo limiter
starts its soft knee at amplitude 0.8 and approaches 1.0 without changing the
left/right balance. Device callbacks only consume the resulting ring buffer.

This is the same shape as WebRTC's NetEQ: adaptive target, concealment,
expand and accelerate at the edges. T-015 tests it with a simulated network
(30 ms ± 10 ms jitter and 2% loss should underrun never, and should stay
under about 70 ms of buffer).

## Latency budget

Design targets, not yet measured. T-017 measures them and T-018 compares
them with TeamSpeak on the same VPS. If the measured figure is worse than
TeamSpeak, the profile table changes.

| Piece | `voice` / `music` | `low_latency` |
|---|---|---|
| Encoder frame | 20 ms | 10 ms |
| Device period, both ends | about 10 ms each | about 10 ms each |
| Jitter target, clean path | 40 ms | 40 ms |
| Encode and decode | under 1 ms each | under 1 ms each |
| Network | one way | one way |

Adding the rows gives about 80 ms plus the one-way network time, before any
processing the user turned on. A noise suppressor adds its own look-ahead on
top, which is why it stays off by default.

## Server

The server does not link libopus. It checks that the track id belongs to the
sending connection, that the sender has `speak`, and that the sender is not
server-muted. It then copies the datagram to every other member of the
channel who is not self-deafened.

A 10-person channel at 80 kb/s is under 1 Mb/s toward each listener and a
small fraction of a core on the VPS. The expensive work is decode and mix,
and that stays on the clients.

## Sources

- Opus 1.5: Deep PLC, DRED (~12–32 kb/s extra, up to about a second of
  redundant features), LACE and NoLACE.
  <https://www.opus-codec.org/demo/opus-1.5/>
- Opus 1.6: revised DRED (a 1.5 payload is ignored, not mis-decoded) and
  neural bandwidth extension. With NoLACE, fullband quality from about
  9 kb/s, and better than EnCodec at that rate and above.
- DRED extension: IETF draft `draft-ietf-mlcodec-opus-dred`. Experimental
  builds have used extension id 126; the draft uses 32. That is why DRED
  stays behind a flag.
- Interspeech 2024 codec listening test: EnCodec at 24 kb/s scored below
  Opus; DAC approaches transparency near 8 kb/s with about 190 ms of delay
  from non-causal layers.
- Lyra V2: 3.2 / 6 / 9.2 kb/s, 20 ms frames. A low-rate speech codec.
- ZipCodec (2026): 0.80 kb/s at 6.25 Hz, 160 ms theoretical latency, 842M
  parameters. A speech-model token codec.
- Rust bindings: `opusic-sys` 0.7.6 (libopus 1.6.1), `opusic-c` with `dred`
  and `osce` features.
- Jitter buffer shape: WebRTC NetEQ. Receiver-side release scheduling (Luby,
  ARC, 2025) is a backlog idea, not the v1 algorithm.

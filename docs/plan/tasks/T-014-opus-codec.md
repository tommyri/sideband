# T-014 — `ss-audio`: Opus encoder/decoder wrapper (libopus 1.6, Deep PLC, DRED flag)

Status: todo
Phase: 1
Size: S
Depends on: T-003
Paths: `crates/ss-audio/src/codec/`

## Goal
A safe, allocation-free-at-runtime Opus wrapper configured from
`VoiceProfile`, with Deep PLC on and DRED optional.

## Scope
- Dependency `opusic-c` (bundled libopus 1.6.x). Features: `dred` → enable
  libopus DRED/OSCE build flags.
- `Encoder::new(profile: VoiceProfile) -> Encoder`: sets application,
  bitrate, channels, `OPUS_SET_INBAND_FEC(1)`, `OPUS_SET_PACKET_LOSS_PERC(5)`,
  complexity 10, VBR on, DTX off; `set_bitrate()`, `set_packet_loss()` for
  adaptation; `encode(&[f32; frame]) -> &[u8]` into a preallocated buffer.
  With `dred`: `set_dred_duration_ms(0..=1000)`.
- `Decoder::new(channels)`: `decode(packet: Option<&[u8]>, fec: bool) ->
  &[f32]`; `None` triggers PLC (Deep PLC when available). With `dred`:
  `decode_dred(packet, lost_frames)` path.
- `OpusPacket::toc()` helpers: frame duration, channel count, to validate
  incoming packets cheaply before buffering.
- Bench (`criterion`): encode+decode per 20 ms frame on CI machines; assert
  well under 1 ms.

## Out of scope
Jitter buffer, networking.

## Acceptance criteria
- Round-trip test: 1 kHz sine at −12 dBFS through voice profile → SNR
  ≥ 20 dB after 100 ms warm-up; music profile stereo preserves channel
  separation.
- Loss test: drop every 10th packet; decode with FEC produces no silence gaps
  longer than one frame.
- `dred` feature compiles and a decode-with-DRED test passes when enabled; the
  default build excludes it.

## Notes / references
- Opus 1.5/1.6 release notes; DRED is experimental (IETF draft, extension id
  may change) which is why it stays behind a flag.

## Notes from implementation

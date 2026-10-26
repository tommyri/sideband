# Design notes

Why the system is shaped the way it is. The plan in `docs/plan/` says what to
build and in which order. These notes are the record of the technical
decisions, especially the voice codec.

| Document | What it settles |
|---|---|
| [decisions.md](decisions.md) | Numbered decisions D1–D12, with the constraints later tasks must keep |
| [voice.md](voice.md) | Opus profiles, encoder contract, processing, devices, latency budget |

The wire layout is in `docs/plan/02-protocol.md`. If a number there and a
number here disagree, the protocol document wins for bytes on the wire and
`voice.md` wins for encoder behavior. Update both in the same change.

The profile numbers are implemented as `VoiceProfile::encoder_params` in
`ss-core`, merged into `main` with T-003. Keep the code, these documents, and
the protocol table in agreement.

T-018 (the listening test against TeamSpeak 3) may revise bitrates, frame
size, and the jitter target. Until that note exists, `voice.md` is what
implementers code against.

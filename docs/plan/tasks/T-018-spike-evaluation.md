# T-018 — Voice-quality release gate: A/B against TeamSpeak 3 and parameter lock

Status: todo
Phase: 1
Size: S
Depends on: T-017
Paths: `docs/plan/notes/spike-results.md`

## Goal
Decide, with numbers and ears, that the voice path meets the TS3 quality
target and lock release defaults before declaring voice quality ready for
release. This gates stable/public release readiness, not UI development,
packaging, internal test builds, or work needed to collect the evidence.

## Scope
- Deploy T-016 on the same VPS as the TS3 server (docker or binary).
- With ≥ 3 people: blind A/B listening sessions TS3 (Opus Voice q10 / Opus
  Music q10) vs SocialSpeak `voice` at 64/80/96 kb/s and `music` at 128 kb/s;
  record both.
- Latency: `--measure-latency` vs TS3 (measure TS3 with a loopback recording
  if possible).
- Impairment: run `--impair loss=2/5/10,jitter=20/50` and note where it
  degrades; test with DRED build on/off.
- Decide defaults: bitrate, frame size (20 vs 10 ms), initial jitter target,
  FEC loss percentage, whether DRED ships as a user-visible option.
- Update `02-protocol.md` §4 table and `ss_core::VoiceProfile` defaults.
- Prefer installable T-075 builds and T-076 guided trials when available;
  the CLI remains valid for controlled measurements. Record exact commits,
  OS/device/backend settings, trial IDs and report bundle references.
- Distinguish network RTT, device estimates and jitter delay from measured
  acoustic/mouth-to-ear latency. Unknown measurements stay unknown.

## Out of scope
Any new features.

## Acceptance criteria
- `docs/plan/notes/spike-results.md` with method, numbers, group verdict.
- Go/no-go recorded. If no-go, a list of concrete fixes becomes new tasks.
- A no-go blocks voice-quality release approval, not continued development
  or distribution of clearly labelled test builds.

## Notes from implementation

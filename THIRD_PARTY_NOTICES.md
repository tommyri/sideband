# Third-party material

SocialSpeak's source license does not replace the licenses of its dependencies
or separately attributed assets.

## Checked-in speech fixture

`crates/ss-audio/tests/fixtures/vad/speech-keyboard.wav` contains a modified excerpt
from VOiCES (SRI International and Lab41 / In-Q-Tel), licensed under **CC BY 4.0**.
The [fixture README](crates/ss-audio/tests/fixtures/vad/README.md) records the source,
license link, modifications, checksums and reproduction procedure. Keep that
attribution when redistributing the fixture. Other test recordings are not bundled.

## Dependencies and native libraries

Rust and frontend versions are pinned in `Cargo.lock`, `fuzz/Cargo.lock` and
`apps/desktop/pnpm-lock.yaml`. They include third-party audio/codecs, cryptography,
GUI and operating-system integrations. Their original license texts and notices
remain applicable. `deny.toml` records Rust dependency policy and scoped exceptions;
its allow-list is not a complete binary-distribution notice bundle.

Before publishing binary releases, collect the license texts/notices for the exact
resolved Rust, npm and bundled native dependencies and enabled features. Include
applicable source-offer/source-availability obligations and assess codec/model
redistribution terms. The production distribution and licensing acceptance tasks
(T-057/T-067) remain open; publishing this source does not mark them complete.

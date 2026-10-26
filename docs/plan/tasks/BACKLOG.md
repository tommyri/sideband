# Backlog (not scheduled)

Ideas and deferred items. Promote to a `T-xxx` task when scheduled.

## Voice
- Time-stretching (WSOLA) in the jitter buffer for smoother adaptation.
- GTCRN noise suppression once a clean Rust/ONNX streaming path is packaged.
- Opus HD / 96 kHz "studio" channel profile (experimental extension).
- Whisper/private talk to one user while in a channel (TeamSpeak-style).
- Positional/3D audio for games (per-user pan).
- Per-channel "voice recording" with consent indicator.
- Audio from the shared screen/app during streams.
- Windows `uiAccess` manifest for PTT visibility into elevated games.
- Bluetooth LE Audio / LC3 headset quality detection improvements.

## Text
- Reactions, pins, threads, message forwarding.
- Image thumbnails and a privacy-preserving image proxy for link previews.
- Custom emoji and stickers.
- Slowmode and timeouts (moderation).
- Blocking users; hiding DMs from specific users.
- Rich embeds for webhooks (full Discord embed compatibility).

## Platform
- Flatpak packaging; Winget/Homebrew casks.
- Mobile clients (iOS/Android) on the shared Rust core.
- Web client via WebTransport (requires browser support matrix review).
- E2EE for DMs (MLS) and optionally voice (per-channel keys; server already
  never decodes).
- Server federation / multi-server identity (explicitly not a goal now).
- Outgoing webhooks and interaction components for bots.
- Simulcast for screen share; HEVC/AV1 where hardware exists.

## Ops
- Automated SQLite backups to S3-compatible storage.
- Prometheus alert rules.

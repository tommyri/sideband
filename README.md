# Sideband

<img src="apps/desktop/public/brand/sideband-mark.svg" alt="Sideband offset-bands logo" width="96" />

Self-hosted voice and text chat for a friend group. One small server you run
yourself, and a desktop app for Windows, macOS and Linux. Built with Rust,
QUIC, SQLite and Tauri.

Previously developed as SocialSpeak. Sideband is the working product name;
internal `ss-*` crates, existing data locations and application IDs remain
stable.

**Development preview.** The goal is TeamSpeak-class voice quality with a
richer chat experience. That comparison has not yet passed its group
acceptance gate; this is not a production-ready release or a claim of
measured quality parity.

<!-- TODO(T-064): add screenshots once taken from an installed build, under
docs/images/ (none exist yet):
1. A server with a voice channel in use: channel tree, members with speaking
   rings, the voice dock.
2. Text chat: Markdown, a reply, an attachment and a link preview.
3. The add-server dialog at the certificate fingerprint step.
4. Audio settings with the processing section visible.
5. Global shortcuts with the platform status panel.
6. Roles and permissions with a channel override.
Use sample accounts; show no real server address, fingerprint or invite. -->

## Features

- **Voice:** push-to-talk, voice activation or open microphone; global
  push-to-talk, mute and deafen shortcuts that do not steal the key from a
  game; per-person volume; Opus with three channel profiles; optional echo
  cancellation, noise suppression and gain control, all off by default.
- **Chat:** text channels, DMs and group DMs with Markdown, mentions, replies,
  edits and deletes, drafts, typing indicators, attachments with media
  viewing, link previews, unread state and [message search](docs/message-search.md).
- **Administration:** invites, roles and permissions with channel overrides,
  channel management, member moderation with timed bans, server settings,
  incoming webhooks and a filtered audit log.
- **Operations:** one static Linux binary or a container image; self-signed
  certificates with fingerprint pinning, ACME, or your own; consistent online
  backups; Prometheus metrics and a Grafana dashboard.
- **Testing aids:** guided tests, live diagnostics and redacted evidence
  exports built into the app.

The server forwards encoded media; it does not decode it. QUIC encrypts the
transport between clients and the server; this is **not end-to-end encryption
against the server operator**.

## Status

- Server and desktop features above are implemented and covered by automated
  tests.
- Signed update packaging and opt-in installation are implemented; see the
  [release guide](docs/release.md). Installed signing and upgrade acceptance
  remain.
- Screen sharing is in progress.
- Hands-on checks on installed apps, real group listening tests and
  production readiness remain open. Every open check is in the
  [acceptance matrix](docs/plan/tasks/ACCEPTANCE.md); local macOS package
  checks exist, Windows and Linux installs are not yet verified by hand.

## Quick start

**Run a server.** On a Linux VPS, follow the
[self-hosting guide](docs/self-hosting.md): Docker Compose or a static binary
with systemd, both listening on UDP 443. To try one locally from a checkout
(prerequisites in [contributing](CONTRIBUTING.md#development-setup)):

```sh
cargo run -p ss-server -- init --data-dir ./data
# Edit data/config.toml: for local development use unprivileged loopback
# ports, e.g. bind_quic = "127.0.0.1:4433", bind_http = "127.0.0.1:8443".
cargo run -p ss-server -- run --data-dir ./data
```

`init` prints an owner invite and the certificate fingerprint. Keep the
invite private; it makes the first account an administrator.

**Connect.** Install the desktop app from
[GitHub Releases](https://github.com/tommyri/sideband/releases)
([installation and checksums](docs/testing-app.md#install-and-identify)), then
**Add your first server** with `host:port` (for the local server above,
`127.0.0.1:4433` and HTTPS port `8443` under **Advanced connection options**),
compare the fingerprint, and create your account with the invite. The
[user guide](docs/user-guide.md) covers the rest.

Test builds are unsigned or ad-hoc signed. Download them only from a source
you trust. A local macOS package can be built with
`node scripts/build-test-desktop.mjs --debug --bundles app`; see
[testing builds](docs/testing-app.md#build-locally).

## Documentation

| For | Read |
|---|---|
| Using the app | [User guide](docs/user-guide.md) |
| Running a server | [Self-hosting guide](docs/self-hosting.md), [deployment](deploy/README.md), [server diagnostics](deploy/grafana/README.md) |
| Writing a bot or client | [Protocol reference](docs/protocol/README.md) |
| Contributing | [Contributing](CONTRIBUTING.md), [engineering conventions](docs/plan/04-conventions.md), [task index](docs/plan/tasks/INDEX.md) |
| Designing the desktop UI | [Design system](DESIGN.md), [FigCraft workspace and walkthroughs](docs/design/README.md#desktop-prototypes) |
| Architecture and audio | [Architecture and plan](docs/plan/00-overview.md), [design decisions and codec contract](docs/design/README.md#technical-design-notes) |
| Testing and releases | [Testing builds and evidence](docs/testing-app.md), [desktop releases](docs/desktop-releases.md), [release guide](docs/release.md), [publishing](docs/publishing.md) |
| Security | [Reporting](SECURITY.md), [security design](docs/security.md) |

## License

Unless a file says otherwise, project source is dual-licensed under
[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
The speech test fixture is **CC BY 4.0**, with attribution and modification details
in its [fixture README](crates/ss-audio/tests/fixtures/vad/README.md).
Dependencies retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).

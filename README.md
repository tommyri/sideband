# Sideband

<img src="apps/desktop/public/brand/sideband-mark.svg" alt="Sideband offset-bands logo" width="96" />

Previously developed as SocialSpeak. Sideband is the working product name;
internal `ss-*` crates, existing data locations and application IDs remain stable.

Self-hosted voice and text chat for a friend group, built with Rust, QUIC,
SQLite, and a Tauri desktop client for Windows, macOS, and Linux.

**Development preview.** The goal is TeamSpeak-class voice quality with a richer
chat experience. That comparison has not yet passed its group acceptance gate;
this is not a production-ready release or a claim of measured quality parity.

## Current state

- Server: authentication, invitations, roles/permissions, channels, voice routing,
  text/history, attachments, DMs, moderation, link previews and incoming webhooks.
- Desktop: onboarding, voice channels and controls, audio settings, global PTT,
  live plain-text channel chat and per-server DMs (history, replies, edits/deletes,
  unread state and group membership),
  guided tests, diagnostics and redacted local evidence exports.
- Still to build: rich text, mentions, attachment UI, notifications, full admin UI,
  production distribution/updating, and the finished screen-sharing experience.
- Automated tests and local macOS package checks exist. Cross-platform installed
  app acceptance, real group listening tests and production readiness remain open.

Audio processing is opt-in and off by default. The server forwards encoded media;
it does not decode it. QUIC encrypts transport between clients and the server;
this is **not end-to-end encryption against the server operator**.

## Try a testing build

Download development installers from [GitHub Releases](https://github.com/tommyri/sideband/releases).
Use [manual releases](docs/desktop-releases.md) to select operating systems and
publish or replace a version. See [testing builds and installation](docs/testing-app.md). The manually dispatched
**Desktop test builds** GitHub Actions workflow produces Windows x64 installers,
macOS Apple Silicon packages, and Linux x64 deb/AppImage packages. Download an
artifact only from a build/source you trust. Test builds are unsigned or ad-hoc
signed; public release binaries and production signing are not promised here.

A local macOS package can be built with:

```sh
node scripts/build-test-desktop.mjs --debug --bundles app
```

Install the development prerequisites below first. Debug packages are for functional
testing, not performance acceptance. Browser-only preview screens use sample data.

## Develop

Use the Rust version pinned in [rust-toolchain.toml](rust-toolchain.toml), Node 24,
pnpm 11.9.0, CMake, a C/C++ toolchain, and [just](https://github.com/casey/just).
Nightly Rust is used only for formatting:

```sh
rustup toolchain install nightly --profile minimal --component rustfmt
cd apps/desktop
pnpm install --frozen-lockfile
cd ../..
just check
just check-desktop
just dev-desktop
```

Native prerequisites:

- **macOS:** Xcode Command Line Tools and CMake; deployment target macOS 13+.
- **Windows:** Visual Studio C++ build tools, CMake and WebView2.
- **Linux:** ALSA/PipeWire development headers, Clang, CMake, WebKitGTK 4.1,
  appindicator and the other packages listed in [desktop CI](.github/workflows/test-desktop.yml).
  Current Linux test packages target Ubuntu 24.04; older-distribution portability
  has not been established.

The standard-library Python repository checks run with
`python3 scripts/check-public-repo.py`. The public repository workflow also scans
Git history with Gitleaks. All GitHub Actions workflows are manual-only for now
to control usage; pushes and pull requests do not automatically run CI.

## Run a server

```sh
cargo run -p ss-server -- init --data-dir ./data
# Edit data/config.toml before running; for local development use unprivileged
# loopback ports, e.g. bind_quic = "127.0.0.1:4433", bind_http = "127.0.0.1:8443".
cargo run -p ss-server -- run --data-dir ./data
```

`init` prints an owner invitation and certificate fingerprint. Keep the invitation
private and compare the fingerprint independently when connecting a client.
Use the explicit `run` subcommand: running `ss-server` without a subcommand starts
the legacy **unauthenticated voice spike**, not the normal authenticated server.

See [deployment](deploy/README.md) for systemd/Docker and
[server diagnostics](deploy/grafana/README.md) for metrics and live connection
inspection. Keep the internal metrics/operator listener on loopback.

## Project guide

- [Architecture and implementation plan](docs/plan/00-overview.md)
- [Task status and remaining acceptance](docs/plan/tasks/INDEX.md)
- [Codec contract and design decisions](docs/design/README.md)
- [Testing and evidence collection](docs/testing-app.md)
- [Contributing](CONTRIBUTING.md)
- [Publishing this repository](docs/publishing.md)
- [Security reporting](SECURITY.md) and [security design](docs/security.md)

## License

Unless a file says otherwise, project source is dual-licensed under
[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
The speech test fixture is **CC BY 4.0**, with attribution and modification details
in its [fixture README](crates/ss-audio/tests/fixtures/vad/README.md).
Dependencies retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).

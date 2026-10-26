# T-032 — Server configuration, CLI, tracing, metrics, graceful shutdown

Status: todo
Phase: 2
Size: S
Depends on: T-021
Paths: `crates/ss-server/src/config.rs`, `crates/ss-server/src/main.rs`, `crates/ss-server/src/http/internal.rs`

## Goal
The server is operable by a non-expert: one config file, sane defaults,
helpful CLI, logs, metrics, clean restarts.

## Scope
- `config.toml` in the data dir (created with comments on first run):
  `[server] name, bind_quic, bind_http, public_host`, `[tls] mode =
  "self-signed"|"acme"|"files"`, `[limits] upload_max_bytes, …`,
  `[previews]`, `[internal] bind = "127.0.0.1:9101"`. Env overrides
  `SS_*`. `figment` or `config` crate.
- CLI (`clap`): `run`, `init`, `invite create [--owner]`, `user list|ban|
  unban|set-password`, `role grant-admin`, `fingerprint`, `check-config`,
  `backup <path>` (SQLite online backup + attachments tar).
- `tracing-subscriber` with env filter, JSON option, file rotation into
  `logs/`.
- Internal listener: `/metrics` (Prometheus text via `metrics` +
  `metrics-exporter-prometheus`): connections, voice members, datagrams
  forwarded/dropped, messages/min, DB latency histograms; `/healthz`.
- Graceful shutdown on SIGTERM/CTRL-C: notice, close connections, flush DB
  (uses T-024 hook).
- `--data-dir` default: `./data` or `$XDG_DATA_HOME/socialspeak`.

## Out of scope
Deployment files (T-034), TLS specifics (T-033).

## Acceptance criteria
- `ss-server init && ss-server run` works from an empty dir and prints the
  owner invite and fingerprint.
- `check-config` catches a typo'd key.
- `/metrics` exposes the listed series; `/healthz` 200.
- Shutdown completes < 2 s with connected clients.

## Notes from implementation

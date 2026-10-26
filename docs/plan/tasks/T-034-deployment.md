# T-034 — Deployment artifacts: static binary, Docker, systemd, backup

Status: todo
Phase: 2
Size: S
Depends on: T-032
Paths: `deploy/`, `.github/workflows/release-server.yml`

## Goal
Installing on the VPS is one of: download a binary + systemd unit, or
`docker compose up`.

## Scope
- Release build `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`
  (static, no glibc dependency); strip; `cargo-dist` or a plain workflow
  attaching binaries to GitHub releases.
- `deploy/Dockerfile` (distroless/static, non-root, `/data` volume),
  `deploy/docker-compose.yml` with UDP+TCP 443 and the internal port bound to
  localhost.
- `deploy/socialspeak.service` systemd unit (DynamicUser, StateDirectory,
  `AmbientCapabilities=CAP_NET_BIND_SERVICE`, hardening flags).
- `deploy/README.md`: VPS quick start, firewall (UDP 443!), UDP buffer
  sysctls (`net.core.rmem_max`), TLS mode choice, backup/restore using
  `ss-server backup`, upgrade procedure, running next to TeamSpeak on the same
  host (port choice).
- Smoke test in CI: start the container, hit `/healthz`, connect with
  `ss-cli --bots 2`.

## Out of scope
Kubernetes, multi-node.

## Acceptance criteria
- A fresh Ubuntu VPS can be running the server in < 10 minutes following the
  README (verify once manually; note in task).
- CI smoke test passes.

## Notes from implementation

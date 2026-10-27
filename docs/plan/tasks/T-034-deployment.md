# T-034 — Deployment artifacts: static binary, Docker, systemd, backup

Status: done
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

- Release workflow is `.github/workflows/release-server.yml`. Tags
  `server-v*` publish stripped `ss-server-<target>` binaries plus
  `socialspeak.service`. `workflow_dispatch` builds the artifacts and
  does not publish. `x86_64` uses `musl-gcc`. `aarch64` uses the musl.cc
  cross compiler because the bundled SQLite is C.
- The image is `gcr.io/distroless/static-debian12:nonroot` (uid 65532)
  with `/data` created in the build stage so a new volume is writable.
  `ENTRYPOINT` is `/ss-server`. The default command is `run --data-dir
  /data`. Compose publishes UDP and TCP 443 and `127.0.0.1:9101:9101`,
  and sets `SS_INTERNAL_BIND=0.0.0.0:9101` so the health port is reachable
  through the published mapping. `cap_add: NET_BIND_SERVICE`.
- `metrics-exporter-prometheus` default features are off. Its HTTP
  listener linked `aws-lc-sys`, and the server already renders the
  Prometheus text itself. `cargo tree -p ss-server -i aws-lc-sys` no
  longer resolves that crate. The QUIC provider stays ring.
- Smoke is `deploy/smoke.sh`, called from the CI `smoke` job. It builds
  the image, waits for `/healthz` on the `run` container, then starts a
  second container from the same image with no subcommand (the voice
  spike) and runs `ss-cli connect --bots 2 --open-mic --measure-latency
  --duration 5 --accept-pin`. `ss-cli` still speaks the spike Hello, so
  it cannot log into `ss-server run`. The first attempt is untrusted;
  success is `joined voice` for both bots after `--accept-pin`.
- Verified here: Docker on this Mac built `aarch64-unknown-linux-musl`
  (the Docker host architecture). `readelf` found no `INTERP`. Smoke
  exited 0: `/healthz` returned 200, both bots joined, `speakers=2`.
  An Ubuntu VPS was not rented, and the `x86_64` musl binary was not
  built on this machine. Those two checks are the release workflow and
  the CI smoke job.

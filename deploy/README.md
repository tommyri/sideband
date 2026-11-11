# Installing Sideband

Two ways onto a VPS: a static binary with the systemd unit, or
`docker compose up`. Both listen for QUIC on UDP 443. A fresh machine
should be serving within a few minutes of the download.

Tags named `server-v*` carry `ss-server-x86_64-unknown-linux-musl` and
`ss-server-aarch64-unknown-linux-musl`. Neither binary needs glibc.

## Binary and systemd

```sh
sudo install -m 0755 ss-server-x86_64-unknown-linux-musl /usr/local/bin/ss-server
sudo cp deploy/socialspeak.service /etc/systemd/system/socialspeak.service
sudo systemctl daemon-reload
sudo systemctl enable --now socialspeak
sudo journalctl -u socialspeak -n 40
```

The unit uses `DynamicUser` and stores the database under
`/var/lib/socialspeak`. Optional overrides go in `/etc/socialspeak.env`
(`SS_SERVER_NAME`, `SS_PUBLIC_HOST`, `SS_TLS_MODE`, and the other `SS_*`
keys from `config.toml`). The first start writes that config and prints
the owner invite and certificate fingerprint in the journal.

`AmbientCapabilities=CAP_NET_BIND_SERVICE` lets the dynamic user bind
port 443. Do not run the process as root. If the service dies before it
logs a listening line, comment out `MemoryDenyWriteExecute=yes` in the
unit and reload. Ring does not JIT, so that line should stay.

## Docker Compose

The manual **Release server** GitHub Actions workflow publishes
`ghcr.io/tommyri/sideband-server` for `linux/amd64` and `linux/arm64`.
Docker selects the correct architecture. No Rust toolchain or source checkout
is needed on the server: copy `deploy/docker-compose.yml` there, then run:

```sh
docker compose pull
docker compose up -d
docker compose logs
```

From a repository checkout, add `-f deploy/docker-compose.yml` to these commands.
The default image version is `0.0.1`. Override it with `SIDEBAND_IMAGE` in a
Compose `.env` file, for example `SIDEBAND_IMAGE=ghcr.io/tommyri/sideband-server:latest`.
A published version is replaced only by a deliberately forced rerun, and `latest`
never moves to an older version. The workflow also publishes `sha-<full-commit>`
tags; use an image digest for immutable deployment. Binaries and images carry
build provenance attestations; see [verify a download](../docs/release.md#verify-a-download).

The official `sideband-server` package is public and can be pulled without login.
For a fork/new package, GitHub initially defaults to private: set the package
visibility to **Public** in its GitHub package settings for anonymous pulls.
If keeping it private, authenticate to GHCR on the server with a token that has
`read:packages` before pulling.

The container runs as uid 65532. Compose publishes UDP and TCP 443, and
publishes the metrics port only on `127.0.0.1:9101`. The data volume is
`/data` inside the container. `docker logs` shows the owner invite and
the fingerprint.

Both Compose files drop every capability, set `no-new-privileges`, and run
with a read-only root filesystem; only the `/data` volume and a `/tmp` tmpfs
are writable. Port 443 still binds without `NET_BIND_SERVICE`, because Docker
sets `net.ipv4.ip_unprivileged_port_start=0` inside containers (Docker 20.10
and later, not with `network_mode: host`). The healthcheck runs
`/ss-server conn list` in the container, which fails unless the internal
listener answers; `docker compose ps` shows the result and
`docker compose up --wait` waits for it. Docker does not restart an unhealthy
container on its own.

For an optional local source build:

```sh
docker build -f deploy/Dockerfile -t socialspeak:local .
SIDEBAND_IMAGE=socialspeak:local docker compose -f deploy/docker-compose.yml up -d
```

To publish a new image, tag a commit on `main` as `server-vX.Y.Z` (matching the
`ss-server` version) and manually run **Release server** on that tag; see
[release the server](../docs/release.md#release-the-server). It compiles once on
each architecture's native runner, checks static linkage and smoke-tests the
non-root runtime locally. After CI and both builds pass and the release is
approved, it pushes one two-architecture image. Nothing is pushed per
architecture, and nothing runs on pushes or pull requests.

## Host NGINX and a public domain certificate

The examples use `sideband.example.com`, a placeholder. You must register or
otherwise control your own domain and configure its DNS. Before installing the
examples, replace every `sideband.example.com` in `nginx-sideband.conf` and
`renew-sideband-certificate.sh` with your chosen hostname. Use that hostname in
the commands below too; no domain or certificate is provided with Sideband.

For NGINX installed on Ubuntu, use `docker-compose.nginx.yml` **instead of**
`docker-compose.yml`. Do not combine them: the latter reserves public TCP 443.
The NGINX variant keeps the same Compose service/data volume, publishes only UDP
443 publicly, and exposes HTTPS on `127.0.0.1:8443` and metrics on localhost.
Run it from the same checkout/project name as the original deployment to preserve
the existing `deploy_socialspeak` volume. Do not use `down -v` during migration.

1. Point `sideband.example.com` directly at the VPS. Ordinary HTTP-only CDN proxies
   cannot carry Sideband's QUIC traffic. Check that NGINX HTTP/3 or another service
   does not already bind UDP 443 (`sudo ss -lunp`). Keep existing website configs.
2. Obtain a publicly trusted certificate for `sideband.example.com` using your existing
   certificate manager. The supplied examples use Certbot paths; adjust them if
   your certificate name/path differs. Certificate issuance remains operator-owned.
3. Install the renewal hook and run it once:

   ```sh
   sudo install -m 0755 deploy/renew-sideband-certificate.sh \
     /etc/letsencrypt/renewal-hooks/deploy/sideband
   sudo /etc/letsencrypt/renewal-hooks/deploy/sideband
   ```

   The hook checks the certificate hostname, expiry and matching private key, then
   atomically installs a combined PEM file in `/etc/sideband/tls`. Only root and
   container group 65532 can read it. The directory is mounted read-only; do not
   mount the entire Let's Encrypt directory or make private keys world-readable.
   Sideband's file watcher reloads new identities without dropping existing calls.
   The bundle is installed before `nginx -t` runs, so a broken unrelated NGINX
   site does not hold back the QUIC certificate; the hook then fails without
   reloading NGINX. The hostname check compares OpenSSL's verdict text, because
   OpenSSL 3.0 `x509 -checkhost` exits 0 on a mismatch. On renewal failure,
   investigate Certbot/hook logs; existing certificates still have an expiry date.
4. Start the container and install the additional NGINX site:

   ```sh
   docker compose -f deploy/docker-compose.nginx.yml pull
   docker compose -f deploy/docker-compose.nginx.yml up -d
   sudo install -m 0644 deploy/nginx-sideband.conf /etc/nginx/sites-available/sideband
   sudo ln -s /etc/nginx/sites-available/sideband /etc/nginx/sites-enabled/sideband
   sudo nginx -t && sudo systemctl reload nginx
   ```

5. Verify `curl --fail https://sideband.example.com/api/v1/server` and
   `curl --fail http://127.0.0.1:9101/healthz`. Open UDP 443 in the firewall and
   connect the desktop app to `sideband.example.com:443` with both client ports set to
   443. Public traffic is HTTPS at NGINX; its HTTPS upstream also validates the
   hostname and certificate chain. Keep metrics out of the NGINX site.

Updated clients use normal hostname/CA validation for public certificates; renewal
does not need fingerprint approval. Existing saved fingerprints migrate to strict
pins because their original approval method is unknown. Disconnect the server,
open its connection dialog, expand **Advanced connection options**, and choose
**Use domain certificate validation**. Both QUIC and HTTPS must validate before
the pin is replaced. An invalid public certificate never silently falls back to
self-signed approval. Older desktop builds retain the old strict-pinning behavior
and need updating; changing the Docker image alone does not change client trust.

NGINX and QUIC may present different valid certificates for the same hostname to
updated public-trust clients. This example shares one certificate for operational
simplicity. Self-signed/pinned deployments still require the same leaf on both
transports.

HTTP traffic reaches Sideband from the proxy address, so the server's per-address
limits (a 200-request burst refilled at 50 per second, 64 open connections) are
one budget shared by every proxied client; forwarded headers are not treated as
trusted client identities. The sample site therefore limits each client address
itself (10 requests per second with a burst of 40, 16 connections; NGINX answers
429 past that) and keeps a pool of reused upstream connections capped below the
server's connection limit. The shared budget remains the ceiling for all clients
together: a busy group can still see 429 from Sideband. Raise the NGINX limits
only together with that in mind.

## Firewall rules

Open the voice port. Forgetting UDP is the usual outage: clients look
connected to TCP and never hear anyone.

```sh
sudo ufw allow 443/udp
sudo ufw allow 443/tcp
```

ACME with `challenge = "http-01"` also needs TCP 80. Other TLS modes do
not open it, so the default Compose file does not reserve that host port.
For HTTP-01, configure ACME and add the optional port mapping:

```sh
docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.acme-http.yml up -d
```

Use both files for subsequent Compose operations on that deployment. TCP 80 must
be available on the host. If an existing web server owns it, route the ACME
challenge through that server or use another TLS mode instead of stopping it.
The default deployment still requires both TCP and UDP 443 to be available.

Leave `9101` on loopback. `/healthz` and `/metrics` are not public.

```sh
curl -fsS http://127.0.0.1:9101/healthz
curl -fsS http://127.0.0.1:9101/metrics
```

## UDP buffers

QUIC drops bursts when the socket buffers stay at the kernel default.
Set both directions and keep the setting across reboot:

```sh
sudo tee /etc/sysctl.d/80-socialspeak.conf >/dev/null <<'EOF'
net.core.rmem_max = 2500000
net.core.wmem_max = 2500000
EOF
sudo sysctl --system
```

## TLS

`bind_http` is an HTTPS listener. It serves the same live certificate as
QUIC. Pinned clients require that leaf on both transports. Public-certificate
clients independently validate each transport for the configured hostname. The internal health and metrics listener stays plain HTTP on
loopback.

`self-signed` is the default. `ss-server tls rotate` replaces
`tls/identity.json`. A running server loads the new leaf without closing
the QUIC or HTTPS socket, and pinned clients re-prompt on their next handshake. Public-certificate clients
accept valid domain-certificate renewals automatically.

`files` uses `cert_path` and `key_path` (PEM, leaf first). Replacing
either file reloads the certificate for new QUIC and HTTPS handshakes.
Existing handshakes stay up.
Relative paths are inside the data directory.

`acme` orders a certificate for `public_host`. Set `acme_email`.
`challenge = "http-01"` always opens TCP 80 for
`/.well-known/acme-challenge/` and redirects every other request to the
HTTPS `bind_http` port. Do not set `bind_http` itself to port 80. The
legacy `http_redirect` setting remains accepted for old config files but
is no longer needed.
`challenge = "tls-alpn-01"` also accepts TCP on the QUIC port for
`acme-tls/1`. QUIC still needs UDP. The cache is `tls/acme/`. The
issued leaf is used by both QUIC and HTTPS.

## Backup and restore

`ss-server backup` is consistent while the server is running. The tar
contains `socialspeak.db` and `attachments/`. It does not contain
`config.toml` or `tls/` (the identity pinned clients trust); keep copies of
those with the tar.

### systemd

The unit's dynamic user is named after the unit, `socialspeak`, and owns
`/var/lib/socialspeak` (really `/var/lib/private/socialspeak`). Run backup
and restore as that same user with `systemd-run`, so they share the running
server's UID:

```sh
sudo systemd-run --wait --pipe --collect \
  --property=User=socialspeak \
  --property=DynamicUser=yes \
  --property=StateDirectory=socialspeak \
  --property=UMask=0077 \
  /usr/local/bin/ss-server backup /var/lib/socialspeak/socialspeak.tar \
  --data-dir /var/lib/socialspeak
sudo cat /var/lib/socialspeak/socialspeak.tar > socialspeak.tar
sudo rm /var/lib/socialspeak/socialspeak.tar
```

`User=socialspeak` matters. Without it the transient unit gets its own
dynamic user, named after `run-u….service`, and systemd changes the owner of
a `StateDirectory=` to the unit's user when the unit starts: the whole state
directory would be chowned to that transient UID under the running server.
If you renamed the unit file, use its name instead of `socialspeak`.

Restore only while the server is stopped. The tar is streamed on standard
input, and `tar` runs as the service user, so every restored file belongs to
it (a root `tar` would leave `socialspeak.db` owned by root, and the server
could not write it). The WAL files are removed first so SQLite does not
replay them over the snapshot:

```sh
sudo systemctl stop socialspeak
sudo systemd-run --wait --pipe --collect \
  --property=User=socialspeak \
  --property=DynamicUser=yes \
  --property=StateDirectory=socialspeak \
  --property=UMask=0077 \
  /bin/sh -c 'cd /var/lib/socialspeak && rm -f socialspeak.db-wal socialspeak.db-shm && exec tar -xf -' \
  < socialspeak.tar
sudo systemctl start socialspeak
```

If a restore was already extracted as root, give the files back to the
directory's owner while the server is stopped:
`sudo chown -R --reference=/var/lib/private/socialspeak /var/lib/private/socialspeak`.

### Docker

While the container is up:

```sh
docker compose -f deploy/docker-compose.yml exec socialspeak \
  /ss-server backup /data/socialspeak.tar --data-dir /data
docker compose -f deploy/docker-compose.yml cp socialspeak:/data/socialspeak.tar ./socialspeak.tar
```

The copy in the volume stays until the next backup replaces it. The image
has no shell or `tar`, so restore through a throwaway container that mounts
the same volume and runs as the server's uid 65532:

```sh
docker compose -f deploy/docker-compose.yml stop socialspeak
docker run --rm -i --network none --user 65532:65532 \
  --volumes-from "$(docker compose -f deploy/docker-compose.yml ps -aq socialspeak)" \
  alpine:3.22 \
  sh -c 'cd /data && rm -f socialspeak.db-wal socialspeak.db-shm && exec tar -xf -' \
  < socialspeak.tar
docker compose -f deploy/docker-compose.yml start socialspeak
```

Use `-f deploy/docker-compose.nginx.yml` throughout for the NGINX variant.

## Upgrade

Migrations run on the next start.

Binary: replace `/usr/local/bin/ss-server`, then
`sudo systemctl restart socialspeak`.

Image: `docker compose -f deploy/docker-compose.yml pull`, followed by
`docker compose -f deploy/docker-compose.yml up -d`.
The volume keeps the database.

## Next to TeamSpeak

TeamSpeak's default voice port is UDP 9987. Sideband's is UDP 443.
They can share a host. If 443 is already taken, set `bind_quic` (or
`SS_BIND_QUIC`) to another UDP port, publish that port, and open it in
the firewall. Point clients at the port you chose. Do not move
Sideband onto 9987 just to share a number; the protocols are not
interchangeable.

## Smoke test

`deploy/smoke.sh` builds the image, waits for `/healthz`, verifies the
public endpoint answers HTTPS and refuses plaintext HTTP, then runs
`ss-cli connect --bots 2` against a second container started from the
same image with the explicit `spike` subcommand. That is the unauthenticated
voice spike. `ss-cli` still speaks that spike; it does not log into
`ss-server run`.

It then starts `docker-compose.yml` and `docker-compose.nginx.yml` as
shipped, replacing only the host ports and (for NGINX) the certificate
directory with a throwaway self-signed bundle. Each must become healthy,
answer `/healthz` and HTTPS, and run `ss-server conn list` in the container.
Every run starts from new volumes and removes its containers, volumes and
log afterwards. Override the host ports with `SMOKE_HEALTH_PORT`,
`SMOKE_QUIC_PORT`, `SMOKE_SPIKE_PORT`, `SMOKE_COMPOSE_HEALTH_PORT` and
`SMOKE_COMPOSE_PORT` when the defaults are taken.

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
During development, rerunning a version replaces its image. The workflow also
publishes `sha-<full-commit>` tags; use an image digest for immutable deployment.

The official `sideband-server` package is public and can be pulled without login.
For a fork/new package, GitHub initially defaults to private: set the package
visibility to **Public** in its GitHub package settings for anonymous pulls.
If keeping it private, authenticate to GHCR on the server with a token that has
`read:packages` before pulling.

The container runs as uid 65532. Compose publishes UDP and TCP 443, and
publishes the metrics port only on `127.0.0.1:9101`. The data volume is
`/data` inside the container. `docker logs` shows the owner invite and
the fingerprint.

For an optional local source build:

```sh
docker build -f deploy/Dockerfile -t socialspeak:local .
SIDEBAND_IMAGE=socialspeak:local docker compose -f deploy/docker-compose.yml up -d
```

To publish a new image, manually run **Release server**, selecting the source
branch and version. It compiles once on each architecture's native runner,
checks static linkage, smoke-tests the non-root runtime and publishes the combined
image only after both builds succeed. Nothing runs on pushes or pull requests.

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
   The hook reloads NGINX after validating its configuration. On renewal failure,
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
transports. HTTP traffic reaches Sideband from the proxy address: existing
server-side per-IP limits are shared across proxied clients; forwarded headers
are not treated as trusted client identities.

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
contains `socialspeak.db` and `attachments/`. The dynamic user owns
`/var/lib/socialspeak`, so run the backup with that identity and write
the tar inside the state directory, then copy it off the machine:

```sh
sudo systemd-run --wait --pipe --collect \
  --property=DynamicUser=yes \
  --property=StateDirectory=socialspeak \
  --property=UMask=0077 \
  /usr/local/bin/ss-server backup /var/lib/socialspeak/socialspeak.tar \
  --data-dir /var/lib/socialspeak
```

Restore only while the process is stopped, and delete the WAL files so
SQLite does not replay them over the snapshot:

```sh
sudo systemctl stop socialspeak
sudo tar -C /var/lib/socialspeak -xf /var/lib/socialspeak/socialspeak.tar
sudo rm -f /var/lib/socialspeak/socialspeak.db-wal /var/lib/socialspeak/socialspeak.db-shm
sudo systemctl start socialspeak
```

Docker, while the container is up:

```sh
docker compose -f deploy/docker-compose.yml exec socialspeak \
  /ss-server backup /data/socialspeak.tar --data-dir /data
```

Copy that tar out of the volume. To restore, stop the container, extract
the tar into the volume, delete any `socialspeak.db-wal` and
`socialspeak.db-shm`, and start again.

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
same image with no subcommand. That entrypoint is the unauthenticated
voice spike. `ss-cli` still speaks that spike; it does not log into
`ss-server run`.

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

From the repository:

```sh
docker compose -f deploy/docker-compose.yml up -d --build
docker compose -f deploy/docker-compose.yml logs
```

The container runs as uid 65532. Compose publishes UDP and TCP 443, and
publishes the metrics port only on `127.0.0.1:9101`. The data volume is
`/data` inside the container. `docker logs` shows the owner invite and
the fingerprint.

To build without compose: `docker build -f deploy/Dockerfile -t socialspeak .`

## Firewall

Open the voice port. Forgetting UDP is the usual outage: clients look
connected to TCP and never hear anyone.

```sh
sudo ufw allow 443/udp
sudo ufw allow 443/tcp
```

ACME with `challenge = "http-01"` also needs TCP 80. Other TLS modes do
not open it.

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
QUIC, so the fingerprint printed by `run` and `fingerprint` pins both
transports. The internal health and metrics listener stays plain HTTP on
loopback.

`self-signed` is the default. `ss-server tls rotate` replaces
`tls/identity.json`. A running server loads the new leaf without closing
the QUIC or HTTPS socket, and clients re-prompt on their next handshake.

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

Image: `docker compose -f deploy/docker-compose.yml up -d --build`.
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

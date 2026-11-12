# Self-hosting guide

How to run a Sideband server for your group on a Linux VPS, from choosing an
install method to fixing the usual "connected but nobody hears anything"
problem.

This guide explains and decides; it does not repeat commands that live
elsewhere. The exact install, TLS, backup and upgrade commands are in
[deployment](../deploy/README.md), and every server command describes itself
with `ss-server <command> --help`.

> **Status.** Sideband is a development preview. Nobody outside the project
> has yet followed this guide on a fresh VPS; that check is open in the
> [acceptance matrix](plan/tasks/ACCEPTANCE.md). The backup and restore
> procedure and the NGINX renewal hook have also not been run on a real VPS
> yet (T-098). Report anything that does not work as written.

## 1. What you need

- **A Linux server** with a public address, x86_64 or arm64. Release binaries
  are static musl builds and the container image is published for both
  architectures, so the distribution does not matter. The deployment examples
  assume Ubuntu with systemd or Docker.
- **UDP reachable from the internet.** Voice and the control channel are QUIC,
  which is UDP only. There is no TCP fallback.
- **Optionally a domain name**, if you want a publicly trusted certificate
  instead of telling your friends a fingerprint (section 5).
- **Sizing.** There is no measured minimum yet. Local load runs are in the
  [load test notes](plan/notes/load-test.md); the full-length VPS runs are
  still owed. The server forwards encoded audio and never decodes it, so CPU
  use is low for a friend group.

Running the server on macOS from a source checkout works for development
(see the [README](../README.md#quick-start)). Windows is not a server target.

## 2. Pick an install method

| Method | When | Instructions |
|---|---|---|
| Docker Compose | Docker is installed, or you want upgrades to be `pull` and `up` | [Docker Compose](../deploy/README.md#docker-compose) |
| Binary and systemd | No Docker; one static file and a hardened unit | [Binary and systemd](../deploy/README.md#binary-and-systemd) |
| Compose behind host NGINX | The host already runs NGINX on TCP 443 with a domain certificate | [Host NGINX](../deploy/README.md#host-nginx-and-a-public-domain-certificate) |
| From source | Development and testing | [README quick start](../README.md#quick-start) |

Release binaries and images carry build provenance. Check them before
installing: [verify a download](release.md#verify-a-download).

Whichever you choose, also apply the
[UDP buffer settings](../deploy/README.md#udp-buffers). Without them QUIC
drops bursts of packets under load.

## 3. Ports

| Purpose | Protocol | Setting (`config.toml`) | Environment | Written by `init` | Compose files | Public? |
|---|---|---|---|---|---|---|
| Voice and control (QUIC) | UDP | `server.bind_quic` | `SS_BIND_QUIC` | `0.0.0.0:443` | `0.0.0.0:443` | Yes |
| HTTPS API: history, uploads, server info | TCP | `server.bind_http` | `SS_BIND_HTTP` | `0.0.0.0:4430` | `0.0.0.0:443` | Yes |
| Health, metrics, connection list | TCP, plain HTTP | `internal.bind` | `SS_INTERNAL_BIND` | `127.0.0.1:9101` | published on host `127.0.0.1:9101` | **Never** |
| ACME HTTP-01 challenge | TCP | only with `tls.mode = "acme"` and `challenge = "http-01"` | | port 80 | optional override file | Yes, while used |

Two defaults trip people up:

- **The systemd unit serves HTTPS on TCP 4430**, because that is what
  `ss-server init` writes, while the firewall commands in the deployment guide
  open TCP 443. Either put `SS_BIND_HTTP=0.0.0.0:443` in
  `/etc/socialspeak.env` (then QUIC and HTTPS share port 443, as in the Compose
  files), or open TCP 4430 and have everyone enter `4430` as the HTTPS port in
  the app's **Advanced connection options**.
- **The desktop app assumes port 5781** when an address has no port. Always
  give people the full `host:port`, for example `voice.example.com:443`.

To use other ports (for example next to a web server or TeamSpeak), see
[next to TeamSpeak](../deploy/README.md#next-to-teamspeak). Open the ports in
the [host firewall](../deploy/README.md#firewall-rules) **and** in your cloud
provider's firewall or security group, which is a separate layer on most VPS
providers. Never publish the internal listener or run the `spike`
subcommand on a public address: the spike lets anyone join without logging
in.

## 4. First start and the owner account

1. Install with the method you picked. On first start the server writes
   `config.toml`, the database and a TLS identity into its data directory,
   then prints two lines to its log: `owner invite: …` and `fingerprint: …`.
   Read them with `sudo journalctl -u socialspeak` or `docker compose logs`.
   Until someone registers, every start prints the same owner invite again.
2. Check the server answers. On the server itself:
   `curl -fsS http://127.0.0.1:9101/healthz`. From outside:
   `curl -k https://<host>:<https port>/api/v1/server` returns the server name
   and certificate fingerprint (`-k` because a self-signed certificate is not
   publicly trusted; compare the fingerprint instead).
3. In the desktop app, add the server, compare the fingerprint the app shows
   with the one in the log, and **Create account** with the owner invite. The
   first account gets the seeded **Admin** role. See the
   [user guide](user-guide.md#add-a-server).
4. Create invites for everyone else in the app (**Invites**), and send each
   person the address, their invite and the fingerprint over a channel you
   already trust. The fingerprint lets them detect someone in the middle
   ([security §3](security.md#3-tofu-what-it-protects-and-what-it-does-not)).
5. When everyone has joined, you can set registration to **closed** in
   **Server settings**. Existing accounts, sessions and invites keep working;
   new accounts cannot be created.

Lost the owner invite before anyone registered?
`ss-server invite create --owner` prints a new one; it fails once any account
exists.

### Running server commands

Every command takes `--data-dir`, which must be the server's own data
directory. List them with `ss-server --help`.

- **Docker:** run the binary inside the container, for example
  `docker compose -f deploy/docker-compose.yml exec socialspeak /ss-server user list --data-dir /data`.
- **systemd:** the unit's files belong to a dynamic user, so run commands as
  that same user, the way the
  [backup procedure](../deploy/README.md#systemd) does:

  ```sh
  sudo systemd-run --wait --pipe --collect \
    --property=User=socialspeak --property=DynamicUser=yes \
    --property=StateDirectory=socialspeak --property=UMask=0077 \
    /usr/local/bin/ss-server user list --data-dir /var/lib/socialspeak
  ```

  Running them directly as root can leave database files the server cannot
  write. This pattern is the deployment guide's, applied to other commands;
  it has not been run on a VPS yet.

Account recovery and moderation from the command line:

| Task | Command | Note |
|---|---|---|
| Reset a password | `ss-server user set-password --username … --password …` | Also ends that account's sessions |
| See or end sessions | `ss-server user sessions list` / `revoke --id … \| --all` | A running server closes the connections within 30 seconds |
| Make someone an administrator | `ss-server role grant-admin --username …` | Takes effect after a server restart; in the app it is immediate |
| Ban or unban | `ss-server user ban` / `unban --username …` | A live session stays connected until restart; banning from the app disconnects at once |
| Rebuild message search | `ss-server reindex-search` | See [message search](message-search.md#rebuild-the-index) |

## 5. Certificates

Pick one `tls.mode`:

| Mode | Use it when | What clients do |
|---|---|---|
| `self-signed` (default) | No domain, or you are happy to share a fingerprint | Show the fingerprint on first connect and pin it; warn loudly if it ever changes |
| `acme` | The server has a DNS name pointing straight at it, and TCP 80 (HTTP-01) or the QUIC port on TCP (TLS-ALPN-01) is free | Validate the hostname and CA chain; renewals need no action |
| `files` | You already manage certificates, for example with Certbot behind NGINX | As `acme` when the certificate is publicly trusted |

Details of each mode, reloading and rotation are in
[TLS](../deploy/README.md#tls). How the app treats each kind of certificate,
and what pinning does and does not protect against, is in
[security §3](security.md#3-tofu-what-it-protects-and-what-it-does-not).

Things worth knowing before you start:

- With a self-signed certificate, the `tls/` directory **is** the server's
  identity. Back it up (backups do not include it) and keep it across
  reinstalls, or every client sees a "certificate changed" warning.
- `ss-server fingerprint` prints the current fingerprint at any time.
  `ss-server tls rotate` replaces a self-signed identity; every client is
  asked to trust the new one.
- Switching an existing server from self-signed to a domain certificate needs
  each client to opt in once: see the end of
  [host NGINX](../deploy/README.md#host-nginx-and-a-public-domain-certificate).

## 6. Configuration

`ss-server init` writes `config.toml` with a comment on every key, and the
comment at its top lists the `SS_*` environment variable that overrides each
one. That file is the reference; this guide does not copy it. A key the
server does not know is an error, so `ss-server check-config` catches typos
before a restart.

Settings people usually change:

- `server.name`: the initial server name. Administrators can rename the
  server in **Server settings**; a later edit of this key replaces that name
  on the next start.
- `server.public_host`: the DNS name, needed for `acme`.
- `limits.upload_max_bytes`: the largest attachment, 25 MiB by default.
- `previews.enabled`, `allow`, `deny`: link previews fetch pages from the
  server; see [security §9](security.md#9-link-previews-ssrf) before
  loosening them.
- `logging.json`: JSON log lines for a log collector. `RUST_LOG` sets the
  level (default `info`). Logs go to standard error and to daily files in
  `<data dir>/logs/`.

## 7. Backups, upgrades and monitoring

- **Backup and restore:** `ss-server backup` is safe while the server runs. It
  covers the database and attachments but not `config.toml` or `tls/`; copy
  those too. Procedures for both install methods:
  [backup and restore](../deploy/README.md#backup-and-restore).
- **Upgrade:** replace the binary or pull the image and restart; migrations
  run on start: [upgrade](../deploy/README.md#upgrade). Migrations only go
  forward, so the way back from a bad upgrade is the backup you took before
  it. The new binary's `ss-server check-migrations` exits non-zero when
  starting it would migrate the database, which is the moment to take that
  backup.
- **Monitoring:** `/healthz` and `/metrics` on the internal listener, a
  Grafana dashboard, and `ss-server conn list` for live per-connection RTT,
  loss and path MTU: [server diagnostics](../deploy/grafana/README.md).

## 8. Troubleshooting

Start with the server's own view: the log line
`listening quic=… https=… internal=…` shows the addresses it actually bound,
and `ss-server conn list` shows whether a client's QUIC connection arrived.

### The app cannot reach the server

| The app says | Likely cause |
|---|---|
| "The server did not respond in time" (after about 12 seconds) | UDP does not reach the server. The server check talks QUIC first. |
| "Cannot reach both the voice and HTTPS services" | The address or a port is wrong, or the HTTPS port is closed. Check the HTTPS port under **Advanced connection options** (section 3). |
| "HTTPS metadata and QUIC certificate do not match" | Something other than Sideband answers HTTPS, for example a proxy with its own certificate in front of a self-signed server. |
| "Server certificate changed" | The `tls/` directory was lost or rotated, or someone is intercepting. Confirm with the server's `ss-server fingerprint` before replacing the pin. |

### UDP is blocked

A firewall that lets TCP through but not UDP gives the classic symptom: HTTPS
answers, `curl` works, but the app never connects or nobody is heard.

1. On the server, check the socket is bound: `sudo ss -lunp | grep 443` (or
   your port).
2. Check both firewall layers: the host (`sudo ufw status`) and the cloud
   provider's firewall or security group.
3. Watch for packets while a client tries to connect:
   `sudo tcpdump -ni any udp port 443`. Nothing at all means the packets are
   dropped before the server; fix the firewall or the port forward.
4. If packets arrive from home but not from a particular network (work,
   school, hotel, some mobile carriers), that network blocks UDP or UDP 443.
   There is no TCP fallback. Some firewalls block only UDP 443 (to turn off
   HTTP/3 in browsers), so another QUIC port can help there.

### The server is behind NAT

On a home connection or behind a router, forward **both** the UDP (QUIC) and
TCP (HTTPS) ports to the server. Clients behind NAT need nothing: they open
the connection, and the app's keepalive every 15 seconds keeps the router's
mapping alive (the idle timeout is 30 seconds).

If people on the same LAN as the server cannot connect by its public name,
your router probably lacks NAT loopback (hairpinning). Use the server's LAN
address there. The app treats each `host:port` as a separate server with its
own saved session and trust, so add the LAN address as its own entry.

### MTU and VPNs

QUIC requires a network path that carries 1200-byte UDP packets; the
handshake is padded to that size. A tunnel with a smaller MTU (some VPNs,
nested tunnels, misconfigured PPPoE links) makes connecting hang or fail
while ordinary web browsing works. Above 1200 bytes the client and server
discover the path MTU themselves; there is nothing to configure. A single
audio frame is at most 1150 bytes, so it always fits once connected.

To check a live connection, compare the **path MTU** in the app's
diagnostics panel ([testing builds](testing-app.md#live-diagnostics)) or the
MTU in `ss-server conn list`. If connecting only fails through a VPN, test
without it or lower the VPN's own MTU settings so it can carry 1200-byte
packets.

### Choppy audio under load

Check the [UDP buffer sysctls](../deploy/README.md#udp-buffers) are applied
(`sysctl net.core.rmem_max`) and look for receive buffer errors with
`netstat -su`. Behind NGINX, HTTP 429 answers come from the shared per-address
budget described at the end of
[host NGINX](../deploy/README.md#host-nginx-and-a-public-domain-certificate).

### The service does not start

Read the last lines of the log (`sudo journalctl -u socialspeak -n 40` or
`docker compose logs`). `ss-server check-config` reports a bad key or value. If
a systemd service dies before logging a listening line, see the
`MemoryDenyWriteExecute` note in
[binary and systemd](../deploy/README.md#binary-and-systemd). "Address in use"
means another program holds the port: `sudo ss -lunp` and `sudo ss -ltnp` show
which.

### Next to TeamSpeak

The two can share a host as long as they use different ports; Sideband
defaults to UDP 443 and TeamSpeak to UDP 9987. See
[next to TeamSpeak](../deploy/README.md#next-to-teamspeak).

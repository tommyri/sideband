# ss-server

The server owns control dispatch, voice membership (`voice::Registry`),
opaque audio fan-out (`media::Router`), accounts (`auth`, `connection`),
permissions (`perms`), channels, presence, voice, text, direct messages, link previews, attachments, moderation, webhooks, and SQLite (`db`). Media is never decoded.

`ss-server init` writes `config.toml`, the database, and a TLS identity, then
prints the owner invite and fingerprint. `ss-server run` serves authenticated
QUIC and HTTP. `/healthz` and `/metrics` listen on `127.0.0.1:9101`. Ctrl-C
and SIGTERM notice clients, close them, and checkpoint SQLite. `check-config`
rejects an unknown key. `backup` tars the database and attachments.

With no subcommand, `--data-dir ./data --bind 127.0.0.1:4433` is still the
unauthenticated spike. `invite create --owner`, `user list`, `user set-password`,
`user ban`, `user unban`, `role grant-admin`, `fingerprint`, and `tls rotate` recover an account.
`--check-migrations` exits without listening. The data dir is
`$XDG_DATA_HOME/socialspeak` when that variable is set, otherwise `./data`.

One audio track is assigned per joined connection. Joins announce existing
tracks to the newcomer and its track to all channel members. Leave, switch,
and disconnect remove the old assignment. Mute/deaf flags affect forwarding.
The spike advertises mono 48 kHz Opus. Authenticated joins use the channel profile.

Sessions and outgoing control queues are bounded; a full 256-member bootstrap
fits before the writer resumes. Oversized Welcome replies return INVALID. Slow control consumers are
closed instead of losing track events. The media path reuses Bytes, validates
ownership, skips the sender and deaf peers, and counts invalid input and send
drops. It drops new packets when Quinn's bounded send queue is full.
Quotas live in `limits`, name rules in `text`, HTTP headers, deadlines, and
the accept loop in `http::hardening`; `docs/security.md` lists them all.

`cargo test -p ss-server` covers real loopback QUIC clients and zero steady
forwarding allocations. Run the ignored `sustained_twenty_clients` test with
`--release -- --ignored --nocapture` for a 30-second, 20-client load check.
To target a separate loopback server for CPU measurement, set `SS_LOAD_PORT`
and `SS_LOAD_FINGERPRINT` to its printed port and SHA-256 certificate pin.

May depend on `ss-core`, `ss-proto`, `ss-net`, Tokio, Quinn, and server-side
storage/HTTP/logging/CLI libraries. Must not depend on `ss-audio`, `ss-input`,
`ss-client-core`, or `ss-cli`.

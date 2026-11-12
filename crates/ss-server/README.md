# ss-server

The server owns control dispatch, voice (`voice::Voice`, seats in
`voice::rooms::Rooms`), opaque audio fan-out (`media::Router`), accounts
(`auth`, `connection`), permissions (`perms`), channels, presence, text, DMs,
search, previews, attachments, moderation, webhooks, bots (`handlers::bots`,
`http::bots`) and SQLite (`db`). Media is never decoded.

`init` writes `config.toml`, the database and a TLS identity, then prints the
owner invite and fingerprint. `run` serves authenticated QUIC and HTTPS;
`/healthz`, `/metrics` and `/connections` listen on `127.0.0.1:9101`.
`SS_WORKER_THREADS=N` (N > 1) runs N workers instead of one. Ctrl-C and SIGTERM
notify clients, close them and checkpoint SQLite. `check-config` rejects an
unknown key; `backup` tars the database and attachments.

A subcommand is required; with none, usage is printed and the exit code is 2.
`spike --bind 127.0.0.1:4433` is the unauthenticated spike. `invite create
--owner`, `user list|set-password|ban|unban|sessions`, `bot create|reset|…`,
`role grant-admin`, `fingerprint` and `tls rotate` manage access; `conn list`,
`reindex-search` and `check-migrations` never listen. The data dir defaults
to `$XDG_DATA_HOME/socialspeak`, else `./data`.

One audio track per joined connection; joins announce tracks both ways, and
leave, switch and disconnect remove it. Mute/deaf flags gate forwarding; joins
use the channel profile (the spike: mono 48 kHz Opus). `media::streams` relays
subscribed video GOPs with per-viewer drop-oldest budgets, cleanup on
permission or membership loss, private quality hints, and counters.

Sessions and control queues are bounded; a slow consumer is closed (code 2),
never sent a gap, and a Welcome sheds DMs to fit one frame. The media path
reuses Bytes, validates ownership, skips the sender and deaf peers, and counts
invalid input and full-queue drops. Quotas live in `limits`, name rules in
`text`, HTTP deadlines and the accept loop in `http::hardening`;
`docs/security.md` lists them all. Ignored test `sustained_twenty_clients`
(`--release -- --ignored`) is a 30 s load check; `SS_LOAD_PORT`/`_FINGERPRINT`
point it at another server.

May depend on `ss-core`, `ss-proto`, `ss-net`, Tokio, Quinn, and server-side
storage/HTTP/logging/CLI libraries; not on `ss-audio`, `ss-input`,
`ss-client-core` or `ss-cli`.

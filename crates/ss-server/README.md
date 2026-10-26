# ss-server

The server binary. Accounts, roles, channels, messages, presence, and the
voice router live here. Media payloads are forwarded, never decoded.

Runs as one static binary plus a data directory containing the SQLite
database, attachments, and the TLS certificate.

## May depend on

`ss-core`, `ss-proto`, `ss-net`, plus `tokio`, `axum`, `sqlx`, `tracing`,
and `anyhow`.

## Must not depend on

`ss-audio`, `ss-input`, `ss-client-core`, `ss-cli`. The server has no
microphone and no user interface.

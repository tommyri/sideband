# sideband-bot-example

An example bot on the native QUIC protocol. It signs in with a bot token
(`Credentials::Bot`) and answers `!echo <text>` and `!roll NdM` in the
channel the command came from. `src/lib.rs` holds the logic, `src/main.rs`
reads `SIDEBAND_HOST`, `SIDEBAND_PORT` (the QUIC port), `SIDEBAND_BOT_TOKEN`
and, for a self-signed server, `SIDEBAND_FINGERPRINT`:

```sh
SIDEBAND_HOST=chat.example.org SIDEBAND_BOT_TOKEN=… cargo run -p sideband-bot-example
```

`crates/ss-server/tests/bots.rs` runs it against a test server. How to create
a bot and what it may do: [`docs/bots.md`](../../docs/bots.md).

May depend on `ss-client-core`, `ss-net`, `ss-core`, Tokio, `getrandom` and `anyhow`;
never on `ss-server`.

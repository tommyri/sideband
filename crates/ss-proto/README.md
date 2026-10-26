# ss-proto

The wire schema. `.proto` files, generated `prost` types, control-stream
framing, and protocol-version constants. The schema is the public API that
bots will use later.

## May depend on

`ss-core`, plus `prost`, `prost-build`, and `bytes`.

## Must not depend on

`ss-net`, `ss-audio`, `ss-input`, `ss-client-core`, `ss-server`, `ss-cli`.
No sockets and no business logic.

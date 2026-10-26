# ss-cli

A terminal client for development. It places a real voice call, measures
mouth-to-ear latency, simulates packet loss, and spawns synthetic users for
load tests.

## May depend on

`ss-client-core` (and through it the client stack), plus `anyhow` and
`tracing` for the binary.

## Must not depend on

`ss-server`. It talks to a server over the network, the same way the
desktop client does.

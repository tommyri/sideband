# ss-cli

Terminal client for a voice call, mouth-to-ear measurement, packet impairment,
and synthetic senders.

`ss-cli connect host:port --name NAME --channel N`

`--channel` is a ULID or a decimal integer. Integers map to one stable id.
`--open-mic` and `--vad` select the transmit gate. Space toggles talk when the
terminal cannot report a key release.

`ss-cli load host:port --bots N --channels M --invite CODE` logs N users into
`ss-server run` and spreads them over voice channels `load-1`…`load-M`. Each
talks pre-encoded Opus in bursts (`--duty`, `--burst` seconds), posts to
`load-text` (`--text-rate` per minute), and with `--churn S` leaves and
rejoins voice. `--storm-every S` drops and reconnects every bot at once.
`--duration`, `--interval`, `--csv`, and `--ramp-ms` shape the run. Tokens go
to `--state`; later runs skip registration and the five-per-minute login
limit. With the owner invite on a fresh server the first bot creates the
channels and a shared invite. Relay delay uses the send clock in each
datagram timestamp, so it needs bots and receivers in one process.

## May depend on

`ss-client-core`, `ss-audio`, `ss-net`, `ss-proto`, and `ss-core`, plus
`anyhow`, `clap`, and `tracing` for the binary.

## Must not depend on

`ss-server` in the binary. It talks to a server over the network. Tests may
link the server as a dev-dependency.

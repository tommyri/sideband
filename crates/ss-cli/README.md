# ss-cli

Terminal client for a voice call, mouth-to-ear measurement, packet impairment,
and synthetic senders.

`ss-cli connect host:port --name NAME --channel N`

`--channel` is a ULID or a decimal integer. Integers map to one stable id.
`--open-mic` and `--vad` select the transmit gate. Space toggles talk when the
terminal cannot report a key release.

## May depend on

`ss-client-core`, `ss-audio`, `ss-net`, `ss-proto`, and `ss-core`, plus
`anyhow`, `clap`, and `tracing` for the binary.

## Must not depend on

`ss-server` in the binary. It talks to a server over the network. Tests may
link the server as a dev-dependency.

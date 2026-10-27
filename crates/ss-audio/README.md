# ss-audio

The real-time voice path: device capture and playback, resampling, Opus
encode and decode, the jitter buffer, the mixer, and the optional
processing chain (echo cancellation, noise suppression, gain control).

Callbacks that run on the audio thread do not allocate, lock, log, or
await. `unsafe` is confined to `platform` and the private `codec::native` bridge;
every block needs a `// SAFETY:` comment. The latter owns native decoder/DRED
state and supplies controls missing from the safe binding.

`codec` provides profile-configured 48 kHz encoders, stream decoders, and
packet framing metadata. Setup allocates; encode/decode reuse buffers.
Deep PLC is compiled through `opusic-c/osce` and decoder complexity 10;
bandwidth extension is explicitly off. Optional `dred` compiles redundancy
support, disabled at runtime until configured on the encoder.

Run `cargo test -p ss-audio --features dred` for optional recovery coverage;
`cargo bench -p ss-audio --bench codec` checks the 1 ms frame processing budget.

`jitter` admits bounded audio timing metadata, estimates delay, and releases
ordered packet/recovery decisions. `receive::TrackReceiver` emits stereo 10 ms
quanta with FEC/optional DRED/PLC and speaking state. `mixer` sums track gains
and UI sounds through a linked soft-knee limiter. These run on the playback
worker; T-010/T-041 will connect their output to the device ring.
`cargo test -p ss-audio --test sim -- --nocapture` reports network simulations.

## May depend on

`ss-core`, plus `cpal`, `rubato`, Opus bindings, and `rtrb`.

## Must not depend on

`ss-proto`, `ss-net`, `ss-input`, `ss-client-core`, `ss-server`, `ss-cli`.
No knowledge of users, channels, or the network.

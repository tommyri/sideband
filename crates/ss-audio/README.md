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

`device` opens capture and playback through `cpal`, resamples with rubato
when the hardware rate is not 48 kHz, and reports hot-plug and lost devices.
`jitter` admits bounded audio timing metadata and releases ordered packets.
`receive::TrackReceiver` emits stereo 10 ms quanta. `mixer` sums them. T-041
connects the mixer to the playback ring.
`cargo test -p ss-audio --test sim -- --nocapture` reports network simulations.

`vad` gates 48 kHz mono/stereo capture in 10 ms quanta, with pre-roll,
hangover, calibration, and a 50 Hz meter. Optional `rnnoise` adds speech
probability; audio stays unchanged. Construct/process on the capture worker.
Run `cargo test -p ss-audio --features rnnoise --test vad` for both modes.

## May depend on

`ss-core`, plus `cpal`, `rubato`, Opus bindings, `rtrb`, optional `nnnoiseless`.

## Must not depend on

`ss-proto`, `ss-net`, `ss-input`, `ss-client-core`, `ss-server`, `ss-cli`.
No knowledge of users, channels, or the network.

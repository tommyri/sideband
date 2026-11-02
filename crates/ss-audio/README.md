# ss-audio

The real-time voice path: device capture and playback, resampling, Opus
encode and decode, the jitter buffer, the mixer, voice activation, and the
optional processing chain (echo cancellation, noise suppression, gain).

Callbacks that run on the audio thread do not allocate, lock, log, or
await. `unsafe` is confined to `platform` and the private `codec::native` bridge;
every block needs a `// SAFETY:` comment. The latter owns native decoder/DRED
state and supplies controls missing from the safe binding.

`codec` provides profile-configured 48 kHz encoders, stream decoders, and
packet framing metadata. Setup allocates; encode/decode reuse buffers.
Deep PLC is compiled through `opusic-c/osce` at decoder complexity 10; bandwidth
extension is off. Optional `dred` compiles redundancy, off until configured.
`cargo test -p ss-audio --features dred` covers recovery;
`cargo bench -p ss-audio --bench codec` checks the 1 ms frame budget.

`device` opens capture and playback through `cpal`, resamples with rubato
when the hardware rate is not 48 kHz, and reports hot-plug, lost devices,
and capture that is only zeros (a denied macOS microphone looks like that).
On macOS, feature `macos-voiceio` adds one full-duplex Apple
VoiceProcessingIO unit. Its capture and playback rings have the same 48 kHz
contract, while the unit owns AEC/NS/AGC, muted-talker detection, and Control
Center mic-mode reporting. `ss-client-core` disables its software processing
chain while this backend is active.
`jitter` admits bounded audio timing metadata and releases ordered packets.
`receive::TrackReceiver` emits stereo 10 ms quanta. `mixer` sums them. T-041
connects the mixer to the playback ring.
`cargo test -p ss-audio --test sim -- --nocapture` reports network simulations.

`vad` gates 10 ms capture quanta (pre-roll, hangover, calibration, 50 Hz meter);
`rnnoise` adds speech probability. `processing::Chain` runs opt-in echo, noise,
and gain stages. Both run on the capture worker, never a callback; with all off
no sample is touched. Features `aec`, `rnnoise`; `deepfilter` is reserved (T-059).
`cargo bench -p ss-audio --all-features --bench processing` checks the budgets.

## May depend on

`ss-core`, `cpal`, `rubato`, Opus bindings, `rtrb`; optional `sonora` and
`nnnoiseless`; on macOS `objc2-avf-audio` for microphone permission and,
optionally, `coreaudio-rs`/AudioToolbox for VoiceProcessingIO.

## Must not depend on

`ss-proto`, `ss-net`, `ss-input`, `ss-client-core`, `ss-server`, `ss-cli`.

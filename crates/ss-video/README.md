# ss-video

Cross-platform source discovery, screen/window capture, scaling, and
low-latency H.264 encoding for Sideband.

The crate wraps `moq-video` as a native media library only. It does not use
MoQ transport, catalogs, relays, or subscriptions. Encoded Annex-B access
units leave through `Publisher::next`, tagged with their rendition;
`ss-client-core` owns Sideband's QUIC group transport.

`Encoders` is the encode stage: the preset's full rendition plus, from a
second hardware session, a 640×360 15 fps low rendition fed only while a
viewer is on it (`set_low_wanted`). It takes frames from capture or from a
caller, so measurements can drive it. `rate::RateController` is the
sender-wide AIMD bitrate control for the full rendition.

Default builds include native capture and OpenH264 fallback. Linux packages
can additionally enable `pipewire`, `vaapi`, or `nvidia`. macOS and Windows
hardware encoders are selected by their platform APIs automatically.

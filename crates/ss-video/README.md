# ss-video

Cross-platform source discovery, screen/window capture, scaling, and
low-latency H.264 encoding for Sideband.

The crate wraps `moq-video` as a native media library only. It does not use
MoQ transport, catalogs, relays, or subscriptions. Encoded Annex-B access
units leave through `Publisher::next`; `ss-client-core` owns Sideband's
QUIC group transport.

Default builds include native capture and OpenH264 fallback. Linux packages
can additionally enable `pipewire`, `vaapi`, or `nvidia`. macOS and Windows
hardware encoders are selected by their platform APIs automatically.

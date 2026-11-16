# Decisions

Each decision is binding for later tasks. A task that needs to break one
updates this file in the same change and says so in the task notes.

## D1 — Rust for the server and the client core, Tauri 2 for the shell

The server, the protocol, the audio graph, and the headless client are Rust
crates in one workspace. The desktop UI is React and TypeScript inside a
Tauri 2 webview. Capture, encode, the network connection, and push-to-talk
run in the Rust process. The webview never sees the microphone samples.

Tauri is the shell because one codebase has to ship macOS, Windows, and
Linux installers, and the audio path has to stay out of a browser engine.
Chromium's audio processing is part of why Discord sounds worse than
TeamSpeak, and a web client would inherit it.

## D2 — Our own QUIC transport

One QUIC connection per client, ALPN `socialspeak/1`, default port UDP 443.

- Control messages ride one bidirectional stream for the life of the
  connection. Framing is a big-endian `u32` length plus a protobuf envelope,
  capped at 1 MiB. See `docs/plan/02-protocol.md` §2–§3.
- Audio rides QUIC datagrams. The server forwards the datagram bytes. It
  does not decode them. The 1–3 byte track id is canonical unsigned LEB128,
  not a QUIC varint; see the exact encoding in protocol §4. Datagrams are
  unreliable and unordered; sequence numbers let the client reorder them.
- Screen video, when it exists, rides one unidirectional QUIC stream per
  group of pictures. See §5 of the protocol and the screen-share rules below.

QUIC is enough because every client connects to a server with a public
address. There is no peer-to-peer mesh, so there is no ICE, STUN, or TURN.
TLS 1.3, multiplexing, and a single UDP port come with the connection.

`quinn` is the QUIC implementation. `rustls` stays on 0.23 because 0.24 is
still a pre-release and `quinn` 0.11 uses 0.23.

Endpoints advertise an 8192-byte UDP receive ceiling and probe the path from
1200 bytes, growing only after acknowledged probes. Peer limits and QUIC
black-hole recovery remain active. This lets capable LAN/loopback paths
coalesce ready frames without an application timer; it does not increase the
1150-byte audio payload limit. macOS uses Quinn's batched UDP socket path.
The T-016 CPU measurement uses a loopback path that supports the larger MTU;
it is not a CPU guarantee for smaller-MTU or differently paced traffic.


## D3 — moq.dev is a library, not the transport

The voice and control transport is written in this repo. The moq.dev crates
(`moq-lite`, `hang`, `moq-relay`) are not a dependency of the server or of
the voice path.

`moq-video` is the capture and codec library for screen sharing, pinned
exactly in the workspace `Cargo.toml` (`=0.1.21` today; a bump is a reviewed
change, since 0.x minors break). Its capture, resize, conversion, encoder and
decoder APIs work independently of MoQ, so `ss-video` uses those APIs directly
and hands Annex-B H.264 access units to Sideband's own GOP transport.
`moq-video` is linked from the client, not from the server, `ss-net` or the
voice path.

Those APIs take a few value types from the MoQ crates, so `ss-video` also
depends on `moq-net` and `hang`, pinned exactly. Only these are allowed, and
only inside `ss-video` (plus `moq_net::Timestamp` in the desktop
`viewer_smoke` example, which builds frames for the viewer):

- `moq_net::Timestamp` and `moq_net::bandwidth::Rate`, the frame time and
  encoder bitrate that `moq-video` frames and encoder configs carry;
- `hang::catalog::VideoConfig` and `hang::catalog::VideoCodec`, the decoder
  configuration `moq-video`'s decoder is built from. `ss-video` fills them
  from `TrackAdded.codec`; a hang catalog is never sent or received.

No MoQ session, relay, broadcast, track, subscription or catalog transport
type is linked into the application protocol, and none of these types
appears in `ss-proto`, `ss-net` or `ss-client-core`'s public API. Any other
MoQ type is a change to this decision.

The relay and publish/subscribe model in that project buys nothing for a
single server, and the crates were at 0.12.x with breaking changes between
minor versions when this was decided. Voice has to stay on a small surface
we control: `quinn` plus libopus.

LiveKit was measured against this path for screen sharing (T-100,
2026-11-16) and not adopted; see the screen-sharing rules below.

## D4 — The server forwards opaque media

The server looks at the datagram header (version, kind, flags, track id,
sequence, timestamp) and at membership. The payload is copied through
unchanged.

Consequences that later tasks must preserve:

- Adding a video codec does not require a server release that understands
  that codec. Codec identity travels in `TrackAdded.codec`.
- Opus extensions inside the payload, including Deep REDundancy (DRED),
  survive forwarding. A selective forwarding server that drops silent
  packets must still forward a packet that carries DRED for the last talker.
- There is no server-side mix. Each listener decodes every speaker and mixes
  locally. That is what keeps per-user volume and mute local, and it is what
  avoids a second encode.

## D5 — Opus, libopus 1.6, three profiles

The codec is Opus through `opusic-c`, built against libopus 1.6.x. Opus is
what TeamSpeak 3 and Discord both use. The quality gap is configuration:
Discord sends 64 kb/s mono and applies noise suppression, automatic gain,
and echo cancellation that the user cannot fully disable. TeamSpeak exposes
the Opus rate and leaves processing off unless the user asks for it.

We take the TeamSpeak side of that split. Profiles, bitrates, and the
encoder contract are specified in [voice.md](voice.md). Deep packet-loss
concealment is on. DRED is a build flag, off unless the user opts in.
Neural codecs (Lyra, EnCodec, DAC, ZipCodec) are not used for voice. The
measurements behind that are in `voice.md`.

`VoiceProfile::encoder_params` in `ss-core` is the code form of the profile
table. The protocol document is the wire form. Change them together.

## D6 — Processing is opt-in and off by default

Echo cancellation, noise suppression, and automatic gain control ship as a
chain the user turns on. The default capture path is samples in, Opus out.

The cross-platform chain is `sonora` (a pure-Rust port of the WebRTC audio
processing module: AEC3, noise suppression, AGC2). Noise suppression can
also be RNNoise or DeepFilterNet, selected in settings. On macOS, an
alternative mode uses Apple's `AUVoiceIO` and then the rest of the chain
stays off, because that unit already includes Apple's canceller and
suppressor.

This is a product decision, not a quality ceiling. Headphones do not need
the chain. Speakers do. Forcing it on everyone is the Discord sound.

## D7 — Device I/O

`cpal` 0.18 or newer, with its native PipeWire backend on Linux.

| OS | Path | Why this one |
|---|---|---|
| Windows | WASAPI shared mode | `IAudioClient3` low-period shared streams match exclusive-mode latency and still mix with the game. Exclusive mode silences every other application. Set the communications category so Windows treats the stream as a call. |
| macOS | CoreAudio through `cpal` | `AUVoiceIO` is a separate, opt-in mode (D6), not the default device path. |
| Linux | PipeWire, then PulseAudio, then ALSA | `cpal` selects that order at runtime. Request a quantum aligned to the Opus frame (`480/48000` is 10 ms). |

Bluetooth headsets that switch the microphone to the hands-free profile
(8 or 16 kHz) are an operating-system limit. Detect that rate and surface
it. Do not hide it inside the encoder.

Capture and playback clocks differ when the microphone and the headphones
are different devices. Shared-mode mixing resamples to the engine rate.
`rubato` covers a device that is not already at 48 kHz. The jitter buffer
absorbs the residual drift.

## D8 — Push-to-talk listens; it does not register a hotkey

The Tauri global-shortcut plugin, Win32 `RegisterHotKey`, and Carbon
`RegisterEventHotKey` consume the key. A game then never sees it, and mouse
side buttons cannot be bound. Those APIs are the wrong tool for
push-to-talk while gaming.

`ss-input` owns a binding and the press/release state machine. A binding is
a USB HID key code or a mouse button (`Left`, `Right`, `Middle`, `Mouse4`,
`Mouse5`) plus optional modifiers. Either side of a modifier counts.
`Backend::detect` selects Windows Raw Input (T-048), a listen-only macOS
event tap (T-049), or Linux XInput2/GlobalShortcuts (T-050). Linux evdev
requires explicit opt-in through `Backend::detect_linux`; unsupported
sessions report unavailable. These backends are not cargo features. Raw
listeners never consume input; portal shortcuts follow compositor policy.

The machine, merged into `main` with T-047:

- Debounce is 30 ms (`DEBOUNCE`). A release followed by a press at or before
  that gap stays transmitting and does not emit `Released`. The wait before
  a release is `max(release_delay, 30 ms)`, so a configured delay of zero
  still swallows chatter.
- The default release delay is 150 ms (`DEFAULT_RELEASE_DELAY`), the same
  default T-041 uses for the voice session. It is configurable.
- `force_release` closes the gate on focus loss, sleep, or a disabled event
  tap, and ignores further downs of keys that were held until the matching
  up. A backend that already sees the key up must inject that up.
- `capture_next` returns `CapturedBinding`. When the compositor owns the
  shortcut it returns `SetBySystem` and does not watch a key.

Each operating system gets a backend that reports edges and lets the event
continue:

| OS | Backend | Pass-through | Permission |
|---|---|---|---|
| Windows | Raw Input with `RIDEV_INPUTSINK`, low-level hooks as fallback | Yes | A game running as administrator is invisible to a non-elevated listener. Accept that. An `uiAccess` manifest needs a signed install and is deferred. |
| macOS | Listen-only `CGEventTap` | Yes | Input Monitoring, not Accessibility. Re-enable the tap if the system disables it, and release a stuck bind. |
| Linux X11 | XInput2 raw events | Yes | None |
| Linux Wayland | XDG GlobalShortcuts portal | Compositor-dependent | One consent dialog. KDE Plasma 5.27+, GNOME 48+, Hyprland. The compositor chooses the key. |
| Linux, portal missing | evdev on `/dev/input`, off unless the user opts in | Yes | Membership of the `input` group. Say plainly that this can read every key. |
| Sway, wlroots, COSMIC without a portal | No global bind | — | Voice activation is the default there. Say why in settings. |

If a backend's experience is bad enough that people miss the key, ship
voice activation on that OS and leave the bind unimplemented. Do not ship a
bind that swallows keys inside a game.

## D9 — SQLite

One database file in the server data directory, `sqlx` 0.9, WAL mode,
embedded migrations. `sqlx` 0.9 requires Rust 1.94; the toolchain is pinned
to 1.99.1 in `rust-toolchain.toml`. The `sqlx` workspace dependency enables
only `sqlite`, `runtime-tokio`, and `migrate`.

A friend group does not need Postgres. Backup is a copy of the data
directory.

## D10 — Trust on first use, ACME when there is a domain

The server generates a self-signed certificate on first start and prints its
SHA-256 fingerprint. The client stores that fingerprint and refuses a later
connection whose certificate does not match, until the user accepts the new
one.

A server whose certificate chains to a public CA (ACME, or operator PEM
files from a public CA) gets a different saved policy: public-certificate
trust (`ss_net::pins::TrustPolicy::PublicCertificate`). Each handshake checks
hostname, validity and chain with WebPKI and Mozilla roots, so renewals and
key rotation are accepted without a prompt; a failed check never falls back
to trust on first use. An explicit pin stays stricter than public trust and
is never silently replaced: legacy pin files migrate to strict pins, and
moving a pinned server to public trust is an explicit action that validates
both transports first. The full policy is in `docs/security.md` section 3.

ACME is an option in the server config for a host that has a domain name.
The same certificate serves QUIC and HTTPS.

T-004 stores the generated DER certificate and PKCS#8 private key together
in `tls/identity.json`, installed atomically with owner-only permissions on
Unix. A corrupt identity fails startup rather than silently changing the pin.
Pins identify the exact leaf certificate DER by SHA-256 and are scoped to
the original host and port. A matching explicit pin overrides CA/name checks,
but TLS handshake signatures are always verified. Unpinned certificates use
WebPKI with Mozilla roots; an accepted public certificate is recorded as a
public-certificate policy before returning the connection. Any later pin
change needs explicit approval.

T-040 reuses that verifier for HTTPS history. The trust policy of the
authenticated QUIC session (pin or public certificate) covers HTTPS on the
same host, including a separately configured TCP port. History
never performs a second trust-on-first-use decision, follows redirects, or
uses environment proxies. The host owns session-token persistence separately
from `PinStore`, so it can use an OS keychain without imposing one on the core.

## D11 — Protobuf is the control API

Control messages are protobuf, generated with `prost` at build time.
`protoc` is vendored through `protoc-bin-vendored`. Fields are only added.
Tags are never reused. The same schema is what a later bot API exposes as
JSON.

Wire enum numbers are not the SQL integer codes in `03-data-model.md`.
Proto3 uses 0 as unspecified.

## D12 — Roles, moderation, DMs, previews, and incoming webhooks are in the MVP

Permissions are a `u64` bitset in `ss-core`, resolved with role overrides
and a hierarchy check. The algorithm is in `docs/plan/03-data-model.md`.
Kick, ban, server mute, and move are server commands with an audit row.
Direct messages are channels with a membership list and the same message
pipeline as text channels. Link previews are fetched by the server, with
SSRF checks, so a link target sees the VPS and not a member's home address.
Incoming webhooks are a POST that creates a message. The full bot API is
later. It should be a client with a bot token, not a second protocol.

## Review decisions of 2026-11-12

Answers to the questions the review waves left open (T-106):

- A ban keeps the member in every member list, and their messages keep
  their name. `MemberLeft` stays reserved for a future account removal.
- Administrators are peers. There is no owner above them.
- On Linux the voice thread asks rtkit over D-Bus for real-time priority,
  as macOS and Windows already get (T-117).
- The capture input picker shows only the inputs a device has, and a
  choice other than Input 1 opens the device's full channel count (T-118).
- Push-to-talk arms only from a release: a key already held when a call
  starts does not transmit until it is pressed again.
- Windows push-to-talk uses Raw Input only. The low-level hook fallback is
  waived: such hooks are often flagged by anti-cheat and antivirus.
- Bot tokens do not expire. Resetting or revoking is the only way to end
  one (T-080).
- Screen sharing: the LiveKit experiment in T-100 runs before the backend
  is chosen. Until then the rules below stand. Decided on 2026-11-16: see
  rule 7 below.

## Screen sharing must fit the transport we have

These rules exist so phase 5 does not require a new voice protocol:

1. Media is a track with a kind (`audio`, `video`, `screen`), a server-assigned
   track id, and a codec description. Voice is not a special packet type
   beyond `kind = audio`.
2. Voice is delivered to every member of the channel who is not deafened.
   Video is delivered only to clients that subscribed.
3. The server keeps a per-subscriber send queue for video and drops the
   oldest group when the queue is over budget. A slow viewer must not stall
   anyone's audio.
4. A viewer can ask the sender for a keyframe and for a maximum height and
   frame rate. The control messages exist before any encoder does.
5. Timestamps share the 48 kHz clock used by audio, so a viewer can align
   a stream with the speaker's voice.
6. The decoded picture is drawn in a native window. The webview is not the
   video surface. WebCodecs inside the webview is unreliable on Linux.
7. Screen sharing stays on this transport. LiveKit was measured and not
   adopted (T-100, 2026-11-16).
   - On macOS it added 30–90 ms of latency and lost two thirds of the frame
     rate on full-motion content with simulcast. It took over 30 s to bring
     a recovered viewer back to full quality.
   - Its costs: a second service with TURN, libwebrtc with statically linked
     LGPL ffmpeg, tokens that self-hosted LiveKit does not revoke, and no
     shared clock with voice (rule 5).
   - A sharer may publish a second, lower rendition. The server picks one
     rendition per subscriber, switching only at a keyframe group, and
     never transcodes (D4). T-074 builds it.
   - Reopen the question if Windows game capture fails 1080p60 through
     moq-video while libwebrtc holds it.

The first video codec is H.264, hardware-encoded, no B-frames, low-latency
rate control. AV1 and HEVC can be added later as other `Codec.name` values
without a server change.

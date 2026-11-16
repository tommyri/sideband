# Screen sharing: Discord and TeamSpeak

Research date: 2026-11-16. Recommendation only; no task or status change.

The question: should the Sideband server receive one stream and make the
lower qualities itself, or should the sharer keep encoding them as
[T-074](../tasks/T-074-adaptive-bitrate.md) plans? This note records what
Discord and TeamSpeak publish about their own screen sharing. It uses their
blogs, developer docs, help-centre pages, changelogs, staff posts and
published code only. Every link was read on 2026-11-16.

Dates: blog posts and changelogs carry their own dates, given inline.
Help-centre pages show only a relative "updated" age, so their dates are
approximate. Discord's developer docs, teamspeak.com and the TeamSpeak
server README and CONFIG are undated living pages. Nothing was built or
measured for this note. Text marked **Our reading** is this note's
reasoning, not something a source says.

## Summary

- **Neither product documents server-side transcoding of screen shares.**
  Discord's server is a forwarding SFU, and since March 2026 it cannot read
  the media at all: every Go Live stream is end-to-end encrypted with DAVE.
  TeamSpeak 6 has no server in the media path yet: screen sharing is
  peer-to-peer WebRTC from the sharer to each viewer.
- **Neither documents simulcast, SVC or a per-viewer quality level.**
  Discord's 2024 overview describes the opposite: one encode, kept within
  what the slowest viewer can take. Whether that is still true is not
  documented. TeamSpeak sends one encode to every viewer, and had to fix a
  bug where one stalled viewer froze the stream for all of them.
- **Codecs:** Discord picks one that the streamer can encode and every
  viewer can decode (VP8, H.264, plus HEVC and AV1 on some platforms).
  TeamSpeak says only that it picks the best codec for the viewers (H.264
  and AV1 in use; VP9 mentioned by staff).
- **For Sideband:** the findings support D4 (no transcoding) and the
  per-viewer queue. They do not show a competitor doing what T-074 does,
  because neither documents per-viewer quality at all. Discord's documented
  2024 behaviour is the slowest-viewer problem T-074 exists to avoid.
  Nothing here confirms or challenges the 360p15 level or the
  keyframe-boundary switch.

## Discord

### Transport and encryption

- In 2018, desktop and mobile clients used one C++ media engine built on
  the native WebRTC library. Native clients skipped SDP and ICE: every
  client connected to a Discord media relay, which also hid client IP
  addresses from each other. Browsers used standard SDP, ICE, DTLS and
  SRTP.
  [Discord engineering, 2018-09-10](https://discord.com/blog/how-discord-handles-two-and-half-million-concurrent-voice-users-using-webrtc)
- Go Live uses Discord's own capture and encoding code, with WebRTC as the
  transport from streamer to viewer.
  [AMD encoder post, 2024-05-01](https://discord.com/blog/from-blocky-to-brilliant-improving-video-quality-on-discord-go-live-on-amd-gpus)
  Audio and video travel as separate RTP packets.
  [Go Live overview, 2024-03-05](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)
- The public voice docs describe the media path as UDP with RTP-style
  packets, and transport encryption with AEAD modes
  (`aead_aes256_gcm_rtpsize` preferred, `aead_xchacha20_poly1305_rtpsize`
  required). Older modes were discontinued on 2024-11-18.
  [Voice connections](https://docs.discord.com/developers/topics/voice-connections)
- **DAVE.** Each sender encrypts every encoded audio or video frame after
  encoding, through WebRTC's encoded-transform API. The key is per sender,
  ratcheted from an MLS group (RFC 9420). The voice gateway runs MLS
  delivery but never holds the media keys. Encrypted frames are packetized
  into RTP and sent to the SFU, which relays them. Transport encryption
  between client and SFU stays on as well. The media cipher is
  AES-128-GCM. A Go Live stream is its own media session with its own MLS
  group.
  [DAVE whitepaper v1.1, last commit 2025-08-14](https://github.com/discord/dave-protocol/blob/main/protocol.md),
  [Meet DAVE, 2024-09-17](https://discord.com/blog/meet-dave-e2ee-for-audio-video)
- DAVE's encryptor is codec-aware. It leaves just enough codec metadata in
  plain text for WebRTC's packetizer and depacketizer: VP8 payload header
  bytes, the non-VCL NAL units of H.264 and H.265, and AV1 OBU headers. VP9
  frames are fully encrypted. All picture data is ciphertext. The plain
  ranges are authenticated too, so the SFU cannot add to or replace frame
  content. Discord ships a patched WebRTC AV1 depacketizer for this.
  [DAVE whitepaper, codec handling](https://github.com/discord/dave-protocol/blob/main/protocol.md#codec-handling)
- Enforcement: from 2026-03-01, clients without DAVE cannot join DMs,
  group DMs, voice channels or Go Live streams. Stage channels keep
  transport encryption only.
  [Bringing DAVE to all platforms, 2025-09-02](https://discord.com/blog/bringing-dave-to-all-discord-platforms),
  [E2EE now standard, 2026-05-18](https://discord.com/blog/every-voice-and-video-call-on-discord-is-now-end-to-end-encrypted)
- On Linux, the hardware encoder's output is encrypted before it is sent.
  [Linux post, 2026-11-12](https://discord.com/blog/discord-linux-a-year-of-upgrades)

**Our reading:** the SFU holds no media keys and the frame payload is
authenticated, so Discord's servers cannot decode, scale or re-encode a
Go Live stream. No Discord source says it cannot transcode; that follows
from the design. Forwarding also predates DAVE: the 2018 post already
describes a forwarding SFU.

### Server architecture

- In 2018, each voice server had a signalling component and a home-grown
  C++ SFU that forwarded audio and video. The SFU translated transport and
  encryption between native and browser clients. It read RTCP reports and
  told senders how much bandwidth was available for video. There were
  over 850 voice servers in 13 regions. The post names SVC as a possible
  future approach and does not mention simulcast.
  [Discord engineering, 2018](https://discord.com/blog/how-discord-handles-two-and-half-million-concurrent-voice-users-using-webrtc)
- In 2024: Go Live goes to Discord's backend, which routes it to viewers.
  Video goes only to members who are watching, and a streamer sends
  nothing until at least one viewer watches.
  [Go Live overview, 2024](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)
- The DAVE whitepaper defines the SFU as the service that relays RTP audio
  and video. The only media it makes itself is silence packets for muted
  sources, which Discord plans to remove.
  [DAVE whitepaper](https://github.com/discord/dave-protocol/blob/main/protocol.md)
- So Go Live runs on the same voice infrastructure, as a separate media
  session. Self-hosting is not offered.

### Quality levels and who chooses

- The 2024 overview describes one stream for all viewers. The stream must
  be watchable by everyone, so the streamer sends no more than the slowest
  connection can support.
  [Go Live overview, 2024](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)
- The streamer picks resolution and frame rate, up to their plan's cap,
  and can change them while live. The help page describes no quality
  control for viewers. For a lagging stream it suggests asking the
  streamer to lower their settings. Streamers in a browser cannot change
  quality.
  [Go Live and Screen Share, updated about 5 months ago](https://support.discord.com/hc/en-us/articles/360040816151-Go-Live-and-Screen-Share)
- In 2020 a Discord engineer, closing a request for bot Go Live, listed
  upcoming multiple-quality streams among the hard parts of video. GitHub
  marks the account only as a contributor; the comment speaks for "our
  team".
  [discord-api-docs #1995, 2020-08-21](https://github.com/discord/discord-api-docs/issues/1995#issuecomment-678550660)
- The voice docs' version table lists a v5 "video sink wants" opcode and
  nothing more. The opcode table documents no video, stream, simulcast or
  quality opcode.
  [Voice connections](https://docs.discord.com/developers/topics/voice-connections),
  [Opcodes](https://docs.discord.com/developers/topics/opcodes-and-status-codes).
  An open community pull request documents that opcode (15) for audio only
  and makes no claim about video.
  [discord-api-docs #8712](https://github.com/discord/discord-api-docs/pull/8712)
- **Not documented:** whether Go Live today uses simulcast, SVC or
  per-viewer layers. The 2020 plan and the 2024 slowest-viewer rule are the
  only first-party statements. Community reverse-engineering of the video
  opcodes exists, but it is not a primary source, so it is left out.

### Codecs and hardware

- Discord negotiates a codec the streamer can encode and every viewer can
  decode, and renegotiates whenever someone joins or leaves the call. It
  supports VP8 and H.264, with HEVC and AV1 encoders on some platforms. It
  prefers GPU encoders and decoders over software ones.
  [Go Live overview, 2024](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)
- AV1 encode and decode arrived in February 2023 for NVIDIA RTX 40 GPUs.
  The changelog advertised 4K streams at about 8 Mb/s.
  [Changelog, 2023-02-20](https://canary.discord.com/blog/discord-update-february-20-2023-changelog)
- On Windows, Discord relies on the operating system's codecs. An AVC
  encoder matters most for Go Live. AV1 improves quality only when every
  participant supports it.
  [Microsoft Store codecs, updated about 9 months ago](https://support.discord.com/hc/en-us/articles/37976724130711-Microsoft-Store-Codecs-for-Discord)
- On Linux: VA-API on Intel and AMD, NVENC on NVIDIA. Capture is a
  PipeWire stream from the Wayland portal.
  [Linux post, 2026-11-12](https://discord.com/blog/discord-linux-a-year-of-upgrades)
- The software fallback encoder is not documented.

### Resolution and frame-rate tiers

The two help-centre pages disagree, so both are given.

| Source | Base | Nitro Basic | Nitro |
| --- | --- | --- | --- |
| [Go Live and Screen Share](https://support.discord.com/hc/en-us/articles/360040816151-Go-Live-and-Screen-Share) (about 5 months old) | 720p30 | not listed | up to 4K60 (Nitro and Nitro Classic); 1080p60 on iOS |
| [Account caps](https://support.discord.com/hc/en-us/articles/33694251638295-Discord-Account-Caps-Server-Caps-and-More) (about 8 months old), screenshare in DMs | 720p30 | 720p60 or 1080p30 | 720p, 1080p or source, at 30 or 60 fps |
| Same page, Go Live in DMs | "730p" at 30 fps (the source's typo for 720p) | 1080p60 | source at 60 fps |

The caps page also has two server-boost rows that disagree with each
other. "Stream Quality" gives 720p30 at base and 720p60 from level 1.
"Go Live Streams" gives 720p60 at base and level 1, and 1080p60 from
level 2.

No tier bitrate is published. The 8,000 kb/s in the AMD post is one test
clip's target, and the 2023 AV1 figure is a marketing line.

### Adapting to a slow viewer or a slow sharer

- The 2024 overview: the encoder's frame rate, resolution and quality
  follow estimated bandwidth. When the network drops sharply, the encoder
  drops frames, and the client trades image quality against latency.
  Discord uses different WebRTC bandwidth estimators for different cases.
  [Go Live overview, 2024](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)
- Under the 2024 slowest-connection rule, a slow viewer lowers the stream
  for everyone. Today's behaviour is not documented.
- Keyframes: in a healthy stream they are 6 to 10 times the size of a
  delta frame, and the target is one per minute. A bug forced one on every
  WebRTC frame-rate change, up to once a second, which used up the
  bandwidth budget. Discord also relaxed rate control to average over a
  longer period, and left headroom after a dropped frame. On AMD GPUs, the
  share of 60 fps sessions below 50 fps fell from 39 % to 18 %.
  [AMD encoder post, 2024](https://discord.com/blog/from-blocky-to-brilliant-improving-video-quality-on-discord-go-live-on-amd-gpus)

**Our reading:** with a one-minute GOP, a new viewer cannot wait for the
next keyframe, so keyframes are presumably sent on request. The post does
not say how.

### Latency

No figure is published. Go Live cannot buffer for seconds as streaming
services do. Its audio and video are synced with each other but not with
voice chat; Discord aims to keep latency low enough that users don't
notice.
[Go Live overview, 2024](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)

### Screen-share audio

Audio comes from the shared process and its child processes through OS
audio APIs
([Go Live overview, 2024](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)).
It works on Windows, macOS, Chrome and mobile, but not on Linux or other
browsers
([help centre](https://support.discord.com/hc/en-us/articles/360040816151-Go-Live-and-Screen-Share)).
Per-app Linux audio through xdg-desktop-portal is being built
([Linux post, 2026](https://discord.com/blog/discord-linux-a-year-of-upgrades)).
The gateway's Soundshare speaking flag marks this audio
([voice docs](https://docs.discord.com/developers/topics/voice-connections)).

### Viewer UX and limits

- A Go Live stream has at most 50 concurrent viewers plus the broadcaster.
  Up to 50 people can share video or screen at once in a voice channel. A
  viewer watches one stream, in one voice channel, at a time.
  [Help centre](https://support.discord.com/hc/en-us/articles/360040816151-Go-Live-and-Screen-Share);
  the caps page repeats the 50
  ([caps](https://support.discord.com/hc/en-us/articles/33694251638295-Discord-Account-Caps-Server-Caps-and-More)).
- Viewers get grid and focus views, a resizable pop-out window with
  stay-on-top, full screen, and per-stream volume and mute. Streamers can
  hide the preview from non-viewers.
  [Help centre](https://support.discord.com/hc/en-us/articles/360040816151-Go-Live-and-Screen-Share)

## TeamSpeak 5 and 6

"TeamSpeak 5" was the development name of the next-generation client,
announced in 2018. The 6.0.0-beta1 release of 2025-01-21 renumbered it to 6
and introduced screen sharing. There is no separate TeamSpeak 5 screen
sharing to describe.
[About us](https://www.teamspeak.com/en/more/about-us),
[6.0.0-beta1 announcement](https://community.teamspeak.com/t/teamspeak-6-0-0-beta1-screen-camera-sharing-communities-design-overhaul/54925)

Who the sources are: client release posts are by the forum's Community
Manager (SYOX). LeonMarcelHD and revide carry the forum title "TeamSpeak
Staff". MuhChy and HuppiN are marked "collaborator" on the official
[teamspeak6-server](https://github.com/teamspeak/teamspeak6-server)
repository. TeamSpeak publishes no protocol or design document for screen
sharing, so most facts below come from changelogs and staff replies.
TeamSpeak 6 is still in beta.

### Transport and encryption

- Screen sharing uses WebRTC. It has no fixed port: ports are negotiated
  per connection (staff, 2025-10-16). The 6.0.0-beta5 client (2026-10-13)
  added a setting for the ICE UDP port range in Developer Tools.
  [Port-range thread](https://community.teamspeak.com/t/allow-custom-port-range-or-start-port-for-screenshare-sessions/62456),
  [6.0.0-beta5](https://community.teamspeak.com/t/teamspeak-6-0-0-beta5-foundation-iii-update/65513)
- It is peer-to-peer: the sharer's client sends to each viewer. A
  collaborator wrote on 2025-12-30 that screen sharing is P2P only for now
  and that server-side streaming is being built.
  [teamspeak6-server #62](https://github.com/teamspeak/teamspeak6-server/issues/62#issuecomment-3700096706).
  Users report that the client shows a "server" option that cannot be
  selected
  ([only-P2P thread](https://community.teamspeak.com/t/only-p2p-available-on-server-not-implemented-yet/55034),
  [#53](https://github.com/teamspeak/teamspeak6-server/issues/53)).
- **Not documented:** whether TeamSpeak adds anything beyond WebRTC's own
  transport security, whether it provides STUN or TURN, and what happens
  behind symmetric NAT. Failures behind symmetric NAT appear only in user
  reports in the port-range thread.

### Server architecture

- Today the server carries no screen-share media.
  [teamspeak6-server #118](https://github.com/teamspeak/teamspeak6-server/issues/118#issuecomment-6220196872)
- A server-side relay is in development as a separate "SFU project". A
  collaborator wrote on 2026-10-16 that SFU support will let streams be
  relayed through the TeamSpeak server, and that it is far along but has
  no release date. Whether it will be part of `tsserver` or a separate
  service, and which ports it will need, is not documented.
  [teamspeak6-server #126](https://github.com/teamspeak/teamspeak6-server/issues/126#issuecomment-6219711997),
  [#118](https://github.com/teamspeak/teamspeak6-server/issues/118#issuecomment-6220196872)
- Self-hosting today: the documented ports are 9987/UDP for voice,
  30033/TCP for file transfer, and optional query ports (10022 SSH,
  10080/10443 HTTP) and a metrics port (9187). None is for screen sharing.
  The server ships with a 32-slot beta licence, renewed every two months,
  and larger licences cannot be bought yet. No source ties screen sharing
  to the licence. The server release notes read for this note, beta10 to
  beta13.1 (2026-05-12 to 2026-09-22), mention screen sharing only as a
  streaming flag added to the client list in beta11.
  [Server README](https://github.com/teamspeak/teamspeak6-server/blob/main/README.md),
  [CONFIG.md](https://github.com/teamspeak/teamspeak6-server/blob/main/CONFIG.md),
  [releases](https://github.com/teamspeak/teamspeak6-server/releases)
- Users report a "Screen Share session already registered" error that
  names the server
  ([#8](https://github.com/teamspeak/teamspeak6-server/issues/8)). **Our
  reading:** the server probably takes part in setting up a share, but no
  source says how.
- **Our reading:** the beta11 server notes fix a crash in usrsctp, the SCTP
  library WebRTC uses for data channels. That suggests the server already
  links a WebRTC stack, but no source says what for.

### Quality levels and who chooses

- Before 6.0.0-beta3, each viewer got their own encode. In March 2025,
  the team announced a "peer-to-peer SFU" that would send one encoded
  stream to all viewers. A staff reply called it an optimisation that
  stays P2P but encodes once instead of once per viewer.
  [TS6 beta community update, 2025-03-13](https://community.teamspeak.com/t/ts6-beta-community-update-insights/58116)
- 6.0.0-beta3 (2025-10-14) shipped P2P streaming with a single encode for
  all viewers. It also added automatic codec selection based on the
  viewers.
  [6.0.0-beta3](https://community.teamspeak.com/t/teamspeak-6-0-0-beta3-foundation-i-update/62417)
- The sharer sets quality and encoder parameters. Staff suggest tuning the
  encoder settings, and say the defaults aim for broad GPU compatibility.
  [6.0.0-beta3 thread](https://community.teamspeak.com/t/teamspeak-6-0-0-beta3-foundation-i-update/62417)
- **Not documented:** simulcast, SVC, any lower rendition, or any viewer
  quality control.

### Codecs and hardware

- 6.0.0-beta3 added hardware encoding through FFmpeg (AMD AMF, Intel QSV,
  NVIDIA NVENC) and OpenH264 for H.264. Users may swap in their own FFmpeg
  build.
  [6.0.0-beta3](https://community.teamspeak.com/t/teamspeak-6-0-0-beta3-foundation-i-update/62417)
- 6.0.0-beta5 added live bitrate updates on macOS VideoToolbox.
  [6.0.0-beta5](https://community.teamspeak.com/t/teamspeak-6-0-0-beta5-foundation-iii-update/65513)
- H.264 and AV1 are both in use. Staff say that on Linux they cannot ship
  FFmpeg's software H.264 decoder because of patents, and VA-API decoding
  needs it, so H.264 VA-API decoding is missing. They added VP9 and AV1
  VA-API support for a later release.
  [Screen-sharing issues thread, staff replies 2026-04](https://community.teamspeak.com/t/a-list-of-issues-and-temporary-solutions-for-screensharing/64437)
- How the codec is chosen is not documented, nor is HEVC.

### Resolution and frame-rate tiers

The TeamSpeak home page advertises 4K at 60 fps screen sharing and
"ultra-low" latency ([teamspeak.com](https://www.teamspeak.com/en/)). No
resolution or frame-rate tiers, paid levels or bitrates are documented.

### Adapting to a slow viewer or a slow sharer

- 6.0.0-beta5 fixed a stalled viewer freezing the shared encoder for every
  viewer. It also fixed P2P shares stuck in "Connecting" after a
  connection loss, and large P2P offers lost after ICE restarts or codec
  changes. It tuned the encoders for low latency and made keyframe
  requests safer and quicker.
  [6.0.0-beta5](https://community.teamspeak.com/t/teamspeak-6-0-0-beta5-foundation-iii-update/65513)
- Congestion control, bandwidth estimation and what a weak viewer sees are
  not documented.

**Our reading:** with one shared encoder and no lower rendition, a slow
viewer can only fall behind or slow the encoder down for everyone. The
beta5 fix shows TeamSpeak hit the second case. Sideband's per-viewer
queue already avoids it.

### Latency

Not documented beyond the "ultra-low" marketing line and the beta5
low-latency encoder tuning.

### Screen-share audio

6.0.0-beta5 added audio capture from several chosen applications on
Windows (10 22H2 or later), Linux and macOS, with per-application volume.
It also improved A/V sync between streams.
[6.0.0-beta5](https://community.teamspeak.com/t/teamspeak-6-0-0-beta5-foundation-iii-update/65513)

### Viewer UX and limits

6.0.0-beta5 added a reworked grid with pagination and a focus view,
picture-in-picture, single-stream pop-outs (macOS included), and full
screen with context menus. 1:1 direct calls can carry screen sharing.
[6.0.0-beta5](https://community.teamspeak.com/t/teamspeak-6-0-0-beta5-foundation-iii-update/65513)
A viewer limit is not documented. **Our reading:** with P2P, the sharer's
upload carries every viewer.

## Comparison

| Question | Discord | TeamSpeak 6 | Sideband today plus T-074 |
| --- | --- | --- | --- |
| Transport | WebRTC media engine over UDP/RTP; in 2018, native clients skipped ICE and connected to a relay | WebRTC, peer-to-peer, ICE with dynamic ports | Own QUIC (`quinn`); one uni stream per GOP |
| Media path | Client → Discord SFU → viewers | Sharer → each viewer directly; server relay in development | Client → `ss-server` relay → viewers |
| End-to-end encryption | Yes, DAVE (MLS, AES-128-GCM per frame), enforced since 2026-03 | Not documented | No (QUIC TLS to the server) |
| Server decodes or transcodes | No (forwarding SFU; cannot read DAVE frames) | No server in the path | No (D4) |
| Lower qualities | Not documented; in 2024, one stream kept within the slowest viewer | None documented; one shared encode since beta3 | Second rendition encoded by the sharer (T-074) |
| Who picks a viewer's quality | No per-viewer control documented; the streamer sets the cap | No per-viewer control documented; the sharer sets it | The relay, per subscriber, capped by `SetPreferredQuality` |
| Switch point | Not documented | Not applicable | Group boundary, at a keyframe |
| Codecs | VP8, H.264; HEVC, AV1 on some platforms; one every viewer can decode | H.264, AV1 (VP9 mentioned); picked from the viewers | H.264; AV1, HEVC later as `Codec.name` |
| Hardware encode | GPU preferred; VA-API, NVENC on Linux; OS codecs on Windows | FFmpeg AMF, QSV, NVENC; VideoToolbox; OpenH264 | Through `moq-video`; VideoToolbox measured on macOS (T-100) |
| Tiers | 720p30 free; up to 1080p60, or 4K60/source, with Nitro; boosts raise server streams | 4K60 advertised; no tiers | Presets, for example 1080p30; low rendition 360p15 |
| Keyframes | Target one per minute (2024) | Requested on demand (beta5) | 2 s GOP; viewer may request one |
| Slow viewer | In 2024, everyone's quality drops; today not documented | Could freeze everyone (fixed in beta5) | That viewer's oldest groups dropped; T-074 adds the low rendition |
| Latency figure | None published | None ("ultra-low") | 15–17 ms capture-to-decoded on loopback (T-100) |
| Screen audio | Shared process and children; not on Linux | Several chosen apps, on all three OSes | Out of scope for T-074 |
| Viewer limit | 50 plus the streamer | Not documented | None stated in T-074 |
| Pop-out | Yes, stay-on-top | Yes, single-stream pop-outs and PiP | Native GPU window |
| Self-hosted | No | Yes, 32-slot beta licence; screen media is P2P | Yes |

## What Sideband can learn

All of this section is **our reading** of the findings above.

1. **Where lower qualities come from.** Neither product documents a server
   that decodes or re-encodes screen shares. Discord's DAVE rules it out: a
   server that cannot decrypt a frame cannot scale it. Discord also chose
   forwarding long before DAVE. TeamSpeak's planned server piece is an
   SFU, a forwarder by name. **This supports D4 and T-074's "no
   transcoding".** A transcoding Sideband server would also close the door
   on end-to-end encryption, which DAVE shows works over a forwarding
   relay.
2. **Who encodes the lower level.** Neither competitor documents a sharer
   that encodes more than one quality. Discord's 2024 answer to a slow
   viewer is to lower everyone, and TeamSpeak has no answer yet. So
   T-074's two encodes by the sharer go further than either documents.
   That is a choice, not a mistake: T-100 measured that one rendition left
   a slow viewer frozen for 27 of 30 s, and T-074's goal is that the other
   viewers keep full quality. **The findings neither support nor challenge
   the two-encode design.** They do support its on-demand rule: Discord
   sends only while someone watches, and T-074 runs the low encoder only
   while a subscriber uses it.
3. **Level counts and resolutions.** Neither product publishes a lower
   level, so there is nothing to compare 360p15 against. Discord's tiers
   are caps the sharer picks (720p30, 1080p60, 4K60 or source), not levels
   served at the same time. **No change suggested.** If Sideband ever
   offers 4K or source presets, the gap from the top down to 360p grows,
   and a middle level may be worth a look. That is outside T-074.
4. **How a viewer is switched.** Neither documents it. Discord's keyframe
   evidence points the other way from Sideband's GOP: keyframes cost 6 to
   10 times a delta frame, and Discord targets one a minute. Sideband's
   2 s GOP buys a switch point every 2 s with no keyframe request, which
   is what T-074's boundary rule needs. It pays in bits, and the cost is
   largest on the 300 kb/s rendition, where a keyframe every 2 s is a big
   share of the budget. **Our verdict:** keep the 2 s GOP for T-074, but
   record the low rendition's keyframe share in the impairment run. If it
   is large, try a longer GOP on the low rendition with a keyframe on
   switch.
5. **The shared-encoder trap.** TeamSpeak's beta5 fix, for a stalled
   viewer freezing the shared encoder for everyone, is the failure that
   screen-sharing rule 3 and T-074's relay tests guard against. **This
   supports T-074's test** that a stalled subscriber must not stop a
   healthy one.
6. **Sender adaptation.** Discord's encoder follows WebRTC's bandwidth
   estimate, and its AMD work shows two pitfalls: forced keyframes on rate
   changes, and per-frame budgets that undershoot. **For T-074's bitrate
   controller:** do not force a keyframe when the bitrate or frame rate
   changes, and let the rate average over more than one frame.
7. **Viewer limits.** Discord caps a stream at 50 viewers. TeamSpeak's P2P
   is bounded by the sharer's upload. Sideband's sharer uploads once per
   rendition whatever the viewer count, so the limit is server egress.
   **Suggestion:** state a viewer cap, or at least the egress per viewer,
   in the T-074 or load-test docs. This does not change T-074's design.
8. **Codecs.** Discord negotiates a codec every viewer can decode and
   renegotiates when anyone joins. With per-viewer renditions, Sideband
   could later give the low rendition a widely decodable codec (H.264)
   even if the full one moves to AV1. **No change for T-074**, which stays
   H.264. D4 already lets new codecs pass the relay without a server
   change.

### What could not be verified

- Whether Discord Go Live uses simulcast or SVC today. The 2020 plan for
  multiple-quality streams and the 2024 slowest-viewer rule are the only
  first-party statements. The voice docs document no video opcodes.
- Discord's software fallback encoder, tier bitrates and latency figures.
- TeamSpeak's encryption beyond WebRTC, STUN/TURN provision, viewer limits,
  quality tiers, congestion control, latency, and the design, ports and
  release date of its server-side SFU.
- These sources were read through a summarising fetch, not as raw text:
  the 2018 Discord post, the three DAVE blog posts, the 6.0.0-beta3
  changelog, and the forum staff replies other than the March 2025 update.
  The Go Live overview, the AMD post, the March 2025 TeamSpeak update, the
  6.0.0-beta5 notes, the DAVE whitepaper, the help-centre pages, the
  GitHub issues and the server files were checked against their raw text.

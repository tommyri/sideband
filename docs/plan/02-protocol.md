# Wire protocol v1

Everything on one QUIC connection per client. ALPN `socialspeak/1`. Default
port UDP 443 (configurable). HTTPS on TCP 443 for REST.

## 1. Connection lifecycle

1. Client opens QUIC connection; verifies server cert against its pin store
   (TOFU: first connect shows fingerprint, user accepts; later connects must
   match). `ss-net` exposes `PinStore` and a `rustls` verifier.
2. Client opens **one bidirectional stream**: the *control stream*. All control
   messages flow here, both directions, for the life of the connection.
3. First client message must be `Hello`. Server replies `Welcome` or `Error`
   and closes.
4. Media: QUIC **datagrams** (audio) and **unidirectional streams** (video,
   phase 5). Clients must negotiate `max_datagram_frame_size`; server rejects
   connections without datagram support.
5. Keepalive: client sends `Ping` every 15 s when idle; connection idle timeout
   30 s on both sides. On loss, client reconnects with `Hello{resume_token}`;
   server restores voice membership if the previous connection is gone.

## 2. Control stream framing

```
u32 length (big-endian, max 1 MiB) | protobuf-encoded Envelope
```

`ss-proto` provides generic `encode`, `decode`, `read_frame`, and
`write_frame` helpers. `ss-net::control::ControlStream` wraps the QUIC
streams, retaining partial frames across cancellation. Its client router
allocates nonzero request ids monotonically, separates events, and sends
`Ping` after 15 seconds without an outbound control frame, once the application
has sent its first Hello. A fresh router never sends Ping before Hello. `Hello`/`Welcome`
and `Ping`/`Pong` use the router's `request` method; commands expecting
`Ack`/`Error` use `call`. Authentication and server-side responses are owned
by the application.

## 3. Message schema (protobuf, `crates/ss-proto/proto/socialspeak/v1/*.proto`)

Conventions: ids are `bytes` ULIDs (16 bytes); timestamps are `int64` Unix
milliseconds; every request carries `request_id` (u32, client-chosen,
monotonic) and gets exactly one `Ack` or `Error` with the same id; server
events have `request_id = 0`.

```proto
message ClientEnvelope { uint32 request_id = 1; oneof msg { /* below */ } }
message ServerEnvelope { uint32 request_id = 1; oneof msg { /* below */ } }
```

### Client → Server

| Message | Fields | Purpose |
|---|---|---|
| `Hello` | `protocol_version`, `client_name`, `client_version`, `os`, `oneof auth { session_token, resume_token, Login{username,password}, Register{invite_code,username,password} }` | Must be first |
| `Ping` | `sent_at` | Keepalive / RTT |
| `JoinVoice` | `channel_id`, `self_mute`, `self_deaf` | Join or switch voice channel |
| `LeaveVoice` | — | |
| `SetVoiceState` | `self_mute`, `self_deaf` | |
| `SendMessage` | `channel_id`, `content`, `reply_to`, `attachment_ids[]`, `nonce` | `nonce` lets client dedupe its own echo |
| `EditMessage` | `message_id`, `content` | |
| `DeleteMessage` | `message_id` | |
| `Typing` | `channel_id` | Rate-limited client side (1 per 5 s) |
| `MarkRead` | `channel_id`, `message_id` | |
| `CreateDm` | `user_ids[]` | One other user reuses that pair's DM. Two to nine others open a new group DM |
| `LeaveDm` | `channel_id` | Leave a group DM |
| `AddDmMembers` | `channel_id`, `user_ids[]` | Group-DM creator adds members, up to nine others in total |
| `CreateWebhook` | `channel_id`, `name` | Token is returned once on the ack |
| `DeleteWebhook` | `webhook_id` | Later posts with that token are rejected |
| `CreateChannel` | `kind`, `name`, `parent_id`, `position`, `topic`, `VoiceSettings` | |
| `UpdateChannel` | `channel_id`, optional fields | Any group-DM member may change the name |
| `DeleteChannel` | `channel_id` | |
| `MoveChannel` | `channel_id`, `parent_id`, `position` | |
| `CreateInvite` | `max_uses`, `expires_at` | |
| `RevokeInvite` | `invite_id` | |
| `KickUser` | `user_id`, `reason` | |
| `BanUser` | `user_id`, `reason`, `expires_at` | |
| `UnbanUser` | `user_id` | |
| `MoveUser` | `user_id`, `channel_id` | Move into another voice channel |
| `ServerMuteUser` | `user_id`, `muted` | |
| `CreateRole` / `UpdateRole` / `DeleteRole` | `Role{name,color,permissions(u64),position}` | |
| `SetUserRoles` | `user_id`, `role_ids[]` | |
| `SetChannelOverride` | `channel_id`, `role_id`, `allow(u64)`, `deny(u64)` | Table exists in MVP; UI may come later |
| `UpdateProfile` | `display_name`, `avatar_attachment_id` | |
| `SetPresence` | `status`, `activity` | Online, idle, or do-not-disturb. Offline is the last disconnect |
| `StartStream` / `StopStream` | `kind` (VIDEO/SCREEN), `codec` / — | One published video track per connection; requires voice membership and STREAM |
| `SubscribeTrack` / `UnsubscribeTrack` | `track_id` | Video (phase 5); audio is auto-subscribed |
| `RequestKeyframe` | `track_id` | Video (phase 5) |
| `SetPreferredQuality` | `track_id`, `max_height`, `max_fps` | Video (phase 5) |

### Server → Client

| Message | Fields | Purpose |
|---|---|---|
| `Welcome` | `user`, `server_info{name, version, features[]}`, `resume_token`, `Snapshot` | `Snapshot` = full channel tree, roles, members with presence, voice states, tracks, read states, DM list with each conversation's last message id |
| `Ack` | `request_id`, `oneof result { Empty, MessageId, ChannelId, Invite, DmChannel, Role, User, Webhook, TrackId }` | `Webhook.token` is only on `CreateWebhook` |
| `Error` | `request_id`, `code` (enum), `message` | Codes: `UNAUTHENTICATED`, `FORBIDDEN`, `NOT_FOUND`, `INVALID`, `RATE_LIMITED`, `CONFLICT`, `BANNED`, `INTERNAL`, `PROTOCOL_VERSION` |
| `Pong` | `sent_at`, `server_time` | |
| `ChannelCreated` / `ChannelUpdated` / `ChannelDeleted` | `Channel` | |
| `MemberJoined` / `MemberLeft` / `MemberUpdated` | `Member{user, roles, presence}` | |
| `PresenceUpdated` | `user_id`, `status`, `activity` | |
| `VoiceStateUpdated` | `user_id`, `conn_id`, `channel_id?`, `self_mute`, `self_deaf`, `server_mute` | |
| `TrackAdded` | `track_id`, `user_id`, `conn_id`, `kind` (AUDIO/VIDEO/SCREEN), `codec{name, sample_rate, channels}`, `channel_id` | Mapping for datagram demux |
| `TrackRemoved` | `track_id` | |
| `MessageCreated` / `MessageUpdated` / `MessageDeleted` | `Message`, `nonce?` | `Message` includes author, content, reply, attachments, mentions, `link_previews[]` (may arrive later via `MessageUpdated`) |
| `TypingStarted` | `channel_id`, `user_id` | |
| `ReadStateUpdated` | `channel_id`, `last_read_message_id`, `mention_count` | |
| `RoleCreated` / `RoleUpdated` / `RoleDeleted` | `Role` | |
| `MemberRolesUpdated` | `user_id`, `role_ids[]` | |
| `InviteCreated` / `InviteRevoked` | `Invite` | admins only |
| `Kicked` / `Banned` | `reason`, `expires_at?` | sent then connection closed |
| `RequestKeyframe` | `track_id` | Private relay to publisher, at most once per 500 ms per track |
| `SubscriberHint` | `track_id`, `max_height`, `max_fps`, `viewer_count` | Private aggregate to publisher; server never transcodes |
| `ServerNotice` | `text` | shutdown warnings etc. |

## 4. Media datagrams (audio)

```
byte 0   : vv kk ffff   v=version(2 bits, =0) k=kind(2 bits: 0=audio) f=flags
                        flags: bit0 TALK_START, bit1 TALK_END, bit2 reserved, bit3 reserved
varint   : track_id (server-assigned per (connection, kind); canonical unsigned LEB128, 1–3 bytes)
u16 BE   : seq (wraps)
u32 BE   : timestamp in 48 kHz samples (wraps)
payload  : one Opus packet (may contain DRED extension; ≤ 1150 bytes)
```

- Track ids use the shortest unsigned LEB128 representation (7 data bits
  per byte, high bit indicates continuation), range `0..=0x1f_ffff`.
  This is not QUIC's 1/2/4/8-byte varint. Overlong encodings, a fourth byte,
  nonzero versions, non-audio datagram kinds, and reserved flags are rejected.
- Client sends only while gated open (PTT/VAD). `TALK_START` on the first
  packet of a burst, `TALK_END` on the last (encoder flushed). Receivers derive
  speaking indicators from these plus a 200 ms arrival timeout.
- The server validates that `track_id` belongs to the sending connection and
  that the sender has `speak` and is not server-muted, then forwards the
  datagram **unchanged** to every other member of the sender's voice channel
  that is not self-deafened.
- Receivers keep one jitter buffer + decoder per `track_id`. Unknown
  `track_id`s are buffered for 500 ms awaiting `TrackAdded`, then dropped.
- Encoder profiles (chosen by channel `VoiceSettings`):

| Profile | Application | Bitrate | Channels | FEC | Frame |
|---|---|---|---|---|---|
| `voice` (default) | VOIP | 80 kb/s (range 48–96) | 1 | on, expected loss 5 % | 20 ms |
| `music` | AUDIO | 128 kb/s (range 96–192) | 2 | on | 20 ms |
| `low_latency` | RESTRICTED_LOWDELAY | 64 kb/s | 1 | on | 10 ms |

Decoder: complexity 10, Deep PLC always on; DRED decode on if the build has
it; BWE off (voice profile is already fullband).

Bitrates are decimal (80 kb/s = 80_000 bit/s). Expected loss is set only
where the table states a percent. Why these profiles, and what the encoder
must leave off, is in [`docs/design/voice.md`](../design/voice.md).

## 5. Media streams (video, phase 5; defined now)

One QUIC unidirectional stream per *group* (keyframe + dependents). Stream
begins with the same header as a datagram with `kind=1 (video)` and an extra
big-endian `u32 group_id`; then a sequence of `u32 len | u32 timestamp | u8 flags | frame`.
The sender resets the previous group's stream when a new keyframe starts if
it hasn't finished. The server forwards each stream only to connections that
sent `SubscribeTrack` for that `track_id` and resets streams to a subscriber
whose send queue exceeds a budget (drop-oldest). Codec parameters are carried
in `TrackAdded.codec` (e.g. `avc1.64001f` + `description`).

T-070 makes this path available after authenticated Hello (`video-streams`
feature in Welcome). `StartStream` appends client envelope tag 39 and returns
`Ack.track_id` (tag 10); `StopStream` is tag 40. Private server events append
`RequestKeyframe` tag 30 and `SubscriberHint` tag 31. Existing tags are unchanged.

Frame `len` is the encoded payload length, excluding the nine-byte frame prefix,
range 1..=4 MiB. Timestamp is on the shared 48 kHz clock. Frame flag bit 0 means
keyframe; other bits are reserved. The first frame in a group must be a keyframe.
The byte-level client rejects invalid/truncated framing; the server treats the
body as opaque and bounds the whole group to 32 MiB instead of parsing a codec.
Sequence/group ids wrap. Senders use common-header flags zero for video.

Publication requires VIEW_CHANNEL + STREAM on the joined voice channel.
Codec name is 1–128 UTF-8 bytes, description at most 64 KiB; video sample_rate
and channels are zero. Audio and video share the monotonically allocated track
id space. A subscriber must be another member of that same voice channel with
VIEW_CHANNEL. At most 16 video subscriptions per connection. Self-deaf and
microphone mute govern audio, independently of video subscriptions.

Each publisher and each subscription retain at most two groups. A newer group
cancels the oldest; each viewer's worker additionally has an eight-chunk queue
(16 KiB chunks). Queue overflow resets that viewer's group without waiting or
stalling other viewers. A finished outgoing group remains cancellable until
QUIC acknowledges it. Reset/stop application code is 0x70. Unsubscribe, leave,
switch, disconnect, channel deletion, or permission revocation cancel affected
streams. A subscriber starts with the next group and may request a keyframe;
there is no GOP replay buffer. StopStream is idempotent cleanup, including after
permission loss.

The server accepts at most four incoming group tasks per connection, allows
five seconds for a header/idle read and thirty seconds total per group. Senders
should open a new group at each keyframe and reset superseded unfinished groups.
`SetPreferredQuality` requires a subscription; height 0..=4320 and fps 0..=240.
Zero means unspecified. Hints use the highest requested value per dimension,
or zero when any viewer leaves it unspecified, and include the viewer count.
Removing the last viewer reports count zero. These are adaptation hints to the
publisher, not promises that the server filters or re-encodes frames.

## 6. REST (HTTPS, `Authorization: Bearer <session_token>`)

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v1/server` | Public: name, version, cert fingerprint, registration mode |
| GET | `/api/v1/channels/{id}/messages?before=<id>&limit=50` | History page, newest first |
| GET | `/api/v1/channels/{id}/messages/around/{message_id}` | Jump to message |
| GET | `/api/v1/channels/{id}/search?q=` | Phase 4 |
| POST | `/api/v1/attachments` | multipart upload → `{attachment_id, url, mime, size, width?, height?}` |
| GET | `/api/v1/attachments/{id}/{filename}` | Download (auth required, signed URL variant for previews) |
| GET | `/api/v1/users/{id}/avatar` | |
| POST | `/api/v1/webhooks/{id}/{token}` | Incoming webhook `{content, username?, avatar_url?}`; no bearer needed |
| GET | `/api/v1/audit-log?before=&limit=` | admins |
| GET | `/metrics` | internal listener only |

## 7. Versioning

`Hello.protocol_version = 1`. Server supports a range; mismatch → `Error
PROTOCOL_VERSION` with `min`/`max`. Protobuf fields are only ever added; never
renumbered. Datagram header version bits allow one future header change.

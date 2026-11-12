# Wire protocol v1

Everything on one QUIC connection per client. ALPN `socialspeak/1`. Default
port UDP 443 (configurable). HTTPS on TCP 443 for REST.

## 1. Connection lifecycle

1. Client opens QUIC connection and verifies the server certificate by the
   trust policy saved for that server (`ss_net::pins::TrustPolicy`, details in
   [`docs/security.md`](../security.md) section 3). A server with a publicly
   trusted certificate is checked by hostname and CA chain on every handshake,
   so renewals are accepted (`PublicCertificate`). A self-signed server is
   pinned on first use (TOFU: first connect shows the fingerprint, the user
   accepts; later connects must match, `PinnedCertificate`). A failed public
   check never falls back to TOFU. `ss-net` exposes `PinStore` and a `rustls`
   verifier.
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

### 1.1 Session tokens

`Welcome.resume_token` is the session token itself: 32 random bytes,
base64url. It is the credential for `Hello.session_token` and
`Hello.resume_token` (the server treats both the same) and the HTTPS bearer
(section 6). There is no separate short-lived resume token. The short-lived
part of a resume is the 60-second voice-seat lease the server keys by this
token after a dropped connection (T-060).

Lifetimes: a token unused for **90 days** expires (idle lifetime), and every
token expires **180 days** after the login or registration that issued it,
however often it is used (absolute lifetime). Each successful `Hello` or
HTTPS request moves expiry to the earlier of now + 90 days and login + 180
days. An expired token is `UNAUTHENTICATED`; the user logs in with their
password again.

A token stops working on both transports as soon as its session is ended:
`Logout`, `RevokeSession`, a password reset, or the operator's
`ss-server user sessions revoke`. Connections that used it receive
`SessionRevoked` and are closed with code 6, at once for commands and within
30 s for changes the CLI makes. A client must then stop reconnecting with that
token and ask for the password. Expired sessions are deleted at startup and
every hour. A kick (`Kicked`) only disconnects; the token keeps working.

QUIC application close codes (`ss_server::registry` and
`ss_server::auth::CLOSE_SIGNED_OUT`):

| Code | Sent by | Meaning |
|---|---|---|
| 0 | either | normal close |
| 1 | server | shutting down; `ServerNotice` first |
| 2 | server | control consumer too slow |
| 3 | server | kicked; `Kicked` first |
| 4 | server | banned; `Banned` first |
| 5 | server | media rate limit; `Error RATE_LIMITED` first |
| 6 | server | signed out: the `Ack` of `Logout` (or of `RevokeSession` for the current session) is the last frame, or `SessionRevoked` came first. The token is dead |
| 73 | client | reconnecting |

A close can overtake the event queued in front of it, so a client decides
by the code as well: after 3, 4 or 6 it does not reconnect, whether or not
`Kicked`, `Banned` or `SessionRevoked` arrived. After any other code, or a
lost connection, it reconnects.

### 1.2 Reconnecting

A client resumes with `Hello{resume_token}`. The server's answer to that
`Hello` decides what happens next:

| `Error` code | Client |
|---|---|
| `UNAUTHENTICATED`, `BANNED`, `FORBIDDEN`, `PROTOCOL_VERSION` | Final: stop and ask the user (log in again, banned, upgrade) |
| `RATE_LIMITED` | Wait `retry_after_ms`, plus up to a quarter more, then try again |
| anything else (`INTERNAL`, …) | Try again after the backoff |

The server allows 30 `Hello`s per address before it refuses with
`RATE_LIMITED` (section 3, Limits), so after a restart clients behind one
NAT are told to wait, not to give up.

Backoff (the Sideband client): 2, 4, 8, 16, then 24 s, each plus up to a
quarter more, so never over 30 s and still spread at the cap. It starts
over after a session that lasted 30 s, or one the server closed with code
1 (shutdown); a session that dies sooner, or with code 5, keeps climbing.
Each resolved address gets 3 s for its QUIC handshake when another address
follows, so an address family that drops packets falls back to the next.
Message sends wait in a queue of at most 100 while reconnecting.

A `SendMessage` whose `Ack` never came is replayed with the same `nonce`.
The server deduplicates a nonce for 5 minutes after storing it, so a client
replays a send only within 4 minutes of first sending it; after that the
send fails as timed out (its outcome is unknown) instead of risking a
duplicate. A send that never left the client is sent whenever the
connection is back.

After `Welcome`, the client re-reads the history it had cached (section 6):
for each channel the newest page, then `before=` pages until one reaches
the oldest cached message, at most 4 pages. Each page replaces the cached
messages in its id range, so edits and deletes made while the client was
away show; if the 4 pages do not reach the old cache, the older messages
are dropped and load again on scroll, so no gap is left. A message a page
brought in before its own `MessageCreated` still counts as new when that
event arrives. A page refused with `429` is asked again after its
`Retry-After` (one second if absent), plus up to a quarter more; after three
refusals that channel keeps its cache and the client says the resync was
deferred.

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
by the application. The router buffers 256 events for the application; when
they are unread it stops reading and waits, so a burst slows the stream (and,
through QUIC flow control, the server) instead of closing it. A reply waits
behind the events in front of it, so the application keeps reading events
while it awaits one; a client that stops reading is closed by the server as a
slow consumer (code 2).

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
| `Hello` | `protocol_version`, `client_name`, `client_version`, `os`, `oneof auth { session_token, resume_token, Login{username,password}, Register{invite_code,username,password}, bot_token }` | Must be first. `bot_token` (tag 9) signs a bot account in (section 7, Bot accounts) |
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
| `ListDms` | `cursor?`, `limit` | Older conversations than `Welcome` carried (tag 45). Acked with `DmPage` |
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
| `CreateRole` / `UpdateRole` / `DeleteRole` | `Role{name,color,permissions(u64),position}` | `UpdateRole.clear_color` explicitly removes color; omitted color otherwise retains it |
| `MoveRole` | `role_id`, optional `before_role_id` | Atomic highest-first reorder; absent target means bottom; requires MANAGE_ROLES and editable source/target |
| `SetUserRoles` | `user_id`, `role_ids[]` | |
| `SetChannelOverride` | `channel_id`, `role_id`, `allow(u64)`, `deny(u64)` | Roles/permissions UI reads saved masks before editing |
| `UpdateProfile` | `display_name`, `avatar_attachment_id` | |
| `SetPresence` | `status`, `activity` | Online, idle, or do-not-disturb. Offline is the last disconnect |
| `StartStream` / `StopStream` | `kind` (VIDEO/SCREEN), `codec` / — | One published video track per connection; requires voice membership and STREAM |
| `SubscribeTrack` / `UnsubscribeTrack` | `track_id` | Video (phase 5); audio is auto-subscribed |
| `RequestKeyframe` | `track_id` | Video (phase 5) |
| `SetPreferredQuality` | `track_id`, `max_height`, `max_fps` | Video (phase 5) |
| `Logout` | — | Ends this connection's session (section 1.1). The `Ack` is the last frame; the server then closes with code 6 and closes the session's other connections with `SessionRevoked` |
| `ListSessions` | — | The caller's own live sessions; `Ack.sessions` lists `SessionInfo{id, device_name, created_at, last_seen_at, expires_at, current}`. Never tokens or hashes |
| `RevokeSession` | `session_id` | Ends one of the caller's own sessions and closes its connections. Another account's id is `NOT_FOUND`; the current session behaves as `Logout` |
| `CreateBot` | `username`, `display_name`, `scopes(u64)` | MANAGE_SERVER, never a bot; scopes only bits the caller holds. `Ack.bot` carries the token once (tag 46) |
| `ResetBotToken` | `bot_id`, `scopes(u64)` | New token, old one revoked and its connections and streams closed; also needs a role above the bot's (tag 47) |
| `RevokeBotToken` | `bot_id` | Revokes the live token and closes what used it (tag 48) |
| `ListBots` | — | `Ack.bots`: every bot with scopes and token state, never a token (tag 49) |

### Server → Client

| Message | Fields | Purpose |
|---|---|---|
| `Welcome` | `user`, `server_info{name, version, features[]}`, `resume_token` (the session token, section 1.1), `Snapshot`, `conn_id` | `Snapshot` = full channel tree, roles, members with presence, voice states, tracks, read states, the most recently active DMs with each conversation's last message id, and `direct_messages_next?` when there are more. `conn_id` names this connection (see below) |
| `Ack` | `request_id`, `oneof result { Empty, MessageId, ChannelId, Invite, DmChannel, Role, User, Webhook, TrackId, Sessions, DmPage, Bot, Bots }` | `Webhook.token` is only on `CreateWebhook`. `Sessions` answers `ListSessions`. `DmPage{channels[], read_states[], next?}` answers `ListDms` (tag 13). `Bot{user, scopes, active, token_created_by, token_created_at, token?}` answers `CreateBot` and `ResetBotToken`, the only place `token` is set (tag 14); `Bots` answers `ListBots` (tag 15) |
| `Error` | `request_id`, `code` (enum), `message`, `min_version?`, `max_version?`, `retry_after_ms?` | Codes: `UNAUTHENTICATED`, `FORBIDDEN`, `NOT_FOUND`, `INVALID`, `RATE_LIMITED`, `CONFLICT`, `BANNED`, `INTERNAL`, `PROTOCOL_VERSION`. `retry_after_ms` is set on every `RATE_LIMITED`: the wait, rounded up, before the same request can succeed |
| `Pong` | `sent_at`, `server_time` | |
| `ChannelCreated` / `ChannelUpdated` / `ChannelDeleted` | `Channel` | |
| `MemberJoined` / `MemberLeft` / `MemberUpdated` | `Member{user, roles, presence}` | `MemberJoined` when an account registers, before its `PresenceUpdated`; not sent to the new member. `MemberLeft` is reserved: no server path disables or removes an account, and a banned account stays in the member list |
| `PresenceUpdated` | `user_id`, `status`, `activity` | |
| `VoiceStateUpdated` | `user_id`, `conn_id`, `channel_id?`, `self_mute`, `self_deaf`, `server_mute` | |
| `TrackAdded` | `track_id`, `user_id`, `conn_id`, `kind` (AUDIO/VIDEO/SCREEN), `codec{name, sample_rate, channels}`, `channel_id` | Mapping for datagram demux |
| `TrackRemoved` | `track_id` | |
| `MessageCreated` / `MessageUpdated` / `MessageDeleted` | `Message`, `nonce?` | `Message` includes author, content, reply, attachments, mentions, `link_previews[]` (may arrive later via `MessageUpdated`), and for a webhook post `webhook_id` and `webhook_name` instead of an author |
| `TypingStarted` | `channel_id`, `user_id` | |
| `ReadStateUpdated` | `channel_id`, `last_read_message_id`, `mention_count` | |
| `RoleCreated` / `RoleUpdated` / `RoleDeleted` | `Role` | |
| `MemberRolesUpdated` | `user_id`, `role_ids[]` | |
| `InviteCreated` / `InviteRevoked` | `Invite` | admins only |
| `Kicked` / `Banned` | `reason`, `expires_at?` | sent then connection closed |
| `RequestKeyframe` | `track_id` | Private relay to publisher, at most once per 500 ms per track |
| `SubscriberHint` | `track_id`, `max_height`, `max_fps`, `viewer_count` | Private aggregate to publisher; server never transcodes |
| `ServerNotice` | `text` | shutdown warnings etc. |
| `SessionRevoked` | `reason` | This connection's session was ended elsewhere (logout, revoke, password reset, operator); sent, then closed with code 6. Do not reconnect with the token |

`Welcome.conn_id` (bytes, tag 5) is the receiving connection's 16-byte id: the
`conn_id` on its own `VoiceState` and tracks. A user may be signed in on several
devices, each its own connection, but holds one voice seat: `JoinVoice` on one
connection first removes the user's seat on any other. Clients find their own
seat and audio track by `conn_id`, never by user id. A resume that restores a
leased voice seat keeps the old id; any other Hello gets a new one. After a
reconnect, a client rejoins voice only if no connection of its user holds a
seat, so a device that lost the seat to another does not take it back. Track
ids are unique only within one server run and start again from 1 after a
restart, so receivers key per-sender state by track id and `conn_id`. Servers
that predate the field send it empty; clients then match their user instead.

`Channel.effective_permissions` (optional uint64, tag 12) contains permissions
resolved for the receiving user, including channel overrides. An absent field
means unknown, not an empty permission set. Welcome and channel events include
it. After role, grant, override, or channel changes, the server diffs each
session's visible channel view: `ChannelCreated` grants visibility,
`ChannelUpdated` refreshes metadata/permissions, and `ChannelDeleted` also
means visibility was revoked. Clients discard that channel's cached content.
Shared command acknowledgements may omit personalized permission bits.
`TrackAdded` and `VoiceStateUpdated` reach only sessions that may view the
named channel; `TrackRemoved` carries only the opaque track id. A
`VoiceStateUpdated` without a channel (a leave) reaches only sessions that were
told that connection was in a channel. The actor's own command reply is
filtered the same way. A member who loses `VIEW_CHANNEL` or `CONNECT` is removed
from voice with the usual leave events.

### Direct messages and the Welcome size

`Snapshot.direct_messages` holds at most the 100 most recently active
conversations (by newest message, else creation). When there are more,
`Snapshot.direct_messages_next` (tag 8) is an opaque cursor; `ListDms` with it
returns the next page of at most 100, newest first, with the caller's read
states for those conversations, and `DmPage.next` while more remain.
`Snapshot.read_states` covers guild channels and the DMs included. If a
`Welcome` would still exceed one frame, the server halves the DM list until it
fits; a login is never refused for its DMs. A conversation the session was
not sent is announced with `ChannelCreated` before the first event in it.

DM channel events reach only that conversation's members: a member sees
`ChannelCreated` the first time and `ChannelUpdated` after; a session that
knew the conversation and is no longer in it sees `ChannelDeleted`. The diff
after role, grant, override and guild channel changes covers guild channels
only and runs at most once per 100 ms per session for a burst.

### Delivery

Each session's control frames are delivered in order or not at all. A
session that cannot keep up (the event bus 1024 events ahead of it, frames
addressed to it backed up for 10 s, or one write stalled for 10 s) gets
`ServerNotice` and the connection closes with code 2; the client reconnects
and resyncs from `Welcome`. A newer `SubscriberHint` or `RequestKeyframe`
for a track replaces one not yet sent. A command that waits on the database
does not stop delivery of events.

The `Welcome` and the events after it do not overlap for new messages: a
message counted in a `Snapshot.read_states` `mention_count` is never sent
after the `Welcome` as `MessageCreated`, and every later message is. A client
adds the mentions it receives to the snapshot's count. The server subscribes
the session to its event bus at the snapshot's read point, while no message
is between its commit and its publish.

### Limits

Quotas and input limits are listed in [`docs/security.md`](../security.md)
sections 6 and 7. The ones a client must handle:

- A rejected `Hello` because of a quota is `RATE_LIMITED` with
  `retry_after_ms`, and the connection closes. Reconnect after the wait
  (section 1.2 lists which `Hello` errors are retryable).
- Commands other than `Ping` share a per-account budget (burst 100, then 20
  per second). `SendMessage` and `Typing` have tighter quotas of their own.
  `Ping` is never rate limited.
- The server answers every first Initial with a QUIC Retry (address
  validation); `quinn` clients follow it transparently. Then at most 32
  connections per address (IPv6: per /64), pending handshakes
  included. Further handshakes are refused.
- Names (display, channel, group DM, role, webhook) are trimmed and stored in
  NFC; hidden characters are `INVALID`: Unicode categories Cc, Cf, Zl, Zp,
  Co, Cn and every default-ignorable code point, except U+200D and
  U+FE0E/U+FE0F, which emoji need. Topics are at most
  1024 characters. A message carries at most 10 attachments, each once.
  Changing the display name needs `CHANGE_NICKNAME`.

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
| GET / PUT | `/api/v1/server/settings` | MANAGE_SERVER profile: name, description, iconId, registrationMode and revision; stale PUT returns 409 |
| POST | `/api/v1/server/icon` | MANAGE_SERVER raw raster upload, up to 2 MiB; returns iconId, applied only by settings PUT |
| GET | `/api/v1/channels/{id}/messages?before=<id>&limit=50` | History page before cursor, ascending ULID order. `after=<id>` instead returns the page after the cursor; `before` and `after` together are `400` |
| GET | `/api/v1/channels/{id}/messages/around/{message_id}?limit=` | Jump to message |
| GET | `/api/v1/search?q=&channel=&from=&before=&after=&has=&limit=` | Message search (FTS5, T-065), below |
| POST | `/api/v1/attachments` | multipart upload → `{attachment_id, url, mime, size, width?, height?}` |
| GET | `/api/v1/attachments/{id}/{filename}` | Download (auth required, signed URL variant for previews). Also serves avatars (`User.avatar_attachment_id`) and the server icon to any signed-in member |
| GET | `/api/v1/channels/{channel_id}/overrides` | Requires MANAGE_CHANNELS and VIEW_CHANNEL; guild-only rows `{roleId: ULID, allow: decimal-u64, deny: decimal-u64}` |
| GET | `/api/v1/webhooks?before=&limit=` | Metadata only; requires `MANAGE_SERVER` |
| POST | `/api/v1/webhooks/{id}/{token}` | Incoming webhook `{content, username?, avatar_url?}`; no bearer needed |
| GET | `/api/v1/moderation/members?before=&q=` | KICK_MEMBERS or BAN_MEMBERS; 50-row member metadata/capability page, literal search up to 100 characters |
| GET | `/api/v1/moderation/bans?before=&q=` | BAN_MEMBERS; 50-row active ban metadata/capability page, expired/lifted bans omitted |
| GET | `/api/v1/audit-log?before=&limit=&actor=&action=&since=&until=` | Requires `VIEW_AUDIT_LOG` |
| GET | `/api/v1/invites?before=&limit=` | Invite metadata: managers see all; creators see their own |
| POST | `/api/v1/channels/{id}/messages` | Bot API (`Authorization: Bot`): `SendMessage` JSON → `{messageId}` |
| PATCH / DELETE | `/api/v1/messages/{message_id}` | Bot API: `EditMessage` JSON, or delete → `204` |
| POST | `/api/v1/dms` | Bot API: `CreateDm` JSON → `{dmChannel}` |
| GET | `/api/v1/members?after=&limit=` | Bot API: `{members[], next?}`, by user id, at most 1000 |
| GET | `/api/v1/channels` | Bot API: `{channels[], directMessages[]}` the bot can see |
| GET | `/api/v1/events?prefix=` | Bot API: `text/event-stream` of `ServerEnvelope` JSON, `Welcome` first |
| GET | `/healthz` | internal listener only (`127.0.0.1:9101`) |
| GET | `/metrics` | internal listener only |
| GET | `/connections` | internal listener only: live connections for `ss-server conn list` |
| GET | `/.well-known/acme-challenge/{token}` | ACME HTTP-01 listener on TCP 80 only; every other path there redirects to HTTPS |

The bot routes take `Authorization: Bot <token>` and refuse a session bearer;
the other routes refuse a bot token. Their bodies and replies are canonical
protobuf JSON (as below), generated from the `.proto` files by `pbjson`.
They are described in [`docs/bots.md`](../bots.md) and in section 7.

`scripts/check-protocol-doc.py` fails when this table and the axum routes in
`ss-server`, or the message tables in section 3 and the envelope `oneof`s,
name different things.

History JSON uses camelCase fields, base64 bytes, decimal strings for 64-bit
integers, and named protocol enums. Pages contain at most 100 messages (the
client requests 50). A message a webhook posted has no `author` and carries
`webhookId` and `webhookName` (`Message.webhook_id`/`webhook_name`, tags 11
and 12) instead. The client sends the Welcome token as the HTTPS bearer
and verifies the certificate with the trust policy of the authenticated QUIC
session (section 1): the same leaf pin for a self-signed server, hostname and
CA chain for a public certificate. "Pinned HTTPS" below means this. It is the same session
token as on QUIC, with the same lifetimes (section 1.1); ending the session
makes the next request `401`. No redirects are followed.
The public listener terminates TLS itself with the same hot-reloaded leaf
certificate as QUIC, so one trust decision covers both transports. Plain HTTP on
the public port is refused; only the loopback health/metrics listener remains
unencrypted. ACME HTTP-01 uses TCP 80 for challenges and redirects every other
request to this HTTPS listener.

Search takes a literal query `q` of 1 to 16 words and at most 256
characters, and optional filters: `channel` and `from` (author) are ULIDs,
`before` and `after` are Unix milliseconds (`after` must be earlier), `has` is
`link`, `file` or `image`, and `limit` is 1–100 (default 50). Only channels the
caller can see, and DMs they belong to, are searched, and each hit is checked
again after the query. Results are an array of `{id, channelId, authorId,
authorName, createdAt, excerpt}` with ULID strings and a decimal `createdAt`,
newest first.

Webhook metadata pages are newest-first by ULID with an exclusive `before` cursor
and default limit 50 (allowed 1–100). Rows contain `id`, `channelId`, `name`,
`createdBy`, `createdAt` and nullable `revokedAt`. IDs are base64 and timestamps
are decimal strings. Tokens, token hashes and posting URLs are never included.
CreateWebhook still reveals its token only in the acknowledgement. The desktop
constructs the HTTPS posting URL from its connected server address and keeps it
only in the open creation panel. Metadata requests use pinned HTTPS,
a ten-second timeout and a 256 KiB response cap.

Audit pages are ordered by creation time and ID descending. The optional `before`
ULID is exclusive; an unknown cursor produces an empty page. The default limit is
50, at most 100 (zero returns an empty page). Optional `actor` (ULID) and `action`
(1–128 bytes) are exact matches. `since` is inclusive and `until` exclusive, both
Unix milliseconds; when both are present, `since` must be earlier. Filters combine
with AND before pagination. Response IDs are base64, `createdAt` is a decimal
string, and `details` is JSON data. The desktop requests 50 rows, caps responses at
1 MiB and uses a ten-second timeout over pinned HTTPS.

Invite pages are newest-first by ULID with a default limit of 50 (allowed 1–100).
`MANAGE_SERVER` grants all records; otherwise `CREATE_INVITES` grants only records
created by the caller. Records use protobuf-JSON identifiers/timestamps and include
creator, uses, optional maximum uses, creation/expiry/revocation timestamps. Neither
plaintext codes nor hashes are returned. A plaintext code is available only in the
successful `CreateInvite` control response; it is not recoverable from history.

Every public response carries `X-Content-Type-Options: nosniff`,
`X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, a
`default-src 'none'; frame-ancestors 'none'; sandbox` CSP, and
`Cache-Control: no-store` unless the route sets its own. A request over a
quota is `429` with `Retry-After` (seconds). Bodies are capped at 64 KiB
except uploads; webhook posts need `Content-Type: application/json` (`415`
otherwise). A request has 10 s to send its head and 30 s to finish (300 s for
attachment transfers, including sending a download body). At most 64
connections per address; a connection idle for 60 s or open for 10 minutes is
closed, so clients must reconnect.

An upload form must send `channel_id` before `file` (`400` otherwise): upload
permission is checked before any file byte is read, and the file streams to
disk. Downloads stream from disk. One account may have 16 uploads and downloads
open at once; more get `429` with `Retry-After: 1`. Signed-URL downloads have
their own pool of 16 per signer, never per client address, separate from the
signer's own transfers, so a leaked URL cannot lock the uploader out. A download is cut
off 300 s after it starts even if the client has stopped reading, which frees
its slot. A deleted message's attachments return `404`, signed URLs included,
and their files are deleted by the next cleanup pass.

The upload reply's `url` carries `token=base64url(expiry ‖ signer ‖ mac)`:
big-endian Unix-millisecond expiry (8 bytes), the uploader's id (16), and
HMAC-SHA256 over attachment id ‖ signer ‖ expiry (32), keyed by
`data-dir/attachment.key`. It is opaque to clients and lives 10 minutes; tokens
issued before this layout stop verifying on upgrade.

## 7. Versioning

`Hello.protocol_version = 1`. Server supports a range; mismatch → `Error
PROTOCOL_VERSION` with `min`/`max`. Protobuf fields are only ever added; never
renumbered. Datagram header version bits allow one future header change.
Clients ignore fields they do not know, such as `Welcome.conn_id` on builds
older than it. The server has no client capability signal, so this holds for
every addition:

- A server event (request id 0) whose `msg` variant the client does not know
  decodes with no message. The client skips and counts it
  (`ClientControl::unknown_messages`). A reply in an unknown shape still ends
  its own request, with an error for that request only.
- An `Error` with request id 0 is an unsolicited notice, for example the
  `RATE_LIMITED` sent before a media rate-limit close. It reaches the
  application as `ClientEvent::ServerError` with its `retry_after_ms`; the
  connection stays up until the server closes it.
- Permission masks keep bits the client has no name for. The mirrored
  `Channel` and `Role` hold the raw mask; `State::permissions` returns only
  the known bits, so an unknown grant enables nothing.
- The server answers a command variant it does not know `INVALID`
  ("unsupported command") and keeps the connection.

### Role management compatibility

Servers advertise `role-management-v2` in Welcome server features for `MoveRole`
and explicit color clearing. Clients disable those additions on older servers.
Role moves and deletion compaction emit `RoleUpdated` for every changed position.
Override reads use the session bearer over pinned HTTPS (section 6).

### Session commands

Servers advertise `sessions` in Welcome server features for `Logout`,
`ListSessions`, `RevokeSession`, `SessionRevoked` and close code 6. An older
server answers those commands `INVALID` ("unsupported command"); a client
that is signing out then only forgets the token locally.

### Bot accounts

Servers advertise `bots` in Welcome server features (T-080). A bot is a
`User` with kind `USER_KIND_BOT`, shown with a BOT tag. It has no password
and no sessions: `Hello{bot_token}` signs it in, and `Welcome.resume_token`
echoes that token, which a client presents as `bot_token` again after a
drop. A bot token is never accepted as `session_token`, `resume_token` or a
`Bearer`, and a session token never as `bot_token` or `Bot`. A bot holds the
bits its roles grant that are also in its token's scopes, never `CONNECT`,
`SPEAK` or `STREAM` (`JoinVoice` is `FORBIDDEN`), and `ADMINISTRATOR` only
when both have it; every check and visibility filter applies this cap.

Revoking or replacing a token closes the QUIC connections that used it
(`SessionRevoked`, code 6) and ends its event streams with a
`sessionRevoked` event, at once; the operator CLI's changes are picked up
within 30 s (section 1.1). An event stream gets the events a QUIC session of
the bot would, with the same filters, and also the bot's own actions. It
ends after 9 minutes or when it falls behind the event bus, with a
`serverNotice` event, and the bot reconnects for a new `welcome`; events
between two streams are not replayed. Each bot spends a budget of 50
requests, then 5 a second, across both transports, and holds at most two
streams.

### Server settings

Settings GET returns `name`, `description`, nullable `iconId`, `registrationMode`,
opaque `revision`, nullable `iconFilename` and `iconMime`. PUT sends only the five
editable/profile fields, including the revision from the last read. Updates are
atomic and audited. Names are 1–100 visible characters, descriptions at most 2048.
Icons are PNG/JPEG/WebP, at most 2 MiB and 4096×4096 pixels. A new icon must be an
unattached upload owned by the caller. A null iconId clears the icon.

Registration modes are `invite` and `closed`; closed stops new accounts without
invalidating sessions or invites. Password login and session resumption remain
available. Both settings routes and uploads use session bearer authentication.

Member moderation pages use `{members, bans, nextCursor, canBan}`; only the requested
collection is populated. Cursors are exclusive ULIDs, newest first. Timestamps are
decimal Unix milliseconds. Member `lastSessionAt` is the maximum retained session
activity timestamp, not precise offline history. Role names include the default
role. Per-target `canKick`, `canBan` and `canUnban` reflect current authority and
hierarchy; mutations still use the existing audited QUIC moderation commands and
recheck server authority. Session tokens and account credentials are never projected.

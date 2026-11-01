# ss-client-core

Headless, UI-independent connections and live server state. Hosts supply a
Tokio runtime, certificate `PinStore`, and optionally a session `Storage`.

`Client::connect` authenticates and applies Welcome before returning.
`connect_with_storage` also saves the session token and last server; the
host decides whether those live in a keychain, file, or memory. Unknown or
changed certificates return an actionable error; only the host can approve.

Subscribe to `ClientEvent`, then read `client.state()`. Notifications describe
already-applied changes. On broadcast lag, read the state again. Do not hold
a state read guard across await. `channel_tree()` supplies stable tree order;
`permissions(channel)` returns server-resolved bits or None for older peers.

Typed commands return resources or errors. During reconnect commands fail;
mutations are never replayed. A timeout may mean the server already acted.
Session resume backs off from 1 to 30 seconds with jitter, replaces the full
snapshot, and rejoins requested voice membership. Kick/ban and trust/auth
failures stop retries. Drop closes the connection; `close().await` also waits
for the owning worker to stop. `ClientSet` owns independent server clients.

`media::VideoTrackSender` opens byte-level GOP writers after StartStream.
One `VideoTrackReceiver` accepts all incoming video groups and exposes their
track ids for demux. Subscribe through control first; reset/drop obsolete GOPs.
No capture, codec, or rendering is part of these APIs.

`load_older` fetches HTTPS history with the QUIC certificate pin and session
bearer, without redirects or environment proxies. A concurrent state change
returns `StateChanged` for retry. Caches retain 200 live messages per channel
plus requested history pages. Reconnect or lost visibility clears the cache.

`VoiceSession` owns capture, processing, Opus, jitter buffers, mixing, and the
client media bridge. Hosts select PTT, VAD, or open mic and receive 500 ms
stats plus actionable device events. Test tone/silence sources need no audio
hardware. `leave().await` releases every stream and worker.

May depend on `ss-core`, `ss-proto`, `ss-net`, `ss-audio`, `ss-input`, Quinn stream handles.
Must not depend on `ss-server`, `ss-cli`, SQL, or windowing toolkits.

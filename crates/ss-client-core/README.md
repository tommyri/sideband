# ss-client-core

Headless, UI-independent connections and live server state. Hosts supply a
Tokio runtime, certificate `PinStore`, and optionally a session `Storage`.
`Client::connect` authenticates and applies Welcome before returning;
`connect_with_storage` also saves the session token and last server wherever
the host chooses. Unknown or changed certificates return an actionable error;
only the host can approve.

Subscribe to `ClientEvent`, then read `client.state()`: events describe
applied changes, so on broadcast lag read the state again, and never hold a
read guard across await. `channel_tree()` gives stable order;
`permissions(channel)` returns server-resolved bits or None for older peers.

Typed commands return resources or errors; a timeout may mean the server
already acted. While reconnecting, only `SendMessage` waits (queue of 100) and
is replayed with its nonce; other commands fail. Resume follows
`02-protocol.md` §1.2 (backoff 2 to 24 s plus jitter, final errors stop it),
replaces the snapshot, rejoins voice and re-reads cached history. Drop closes
the connection; `close().await` also waits for the worker. `ClientSet` holds
one client per server.

`media::VideoTrackSender` opens GOP writers after StartStream; one
`VideoTrackReceiver` demuxes incoming video groups by track id. With the
`screen-share` feature, `ScreenShareSession` feeds `ss-video` capture and H.264
into those writers, answers keyframe requests and removes its track when
capture ends. Rendering stays outside this crate.

`load_older` fetches HTTPS history with the QUIC session's trust policy and
bearer, without redirects or proxies; a concurrent change returns
`StateChanged`. Caches keep 200 live messages per channel plus loaded pages;
lost visibility clears a channel's cache.

`VoiceSession` owns capture, processing, Opus, jitter buffers, mixing, and the
media bridge. Hosts pick PTT, VAD or open mic and get 500 ms stats and device
events; test sources need no hardware. `leave().await` releases everything.

May depend on `ss-core`, `ss-proto`, `ss-net`, `ss-audio`, `ss-input`, optional
`ss-video`, and Quinn stream handles. Must not depend on `ss-server`, `ss-cli`,
SQL, or windowing toolkits.

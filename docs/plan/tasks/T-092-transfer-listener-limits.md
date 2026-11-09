# T-092 — Stream attachments, bound the remaining listeners, retire the spike default

Status: todo
Phase: 4
Size: M
Depends on: T-027, T-033, T-066
Paths: `crates/ss-server/src/http/attachments.rs`, `crates/ss-server/src/storage.rs`, `crates/ss-server/src/http/hardening.rs`, `crates/ss-server/src/tls.rs`, `crates/ss-server/src/main.rs`, `crates/ss-server/src/previews/fetch.rs`, `deploy/socialspeak.service`, `deploy/docker-compose*.yml`

## Goal
Memory and file descriptors stay bounded under hostile but authenticated
use, and running the binary with no arguments is safe.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced or measured.

## Scope
- Downloads read the whole blob with `tokio::fs::read` per request and the
  request deadline covers only the handler, not sending the body. A member
  who uploads one 25 MiB file and opens many HTTP/2 streams without reading
  pins 25 MiB each until the idle or max-age close. Stream from disk and
  cap concurrent transfers per account. Uploads are also buffered in full,
  before the permission check: stream to the temporary file and check
  permission before reading the body.
- A deleted message's attachment stays downloadable to anyone holding the
  id, because `may_read` ignores `deleted_at`. Return 404 and decide
  whether the blob is unlinked.
- `tls-alpn-01` mode hands every accepted TCP socket to `rustls-acme`
  with no cap or timeout, so idle connections exhaust descriptors. Wrap
  that path in the same per-address cap and handshake timeout as
  `serve_tls`. The HTTPS accept loop `continue`s on a persistent error with
  no backoff; sleep briefly. ACME cache files, the certificate key
  included, are written with default permissions; create `tls/acme` as
  0700.
- The systemd unit sets no `LimitNOFILE`, and the server's own HTTP cap is
  1024 connections, equal to the usual soft limit. Add `LimitNOFILE=65536`
  and `ulimits.nofile` in both compose files, or raise the soft limit at
  startup.
- `ss-server` with no subcommand still starts the unauthenticated voice
  spike. Require a subcommand (move the spike behind an explicit `spike`
  subcommand or delete it), and update the README, `docs/security.md` and
  `deploy/smoke.sh`.
- `HostGate.hosts` in the preview fetcher grows by one entry per distinct
  host for the life of the process; the media `budgets` map never removes
  entries. Bound both.
- Unconfirmed: the preview `reqwest` client does not call `.no_proxy()`;
  with `HTTP(S)_PROXY` set the DNS pin would be bypassed. Confirm against
  the enabled features and close it. `cached_fingerprint` returns the first
  `cached_cert_*` in directory order; check with two cache files.

## Out of scope
NGINX sample configuration (T-098).

## Acceptance criteria
- Test: 200 concurrent stalled downloads of a 25 MiB file keep resident
  memory within a stated bound.
- Test: an upload without permission is rejected before the body is read.
- Test: `ss-server` with no arguments prints usage and exits non-zero.
- Test: idle TCP connections to the ALPN listener are closed at the
  handshake deadline and capped per address.

## Notes from implementation

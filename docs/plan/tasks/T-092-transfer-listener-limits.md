# T-092 — Stream attachments, bound the remaining listeners, retire the spike default

Status: done
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

Each finding has a test written first and seen failing for the stated reason,
except the ALPN listener, whose old path (`rustls-acme`'s `Incoming`) had no
seam to test; its test was mutation-checked instead (no cap, no deadline:
fails).

| Finding | Test | Before the fix |
|---|---|---|
| Downloads buffered whole | `tests/transfer_memory.rs` | 200 stalled HTTP/2 streams grew RSS by 4.9 GiB |
| Upload read before permission | `transfers.rs` `an_upload_without_permission_is_rejected_before_the_file_is_read` | waited for a body that never ends |
| No per-account transfer cap | `transfers.rs` `concurrent_transfers_are_capped_per_account` | 17th open download was 200 |
| Deleted message's file served | `transfers.rs` `a_deleted_messages_attachment_is_gone` | bearer and signed URL were 200 |
| Spike is the default | `ops.rs` `no_subcommand_prints_usage_and_exits_non_zero` | ran until killed |
| ALPN listener unbounded | `tls::tests::idle_alpn_connections_...` | (new seam, see above) |
| `cached_fingerprint` first-in-dir | `tls::tests::the_fingerprint_comes_from_the_configured_cache_file` | returned a decoy's leaf |
| Preview proxy bypass | `previews::fetch::tests::proxy_variables_do_not_route_preview_fetches` | child connected to the proxy |
| `HostGate` grows | `previews::fetch::tests::the_host_gate_forgets_hosts_nobody_is_fetching` | 4 hosts kept, expected 1 |
| Media `budgets` grows | `media::permitted::tests::budgets_of_quiet_senders_are_dropped` | 257 kept, expected 1 |

- **Memory bound.** The test allows 64 MiB of RSS growth for 200 stalled
  downloads of one 25 MiB file on one HTTP/2 connection. It runs twice: with
  the default cap (16 served, 184 get 429) and with the cap raised to 200 so
  every stream is served. Measured growth was 0–18 MiB and 17–23 MiB on macOS.
  A download reads 64 KiB chunks only when hyper takes the previous one, so
  per stream it holds at most two chunks plus what flow control queues
  (hyper's server sends at most 400 KiB ahead). `Content-Length` is set from
  the file. The test is `#[cfg(unix)]` (it samples RSS with `ps`).
- **Transfer cap.** `UploadPolicy::transfers_per_account`, default 16:
  uploads plus downloads open at once per account (the desktop allows 10 per
  app). Over the cap is 429 `too many concurrent transfers` with
  `Retry-After: 1`. 304s take no slot. Map entries leave with their last
  transfer.
- **Signed URLs count against their signer** (review follow-up). Keying them
  on the peer address let one member block everyone's signed downloads behind
  NGINX. The token is now `expiry(8) ‖ signer(16) ‖ HMAC(id ‖ signer ‖
  expiry)(32)`, base64url; the only issuer is the upload reply, so the signer
  is the uploader. No client parses the token (desktop and core download with
  the bearer and read only `attachmentId` from the reply). Old 40-byte tokens
  fail to verify after upgrade; they lived 10 minutes (`token_ttl`). The
  server has no trusted-proxy setting and forwarded headers are not read.
  Every allowed download is now attributable, so there is no address or
  global bucket. Test: `signed_downloads_through_one_proxy_count_against_their_signer`
  (red: 429 for the second member).
- **Signed downloads use their own pool** (merge follow-up on main, second
  review). Charging them to the signer's account let readers of a leaked URL
  (valid 10 minutes) hold all 16 of the uploader's slots and lock them out of
  their own uploads and downloads. Signed downloads now enter a separate
  per-signer gate of the same size. Test:
  `a_leaked_signed_url_cannot_lock_its_signer_out` (red: the signer's bearer
  download got 429).
- **Deadline independent of polling** (review follow-up). A download's file
  and slot belong to a spawned task that reads into a one-slot channel and
  stops when the last chunk is queued, the body is dropped, or
  `UploadPolicy::transfer_timeout` (default 300 s) passes. On timeout the file
  and slot go at once; the task then waits only to hand the body an error, so
  a cut-off download never looks complete. Test:
  `a_download_nobody_reads_frees_its_slot_at_the_deadline` with a 300 ms
  deadline (red: still 429 after 700 ms). Uploads remain bounded by the
  request deadline middleware.
- **Uploads.** `channel_id` must precede `file` (400 `channel id must come
  before the file`); the official client already sends it first. Permission
  is checked when `channel_id` arrives, before any file byte is read. The file
  streams through `storage::Incoming` into `attachments/.incoming/`, hashed
  and size-capped as it goes; only the first 64 KiB is kept for `infer`, and
  image dimensions are read from the file. A second `channel_id` is 400.
  Unknown fields are drained, not buffered. `Store::save(&[u8])` (server icon)
  goes through the same path. `.incoming` files older than the orphan window
  (crash leftovers) are removed by the cleanup pass. `Store::read` became
  `Store::open_blob`.
- **Fuzz target.** `fuzzing::upload_form` keeps its signature and still
  returns the bytes, but now rejects forms that send `file` before
  `channel_id` or send `channel_id` twice. `fuzz/fuzz_targets/multipart_upload.rs`
  needs no change; its accepted inputs narrow.
- **Deleted messages.** Downloads of a deleted message's files are 404 for
  bearer and signed URL alike. The blobs are unlinked: the cleanup pass
  (every 5 minutes, and `collect_orphans`) now also deletes rows whose message
  has `deleted_at` set (new `db::attachments::on_deleted_messages`), except
  avatars and the server icon; a deduplicated blob goes when no row points at
  it. No migration.
- **Listeners.** The HTTPS accept loop sleeps 100 ms (`ACCEPT_BACKOFF`) after
  a failed accept. `tls-alpn-01` now runs its own accept loop
  (`tls::serve_challenges`): 64 per address, 1024 in all, 10 s handshake
  deadline, then close; the ACME state machine is polled by `drive_acme` for
  both challenge types. `tls/acme` (and a missing `tls/`) is created 0700 and
  an existing one is narrowed to 0700. Cache files inside keep rustls-acme's
  default mode; the directory is the protection.
- **Unconfirmed items: both real.** reqwest 0.13 with default features off
  still adds `Matcher::from_system()`, which in hyper-util always reads
  `HTTP(S)_PROXY`/`ALL_PROXY`; the `system-proxy` feature only adds the macOS
  and Windows system settings. Fixed with `.no_proxy()`. `cached_fingerprint`
  now reads exactly the file `DirCache` writes for the configured
  `public_host` and directory (SHA-256 of domain, NUL, directory URL,
  base64url); the test cross-checks the name against `DirCache::store_cert`.
- **Descriptors.** `LimitNOFILE=65536` in the unit and `ulimits.nofile` 65536
  in `docker-compose.yml` and `docker-compose.nginx.yml`. The soft limit is not
  raised at startup.
- **CLI.** `ss-server` with no arguments prints usage to stderr and exits 2.
  The spike is `ss-server spike [--data-dir] [--bind]`, default bind now
  `127.0.0.1:4433` (it was `0.0.0.0:443`, contradicting `docs/security.md`).
  `--check-migrations` became the `check-migrations` subcommand.
- **Bounded maps.** `HostGate` drops a host when its last holder or waiter
  leaves. Media `budgets` prunes entries older than the 1 s window when a new
  sender arrives and the map has 256 or more (small edit in T-094's
  `media/permitted.rs`).
- Outside the listed paths: `http/hardening.rs` backoff is listed; also
  `db/attachments.rs` (query), `fuzzing.rs` (new reader), `media/permitted.rs`
  (T-094's), `Cargo.toml` (`hyper` client as a dev-dependency), new tests
  `tests/transfers.rs` and `tests/transfer_memory.rs`, a test in `tests/ops.rs`,
  `README.md`, `crates/ss-server/README.md`, `docs/security.md`,
  `docs/plan/02-protocol.md` §6 (upload field order, transfer cap, deleted
  files), `deploy/smoke.sh`.
- `deploy/README.md` "Smoke test" still says the spike container runs "with
  no subcommand"; it was left for T-098, which owns the rest of `deploy/`.
- `just run-server` (`cargo run -p ss-server` with no subcommand) now prints
  usage instead of starting the spike; the justfile is outside this task's
  paths and was left unchanged.

# T-115 — Make the default ports agree

Status: awaiting-acceptance
Phase: 4
Size: S
Depends on: T-064, T-092
Paths: `crates/ss-server/src/config.rs`, `crates/ss-client-core/src/connection.rs`, `apps/desktop/src/features/onboarding/`, `deploy/`, `docs/self-hosting.md`, `docs/plan/02-protocol.md`

## Goal
A server set up with the defaults, and a client given only its host name,
reach each other over both QUIC and HTTPS.

Source: T-064, which found the mismatch while writing the guides; confirmed
by the maintainer's session on main.

## Scope
- The client derives HTTPS from the QUIC port: `ServerAddr::new` sets
  `https_port: port` ("normally the same numeric port"). But
  `ss-server init` writes `bind_quic = "0.0.0.0:443"` and
  `bind_http = "0.0.0.0:4430"` (`config.rs` template and defaults). On a
  default systemd install the client's HTTPS requests (history, search,
  attachments) go to TCP 443, where nothing listens.
- The desktop's address validation (`onboarding/validation.ts`) uses port
  5781 when the address has none, while the server and `ss_net::DEFAULT_PORT`
  use 443. A host name typed without a port does not reach a default server.
- `deploy/README.md` opens TCP 443 and Compose publishes TCP 443, and
  `02-protocol.md` says "HTTPS on TCP 443", while `init` writes 4430.
- Pick one rule and apply it everywhere: for example, default both binds to
  443 (UDP and TCP), make the desktop default 443 like `ss_net`, or have the
  server advertise its HTTPS port in `Welcome` (a protocol change, adding
  only a new field). Keep existing saved addresses working.

## Out of scope
NGINX in front (its sample already proxies TCP 443).

## Acceptance criteria
- Test: a server started from the `init` defaults serves the client's
  history request at the address the client derives.
- Test: the desktop's default port equals `ss_net::DEFAULT_PORT`.
- `deploy/README.md`, `docs/self-hosting.md` and `02-protocol.md` state the
  same ports.

## Notes from implementation

Implemented on 2026-11-12 on `task/T-115-default-ports-agree`.

- **Rule: one port number for both transports, default 443.** QUIC on UDP,
  HTTPS on TCP. `ss-server init` (template and `ServerConfig::default`) now
  writes `bind_http = "0.0.0.0:443"` beside `bind_quic = "0.0.0.0:443"`, the
  desktop uses 443 when an address has no port, and `ServerAddr::new` keeps
  deriving HTTPS from the QUIC port. That is what Compose, the NGINX sample
  (public TCP 443) and `02-protocol.md` already said, so only `init` and the
  desktop moved.
- **Why not the alternatives.** Advertising the HTTPS port in `Welcome` comes
  too late: the desktop probes `GET /api/v1/server` over HTTPS before it opens
  QUIC (`onboarding.rs`). The server also cannot know its public HTTPS port from
  `bind_http` behind Docker port mappings or NGINX, so it would need a new
  setting. No protocol field was added; `Welcome` tag 6 stays free.
  Changing only the desktop default would still leave `init` on 4430.
- **TLS-ALPN-01 collision.** `tls-alpn-01` binds TCP on `bind_quic`, which
  with the new defaults is `bind_http`'s address, so the server failed at
  start with `AddrInUse` (by reading, the Compose files, which set both to
  443, had the same problem with `tls-alpn-01`). When the two TCP addresses overlap (same port, same IP or
  a wildcard on either side) there is now no separate listener: the HTTPS
  listener also offers `acme-tls/1`, which the shared `rustls-acme` resolver
  answers with the challenge certificate, and `serve_tls` closes any
  connection that negotiated `acme-tls/1` after the handshake, before reading
  HTTP. Normal clients never offer that ALPN; without an order in flight the
  resolver returns no certificate and the handshake fails. Non-overlapping
  configs (an old `init` file with 4430) keep the separate listener.
- **Tests, each seen failing first.**
  - `crates/ss-server/tests/default_ports.rs`: runs `ss-server init`, moves
    the QUIC port to a free one while keeping the HTTPS port at the distance
    `init` wrote, starts `ss-server run`, registers with
    `ServerAddr::new(host, quic_port)` and loads history. Red: "history at
    the derived HTTPS port 50020 failed while the server serves HTTPS on
    54007". It also checks `ServerAddr::new(public_host, DEFAULT_PORT)`
    against the written binds.
  - `apps/desktop/src/features/onboarding/validation.test.ts`: reads
    `DEFAULT_PORT` from `crates/ss-net/src/lib.rs` (`?raw` import) and checks
    `parseAddress('voice.example.com')`. Red: 5781 instead of 443. A typed
    port (`host:5781`) is kept. `vite.config.ts` allows Vite to read
    `crates/ss-net/src` for this.
  - `tls::tests::tls_alpn_01_on_the_init_ports_leaves_the_https_port_to_the_https_listener`:
    `prepare` in `acme`/`tls-alpn-01` mode on the template's ports (moved to
    loopback), then binds `bind_http`. Red: `AddrInUse`, "bound twice".
    The ACME directory is unreachable; no order is needed.
  - `tls::tests::the_https_listener_closes_an_acme_tls_handshake_without_serving_http`.
    Red: the `acme-tls/1` connection got the route's 200.
  - `config::tests` now also asserts both template ports equal
    `ss_net::DEFAULT_PORT` and that the template equals the defaults.
- **Migration.** An existing `config.toml` is never rewritten, so servers set
  up by an older `init` keep HTTPS on 4430 and behave as before (their
  clients already enter 4430 as the HTTPS port). The guides tell operators to
  move it to 443 (file or `SS_BIND_HTTP`) or keep telling clients. Saved
  desktop servers store `port` and `https_port` explicitly and keep their
  `host:port` server IDs, so `host:5781` and others are unaffected; only a
  newly typed address without a port changes (5781 to 443). The server does
  not warn about split ports at start; see open questions.
- **Docs.** `02-protocol.md` (one port, why it is not announced),
  `docs/self-hosting.md` (ports table, rule, upgrade note, ACME row),
  `deploy/README.md` (intro, systemd first start, NGINX step 5, TLS-ALPN-01,
  upgrade, next to TeamSpeak). Compose files already published UDP and TCP
  443 and are unchanged.
- **Files outside the Paths:** `crates/ss-server/src/tls.rs` and
  `crates/ss-server/src/http/hardening.rs` (the collision fix), new
  `crates/ss-server/tests/default_ports.rs`, `apps/desktop/vite.config.ts`
  (`server.fs.allow`), `docs/user-guide.md` (said 5781), `docs/security.md`
  §8 (TLS-ALPN-01 paragraph), `docs/plan/tasks/INDEX.md` and
  `docs/plan/tasks/ACCEPTANCE.md` rows. In `ss-client-core` only the
  `ServerAddr` doc comments changed.
- **Validation here (macOS).** `just check` passes (nightly fmt, clippy,
  workspace tests, `plan-check` with the link check);
  `cargo clippy -p socialspeak-desktop --all-targets -- -D warnings`;
  frontend `tsc -b`, `eslint`, `prettier --check`, `vitest run` (157 tests);
  `docker compose config` for both Compose files. `pnpm` was not run and the
  lockfile is unchanged.
- **Unverified.** No real CA has validated `tls-alpn-01` through the shared
  HTTPS listener, and no fresh systemd install was tried; both are the
  acceptance row. The NGINX variant with `tls-alpn-01` cannot work (NGINX
  owns TCP 443) and is out of scope.
- **Open questions.** Should `ss-server run` log a warning when `bind_http`
  and `bind_quic` use different ports (it would catch upgraded 4430 configs)?
  Left out to stay within the Paths.

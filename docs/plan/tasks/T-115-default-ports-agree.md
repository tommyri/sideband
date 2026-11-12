# T-115 — Make the default ports agree

Status: todo
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

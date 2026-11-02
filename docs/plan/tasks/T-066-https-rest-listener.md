# T-066 — Serve the public REST listener over TLS

Status: done
Phase: 4
Size: M
Depends on: T-033, T-061
Paths: `crates/ss-server/src/http/`, `crates/ss-server/src/tls.rs`, `crates/ss-client-core/`, `deploy/smoke.sh`, `docs/security.md`, `docs/plan/02-protocol.md`

## Goal
Bearer tokens, message history, and attachments no longer cross the network
in plaintext. T-061 rated this the one high finding it did not fix.

## Scope
- The public HTTP listener (`bind_http`) serves HTTPS with the same
  certificate and key as the QUIC endpoint, in all three TLS modes
  (self-signed, files, ACME), and follows certificate reloads (T-033).
- ACME HTTP-01 keeps working: the port-80 listener answers challenges and
  redirects everything else to HTTPS.
- Clients verify the HTTPS certificate with the same pinned fingerprint
  (TOFU) or WebPKI trust used for QUIC. `ss-client-core` history and
  attachment requests use that verifier (`reqwest` with a custom
  `rustls` config); the Tauri `fetch` wrapper (T-042) inherits it.
- Plain HTTP on the public listener is refused or redirected; the internal
  `127.0.0.1:9101` listener stays plain.
- Update `deploy/smoke.sh`, `deploy/README.md`, `docs/security.md`
  section 8.1 and the known-gaps list, and the HTTP section of
  `docs/plan/02-protocol.md`.

## Out of scope
HTTP/3 for REST; serving the web UI.

## Acceptance criteria
- Integration test: a request with a bearer token over plain HTTP is refused
  or redirected; the same request over HTTPS with the pinned certificate
  succeeds; a changed certificate is rejected by the client verifier.
- `deploy/smoke.sh` passes against the container with HTTPS.
- Certificate rotation (`ss-server tls rotate`) is picked up by the HTTPS
  listener without a restart.

## Notes from implementation

- The hardened public accept loop now performs bounded rustls handshakes and
  then applies the existing connection caps, peer quotas, HTTP deadlines, and
  idle/age shutdown behavior. The internal health/metrics listener remains
  plain HTTP.
- HTTPS and QUIC share the live `ResolvesServerCert`. Self-signed rotation and
  file/ACME reloads therefore affect new handshakes on both sockets without a
  restart. The listener advertises HTTP/2 and HTTP/1.1 with ALPN.
- Client history already constructed an HTTPS-only `reqwest` client with the
  approved QUIC pin, disabled proxies, and no redirects. The Tauri bridge calls
  that client-core path rather than exposing direct webview networking.
- ACME HTTP-01 now always binds TCP 80 for challenges and redirects every other
  request to the configured HTTPS port. `bind_http = :80` is rejected for that
  mode; the legacy `http_redirect` key remains accepted for existing configs.
- Integration coverage proves plaintext refusal, bearer-authenticated pinned
  HTTPS, wrong/rotated pin rejection, and hot rotation without rebinding. The
  Docker smoke test verifies HTTPS before running the voice clients.

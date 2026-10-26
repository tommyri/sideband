# T-033 — TLS modes: self-signed + fingerprint, files, ACME

Status: todo
Phase: 2
Size: S
Depends on: T-004, T-032
Paths: `crates/ss-server/src/tls.rs`

## Goal
Secure by default with no domain required; painless upgrade to a publicly
trusted cert when a domain exists.

## Scope
- `self-signed` (default): T-004's `generate_self_signed` persisted under
  `tls/`; fingerprint printed by `run` and `fingerprint` and exposed in
  `GET /api/v1/server`. Rotation command `ss-server tls rotate` (clients will
  re-prompt).
- `files`: `cert_path`, `key_path`, hot-reload on change (`notify`).
- `acme`: `rustls-acme` with HTTP-01 on the HTTP listener (requires TCP 80 →
  443 redirect option) or TLS-ALPN-01 on 443; cache under `tls/acme/`;
  contact email in config. Same cert served to QUIC and HTTPS.
- Clients (ss-net `PinStore`) treat a publicly trusted cert as
  automatically trusted but still record the fingerprint and warn on change
  (TOFU still applies to self-signed).
- Document port requirements for each mode in `deploy/README.md` (T-034).

## Out of scope
Client UI for cert changes (T-043).

## Acceptance criteria
- Tests: self-signed start produces a stable fingerprint across restarts;
  `files` mode reloads on cert replacement without dropping QUIC listener;
  `acme` mode is exercised with Pebble in CI (optional job, allowed to be
  manual if too slow).

## Notes from implementation

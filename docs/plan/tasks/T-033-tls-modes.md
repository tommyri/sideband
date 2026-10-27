# T-033 — TLS modes: self-signed + fingerprint, files, ACME

Status: done
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

- Hot reload is a `ResolvesServerCert` inside the existing Quinn endpoint.
  `Endpoint::bind` is unchanged. `Endpoint::bind_resolver` is what `run` uses.
  `CertResolver::replace` swaps the leaf future handshakes see. Open
  connections keep the handshake they already finished.
- `ss-server tls rotate` deletes `tls/identity.json` and generates a new
  self-signed leaf. A running server watches that file. `files` mode watches
  `cert_path` and `key_path`; a failed read keeps the previous certificate.
  Relative paths are inside the data directory.
- ACME uses `rustls-acme` 0.13 with the ring provider. `http-01` is mounted
  on the public HTTP listener. `http_redirect` also binds TCP 80 for the
  challenge and 308s every other path. `tls-alpn-01` accepts TCP on the QUIC
  port with ALPN `acme-tls/1`. The cache is `tls/acme/`. The same resolver is
  the QUIC certificate. The public REST listener stays plaintext, as in
  T-032. Pebble is not in CI. Until the first order completes, the printed
  fingerprint is empty and new QUIC handshakes fail.
- `GET /api/v1/server` is public and reads the live fingerprint plus
  `registration_mode` from SQLite.
- Client trust was already implemented: a WebPKI chain is accepted and
  recorded, and a changed pin returns `FingerprintChanged`. TOFU still
  applies to self-signed certificates. No client change in this task.
- Port requirements are in `deploy/README.md`. The container, systemd unit,
  and release workflow stay with T-034.

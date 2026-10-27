# Ports

T-034 owns the container, the systemd unit, and the release workflow. This
page is only the ports each TLS mode needs.

## self-signed

UDP on the QUIC address, usually 443. Clients pin the fingerprint printed by
`run` and `fingerprint`. `ss-server tls rotate` replaces `tls/identity.json`.
A running server picks up the new leaf without closing the UDP socket.
Connected clients re-prompt.

## files

The same UDP port. `cert_path` and `key_path` are PEM, leaf certificate
first. Replacing either file reloads the certificate. The QUIC socket and
connections that already finished the handshake stay up.

## acme

`challenge = "http-01"` serves `/.well-known/acme-challenge/` on the public
HTTP port. Let's Encrypt calls that path on TCP 80, so either set
`bind_http` to port 80 or set `http_redirect = true`. The redirect listener
binds TCP 80, serves the challenge, and answers every other path with 308 to
`https://<public_host>` (including `bind_http`'s port when that port is not
443). `challenge = "tls-alpn-01"` accepts TCP on the QUIC port for the
`acme-tls/1` handshake. QUIC still needs UDP on that port. The account and
certificate cache is `tls/acme/`. `public_host` must be a DNS name and
`acme_email` is required. The issued leaf is the QUIC certificate. Issuance
against Pebble or the Let's Encrypt staging directory is a manual check.

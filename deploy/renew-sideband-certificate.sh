#!/usr/bin/env bash
# Certbot deploy hook; copies only this site's identity, never the LE directory.
set -euo pipefail
# Set this to your own domain before installing the renewal hook.
domain="sideband.example.com"
lineage="${RENEWED_LINEAGE:-/etc/letsencrypt/live/$domain}"
if [[ -n "${RENEWED_DOMAINS:-}" && " $RENEWED_DOMAINS " != *" $domain "* ]]; then
  exit 0
fi
destination="${SIDEBAND_TLS_DIR:-/etc/sideband/tls}"
# OpenSSL 3.0 (Ubuntu 22.04 and 24.04) exits 0 from -checkhost even when the
# name does not match, so compare its verdict line instead of the status.
verdict=$(openssl x509 -in "$lineage/fullchain.pem" -noout -checkhost "$domain")
[[ "$verdict" == "Hostname $domain does match certificate" ]] || {
  echo "Certificate does not cover $domain: $verdict" >&2
  exit 1
}
openssl x509 -in "$lineage/fullchain.pem" -noout -checkend 0
cert_key=$(openssl x509 -in "$lineage/fullchain.pem" -pubkey -noout | openssl pkey -pubin -outform DER | sha256sum)
private_key=$(openssl pkey -in "$lineage/privkey.pem" -pubout -outform DER | sha256sum)
[[ "$cert_key" == "$private_key" ]] || { echo 'Certificate/key mismatch' >&2; exit 1; }
install -d -m 0750 -o root -g 65532 "$destination"
temporary=$(mktemp "$destination/.bundle.XXXXXX")
trap 'rm -f "$temporary"' EXIT
cat "$lineage/fullchain.pem" "$lineage/privkey.pem" > "$temporary"
chown root:65532 "$temporary"
chmod 0640 "$temporary"
# One atomic file replacement avoids publishing half of a certificate/key pair.
mv -f "$temporary" "$destination/bundle.pem"
# Sideband's watcher picks the bundle up on its own. NGINX is tested only now,
# so an unrelated broken site cannot hold back the QUIC certificate; a failed
# test still fails the hook, with the new bundle already in place.
if ! nginx -t; then
  echo "Installed $destination/bundle.pem; NGINX not reloaded because nginx -t failed" >&2
  exit 1
fi
systemctl reload nginx

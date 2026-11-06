#!/usr/bin/env bash
# Certbot deploy hook; copies only this site's identity, never the LE directory.
set -euo pipefail
domain="sideband.badask.no"
lineage="${RENEWED_LINEAGE:-/etc/letsencrypt/live/$domain}"
if [[ -n "${RENEWED_DOMAINS:-}" && " $RENEWED_DOMAINS " != *" $domain "* ]]; then
  exit 0
fi
destination="${SIDEBAND_TLS_DIR:-/etc/sideband/tls}"
openssl x509 -in "$lineage/fullchain.pem" -noout -checkhost "$domain"
openssl x509 -in "$lineage/fullchain.pem" -noout -checkend 0
cert_key=$(openssl x509 -in "$lineage/fullchain.pem" -pubkey -noout | openssl pkey -pubin -outform DER | sha256sum)
private_key=$(openssl pkey -in "$lineage/privkey.pem" -pubout -outform DER | sha256sum)
[[ "$cert_key" == "$private_key" ]] || { echo 'Certificate/key mismatch' >&2; exit 1; }
nginx -t
install -d -m 0750 -o root -g 65532 "$destination"
temporary=$(mktemp "$destination/.bundle.XXXXXX")
trap 'rm -f "$temporary"' EXIT
cat "$lineage/fullchain.pem" "$lineage/privkey.pem" > "$temporary"
chown root:65532 "$temporary"
chmod 0640 "$temporary"
# One atomic file replacement avoids publishing half of a certificate/key pair.
mv -f "$temporary" "$destination/bundle.pem"
systemctl reload nginx

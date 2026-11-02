#!/usr/bin/env bash
# Build the image, confirm /healthz, then join the spike entrypoint with two bots.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

image="${SMOKE_IMAGE:-socialspeak:smoke}"
health_port="${SMOKE_HEALTH_PORT:-19101}"
quic_port="${SMOKE_QUIC_PORT:-4443}"
spike_port="${SMOKE_SPIKE_PORT:-4433}"

docker build -f deploy/Dockerfile -t "$image" .
docker rm -f socialspeak-smoke socialspeak-spike >/dev/null 2>&1 || true

cleanup() {
  docker rm -f socialspeak-smoke socialspeak-spike >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker run -d --name socialspeak-smoke \
  --user 65532:65532 \
  --cap-add NET_BIND_SERVICE \
  -p "127.0.0.1:${health_port}:9101" \
  -p "127.0.0.1:${quic_port}:443/udp" \
  -p "127.0.0.1:${quic_port}:443/tcp" \
  -e SS_BIND_QUIC=0.0.0.0:443 \
  -e SS_BIND_HTTP=0.0.0.0:443 \
  -e SS_INTERNAL_BIND=0.0.0.0:9101 \
  -v socialspeak-smoke:/data \
  "$image"

ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; do
  if curl -fsS "http://127.0.0.1:${health_port}/healthz" >/dev/null; then
    ready=1
    break
  fi
  sleep 1
done
if [[ "$ready" -ne 1 ]]; then
  docker logs socialspeak-smoke >&2 || true
  echo "healthz did not return 200" >&2
  exit 1
fi

https=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
  if curl -kfsS "https://127.0.0.1:${quic_port}/api/v1/server" >/dev/null; then
    https=1
    break
  fi
  sleep 1
done
if [[ "$https" -ne 1 ]]; then
  docker logs socialspeak-smoke >&2 || true
  echo "public HTTPS did not return server info" >&2
  exit 1
fi
if curl -fsS "http://127.0.0.1:${quic_port}/api/v1/server" >/dev/null 2>&1; then
  echo "public listener accepted plaintext HTTP" >&2
  exit 1
fi

docker run -d --name socialspeak-spike \
  --user 65532:65532 \
  -p "127.0.0.1:${spike_port}:4433/udp" \
  -v socialspeak-spike:/data \
  "$image" --data-dir /data --bind "0.0.0.0:4433"

spike=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; do
  if docker logs socialspeak-spike 2>&1 | grep -q "voice spike listening"; then
    spike=1
    break
  fi
  sleep 1
done
if [[ "$spike" -ne 1 ]]; then
  docker logs socialspeak-spike >&2 || true
  echo "spike did not start" >&2
  exit 1
fi

log="$(mktemp)"
cargo run -p ss-cli --quiet -- \
  connect "127.0.0.1:${spike_port}" \
  --name smoke \
  --channel 1 \
  --bots 2 \
  --open-mic \
  --measure-latency \
  --duration 5 \
  --accept-pin \
  >"$log" 2>&1 || {
    cat "$log" >&2
    exit 1
  }
cat "$log"
if ! grep -q "joined channel" "$log"; then
  echo "ss-cli did not join" >&2
  exit 1
fi
# The live speaker count is the stable observable; bot task log strings are
# intentionally not part of the CLI output contract.
if ! grep -q "speakers=2" "$log"; then
  echo "the two bots did not become audible" >&2
  exit 1
fi

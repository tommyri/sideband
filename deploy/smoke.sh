#!/usr/bin/env bash
# Build the image, confirm /healthz, then join the `spike` subcommand with two bots.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

image="${SMOKE_IMAGE:-socialspeak:smoke}"
health_port="${SMOKE_HEALTH_PORT:-19101}"
quic_port="${SMOKE_QUIC_PORT:-4443}"
spike_port="${SMOKE_SPIKE_PORT:-4433}"

compose_health_port="${SMOKE_COMPOSE_HEALTH_PORT:-19102}"
compose_port="${SMOKE_COMPOSE_PORT:-4444}"
compose_project=sideband-smoke
log=""
tls_dir=""

docker build -f deploy/Dockerfile -t "$image" .

# Every run starts from empty volumes and leaves nothing behind: no
# containers, no volumes, no log file.
compose_down() {
  for file in docker-compose.yml docker-compose.nginx.yml; do
    docker compose -p "$compose_project" -f "deploy/$file" down -v --remove-orphans >/dev/null 2>&1 || true
  done
}
cleanup() {
  docker rm -f socialspeak-smoke socialspeak-spike >/dev/null 2>&1 || true
  docker volume rm -f socialspeak-smoke socialspeak-spike >/dev/null 2>&1 || true
  compose_down
  if [[ -n "$log" ]]; then rm -f "$log"; fi
  if [[ -n "$tls_dir" ]]; then rm -rf "$tls_dir"; fi
}
cleanup
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
# Gitleaks otherwise mistakes the later Docker --user UID:GID for curl credentials.
if curl -fsS "http://127.0.0.1:${quic_port}/api/v1/server" >/dev/null 2>&1; then # gitleaks:allow
  echo "public listener accepted plaintext HTTP" >&2
  exit 1
fi

docker run -d --name socialspeak-spike \
  --user 65532:65532 \
  -p "127.0.0.1:${spike_port}:4433/udp" \
  -v socialspeak-spike:/data \
  "$image" spike --data-dir /data --bind "0.0.0.0:4433"

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

# Both Compose files as shipped: no capabilities, read-only root, the
# healthcheck, and the non-root bind of 443 inside the container. Only the
# host ports (and, for NGINX, the certificate directory) are replaced, and
# the healthcheck interval is shortened so `--wait` returns quickly.
compose_up() {
  local file="$1" override="$2"
  SIDEBAND_IMAGE="$image" docker compose -p "$compose_project" \
    -f "deploy/$file" -f - up -d --wait --wait-timeout 120 <<<"$override" || {
    SIDEBAND_IMAGE="$image" docker compose -p "$compose_project" -f "deploy/$file" logs >&2 || true
    echo "$file did not become healthy" >&2
    exit 1
  }
}
compose_check() {
  local file="$1"
  curl -fsS --retry 5 --retry-all-errors --retry-delay 1 \
    "http://127.0.0.1:${compose_health_port}/healthz" >/dev/null
  curl -kfsS --retry 5 --retry-all-errors --retry-delay 1 \
    "https://127.0.0.1:${compose_port}/api/v1/server" >/dev/null
  # The command in deploy/grafana/README.md, inside the container.
  SIDEBAND_IMAGE="$image" docker compose -p "$compose_project" -f "deploy/$file" \
    exec -T socialspeak /ss-server conn list --data-dir /data >/dev/null
  echo "$file: healthy, HTTPS and conn list ok"
  SIDEBAND_IMAGE="$image" docker compose -p "$compose_project" -f "deploy/$file" down -v >/dev/null 2>&1
}

compose_up docker-compose.yml "services:
  socialspeak:
    healthcheck: { interval: 2s }
    ports: !override
      - \"127.0.0.1:${compose_port}:443/udp\"
      - \"127.0.0.1:${compose_port}:443/tcp\"
      - \"127.0.0.1:${compose_health_port}:9101\""
compose_check docker-compose.yml

tls_dir="$(mktemp -d)"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 \
  -subj /CN=localhost -addext subjectAltName=DNS:localhost \
  -keyout "$tls_dir/key.pem" -out "$tls_dir/cert.pem" 2>/dev/null
cat "$tls_dir/cert.pem" "$tls_dir/key.pem" >"$tls_dir/bundle.pem"
chmod 0755 "$tls_dir"
chmod 0644 "$tls_dir/bundle.pem"
compose_up docker-compose.nginx.yml "services:
  socialspeak:
    healthcheck: { interval: 2s }
    ports: !override
      - \"127.0.0.1:${compose_port}:443/udp\"
      - \"127.0.0.1:${compose_port}:443/tcp\"
      - \"127.0.0.1:${compose_health_port}:9101/tcp\"
    volumes: !override
      - socialspeak:/data
      - ${tls_dir}:/certificates:ro"
compose_check docker-compose.nginx.yml

#!/usr/bin/env bash
# T-100 measurement runner. Usage: ./run.sh <out-dir> <scenario>...
# Scenarios: see the case statement at the bottom and README.md.
# lk-* scenarios need the LiveKit container (see README.md); the runner
# restarts it with the playout-delay setting each scenario names.
set -euo pipefail

OUT=${1:?out dir}
shift
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${BIN:-$HERE/target/release/livekit-spike}
IMAGE=livekit/livekit-server:v1.13.11
SCREEN=${SCREEN:-screen:1}
SECS=${SECS:-60}
mkdir -p "$OUT"
BG="" SAMPLERS=""
trap stop_all EXIT

lk_up() { # $1 = default | lowlat
  docker rm -f t100-livekit >/dev/null 2>&1 || true
  local body=""
  if [ "$1" = lowlat ]; then
    body=$(printf 'room:\n  playout_delay:\n    enabled: true\n    min: 0\n    max: 0\n')
  elif [ "$1" = lowlat10 ]; then
    body=$(printf 'room:\n  playout_delay:\n    enabled: true\n    min: 0\n    max: 10\n')
  fi
  docker run -d --name t100-livekit \
    -p 127.0.0.1:7880:7880 -p 127.0.0.1:7881:7881 -p 127.0.0.1:7882:7882/udp \
    "$IMAGE" --dev --bind 0.0.0.0 --node-ip 127.0.0.1 ${body:+--config-body "$body"} >/dev/null
  for _ in $(seq 1 40); do "$BIN" lk-ping >/dev/null 2>&1 && return 0; sleep 0.25; done
  echo "LiveKit did not start" >&2
  return 1
}

samplers() { # $1 = dir; GPU always, the LiveKit container when present
  ( while true; do
      u=$(ioreg -r -d 1 -c IOAccelerator | grep -o '"Device Utilization %"=[0-9]*' | head -1 | cut -d= -f2)
      r=$(ioreg -r -d 1 -c IOAccelerator | grep -o '"Renderer Utilization %"=[0-9]*' | head -1 | cut -d= -f2)
      echo "{\"t_ms\":$(($(date +%s) * 1000)),\"role\":\"gpu\",\"device_pct\":${u:-null},\"renderer_pct\":${r:-null}}"
      sleep 1
    done ) >"$1/gpu.log" 2>/dev/null &
  SAMPLERS=$!
  if docker ps --format '{{.Names}}' | grep -q '^t100-livekit$'; then
    ( while true; do
        docker stats --no-stream --format '{"t_ms":'"$(($(date +%s) * 1000))"',"role":"livekit-server","cpu":"{{.CPUPerc}}","mem":"{{.MemUsage}}","net":"{{.NetIO}}"}' t100-livekit
        sleep 1
      done ) >"$1/livekit-server.log" 2>/dev/null &
    SAMPLERS="$SAMPLERS $!"
  fi
}

stop_all() {
  # shellcheck disable=SC2086
  kill ${SAMPLERS:-} ${BG:-} 2>/dev/null || true
  wait 2>/dev/null || true
  SAMPLERS="" BG=""
}

baseline() { # $1 = name, $2 = content, $3 = viewer2 mode (direct|impair), $4 = seconds, $5.. = viewer1 args
  local d="$OUT/$1" content=$2 mode=$3 secs=$4
  shift 4
  rm -rf "$d"; mkdir -p "$d"
  samplers "$d"
  "$BIN" baseline-server --run-dir "$d" >"$d/server.log" 2>"$d/server.err" & BG="$BG $!"
  for _ in $(seq 1 100); do [ -s "$d/baseline.json" ] && break; sleep 0.1; done
  local port
  port=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["port"])' "$d/baseline.json")
  local via=""
  if [ "$mode" = impair ]; then
    "$BIN" impair --listen 127.0.0.1:47000 --upstream "127.0.0.1:$port" \
      --schedule "20:clean,30:${SHAPE:-600kbps/2%/40ms},$((secs - 48)):clean" >"$d/impair.log" 2>&1 & BG="$BG $!"
    via="--via 47000"
  fi
  "$BIN" baseline-pub --run-dir "$d" --content "$content" --seconds $((secs + 6)) >"$d/publisher.log" 2>"$d/publisher.err" & local pub=$!
  sleep 1
  "$BIN" baseline-view --run-dir "$d" --name viewer1 --seconds "$secs" "$@" >"$d/viewer1.log" 2>"$d/viewer1.err" & local v1=$!
  # shellcheck disable=SC2086
  "$BIN" baseline-view --run-dir "$d" --name viewer2 --seconds "$secs" $via >"$d/viewer2.log" 2>"$d/viewer2.err" & local v2=$!
  wait $v1 $v2 $pub || true
  stop_all
}

livekit() { # $1 = name, $2 = content, $3 = viewer2 mode, $4 = seconds, $5 = server config, $6.. = viewer1 args
  local d="$OUT/$1" content=$2 mode=$3 secs=$4 config=$5
  shift 5
  rm -rf "$d"; mkdir -p "$d"
  lk_up "$config"
  docker inspect t100-livekit --format '{{.Config.Image}} {{.Image}}' >"$d/livekit-image.txt"
  samplers "$d"
  local turn=""
  if [ "$mode" = impair ]; then
    "$BIN" turn --listen 127.0.0.1:47001 >"$d/turn.log" 2>&1 & BG="$BG $!"
    "$BIN" impair --listen 127.0.0.1:47000 --upstream 127.0.0.1:47001 \
      --schedule "20:clean,30:${SHAPE:-600kbps/2%/40ms},$((secs - 48)):clean" >"$d/impair.log" 2>&1 & BG="$BG $!"
    turn="--turn 47000"
  fi
  "$BIN" lk-pub --content "$content" --seconds $((secs + 6)) ${LKPUB_ARGS:-} >"$d/publisher.log" 2>"$d/publisher.err" & local pub=$!
  sleep 1
  "$BIN" lk-view --name viewer1 --seconds "$secs" "$@" >"$d/viewer1.log" 2>"$d/viewer1.err" & local v1=$!
  # shellcheck disable=SC2086
  "$BIN" lk-view --name viewer2 --seconds "$secs" $turn ${LKVIEW2_ARGS:-} >"$d/viewer2.log" 2>"$d/viewer2.err" & local v2=$!
  wait $v1 $v2 $pub || true
  stop_all
  docker logs t100-livekit >"$d/livekit-container.log" 2>&1 || true
}

for scenario in "$@"; do
  echo "== $scenario"
  case $scenario in
    base-pattern) baseline base-pattern pattern direct "$SECS" ;;
    base-hard) baseline base-hard pattern-hard direct "$SECS" ;;
    base-screen) baseline base-screen "$SCREEN" direct "$SECS" ;;
    lk-pattern) livekit lk-pattern pattern direct "$SECS" default ;;
    lk-pattern-lowlat) livekit lk-pattern-lowlat pattern direct "$SECS" lowlat ;;
    lk-pattern-lowlat10) livekit lk-pattern-lowlat10 pattern direct "$SECS" lowlat10 ;;
    lk-hard) livekit lk-hard pattern-hard direct "$SECS" default ;;
    lk-hard-single) LKPUB_ARGS=--no-simulcast livekit lk-hard-single pattern-hard direct "$SECS" default ;;
    lk-screen) livekit lk-screen "$SCREEN" direct "$SECS" default ;;
    lk-pattern-single) LKPUB_ARGS=--no-simulcast livekit lk-pattern-single pattern direct "$SECS" default ;;
    base-impair) baseline base-impair pattern impair 100 ;;
    lk-impair) livekit lk-impair pattern impair 100 default ;;
    lk-impair-lowlat) livekit lk-impair-lowlat pattern impair 100 lowlat ;;
    lk-impair-single) LKPUB_ARGS=--no-simulcast livekit lk-impair-single pattern impair 100 default ;;
    base-impair-hard) SHAPE=1500kbps/2%/40ms baseline base-impair-hard pattern-hard impair 100 ;;
    lk-impair-hard) SHAPE=1500kbps/2%/40ms livekit lk-impair-hard pattern-hard impair 100 default ;;
    base-life) baseline base-life pattern direct 60 --leave-at 15 --rejoin-at 25 --reconnect-at 40 ;;
    lk-life) livekit lk-life pattern direct 80 default \
      --signal-reconnect-at 10 --full-reconnect-at 20 --revoke-at 30 --restore-at 38 \
      --move-at 46 --move-back-at 54 --kick-at 64 ;;
    lk-ttl) LKVIEW2_ARGS="--ttl 15 --full-reconnect-at 30" livekit lk-ttl pattern direct 45 default ;;
    *) echo "unknown scenario $scenario" >&2; exit 2 ;;
  esac
done

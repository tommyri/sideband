#!/usr/bin/env bash
# One load scenario: scrape the server while `ss-cli load` runs, then graph it.
#
#   scripts/load-run.sh OUT_DIR NAME -- ss-cli load HOST:PORT --bots 25 ...
#
# Environment:
#   SS_PID          server process id (default: pgrep -n ss-server)
#   SS_METRICS_URL  default http://127.0.0.1:9101/metrics
#   SS_SCRAPE_EVERY seconds between samples (default 5)
#
# Writes NAME-server.csv, NAME-bots.csv, NAME.svg, NAME-summary.txt, NAME-bots.log.
# The scrape must run on the server host; ss-cli may run elsewhere when its
# address is the server's public QUIC port.
set -euo pipefail

if [[ $# -lt 4 || "$3" != "--" ]]; then
  sed -n '2,13p' "$0" >&2
  exit 2
fi
out="$1"
name="$2"
shift 3
here="$(cd "$(dirname "$0")" && pwd)"
pid="${SS_PID:-$(pgrep -n ss-server)}"
url="${SS_METRICS_URL:-http://127.0.0.1:9101/metrics}"
mkdir -p "$out"

python3 "$here/load-metrics.py" scrape --pid "$pid" --url "$url" \
  --interval "${SS_SCRAPE_EVERY:-5}" --out "$out/$name-server.csv" &
scraper=$!
trap 'kill "$scraper" 2>/dev/null || true' EXIT

status=0
"$@" --csv "$out/$name-bots.csv" 2>&1 | tee "$out/$name-bots.log" || status=$?
# One more sample after the bots leave shows what the server keeps.
sleep "${SS_SCRAPE_EVERY:-5}"
kill -TERM "$scraper" 2>/dev/null || true
wait "$scraper" 2>/dev/null || true

python3 "$here/load-metrics.py" plot "$out/$name-server.csv" \
  --out "$out/$name.svg" --title "$name"
python3 "$here/load-metrics.py" summary "$out/$name-server.csv" | tee "$out/$name-summary.txt"
exit "$status"

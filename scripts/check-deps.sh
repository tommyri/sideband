#!/usr/bin/env bash
# Fail if the workspace dependency graph grows an edge that the architecture
# forbids. See docs/plan/01-architecture.md.
set -euo pipefail

cd "$(dirname "$0")/.."

deps_of() {
  cargo tree -e no-dev --prefix none -p "$1" | awk '{ print $1 }'
}

forbid() {
  local crate="$1"
  local bad="$2"
  if deps_of "$crate" | grep -qx "$bad"; then
    echo "forbidden dependency: $crate -> $bad" >&2
    exit 1
  fi
}

for bad in ss-proto ss-net ss-audio ss-input ss-video ss-client-core ss-cli ss-server; do
  forbid ss-core "$bad"
done

for bad in ss-proto ss-net ss-input ss-video ss-client-core ss-cli ss-server; do
  forbid ss-audio "$bad"
done

for bad in ss-proto ss-net ss-audio ss-video ss-client-core ss-cli ss-server; do
  forbid ss-input "$bad"
done

for bad in ss-net ss-audio ss-input ss-video ss-client-core ss-cli ss-server; do
  forbid ss-proto "$bad"
done

for bad in ss-audio ss-input ss-video ss-client-core ss-cli ss-server; do
  forbid ss-net "$bad"
done

for bad in ss-core ss-proto ss-net ss-audio ss-input ss-client-core ss-cli ss-server; do
  forbid ss-video "$bad"
done

for bad in ss-cli ss-server; do
  forbid ss-client-core "$bad"
done

for bad in ss-audio ss-input ss-video ss-client-core ss-cli; do
  forbid ss-server "$bad"
done

echo "dependency graph ok"

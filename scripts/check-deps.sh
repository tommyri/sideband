#!/usr/bin/env bash
# Fail if the workspace dependency graph grows an edge that the architecture
# forbids. See docs/plan/01-architecture.md.
#
# A failing `cargo tree` (unknown crate, broken manifest, missing lockfile
# entry) fails the check: the graph is captured first so `set -e` applies,
# then searched. scripts/check-deps.test.sh covers both failure paths.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

deps_of() {
  local tree
  if ! tree=$(cargo tree --locked -e no-dev --prefix none -p "$1"); then
    echo "cargo tree failed for $1" >&2
    exit 1
  fi
  awk '{ print $1 }' <<<"$tree"
}

forbid() {
  local crate="$1"
  local bad="$2"
  local deps
  deps=$(deps_of "$crate") || exit 1
  if grep -qx "$bad" <<<"$deps"; then
    echo "forbidden dependency: $crate -> $bad" >&2
    exit 1
  fi
}

main() {
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
}

# Sourcing (as the test does) defines the functions without running the check.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi

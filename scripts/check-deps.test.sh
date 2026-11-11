#!/usr/bin/env bash
# Tests for check-deps.sh: a failing `cargo tree` must fail the check, and a
# forbidden edge must be reported. CI runs this before check-deps.sh.
set -uo pipefail

cd "$(dirname "$0")/.."

failures=0

expect_failure() {
  local name="$1"
  shift
  local output
  if output=$("$@" 2>&1); then
    echo "FAIL: $name: exited 0" >&2
    failures=$((failures + 1))
  elif grep -q "dependency graph ok" <<<"$output"; then
    echo "FAIL: $name: printed success" >&2
    failures=$((failures + 1))
  else
    echo "ok: $name"
  fi
}

expect_success() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    echo "ok: $name"
  else
    echo "FAIL: $name: exited non-zero" >&2
    failures=$((failures + 1))
  fi
}

# Each case runs in a subshell so `exit` inside forbid ends only that case.
expect_failure "missing crate fails" bash -c 'source scripts/check-deps.sh; forbid ss-no-such-crate ss-core'
expect_failure "forbidden edge fails" bash -c 'source scripts/check-deps.sh; forbid ss-server ss-core'
expect_success "allowed edge passes" bash -c 'source scripts/check-deps.sh; forbid ss-core ss-server'

if [[ "$failures" -ne 0 ]]; then
  echo "$failures check-deps test(s) failed" >&2
  exit 1
fi

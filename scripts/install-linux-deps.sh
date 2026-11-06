#!/usr/bin/env bash
# Hosted Ubuntu runners only. Bound network stalls and avoid recommended extras.
set -euo pipefail
case "${1:-}" in
  desktop) packages=(libasound2-dev libpipewire-0.3-dev libclang-dev pkg-config cmake libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf) ;;
  audio) packages=(libasound2-dev libpipewire-0.3-dev libclang-dev pkg-config cmake) ;;
  input) packages=(dbus xvfb xdotool) ;;
  musl) packages=(musl-tools) ;;
  *) echo 'Usage: install-linux-deps.sh desktop|audio|input|musl' >&2; exit 2 ;;
esac
options=(-o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=60)
timeout --signal=TERM --kill-after=15s 3m sudo env DEBIAN_FRONTEND=noninteractive apt-get "${options[@]}" update
timeout --signal=TERM --kill-after=15s 6m sudo env DEBIAN_FRONTEND=noninteractive apt-get "${options[@]}" install -y --no-install-recommends "${packages[@]}"

#!/usr/bin/env bash
# Hosted Ubuntu runners only. Bound network stalls and avoid recommended extras.
set -euo pipefail
# libdbus-1-dev: ss-audio asks rtkit over D-Bus for real-time voice threads (T-117).
case "${1:-}" in
  desktop) packages=(libasound2-dev libpipewire-0.3-dev libdbus-1-dev libclang-dev pkg-config cmake libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf) ;;
  audio) packages=(libasound2-dev libpipewire-0.3-dev libdbus-1-dev libclang-dev pkg-config cmake) ;;
  input) packages=(dbus xvfb xdotool) ;;
  musl) packages=(musl-tools) ;;
  *) echo 'Usage: install-linux-deps.sh desktop|audio|input|musl' >&2; exit 2 ;;
esac
options=(-o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=60)
run_apt() { timeout --signal=TERM --kill-after=15s "$1" sudo env DEBIAN_FRONTEND=noninteractive apt-get "${options[@]}" "${@:2}"; }
fetch() { run_apt 75s update && run_apt "$1" install -y --no-install-recommends "${packages[@]}"; }
if ! fetch 2m; then
  # Hosted runners list a regional mirror first (priority:1), and it can stall.
  # Drop it so apt falls back to archive.ubuntu.com. A healthy install takes under 40 s.
  echo '::warning::apt stalled on the primary mirror; retrying on archive.ubuntu.com'
  if [ -f /etc/apt/apt-mirrors.txt ]; then sudo sed -i '/priority:1$/d' /etc/apt/apt-mirrors.txt; fi
  sudo dpkg --configure -a
  fetch 4m
fi

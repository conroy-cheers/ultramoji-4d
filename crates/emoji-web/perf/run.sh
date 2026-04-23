#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v npm >/dev/null 2>&1; then
  echo "npm is required" >&2
  exit 1
fi

if [ ! -d node_modules/playwright ]; then
  npm install
  npx playwright install chromium
fi

export CHROMIUM_PATH
if [ -z "${CHROMIUM_PATH:-}" ]; then
  CHROMIUM_PATH="$(nix shell nixpkgs#chromium -c bash -lc 'command -v chromium')"
fi

nix shell nixpkgs#nodejs nixpkgs#chromium nixpkgs#xvfb -c bash -lc '
  Xvfb :99 -screen 0 1280x960x24 >/tmp/emoji-web-xvfb.log 2>&1 &
  xvfb_pid=$!
  trap "kill $xvfb_pid" EXIT
  export DISPLAY=:99
  export PLAYWRIGHT_HEADLESS=0
  export CHROMIUM_PATH="'"$CHROMIUM_PATH"'"
  node run.mjs "$@"
' bash "$@"

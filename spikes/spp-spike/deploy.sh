#!/usr/bin/env bash
# Cross-build spp-spike and copy it to the dev Pi from scripts/pi-deploy.local.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
PI_HOST=""; PI_USER=""; PI_TARGET="aarch64-unknown-linux-gnu"; PI_DIR="ktb-dev"
# shellcheck source=/dev/null
[[ -f "$repo/scripts/pi-deploy.local" ]] && source "$repo/scripts/pi-deploy.local"
[[ -n "$PI_HOST" ]] || { echo "PI_HOST isn't set in scripts/pi-deploy.local" >&2; exit 1; }
remote="${PI_USER:+$PI_USER@}$PI_HOST"
profile="debug"; cargo_profile=()
[[ "${1:-}" == "--release" ]] && { profile="release"; cargo_profile=(--release); }
(cd "$here" && cross build --target "$PI_TARGET" ${cargo_profile[@]+"${cargo_profile[@]}"})
ssh "$remote" "mkdir -p '$PI_DIR/spike'"
rsync -az "$here/target/$PI_TARGET/$profile/spp-spike" "$here/../usb-inspect.sh" "$here/../unblock-bt.sh" "$here/../coexist.conf" "$remote:$PI_DIR/spike/"
echo "deployed to $remote:$PI_DIR/spike/"

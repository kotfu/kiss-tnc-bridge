#!/usr/bin/env bash
# Cross-build ble-update and copy it to the dev Pi from scripts/pi-deploy.local.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
PI_HOST=""; PI_USER=""; PI_TARGET="aarch64-unknown-linux-gnu"; PI_DIR="ktb-dev"; PI_DOCKER_CONTEXT=""
# shellcheck source=/dev/null
[[ -f "$repo/scripts/pi-deploy.local" ]] && source "$repo/scripts/pi-deploy.local"
[[ -n "$PI_HOST" ]] || { echo "PI_HOST isn't set in scripts/pi-deploy.local" >&2; exit 1; }
remote="${PI_USER:+$PI_USER@}$PI_HOST"
profile="debug"; cargo_profile=()
[[ "${1:-}" == "--release" ]] && { profile="release"; cargo_profile=(--release); }
if [[ -n "$PI_DOCKER_CONTEXT" ]]; then
    # Build on another machine's Docker engine; cross leaves the binary in
    # its build volume, so it's fetched from there.
    export DOCKER_CONTEXT="$PI_DOCKER_CONTEXT" CROSS_REMOTE=1
fi
(cd "$here" && cross build --target "$PI_TARGET" ${cargo_profile[@]+"${cargo_profile[@]}"})
ssh "$remote" "mkdir -p '$PI_DIR/spike'"
rsync -az "$here/target/$PI_TARGET/$profile/ble-update" "$remote:$PI_DIR/spike/"
echo "deployed to $remote:$PI_DIR/spike/"

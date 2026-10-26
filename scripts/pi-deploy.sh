#!/usr/bin/env bash
#
# Build kiss-tnc-bridge for a development Raspberry Pi, copy it over, and run
# it in the foreground there with its log streaming back to this terminal.
#
# While the dev build runs, the Pi's installed kiss-tnc-bridge service is
# stopped (only one instance can hold the Bluetooth adapter and the instance
# lock). It's started again when the dev build exits, including on Ctrl-C.
#
# Settings come from scripts/pi-deploy.local (not tracked by git), a bash
# file of variable assignments:
#
#   PI_HOST=raspberrypi.local     # required
#   PI_USER=pi                    # default: your ssh config / local user
#   PI_TARGET=aarch64-unknown-linux-gnu   # or arm-unknown-linux-gnueabihf
#   PI_DIR=ktb-dev                # remote directory, relative to home
#   PI_SERVICE=kiss-tnc-bridge    # systemd service to stop while testing
#   PI_CONFIG=path/to/test.conf   # optional local config to copy and use
#
# Usage:
#   scripts/pi-deploy.sh [--release] [--no-build] [--copy-only] [-- ARGS...]
#
#   --release    build with the release profile (default: debug, faster to build)
#   --no-build   skip the build and copy the last one
#   --copy-only  build and copy, but don't run anything on the Pi
#   ARGS         passed to kiss-tnc-bridge on the Pi (default: -d)
#
# Running on the Pi needs root, so sudo on the Pi may ask for your password
# (again when restarting the service, if a long run outlasts sudo's
# timeout). Run this script from a terminal so that prompt can reach you.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
local_settings="${PI_DEPLOY_SETTINGS:-$repo_root/scripts/pi-deploy.local}"

PI_HOST=""
PI_USER=""
PI_TARGET="aarch64-unknown-linux-gnu"
PI_DIR="ktb-dev"
PI_SERVICE="kiss-tnc-bridge"
PI_CONFIG=""

if [[ -f "$local_settings" ]]; then
    # shellcheck source=/dev/null
    source "$local_settings"
fi

die() {
    echo "pi-deploy: $*" >&2
    exit 1
}

profile="debug"
build=true
run=true
bin_args=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) profile="release" ;;
        --no-build) build=false ;;
        --copy-only) run=false ;;
        -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --) shift; bin_args=("$@"); break ;;
        *) die "unknown option '$1' (try --help)" ;;
    esac
    shift
done
[[ ${#bin_args[@]} -eq 0 ]] && bin_args=(-d)

[[ -n "$PI_HOST" ]] || die "PI_HOST isn't set; put it in $local_settings"
command -v rsync >/dev/null || die "rsync not found"
remote="${PI_USER:+$PI_USER@}$PI_HOST"

# ── Build ────────────────────────────────────────────────────────────────

binary="$repo_root/target/$PI_TARGET/$profile/kiss-tnc-bridge"

if $build; then
    command -v cross >/dev/null || die "cross not found; install it with: cargo install cross"
    docker info >/dev/null 2>&1 || die "the Docker engine isn't running (cross builds in a container)"
    cargo_profile=()
    [[ "$profile" == "release" ]] && cargo_profile=(--release)
    echo "pi-deploy: building $PI_TARGET ($profile)"
    (cd "$repo_root" && cross build --target "$PI_TARGET" ${cargo_profile[@]+"${cargo_profile[@]}"})
fi
[[ -x "$binary" ]] || die "no build at $binary; run without --no-build"

# ── Copy ─────────────────────────────────────────────────────────────────

staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
cp "$binary" "$staging/kiss-tnc-bridge"
ktbctl="$(dirname "$binary")/ktbctl"
[[ -x "$ktbctl" ]] && cp "$ktbctl" "$staging/ktbctl"

if [[ -n "$PI_CONFIG" ]]; then
    [[ -f "$PI_CONFIG" ]] || die "PI_CONFIG file not found: $PI_CONFIG"
    cp "$PI_CONFIG" "$staging/$(basename "$PI_CONFIG")"
    bin_args=(-c "./$(basename "$PI_CONFIG")" "${bin_args[@]}")
fi

# The runner executes on the Pi: stop the service, run the dev build in the
# foreground, and restart the service however the run ends.
cat > "$staging/run.sh" <<'RUNNER'
#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "$0")"
service="$1"
shift

sudo -v || exit 1

was_active=false
if systemctl is-active --quiet "$service"; then
    was_active=true
    echo "pi-deploy: stopping $service"
    sudo systemctl stop "$service"
fi

restore() {
    if $was_active; then
        echo "pi-deploy: starting $service again"
        sudo systemctl start "$service"
    fi
}
trap restore EXIT

echo "pi-deploy: running $(./kiss-tnc-bridge --version 2>/dev/null) $*  (Ctrl-C to stop)"
sudo ./kiss-tnc-bridge "$@"
RUNNER
chmod +x "$staging/run.sh"

echo "pi-deploy: copying to $remote:$PI_DIR/"
ssh "$remote" "mkdir -p '$PI_DIR'"
rsync -az "$staging/" "$remote:$PI_DIR/"

$run || { echo "pi-deploy: copied; not running (--copy-only)"; exit 0; }

# ── Run ──────────────────────────────────────────────────────────────────

[[ -t 0 ]] || die "run this from a terminal: sudo on the Pi needs to ask for your password"

remote_cmd="$PI_DIR/run.sh $(printf '%q ' "$PI_SERVICE" "${bin_args[@]}")"
ssh -t "$remote" "$remote_cmd"

#!/usr/bin/env bash
#
# Run clippy on the Linux build, which covers the Linux-only code (BlueZ,
# BLE, SPP) that clippy on macOS never sees. Warnings are errors, as in CI.
#
# It builds with cross, like scripts/pi-deploy.sh, and reads the same
# settings file (scripts/pi-deploy.local, or the one PI_DEPLOY_SETTINGS
# names) for PI_TARGET and PI_DOCKER_CONTEXT. Without PI_DOCKER_CONTEXT it
# runs plain `cross clippy` on the local Docker engine.
#
# With PI_DOCKER_CONTEXT (a remote Docker engine), `cross clippy` doesn't
# work: in remote mode cross copies this machine's ~/.cargo into the
# container, and cargo finds this machine's cargo-clippy in its bin
# directory before the Linux one, and can't run it. So this runs
# `cross check` with clippy-driver as the rustc wrapper instead, which is
# what cargo-clippy does, set only inside the container (cross also runs
# rustc on this machine, where the Linux clippy-driver can't run).
#
# Usage:
#   scripts/clippy-linux.sh [--target TRIPLE] [-- CARGO_ARGS...]
#
#   --target     the Linux target [default: PI_TARGET, else aarch64-unknown-linux-gnu]
#   CARGO_ARGS   passed to cargo [default: --all-targets]

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
local_settings="${PI_DEPLOY_SETTINGS:-$repo_root/scripts/pi-deploy.local}"

PI_TARGET="aarch64-unknown-linux-gnu"
PI_DOCKER_CONTEXT=""

if [[ -f "$local_settings" ]]; then
    # shellcheck source=/dev/null
    source "$local_settings"
fi

die() {
    echo "clippy-linux: $*" >&2
    exit 1
}

target="$PI_TARGET"
cargo_args=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) shift; target="${1:-}"; [[ -n "$target" ]] || die "--target needs a value" ;;
        -h|--help) sed -n '2,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --) shift; cargo_args=("$@"); break ;;
        *) die "unknown option '$1' (try --help)" ;;
    esac
    shift
done
[[ ${#cargo_args[@]} -eq 0 ]] && cargo_args=(--all-targets)

command -v cross >/dev/null || die "cross not found; install it with: cargo install cross"
cd "$repo_root"

if [[ -z "$PI_DOCKER_CONTEXT" ]]; then
    docker info >/dev/null 2>&1 || die "the Docker engine isn't running (cross builds in a container)"
    echo "clippy-linux: $target on the local Docker engine"
    exec cross clippy --target "$target" "${cargo_args[@]}" -- -D warnings
fi

export DOCKER_CONTEXT="$PI_DOCKER_CONTEXT" CROSS_REMOTE=1 CROSS_REMOTE_SKIP_BUILD_ARTIFACTS=1
docker info >/dev/null 2>&1 \
    || die "can't reach the Docker engine for context '$PI_DOCKER_CONTEXT'; is Docker running there?"

# The toolchain cross runs in the container: this one, built for the
# engine's architecture. Inside the container it's at the same path as here.
case "$(docker info --format '{{.Architecture}}')" in
    x86_64) host_triple="x86_64-unknown-linux-gnu" ;;
    aarch64) host_triple="aarch64-unknown-linux-gnu" ;;
    *) die "don't know which toolchain cross uses on a $(docker info --format '{{.Architecture}}') engine" ;;
esac
toolchain="$(rustup show active-toolchain | cut -d' ' -f1)"
toolchain="${toolchain%-*-*-*}-$host_triple"
driver="$(rustup show home)/toolchains/$toolchain/bin/clippy-driver"
[[ -x "$driver" ]] \
    || die "no clippy in the $toolchain toolchain; add it with: rustup component add clippy --toolchain $toolchain"

echo "clippy-linux: $target on '$PI_DOCKER_CONTEXT'"
export CROSS_CONTAINER_OPTS="-e CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=$driver -e CLIPPY_ARGS=-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__"
exec cross check --target "$target" "${cargo_args[@]}"

#!/bin/sh
set -e

# Pick up the ModemManager rule for radios without a reboot.
if command -v udevadm >/dev/null; then
    udevadm control --reload-rules || true
fi

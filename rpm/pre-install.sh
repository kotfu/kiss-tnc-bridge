#!/bin/sh
set -e

# The control socket group: members can use ktbctl.
if ! getent group kiss-tnc-bridge >/dev/null; then
    groupadd --system kiss-tnc-bridge
fi

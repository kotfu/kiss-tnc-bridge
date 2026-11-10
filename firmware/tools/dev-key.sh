#!/bin/sh
# Makes the development signing key (dev_signing_key.pem) once. Release
# images are signed with a different key that never leaves its owner.
set -e
cd "$(dirname "$0")/.."
if [ -e dev_signing_key.pem ]; then
    echo "dev_signing_key.pem already exists"
    exit 0
fi
espsecure generate-signing-key --version 1 --scheme ecdsa256 dev_signing_key.pem

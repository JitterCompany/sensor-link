#!/usr/bin/env bash
# Create the keys a development build needs, in examples/demo-firmware/keys/
# (gitignored):
#
#   app-signing-key.pem   P-256 private key that signs firmware images
#   app-signing-pub.pem   its public key, built into the bootloader
#   mqtt-ca.pem           the CA the firmware trusts for the broker
#
# Usage: scripts/gen-dev-keys.sh [path-to-broker-ca.pem]
#
# Without an argument the CA is a throwaway one: the firmware builds, but it
# only connects to a broker whose certificate that CA signed. Pass the CA of
# your (development) broker instead. Existing files are kept.

set -euo pipefail

KEYS="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/keys"
mkdir -p "$KEYS"

if [ ! -f "$KEYS/app-signing-key.pem" ]; then
    openssl ecparam -name prime256v1 -genkey -noout -out "$KEYS/app-signing-key.pem"
    echo "created $KEYS/app-signing-key.pem"
fi
openssl ec -in "$KEYS/app-signing-key.pem" -pubout -out "$KEYS/app-signing-pub.pem" 2>/dev/null

if [ $# -ge 1 ]; then
    cp "$1" "$KEYS/mqtt-ca.pem"
    echo "using $1 as the broker CA"
elif [ ! -f "$KEYS/mqtt-ca.pem" ]; then
    openssl ecparam -name prime256v1 -genkey -noout -out "$KEYS/mqtt-ca.key"
    openssl req -x509 -new -key "$KEYS/mqtt-ca.key" -sha256 -days 3650 \
        -subj "/O=Demo/CN=demo throwaway CA" -out "$KEYS/mqtt-ca.pem"
    echo "created a throwaway broker CA in $KEYS/mqtt-ca.pem"
fi

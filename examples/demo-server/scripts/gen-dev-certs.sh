#!/usr/bin/env bash
# Generate a throwaway CA plus broker, server, admin and device certificates
# for trying the mTLS setup. Not for production: the CA key sits next to the
# certs, unprotected.
#
# Usage: scripts/gen-dev-certs.sh [device-id ...]   (default: DEMO-0001)
# Output: certs/ (git-ignored), one <name>.cert + <name>.key per identity.
# The certificate CN is the MQTT identity, see deploy/mosquitto/demo.acl.
set -euo pipefail

OUT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/certs"
DAYS=825
BROKER_HOSTS="${BROKER_HOSTS:-localhost,mosquitto,mqtt.demo.example}"

mkdir -p "$OUT"
cd "$OUT"

new_key() {
    openssl ecparam -name prime256v1 -genkey -noout -out "$1.key"
    chmod 600 "$1.key"
}

# $1 = file name, $2 = CN, $3 = extensions file
sign() {
    new_key "$1"
    openssl req -new -key "$1.key" -subj "/CN=$2" -out "$1.csr"
    openssl x509 -req -in "$1.csr" -CA ca.pem -CAkey ca.key -CAcreateserial \
        -days "$DAYS" -sha256 -extfile "$3" -out "$1.cert"
    rm "$1.csr"
}

if [ ! -f ca.pem ]; then
    new_key ca
    openssl req -x509 -new -key ca.key -sha256 -days "$DAYS" \
        -subj "/CN=Demo throwaway CA" -out ca.pem
fi

san=$(echo "$BROKER_HOSTS" | sed 's/[^,]*/DNS:&/g')
printf 'subjectAltName=%s\nextendedKeyUsage=serverAuth\n' "$san" > server.ext
printf 'extendedKeyUsage=clientAuth\n' > client.ext

sign broker localhost server.ext
sign demo-server demo-server client.ext
sign admin admin client.ext
for device in "${@:-DEMO-0001}"; do
    sign "$device" "$device" client.ext
done
rm server.ext client.ext

echo "Certificates in $OUT"

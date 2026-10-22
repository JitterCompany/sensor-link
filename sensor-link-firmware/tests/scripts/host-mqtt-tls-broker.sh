#!/usr/bin/env bash
# mTLS mosquitto for the quectel-ppp TLS host tests, mirroring the production
# broker setup: x509-only auth, identity from the client-cert CN.
#
# The throwaway PKI mirrors what production must become for the device's
# rustpki verifier (P-256 + SHA-256 only): CA self-signature and broker cert
# signed SHA-256. mosquitto/OpenSSL sends the CA in the TLS chain, and rustpki
# verifies every chain entry, so the CA self-signature digest matters too.
# Client certs stay SHA-384 (production parity; only the broker verifies them).
# Production migration: re-sign the CA cert with the SAME key using -sha256
# (issued certs stay valid) and re-issue the broker cert with -sha256.
#
#   host-mqtt-tls-broker.sh start   # broker on localhost:18885, certs in target/host-tls-test/
#   host-mqtt-tls-broker.sh stop
set -euo pipefail

NAME=sensor-link-host-mqtt-tls
PORT=18885
DIR="$(cd "$(dirname "$0")/../.." && pwd)/target/host-tls-test"

case "${1:-}" in
start)
    rm -rf "$DIR" && mkdir -p "$DIR"
    cd "$DIR"

    openssl ecparam -name prime256v1 -genkey -noout -out ca.key
    openssl req -new -x509 -key ca.key -subj "/CN=Test MQTT ECDSA CA" \
        -days 30 -sha256 -out ca.pem

    openssl ecparam -name prime256v1 -genkey -noout -out server.key
    openssl req -new -key server.key -subj "/CN=localhost" -out server.csr
    openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
        -days 30 -sha256 \
        -extfile <(printf "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n") \
        -out server.crt

    openssl ecparam -name prime256v1 -genkey -noout -out client.key
    openssl req -new -key client.key -subj "/CN=host-tls-client" -out client.csr
    openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
        -days 30 -sha384 \
        -extfile <(printf "extendedKeyUsage=clientAuth\n") \
        -out client.crt

    cat > mosquitto.conf <<'EOF'
listener 8883
tls_version tlsv1.2
cafile /certs/ca.pem
certfile /certs/server.crt
keyfile /certs/server.key
require_certificate true
use_identity_as_username true
use_username_as_clientid true
allow_anonymous false
log_dest stdout
EOF
    # mosquitto in the container runs as UID 1883 and must read the key.
    chmod 644 server.key
    docker run -d --rm --name "$NAME" -p "$PORT:8883" \
        -v "$DIR:/certs:ro" \
        -v "$DIR/mosquitto.conf:/mosquitto/config/mosquitto.conf:ro" \
        eclipse-mosquitto >/dev/null
    for _ in $(seq 1 50); do
        if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
            exec 3>&- || true
            echo "TLS broker ready on :$PORT, certs in $DIR"
            exit 0
        fi
        sleep 0.1
    done
    echo "broker did not come up" >&2
    docker logs "$NAME" >&2 || true
    exit 1
    ;;
stop)
    docker stop "$NAME" >/dev/null 2>&1 || true
    ;;
*)
    echo "usage: $0 start|stop" >&2
    exit 2
    ;;
esac

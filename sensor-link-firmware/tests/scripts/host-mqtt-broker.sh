#!/usr/bin/env bash
# Plain-TCP mosquitto for the quectel-ppp MQTT core host tests.
#
#   host-mqtt-broker.sh start   # broker on localhost:18884, anonymous
#   host-mqtt-broker.sh stop
#
# Requires docker. TLS variants are handled separately (WP2).
set -euo pipefail

NAME=sensor-link-host-mqtt
PORT=18884

case "${1:-}" in
start)
    dir=$(mktemp -d)
    cat > "$dir/mosquitto.conf" <<'EOF'
listener 1883
allow_anonymous true
log_dest stdout
EOF
    docker run -d --rm --name "$NAME" -p "$PORT:1883" \
        -v "$dir/mosquitto.conf:/mosquitto/config/mosquitto.conf:ro" \
        eclipse-mosquitto >/dev/null
    # Wait for the listener to come up.
    for _ in $(seq 1 50); do
        if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
            exec 3>&- || true
            echo "broker ready on :$PORT"
            exit 0
        fi
        sleep 0.1
    done
    echo "broker did not come up" >&2
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

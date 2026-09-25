# sensor-link-console

A command line console to a sensor-link device over MQTT. It takes the server's
side of the protocol: it publishes commands to the device, read line by line
from stdin, and prints what the device publishes. It is not a server: it only
sends what you type and covers a single device.

## Running

```sh
cargo run -p sensor-link-console -- --device-id mock_0000
```

It connects to `localhost:1883` by default. Point it elsewhere with
`--broker-host` / `--broker-port`, or the `MQTT_BROKER_HOST_NAME` /
`MQTT_BROKER_PORT` environment variables.

With TLS, using a client certificate:

```sh
cargo run -p sensor-link-console -- --device-id mock_0000 --broker-port 8883 \
    --cacert path/to/CA.pem --client-cert path/to/client.cert --client-key path/to/client.key
```

Passing `--cacert` enables TLS and requires `--client-cert` and `--client-key`.

## Commands

| Input | Topic | Payload |
| --- | --- | --- |
| `start` | `t/<device-id>/commands` | `{"cmd":"start"}` |
| `stop` | `t/<device-id>/commands` | `{"cmd":"stop"}` |
| `blink` | `t/<device-id>/commands` | `{"cmd":"blink"}` |
| `reboot` | `t/<device-id>/commands` | `{"cmd":"reboot"}` |
| `time` | `t/<device-id>/time` | the current time, `{"time":<µs since epoch>}` |
| `diagnostics on` | `t/<device-id>/commands` | `{"cmd":"diagnostics_on"}` |
| `diagnostics off` | `t/<device-id>/commands` | `{"cmd":"diagnostics_off"}` |
| `fwupdate <url>` | `t/<device-id>/fw_update/meta` | `{"url":"<url>","timestamp":null}` |
| `q` | | exit |

`fwupdate` announces a firmware update: the device downloads the image from
`<url>` (at most 128 bytes) and reports its progress on `fw_update/status`.

`diagnostics on` and `diagnostics off` switch publishing the device's own log
records on `log` on and off. Only firmware built with the `mqtt-log` feature
acts on them; other firmware ignores them. The device starts in the state its
application configured.

Each command is published with QoS 1 and the tool waits for the broker's
acknowledgement before reading the next line. The tool also exits when stdin
closes, so commands can be piped in:

```sh
echo start | cargo run -p sensor-link-console -- --device-id mock_0000
```

Messages are not retained. A device that is offline between syncs receives a
command sent in the meantime only if the broker kept its session.

## What the device publishes

The tool subscribes to `f/<device-id>/#` and prints every message it receives,
with the time it arrived (UTC) and the topic:

| Topic | Shown as |
| --- | --- |
| `log` | one line: time, level, target and message |
| `benchmark_data` | sample count, rate and time span, and min/max/mean per channel |
| other protocol topics | pretty-printed JSON |
| topics the protocol doesn't know, such as a product's data topic | the payload as received |

Binary payloads that can't be decoded are shown as hex, up to 64 bytes. Pass
`--raw` to show every payload as received.

# mock-device

A mock sensor device that speaks the sensor-link MQTT protocol from a host
machine, for exercising a broker and server without hardware.

It consists of a rumqttc MQTT driver, a CLI, a signal generator, and the
dispatch store, buffer and upload allocator. The pieces `network_task` is
generic over (status payload, device metadata, action queue) are implemented
as the smallest thing that satisfies each trait. The mock reconnects on its
own when the connection drops.

## Running

```sh
cargo run -p mock-device
```

It connects to `localhost:1883` by default. Point it elsewhere with
`--broker-host` / `--broker-port`, or the `MQTT_BROKER_HOST_NAME` /
`MQTT_BROKER_PORT` environment variables.

With TLS, using client certificates:

```sh
cargo run -p mock-device -- --tls --cacert path/to/sensor_CA.pem --certs path/to/mqtt_client_certs
```

Each instance is `mock_{:04}` (so `mock_0000` for the first). With `-n > 1`, the
cert directory must hold a `mock_0001/mock_0001.cert` + `mock_0001.key` pair per
instance.

Interactive commands on stdin: `start`, `stop`, `sync`, `q`. Pass
`--no-interactive` to skip the prompt.

## Syncing

Like a real device, the mock is offline between syncs: it connects, uploads
whatever the store holds, and disconnects once the connection goes idle. The
first sync happens at startup and the next one `--sync-interval` seconds later
(300 by default).

An earlier sync is triggered by anything that raises `Signal::UrgentEvent` —
an urgent event from the dispatch pipeline, or the `sync` command. A trigger
that arrives while a sync is already running starts the next one as soon as that
sync finishes.

## What it publishes

| Topic | Contents |
| --- | --- |
| `online` | birth and last-will messages |
| `info_v3` | device metadata (device type `mock`) |
| `status` | operational status, published once per sync |
| `events` | `Started` / `Stopped` around the measuring task |
| `benchmark_data` | simulated sensor data, 4 channels at 1 Hz |
| `log` | the device's own log records, in diagnostic mode (see below) |

Sensor data goes through the real dispatch pipeline: the measuring task feeds
`MockBuffer`, which accumulates samples until the buffer is full or its 30 s
maximum latency expires, serializes them in the uniform-sample (Q15XL) format,
and persists them to a stream store running on a 32 MiB in-memory flash. The
network task uploads from that store, so everything measured between syncs is
sent on the next one.

The protocol has no generic sensor-data topic — a product device declares a
`Data` topic of its own — so the mock reports on the protocol's `BenchmarkData`
test topic.

## Logs over MQTT

The crate enables the `mqtt-log` feature of `sensor-link-firmware`, so the
device's own log records can be published on its log topic. `--log-level` sets
what reaches the terminal and `--mqtt-log-level` (default `warn`) what is also
published; the latter cannot be more verbose than the former.

Records are only published in diagnostic mode, which starts off. The
`diagnostics_on` command starts it for the timeout it carries, and
`diagnostics_off` or the timeout ends it. In `sensor-link-console` that is
`diagnostics on [<seconds>]` and `diagnostics off`. While it lasts, the sync in
progress does not end when the connection goes idle, so records are published as
they are logged.

Take care when logging in response to dispatch activity: a record published over
MQTT can trigger the signal that produced it, which publishes another record,
forever. `Signal::DispatchQueueEmpty` is raised once per dispatch-task
iteration for exactly this reason and is logged at `trace`, below any level
worth publishing.

Log records bypass the dispatch pipeline: they are queued in memory only and the
network task publishes them whenever it has nothing else to send. With `-n > 1`
only the first instance publishes log records, since the logger they come from
is global to the process. Diagnostic mode is global too, so it keeps every
instance online.

# demo-server

Server for the demo device, built on `sensor-link-server-core`,
`sensor-link-mqtt` and `task-supervisor`. It speaks
[demo-protocol](../demo-protocol/) and stores everything in TimescaleDB.

| Feature | Where |
|---|---|
| Device registration and version tracking (`info_v3`) | [src/device/info_handler.rs](src/device/info_handler.rs) |
| Online tracking | [src/device.rs](src/device.rs) |
| Status, commands, firmware announce (core `StatusHandler`) | [src/device/status_handler.rs](src/device/status_handler.rs) |
| Settings sync by hash | [src/device/settings_sync.rs](src/device/settings_sync.rs) |
| Events and the 4 measurement channels | [src/db.rs](src/db.rs), [src/mqtt/processing.rs](src/mqtt/processing.rs) |
| Firmware directory watcher | [src/firmware.rs](src/firmware.rs) |
| HTTP status, `/api/v1/status` | [src/http.rs](src/http.rs) |

The schema and what an admin may change are in [database.md](database.md).

## Run locally

```sh
cd examples/demo-server
docker compose -f dev/docker-compose.yml up -d
cp .cargo/config.toml.example .cargo/config.toml
cargo run
curl localhost:8087/api/v1/status
```

Then point devices (or `demo-mock-device`) at `localhost:1884`, and manage them
with [tools/db-admin](tools/db-admin/):

```sh
tools/db-admin/db-admin.py   # http://127.0.0.1:8088
```

More in [dev/README.md](dev/README.md).

## mTLS

```sh
scripts/gen-dev-certs.sh DEMO-0001 DEMO-0002
```

- Writes a throwaway CA plus broker, `demo-server`, `admin` and device
  certificates into `certs/`, which is git-ignored.
- The server uses mTLS when `MQTT_CA_CERT_PATH`, `MQTT_CLIENT_CERT_PATH` and
  `MQTT_CLIENT_KEY_PATH` are all set.
- [deploy/](deploy/) runs the full stack with the mTLS broker.

## Firmware images

Copy signed images named `<binary>-v<version>-<git rev>-<suffix>.bin.sig.b64`
into `FIRMWARE_DIR`. The server adds a `firmware` row per image, and removes
the row when the file goes. Assign an image to devices with db-admin.

## Tests

The unit tests use mock stores and need no database:

```sh
cargo test -p demo-server
```

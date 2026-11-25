# Local development

Run the server on your machine against local containers. No TLS, no
authentication, non-default ports so nothing clashes with other local
services.

| Service | Endpoint | |
|---|---|---|
| MQTT broker | `localhost:1884` | plain, anonymous |
| TimescaleDB | `localhost:5433` | db `demo`, user/password `demo`/`demo` |
| Firmware files | `localhost:8089/fw/` | serves `dev/firmwares/` |
| HTTP API | `localhost:8087` | `cargo run` on the host |

## Run

Start the containers, configure the env vars once, run the server:

```sh
cd examples/demo-server
docker compose -f dev/docker-compose.yml up -d
cp .cargo/config.toml.example .cargo/config.toml
cargo run
```

- The server applies the migrations on start.
- TLS is a config-only difference: set the `MQTT_*_PATH` vars and `sslmode` in
  `TIMESCALEDB_URL`, see [../deploy/](../deploy/).

## Verify

```sh
curl localhost:8087/api/v1/status
# {"mqtt_connected":true,"db_connected":true,"version":"dev",...}
```

- `docker compose -f dev/docker-compose.yml stop mosquitto` flips
  `mqtt_connected` to `false`, `start` flips it back.
- The same works for `timescaledb`, within about 10 s.

## Checks

Same checks as CI, from the repo root:

```sh
cargo +nightly fmt --all
./scripts/clippy-changed.sh
cargo test --workspace --features sensor-link-firmware/test-mono
```

After a schema change, regenerate the tables in [../database.md](../database.md):

```sh
scripts/generate-db-docs.sh
```

## Teardown

```sh
docker compose -f dev/docker-compose.yml down      # keep database data
docker compose -f dev/docker-compose.yml down -v   # discard database data
```

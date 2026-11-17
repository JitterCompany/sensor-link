# db-admin

Local-only web page for the demo-server database:

- list devices and firmware images
- set the sync interval and data rate (`device_settings`)
- queue a command: Start, Stop, Reboot or Blink (`devices.command`)
- assign a firmware update (`devices.marked_for_update`)
- plot a device's `measurement_data`, with its power (VBUS × IBUS) and the
  periods it was online (`device_online_events`) shaded behind every chart
- list a device's `device_events`

| | |
|---|---|
| [db-admin.py](db-admin.py) | HTTP server + the SQL, stdlib only |
| [index.html](index.html) | the whole frontend, re-read per request, so edits need no restart |

## Running

```sh
docker compose -f ../../dev/docker-compose.yml up -d   # if not running yet
./db-admin.py
```

It prints the command it runs SQL through and the database and role it
reached, then serves <http://127.0.0.1:8088>.

- Binding is localhost-only. The tool has no authentication of its own.
- It connects as the database owner, so the only guard on a mistake is the
  confirmation dialog.
- SQL runs as `psql` inside the TimescaleDB container. Queries return their
  rows as JSON (`json_agg`), so nothing parses psql's text output.

| Env var | Default | |
|---|---|---|
| `PORT` | `8088` | HTTP port |
| `DB_NAME` | `demo` | database |
| `DB_USER` | `demo` | PostgreSQL role |
| `DB_ADMIN_SQL_CMD` | `docker compose -f ../../dev/docker-compose.yml exec -T timescaledb psql …` | any command that reads SQL on stdin and prints psql output |

A deployed stack, over SSH:

```sh
DB_ADMIN_SQL_CMD="ssh host 'docker compose exec -T timescaledb psql -qX -U demo -d demo'" \
    ./db-admin.py
```

## Using it

Tick devices in the devices table first. Every applier acts on that selection.

**Sync interval and data rate.** Enter a whole number of seconds and press
**Apply sync interval**, or pick a data rate and press **Apply data rate**.

- The sync interval must be 0 (always online) or 180 ... 14400 s (4 h), the
  same bounds as the `CHECK` on the column.
- A device without a `device_settings` row gets one. The column you did not
  set takes its column default, which matches the protocol default:
  `_1spm` and 900 s.
- No push is needed. The server hashes the stored settings at every sync and
  sends them when the hash differs from what the device reports.

**Commands.** Pick one and press **Send command**. The server delivers it at
the device's next sync. The *Pending command* column shows it until then.

**Firmware.** Pick an image and press **Apply to N selected**. **Clear update**
sets `marked_for_update` back to `NULL`.

- Refused when a device's `device_type` differs from the image's.
- Refused when the `firmware.id` does not exist (the column has no foreign key).
- Ids must match `^[A-Za-z0-9._:-]{1,128}$` and are quoted before they reach
  SQL.

**Measurements.** Pick a device on the Measurements tab, or click its id in
the devices table.

- Four plots: board temperature (°C), VBUS and VSYS (V), IBUS (A), power
  (VBUS × IBUS, W).
- Click a plot's title to fold it. Folded plots stay folded in this browser.
- The range is the last 1, 3, 12 or 24 hours, or a calendar day or week. ◀ and
  ▶ step one period, **Now** returns to the current one.
- Ranges over an hour are averaged into time buckets on the server, to about
  4000 points. At most 20 000 rows are fetched.
- Channels without a value are `NULL`. The line breaks there.
- The chart is plain inline SVG, nothing is loaded off the network.

**Recent events.** The newest 200 events of the picked device.

## Endpoints

| | |
|---|---|
| `GET /` | the page |
| `GET /api/overview` | `{ devices: [...], firmware: [...], data_rates: [{ value, label }] }` |
| `GET /api/measurements?device_id=…&start=…&end=…` | samples from `start` up to `end` (ISO 8601 with zone, at most 8 days apart): `{ device_id, start, end, bucket_s, channels, truncated, rows: [{ ts, fs, …channels }] }`, oldest first, `ts` in epoch ms |
| `GET /api/online?device_id=…&start=…&end=…` | `{ device_id, initial, truncated, rows: [{ ts, online, graceful }] }`, oldest first; `initial` is the state at `start`, `null` when unknown |
| `GET /api/events?device_id=…` | `{ device_id, truncated, events: [{ ts, received_at, event, message }] }`, newest first |
| `POST /api/assign` | `{ device_ids: [...], firmware_id: "…" \| null }` → `{ updated: N }` |
| `POST /api/sync-interval` | `{ device_ids: [...], sync_interval_s: N }` → `{ updated: N }`, `N` is 0 or in `180 … 14400` |
| `POST /api/data-rate` | `{ device_ids: [...], data_rate: "_1spm" }` → `{ updated: N }` |
| `POST /api/command` | `{ device_ids: [...], command: "Start" \| "Stop" \| "Reboot" \| "Blink" }` → `{ updated: N }` |

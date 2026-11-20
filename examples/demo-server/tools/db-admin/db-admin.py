#!/usr/bin/env python3
"""Local-only admin web tool for the demo-server database: list devices and
firmware images, change settings, assign firmware updates, queue commands and
plot measurements.

SQL runs as `psql` inside the TimescaleDB container, so it needs no open
database port. Results come back as JSON so no output parsing is needed.

Stdlib only, no venv:

    ./db-admin.py                          # local dev database (../../dev/)
    SSH_TARGET=demo@host ./db-admin.py     # a deployed stack, via SSH
"""

import json
import os
import re
import shlex
import subprocess
import sys
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs

# Localhost only: the tool has no authentication of its own and acts as the
# database owner.
BIND_ADDR = "127.0.0.1"
PORT = int(os.environ.get("PORT", "8088"))

DB_NAME = os.environ.get("DB_NAME", "demo")
DB_USER = os.environ.get("DB_USER", "demo")
DEV_COMPOSE = Path(__file__).resolve().parents[2] / "dev" / "docker-compose.yml"
# A deployed stack: the compose project lives in the SSH user's home dir, which
# is ssh's working directory (see ../../deploy/README.md).
SSH_TARGET = os.environ.get("SSH_TARGET")

PSQL = f"exec -T timescaledb psql -qX -U {DB_USER} -d {DB_NAME}"
if SSH_TARGET:
    DEFAULT_SQL_CMD = f"ssh {shlex.quote(SSH_TARGET)} 'docker compose {PSQL}'"
else:
    DEFAULT_SQL_CMD = f"docker compose -f {shlex.quote(str(DEV_COMPOSE))} {PSQL}"

# Base psql invocation; SQL is fed on stdin. Override to point somewhere else
# without touching the code.
SQL_CMD = os.environ.get("DB_ADMIN_SQL_CMD", DEFAULT_SQL_CMD)

INDEX_HTML = Path(__file__).with_name("index.html")

# Ids are device ids and UUIDs; anything else is a bug or an injection attempt.
ID_RE = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")


class SqlError(Exception):
    pass


def sql_literal(value):
    """Quote a Python value as a SQL literal (no libpq parameter binding here)."""
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "TRUE" if value else "FALSE"
    if isinstance(value, (int, float)):
        return repr(value)
    text = str(value)
    if "\x00" in text:
        raise SqlError("NUL byte in value")
    return "'" + text.replace("'", "''") + "'"


def run_sql(sql):
    """Run `sql` and return its raw stdout, or raise SqlError."""
    proc = subprocess.run(
        SQL_CMD,
        shell=True,
        input=sql,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if proc.returncode != 0:
        raise SqlError((proc.stderr or proc.stdout).strip() or "psql failed")
    return proc.stdout


def query_json(select_sql, cte=None):
    """Run a SELECT and return its rows as a list of dicts.

    `json_agg` does the row-to-JSON conversion server-side, so psql only has to
    print one value. The `\\pset` lines strip the header, footer and alignment
    that would otherwise wrap it — done here rather than with `-qtA` flags so
    that overriding DB_ADMIN_SQL_CMD cannot drop them.

    `cte` is prepended as a `WITH` clause. Postgres only accepts a
    data-modifying CTE at the top level of a statement, so an UPDATE has to be
    hoisted out here instead of being nested inside `select_sql`.
    """
    sql = (
        "\\pset pager off\n"
        "\\pset format unaligned\n"
        "\\pset tuples_only on\n"
        + (f"WITH {cte}\n" if cte else "")
        + "SELECT coalesce(json_agg(t), '[]'::json) FROM (\n"
        f"{select_sql}\n"
        ") t;\n"
    )
    out = run_sql("\\set ON_ERROR_STOP on\n" + sql).strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError as err:
        raise SqlError(f"unexpected psql output: {out[:400]!r}") from err


# LEFT JOIN: a device has no `device_settings` row until something writes one,
# so `sync_interval_s` comes back NULL for a device that was never configured.
DEVICES_SQL = """
    SELECT d.id, d.device_type, d.last_contact, d.version,
           d.marked_for_update, d.online_since, d.command, d.config_confirmed,
           s.sync_interval_s, s.data_rate
    FROM devices d
    LEFT JOIN device_settings s ON s.device_id = d.id
    ORDER BY d.id
"""

FIRMWARE_SQL = """
    SELECT id, version, description, date, recommended, device_type
    FROM firmware
    ORDER BY date DESC
"""


# Channels plotted by the graph, in the order the frontend draws them.
MEASUREMENT_CHANNELS = (
    "board_temperature",
    "vbus",
    "ibus",
    "vsys",
)

# The bucketing below keeps a window under this, so the cap only ever trips on
# unexpected data. It keeps a runaway device from pulling an unbounded result
# set through the SSH pipe into the browser.
MEASUREMENT_ROW_LIMIT = 20000

# Longest range the page may ask for: a week, with slack for the 25 h day of a
# DST changeover.
MEASUREMENT_MAX_SPAN_S = 8 * 24 * 3600

# A range is averaged into time buckets sized to leave about this many rows,
# so a day at 1 SPS does not push 86 400 rows through the pipe. An hour keeps
# its 1 s buckets, which pass the samples through as they are.
MEASUREMENT_TARGET_POINTS = 4000

def overview():
    return {
        "devices": query_json(DEVICES_SQL),
        "firmware": query_json(FIRMWARE_SQL),
        # So the page's data-rate dropdown lists exactly the values the
        # protocol enum has, without repeating them in the HTML.
        "data_rates": [{"value": v, "label": l} for v, l in DATA_RATES],
    }


def parse_time(value, name):
    """Return `value` (ISO 8601 with zone, as the page sends it) as a datetime."""
    try:
        # fromisoformat rejects the trailing Z of toISOString() before 3.11.
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise SqlError(f"invalid {name} time {value!r}") from None
    if parsed.tzinfo is None:
        raise SqlError(f"{name} time {value!r} has no timezone")
    return parsed


def timestamptz_sql(t):
    return f"{sql_literal(t.isoformat())}::timestamptz"


def measurements(device_id, start, end):
    """Return one device's samples from `start` up to `end`, oldest first.

    `time` becomes `ts`, epoch milliseconds, so the browser can feed it straight
    to `new Date()`. Channels stay NULL where the device had no value: the graph
    breaks its line there rather than drawing through a gap.

    Samples are averaged per time bucket. `fs` becomes the bucket rate where
    that is below the device's.

    Buckets start at `start`, so the first one does not reach before the range.

    The row cap has to apply to the *newest* rows, hence the inner DESC sort,
    with the outer one putting them back in plotting order.
    """
    if not ID_RE.match(str(device_id)):
        raise SqlError(f"invalid device id {device_id!r}")
    start_t, end_t = parse_time(start, "start"), parse_time(end, "end")
    span_s = (end_t - start_t).total_seconds()
    if not 0 < span_s <= MEASUREMENT_MAX_SPAN_S:
        raise SqlError(
            f"range must be longer than 0 and at most {MEASUREMENT_MAX_SPAN_S} s, "
            f"got {span_s:g} s"
        )
    bucket_s = max(1, int(span_s) // MEASUREMENT_TARGET_POINTS)

    # Power is averaged per sample rather than computed from the averages, so
    # a short burst of current at high voltage is not diluted.
    channels = ", ".join(f"avg({c}) AS {c}" for c in MEASUREMENT_CHANNELS)
    channels += ", avg(vbus * ibus) AS power"
    rows = query_json(f"""
        SELECT * FROM (
            SELECT extract(epoch FROM time_bucket(make_interval(secs => {bucket_s}), time, {timestamptz_sql(start_t)})) * 1000 AS ts,
                   least(min(fs), 1.0 / {bucket_s}) AS fs,
                   {channels}
            FROM measurement_data
            WHERE device_id = {sql_literal(device_id)}
              AND time >= {timestamptz_sql(start_t)}
              AND time < {timestamptz_sql(end_t)}
            GROUP BY ts
            ORDER BY ts DESC
            LIMIT {MEASUREMENT_ROW_LIMIT}
        ) recent
        ORDER BY ts
    """)
    return {
        "device_id": device_id,
        "start": start_t.isoformat(),
        "end": end_t.isoformat(),
        "bucket_s": bucket_s,
        "channels": [*MEASUREMENT_CHANNELS, "power"],
        "truncated": len(rows) >= MEASUREMENT_ROW_LIMIT,
        "rows": rows,
    }


# Newest events listed per device.
EVENTS_ROW_LIMIT = 200

# Two rows per sync, so even a week at the shortest interval stays far below.
ONLINE_ROW_LIMIT = 10000


def online_events(device_id, start, end):
    """Return one device's online/offline transitions from `start` up to `end`.

    `initial` is the state at `start`, from the last row before it: null when
    there is none, so the graph does not invent a state. Times are epoch ms.
    """
    if not ID_RE.match(str(device_id)):
        raise SqlError(f"invalid device id {device_id!r}")
    start_t, end_t = parse_time(start, "start"), parse_time(end, "end")
    device = sql_literal(device_id)
    initial = query_json(f"""
        SELECT online
        FROM device_online_events
        WHERE device_id = {device} AND time < {timestamptz_sql(start_t)}
        ORDER BY time DESC
        LIMIT 1
    """)
    rows = query_json(f"""
        SELECT extract(epoch FROM time) * 1000 AS ts, online, graceful
        FROM device_online_events
        WHERE device_id = {device}
          AND time >= {timestamptz_sql(start_t)}
          AND time < {timestamptz_sql(end_t)}
        ORDER BY time
        LIMIT {ONLINE_ROW_LIMIT}
    """)
    return {
        "device_id": device_id,
        "initial": initial[0]["online"] if initial else None,
        "truncated": len(rows) >= ONLINE_ROW_LIMIT,
        "rows": rows,
    }


def device_events(device_id):
    """Return one device's most recent events, newest first.

    Times are epoch milliseconds. `ts` is the device's clock, 0 when it was not
    yet set; `received_at` is when the server stored the event.
    """
    if not ID_RE.match(str(device_id)):
        raise SqlError(f"invalid device id {device_id!r}")
    events = query_json(f"""
        SELECT extract(epoch FROM time) * 1000 AS ts,
               extract(epoch FROM received_at) * 1000 AS received_at,
               event, message
        FROM device_events
        WHERE device_id = {sql_literal(device_id)}
        ORDER BY time DESC
        LIMIT {EVENTS_ROW_LIMIT}
    """)
    return {
        "device_id": device_id,
        "truncated": len(events) >= EVENTS_ROW_LIMIT,
        "events": events,
    }


# The `DataRate` variants of `demo_protocol::settings`, as the serde
# names the column stores, with the sample rate each one stands for.
DATA_RATES = (
    ("_1sps", "1 sample per second"),
    ("_1spm", "1 sample per minute"),
    ("_6sph", "6 samples per hour"),
    ("_2sph", "2 samples per hour"),
    ("_1sph", "1 sample per hour"),
)

# Matches the CHECK on `device_settings.sync_interval_s`. The device turns
# 1 ... 179 s into 0 (always online) and clamps to 4 h, so only these values
# mean what they say.
SYNC_INTERVAL_MIN = 180
SYNC_INTERVAL_MAX = 4 * 3600

# Values for `devices.command`. Start, Stop and Reboot go through the core
# command handler; the demo-server sends Blink itself.
COMMANDS = ("Start", "Stop", "Reboot", "Blink")


def write_settings(device_ids, values):
    """Set the named `device_settings` columns for the given devices.

    Creates the settings row where a device has none yet; the columns `values`
    does not name get their column defaults, which match the protocol defaults
    (`_1spm`, 900 s). The server picks the change up on its own: it hashes the
    stored settings every sync and pushes them to the device when that hash
    stops matching what the device reports.
    """
    if not device_ids:
        raise SqlError("no devices selected")
    for device_id in device_ids:
        if not ID_RE.match(str(device_id)):
            raise SqlError(f"invalid device id {device_id!r}")

    ids = ", ".join(sql_literal(i) for i in device_ids)

    # `device_settings.device_id` is an FK into `devices`, so select the ids
    # through `devices` rather than inserting them blind: an id that is not
    # there is then a short row count instead of a constraint violation.
    found = query_json(f"SELECT id FROM devices WHERE id IN ({ids})")
    missing = set(device_ids) - {row["id"] for row in found}
    if missing:
        raise SqlError(f"unknown device id(s): {', '.join(sorted(missing))}")

    columns = ", ".join(values)
    literals = ", ".join(sql_literal(v) for v in values.values())
    updates = ", ".join(f"{c} = EXCLUDED.{c}" for c in values)
    rows = query_json(
        "SELECT count(*) AS updated FROM upd",
        cte=(
            "upd AS ("
            f" INSERT INTO device_settings (device_id, {columns})"
            f" SELECT id, {literals} FROM devices"
            f" WHERE id IN ({ids})"
            f" ON CONFLICT (device_id) DO UPDATE SET {updates}"
            " RETURNING 1)"
        ),
    )
    return {"updated": rows[0]["updated"] if rows else 0}


def set_data_rate(device_ids, data_rate):
    """Set `device_settings.data_rate` for the given devices."""
    if data_rate not in {value for value, _ in DATA_RATES}:
        raise SqlError(f"unknown data rate {data_rate!r}")
    return write_settings(device_ids, {"data_rate": data_rate})


def set_sync_interval(device_ids, sync_interval_s):
    """Set `device_settings.sync_interval_s` for the given devices."""
    if isinstance(sync_interval_s, bool) or not isinstance(sync_interval_s, int):
        raise SqlError(f"sync interval must be a whole number of seconds, got {sync_interval_s!r}")
    if sync_interval_s != 0 and not SYNC_INTERVAL_MIN <= sync_interval_s <= SYNC_INTERVAL_MAX:
        raise SqlError(
            f"sync interval must be 0 or {SYNC_INTERVAL_MIN} ... {SYNC_INTERVAL_MAX} s, got {sync_interval_s}"
        )
    return write_settings(device_ids, {"sync_interval_s": sync_interval_s})


def set_command(device_ids, command):
    """Queue `command` in `devices.command`; the server sends it at the next sync."""
    if command not in COMMANDS:
        raise SqlError(f"unknown command {command!r}")
    if not device_ids:
        raise SqlError("no devices selected")
    for device_id in device_ids:
        if not ID_RE.match(str(device_id)):
            raise SqlError(f"invalid device id {device_id!r}")
    ids = ", ".join(sql_literal(i) for i in device_ids)
    rows = query_json(
        "SELECT count(*) AS updated FROM upd",
        cte=(
            "upd AS ("
            f" UPDATE devices SET command = {sql_literal(command)}"
            f" WHERE id IN ({ids}) RETURNING 1)"
        ),
    )
    return {"updated": rows[0]["updated"] if rows else 0}


def assign(device_ids, firmware_id):
    """Set `marked_for_update` on the given devices; `firmware_id` None clears it."""
    if not device_ids:
        raise SqlError("no devices selected")
    for value in list(device_ids) + ([firmware_id] if firmware_id else []):
        if not ID_RE.match(str(value)):
            raise SqlError(f"invalid id {value!r}")

    # `marked_for_update` is a plain copy of `firmware.id` without an FK, so
    # check the image exists rather than letting a typo mark a device for a
    # firmware that is not there.
    if firmware_id:
        found = query_json(
            f"SELECT id FROM firmware WHERE id = {sql_literal(firmware_id)}"
        )
        if not found:
            raise SqlError(f"unknown firmware id {firmware_id}")

    ids = ", ".join(sql_literal(i) for i in device_ids)
    rows = query_json(
        "SELECT count(*) AS updated FROM upd",
        cte=(
            "upd AS ("
            f" UPDATE devices SET marked_for_update = {sql_literal(firmware_id)}"
            f" WHERE id IN ({ids}) RETURNING 1)"
        ),
    )
    return {"updated": rows[0]["updated"] if rows else 0}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

    def _send(self, code, body, content_type):
        payload = body if isinstance(body, bytes) else body.encode()
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _json(self, code, obj):
        self._send(code, json.dumps(obj), "application/json")

    def do_GET(self):
        path, _, query = self.path.partition("?")
        if path == "/":
            # Read per request, so editing the page needs no restart.
            self._send(200, INDEX_HTML.read_bytes(), "text/html; charset=utf-8")
        elif path == "/api/overview":
            try:
                self._json(200, overview())
            except SqlError as err:
                self._send(500, str(err), "text/plain; charset=utf-8")
        elif path == "/api/measurements":
            params = parse_qs(query)
            device_id = params.get("device_id", [""])[0]
            start = params.get("start", [""])[0]
            end = params.get("end", [""])[0]
            try:
                self._json(200, measurements(device_id, start, end))
            except SqlError as err:
                self._send(500, str(err), "text/plain; charset=utf-8")
        elif path == "/api/online":
            params = parse_qs(query)
            device_id = params.get("device_id", [""])[0]
            start = params.get("start", [""])[0]
            end = params.get("end", [""])[0]
            try:
                self._json(200, online_events(device_id, start, end))
            except SqlError as err:
                self._send(500, str(err), "text/plain; charset=utf-8")
        elif path == "/api/events":
            device_id = parse_qs(query).get("device_id", [""])[0]
            try:
                self._json(200, device_events(device_id))
            except SqlError as err:
                self._send(500, str(err), "text/plain; charset=utf-8")
        else:
            self._send(404, "not found", "text/plain; charset=utf-8")

    def do_POST(self):
        if self.path not in ("/api/assign", "/api/sync-interval", "/api/data-rate", "/api/command"):
            self._send(404, "not found", "text/plain; charset=utf-8")
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            req = json.loads(self.rfile.read(length) or b"{}")
            device_ids = req.get("device_ids") or []
            if self.path == "/api/assign":
                self._json(200, assign(device_ids, req.get("firmware_id")))
            elif self.path == "/api/data-rate":
                self._json(200, set_data_rate(device_ids, req.get("data_rate")))
            elif self.path == "/api/command":
                self._json(200, set_command(device_ids, req.get("command")))
            else:
                self._json(200, set_sync_interval(device_ids, req.get("sync_interval_s")))
        except SqlError as err:
            self._send(400, str(err), "text/plain; charset=utf-8")
        except (ValueError, KeyError, TypeError) as err:
            self._send(400, f"bad request: {err}", "text/plain; charset=utf-8")


def main():
    # flush: stdout is block-buffered when redirected, and serve_forever() never
    # returns, so unflushed startup lines would never show up in a log file.
    print(f"SQL via: {SQL_CMD}", flush=True)
    try:
        info = query_json("SELECT current_database() AS db, current_user AS usr")
    except SqlError as err:
        sys.exit(f"cannot reach the database: {err}")
    print(f"connected to {info[0]['db']} as {info[0]['usr']}", flush=True)
    print(f"listening on http://{BIND_ADDR}:{PORT}", flush=True)
    ThreadingHTTPServer((BIND_ADDR, PORT), Handler).serve_forever()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass

-- Initial schema of the demo server.
--
-- The COMMENT ON statements are the single source of truth for the schema
-- reference in database.md. After changing the schema or a comment, regenerate
-- the doc with scripts/generate-db-docs.sh (CI fails while it is stale).

-- Devices: one row per device (current/latest state).
CREATE TABLE devices (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    device_type TEXT NOT NULL,
    last_contact BIGINT,
    version JSONB,
    marked_for_update TEXT,
    command TEXT NOT NULL,
    config_confirmed BOOLEAN NOT NULL,
    config_last_sent_at BIGINT,
    status_since BIGINT NOT NULL,
    register_time BIGINT NOT NULL,
    sim_iccid TEXT NOT NULL,
    online_since TIMESTAMPTZ
);

COMMENT ON TABLE devices IS
    'One row per registered device: current/latest state (no history).';
COMMENT ON COLUMN devices.id IS 'Device id, as used in the MQTT topics';
COMMENT ON COLUMN devices.name IS 'Human-readable name, set server-side (empty until assigned)';
COMMENT ON COLUMN devices.device_type IS 'Device type; currently always Demo';
COMMENT ON COLUMN devices.last_contact IS 'Last contact over MQTT, Unix epoch milliseconds';
COMMENT ON COLUMN devices.version IS 'Version info as reported by the device (JSON)';
COMMENT ON COLUMN devices.marked_for_update IS 'firmware.id this device should update to (plain copy, no FK); set by an admin to schedule a firmware update';
COMMENT ON COLUMN devices.command IS 'Pending server-to-device command: None, Start, Stop, Reboot or Blink; writable by an admin, reset to None by the server once handled';
COMMENT ON COLUMN devices.config_confirmed IS 'Whether the device confirmed the last settings sent to it';
COMMENT ON COLUMN devices.config_last_sent_at IS 'When settings were last sent to the device, Unix epoch milliseconds';
COMMENT ON COLUMN devices.status_since IS 'When the current status started, Unix epoch milliseconds';
COMMENT ON COLUMN devices.register_time IS 'First registration, Unix epoch milliseconds';
COMMENT ON COLUMN devices.sim_iccid IS 'ICCID of the SIM card in the device; writable by an admin';
COMMENT ON COLUMN devices.online_since IS 'Start of the current MQTT connection (UTC); NULL when offline';

-- Device status: time-series of status samples per device.
CREATE TABLE device_status (
    time TIMESTAMPTZ NOT NULL,
    device_id TEXT NOT NULL,
    settings_hash BIGINT NOT NULL,
    status TEXT NOT NULL,
    signal_strength INTEGER NOT NULL,
    temperature REAL NOT NULL,
    PRIMARY KEY (time, device_id)
);

SELECT create_hypertable('device_status', 'time');
ALTER TABLE device_status SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'device_id',
    timescaledb.compress_orderby = 'time DESC'
);
SELECT add_compression_policy('device_status', INTERVAL '7 days');
SELECT add_retention_policy('device_status', INTERVAL '1 year');

COMMENT ON TABLE device_status IS
    'Time series of status samples per device. Compressed after 7 days, dropped after 1 year.';
COMMENT ON COLUMN device_status.time IS 'When the status was received (UTC)';
COMMENT ON COLUMN device_status.device_id IS 'Device the status belongs to; joins to devices.id';
COMMENT ON COLUMN device_status.settings_hash IS 'Hash of the settings active on the device (u64 stored as signed)';
COMMENT ON COLUMN device_status.status IS 'Device status: unknown, startup, inactive, active, tryactive, idle, waiting, nosensor, configoutdated, noconfig or error';
COMMENT ON COLUMN device_status.signal_strength IS 'Cellular signal strength as reported by the modem';
COMMENT ON COLUMN device_status.temperature IS 'Board temperature, degrees Celsius';

-- Device settings: one row per device, holding the operational settings that
-- can change on-the-fly (the `Settings` payload of the settings topic). The
-- settings hash is not stored: the server derives it from the row.
--
-- Plain relational table: this is current state, not a time series. The
-- device id is both the primary key and a foreign key into `devices`, so
-- settings disappear along with their device.
CREATE TABLE device_settings (
    device_id       TEXT PRIMARY KEY REFERENCES devices (id) ON DELETE CASCADE,
    data_rate       TEXT NOT NULL DEFAULT '_1spm',
    -- 4 h is the longest interval the device accepts, so a larger value
    -- could never be confirmed.
    sync_interval_s INTEGER NOT NULL DEFAULT 900
        CHECK (sync_interval_s = 0 OR sync_interval_s BETWEEN 180 AND 14400)
);

COMMENT ON TABLE device_settings IS
    'Operational settings per device (current state, no history). Deleted along with the device. Writable by an admin: the server picks up changes and syncs them to the device.';
COMMENT ON COLUMN device_settings.device_id IS 'Device the settings belong to; FK to devices.id';
COMMENT ON COLUMN device_settings.data_rate IS 'Measurement data rate: _1sps, _1spm, _6sph, _2sph or _1sph (samples per second/minute/hour)';
COMMENT ON COLUMN device_settings.sync_interval_s IS 'Maximum interval between status updates, seconds: 0 (always online) or 180 ... 14400 (4 h)';

-- Firmware: one row per firmware image (per device type).
--
-- `id` is a TEXT primary key to satisfy the upstream `DataStoreId = String`
-- contract (the id is copied verbatim into `devices.marked_for_update`). It is
-- generated server-side so the Rust side never mints one.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE firmware (
    id          TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    version     TEXT NOT NULL,
    description TEXT NOT NULL,
    date        TIMESTAMPTZ NOT NULL,
    v2_bin_id   TEXT NOT NULL,
    recommended BOOLEAN NOT NULL,
    device_type TEXT NOT NULL
);

-- `get_firmwares` and the recommend/unrecommend pair all filter by device_type.
CREATE INDEX firmware_device_type_idx ON firmware (device_type);

COMMENT ON TABLE firmware IS
    'One row per firmware image in the served firmware directory (per device type); kept in sync with that directory by the server.';
COMMENT ON COLUMN firmware.id IS 'Server-generated id (UUID as text); referenced by devices.marked_for_update';
COMMENT ON COLUMN firmware.version IS 'Firmware version string, parsed from the image file name';
COMMENT ON COLUMN firmware.description IS 'Human-readable description of the image';
COMMENT ON COLUMN firmware.date IS 'When the server found the image (UTC)';
COMMENT ON COLUMN firmware.v2_bin_id IS 'Download URL of the firmware binary';
COMMENT ON COLUMN firmware.recommended IS 'Whether this is the recommended image for its device type';
COMMENT ON COLUMN firmware.device_type IS 'Device type the image is for; currently always Demo';

-- Sensor server log: time-series of server-side log entries per device.
-- The domain `SensorServerLog` also carries `group_id` and `duration`; device
-- groups are not modelled here and the duration is not needed.
CREATE TABLE sensor_server_log (
    time TIMESTAMPTZ NOT NULL,
    sensor_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    type TEXT NOT NULL,
    header TEXT NOT NULL,
    body TEXT[] NOT NULL,
    PRIMARY KEY (time, sensor_id)
);

SELECT create_hypertable('sensor_server_log', 'time');
ALTER TABLE sensor_server_log SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'sensor_id',
    timescaledb.compress_orderby = 'time DESC'
);
SELECT add_compression_policy('sensor_server_log', INTERVAL '7 days');
SELECT add_retention_policy('sensor_server_log', INTERVAL '1 year');

COMMENT ON TABLE sensor_server_log IS
    'Time series of server-side log entries per device. Compressed after 7 days, dropped after 1 year.';
COMMENT ON COLUMN sensor_server_log.time IS 'When the entry was logged (UTC)';
COMMENT ON COLUMN sensor_server_log.sensor_id IS 'Device the entry is about; joins to devices.id';
COMMENT ON COLUMN sensor_server_log.user_id IS 'User that triggered the event (empty for system events)';
COMMENT ON COLUMN sensor_server_log.type IS 'Log entry type';
COMMENT ON COLUMN sensor_server_log.header IS 'Short summary line';
COMMENT ON COLUMN sensor_server_log.body IS 'Detail lines';

-- Measurement data: one row per sample published on the `data` topic, with a
-- column per channel. A channel the device had no value for is NULL rather
-- than NaN: NaN poisons every aggregate over the column (Postgres sorts it
-- above all other floats), while aggregates skip NULL.
--
-- No retention policy: the measurement data is the primary product data.
CREATE TABLE measurement_data (
    time              TIMESTAMPTZ NOT NULL,
    device_id         TEXT NOT NULL,
    fs                REAL NOT NULL,
    board_temperature REAL,
    vbus              REAL,
    ibus              REAL,
    vsys              REAL,
    PRIMARY KEY (device_id, time)
);

SELECT create_hypertable('measurement_data', 'time');
ALTER TABLE measurement_data SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'device_id',
    timescaledb.compress_orderby = 'time DESC'
);
SELECT add_compression_policy('measurement_data', INTERVAL '7 days');

COMMENT ON TABLE measurement_data IS
    'Measurement samples published by devices on the MQTT data topic; one row per sample. Primary product data: compressed after 7 days, never dropped (no retention policy).';
COMMENT ON COLUMN measurement_data.time IS 'Sample timestamp (UTC)';
COMMENT ON COLUMN measurement_data.device_id IS 'Device that recorded the sample; joins to devices.id';
COMMENT ON COLUMN measurement_data.fs IS 'Sampling frequency (Hz) of the message this sample arrived in';
COMMENT ON COLUMN measurement_data.board_temperature IS 'Board temperature, degrees Celsius; NULL without a measurement';
COMMENT ON COLUMN measurement_data.vbus IS 'Charger input bus voltage, V; NULL without a measurement';
COMMENT ON COLUMN measurement_data.ibus IS 'Charger input bus current, A; NULL without a measurement';
COMMENT ON COLUMN measurement_data.vsys IS 'System rail voltage, V; NULL without a measurement';

-- Device events: one row per event published on the MQTT events topic.
-- Rare (a few per device per day), so a plain table without compression or
-- retention. The key deduplicates batches the device resends after a lost
-- PUBACK.
CREATE TABLE device_events (
    time TIMESTAMPTZ NOT NULL,
    device_id TEXT NOT NULL,
    event TEXT NOT NULL,
    message TEXT,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (device_id, time, event)
);

COMMENT ON TABLE device_events IS
    'Events published by devices on the MQTT events topic (boot, start/stop, sensor errors, button presses); one row per event, never dropped.';
COMMENT ON COLUMN device_events.time IS 'Event timestamp from the device (UTC); 1970-01-01 when the device clock was not yet set';
COMMENT ON COLUMN device_events.device_id IS 'Device that sent the event; joins to devices.id';
COMMENT ON COLUMN device_events.event IS 'Event name as in the MQTT payload, e.g. booted, started, sensorerror, blink, button_pressed';
COMMENT ON COLUMN device_events.message IS 'Event description (at most 16 characters); NULL for events without one';
COMMENT ON COLUMN device_events.received_at IS 'When the server stored the event';

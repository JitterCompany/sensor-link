-- Online and offline transitions as reported over MQTT, to plot connectivity
-- next to the measurements. Two rows per sync per device, so a hypertable with
-- the same compression and retention as device_status.
CREATE TABLE device_online_events (
    time TIMESTAMPTZ NOT NULL,
    device_id TEXT NOT NULL,
    online BOOLEAN NOT NULL,
    graceful BOOLEAN NOT NULL,
    PRIMARY KEY (device_id, time)
);

SELECT create_hypertable('device_online_events', 'time');
ALTER TABLE device_online_events SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'device_id',
    timescaledb.compress_orderby = 'time DESC'
);
SELECT add_compression_policy('device_online_events', INTERVAL '7 days');
SELECT add_retention_policy('device_online_events', INTERVAL '1 year');

COMMENT ON TABLE device_online_events IS
    'Online and offline transitions per device, from the MQTT online topic and the broker''s last will; one row per message.';
COMMENT ON COLUMN device_online_events.time IS 'When the server received the message (UTC). The offline last will carries no device timestamp';
COMMENT ON COLUMN device_online_events.device_id IS 'Device the message is about; joins to devices.id';
COMMENT ON COLUMN device_online_events.online IS 'true: the device connected; false: it disconnected';
COMMENT ON COLUMN device_online_events.graceful IS 'false for an offline row from the broker''s last will: the connection dropped without a clean disconnect. Always true for online rows';

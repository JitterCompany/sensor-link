diesel::table! {
    firmware (id) {
        id -> Text,
        version -> Text,
        description -> Text,
        date -> Timestamptz,
        v2_bin_id -> Text,
        recommended -> Bool,
        device_type -> Text,
    }
}

diesel::table! {
    device_status (time, device_id) {
        time -> Timestamptz,
        device_id -> Text,
        settings_hash -> Int8,
        status -> Text,
        signal_strength -> Int4,
        temperature -> Float4,
    }
}

diesel::table! {
    measurement_data (device_id, time) {
        time -> Timestamptz,
        device_id -> Text,
        fs -> Float4,
        board_temperature -> Nullable<Float4>,
        vbus -> Nullable<Float4>,
        ibus -> Nullable<Float4>,
        vsys -> Nullable<Float4>,
    }
}

diesel::table! {
    device_events (device_id, time, event) {
        time -> Timestamptz,
        device_id -> Text,
        event -> Text,
        message -> Nullable<Text>,
        received_at -> Timestamptz,
    }
}

diesel::table! {
    device_online_events (device_id, time) {
        time -> Timestamptz,
        device_id -> Text,
        online -> Bool,
        graceful -> Bool,
    }
}

diesel::table! {
    sensor_server_log (time, sensor_id) {
        time -> Timestamptz,
        sensor_id -> Text,
        user_id -> Text,
        #[sql_name = "type"]
        log_type -> Text,
        header -> Text,
        body -> Array<Text>,
    }
}

diesel::table! {
    devices (id) {
        id -> Text,
        name -> Text,
        device_type -> Text,
        last_contact -> Nullable<Int8>,
        version -> Nullable<Jsonb>,
        marked_for_update -> Nullable<Text>,
        command -> Text,
        config_confirmed -> Bool,
        config_last_sent_at -> Nullable<Int8>,
        status_since -> Int8,
        register_time -> Int8,
        sim_iccid -> Text,
        online_since -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    device_settings (device_id) {
        device_id -> Text,
        data_rate -> Text,
        sync_interval_s -> Int4,
    }
}

diesel::joinable!(device_settings -> devices (device_id));
diesel::allow_tables_to_appear_in_same_query!(device_settings, devices);

// Allows joining `devices` to its `device_status` samples (see the device reads
// in `db.rs`). There is no FK, so the join is expressed explicitly via `.on(...)`.
diesel::allow_tables_to_appear_in_same_query!(device_status, devices);

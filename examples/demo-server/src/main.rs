use std::{sync::Arc, time::Duration};

use demo_server::{
    db, device, firmware, http, mqtt, BUILD_DATE, BUILD_VERSION, CARGO_VERSION, GIT_REV,
};
use task_supervisor::PanicCallback;
use tokio::{
    select, signal,
    sync::{mpsc, watch},
};
use tracing_subscriber::EnvFilter;

fn print_boot_message() {
    let build_version = BUILD_VERSION.unwrap_or("dev");
    let git_revision = GIT_REV.unwrap_or("dev");
    let build_date = BUILD_DATE.unwrap_or("dev");
    tracing::info!("******");
    tracing::info!("# Starting Demo Server v{build_version} ({git_revision})");
    tracing::info!("# Cargo version: {CARGO_VERSION}");
    tracing::info!("# Build date: {build_date}");
    tracing::info!("******");
}

fn print_shutdown_message() {
    let build_version = BUILD_VERSION.unwrap_or("dev");
    let git_revision = GIT_REV.unwrap_or("dev");
    tracing::info!("******");
    tracing::info!("# Stopping Demo Server v{build_version} ({git_revision})");
    tracing::info!("******");
}

async fn shutdown() {
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
        .expect("Failed to create SIGTERM handler");
    let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt())
        .expect("Failed to create SIGINT handler");
    select! {
        _ = sigterm.recv() => tracing::info!("Received SIGTERM"),
        _ = sigint.recv() => tracing::info!("Received SIGINT"),
    };
}

fn make_panic_hook() -> PanicCallback {
    Arc::new(|nr_panics, task_name| {
        tracing::error!("Task {task_name} panicked ({nr_panics} time(s))");
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    print_boot_message();

    let (mqtt_status_tx, mqtt_status_rx) = watch::channel(false);
    let (db_status_tx, db_status_rx) = watch::channel(false);
    let (data_tx, data_rx) = mpsc::channel(256);
    let (device_tx, device_rx) = mpsc::channel(256);
    let (tx_to_mqtt, rx_to_mqtt) = mpsc::channel(256);

    // 1. Database pool + health check
    let db_health_handle = db::connect_timescaledb()
        .await
        .map(|pool| db::start_health_check_task(pool, db_status_tx, make_panic_hook()));
    let database = db::connect_timescaledb()
        .await
        .map(db::TimescaleDb::new)
        .ok_or(anyhow::anyhow!("Failed to connect to database"))?;
    let device_sync_handle =
        device::start_task(database.clone(), tx_to_mqtt, device_rx, make_panic_hook());

    let data_processing_handle =
        mqtt::processing::start_task(data_rx, database.clone(), make_panic_hook());

    let firmware_sync_handle = firmware::start_task(database.clone(), make_panic_hook());

    // 2. HTTP server
    let http_handle = http::start_task(
        http::ServerStatus {
            mqtt_connected: mqtt_status_rx,
            db_connected: db_status_rx,
        },
        make_panic_hook(),
    );

    // 3. MQTT client
    let mqtt_handle = mqtt::start_task(
        mqtt_status_tx,
        data_tx,
        device_tx,
        rx_to_mqtt,
        database,
        make_panic_hook(),
    )?;

    // 4. Await shutdown signal & shutdown
    shutdown().await;
    print_shutdown_message();

    let _ = mqtt_handle
        .shutdown_with_timeout(Duration::from_secs(5))
        .await;
    let _ = http_handle
        .shutdown_with_timeout(Duration::from_secs(10))
        .await;
    if let Some(handle) = db_health_handle {
        let _ = handle.shutdown_with_timeout(Duration::from_secs(5)).await;
    }

    let _ = device_sync_handle
        .shutdown_with_timeout(Duration::from_secs(30))
        .await;

    let _ = data_processing_handle
        .shutdown_with_timeout(Duration::from_secs(5))
        .await;

    let _ = firmware_sync_handle
        .shutdown_with_timeout(Duration::from_secs(5))
        .await;

    tracing::info!("Exit");
    Ok(())
}

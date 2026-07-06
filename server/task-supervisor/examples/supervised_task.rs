//! Run with: cargo run --example supervised_task
//!
//! Spawns a supervised worker that panics on its third tick; the supervisor
//! restarts it (after a 5s backoff) and invokes the panic callback. After
//! ~12 seconds the task is shut down gracefully.

use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::watch;

async fn worker(mut shutdown_rx: watch::Receiver<()>, tick_count: Arc<AtomicU32>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                tracing::info!("worker: shutdown requested, cleaning up...");
                break;
            }
            _ = interval.tick() => {
                let ticks = tick_count.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!("worker: tick {ticks}");
                if ticks == 3 {
                    panic!("simulated crash on tick 3");
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let on_panic: PanicCallback = Arc::new(|nr_panics, task_name| {
        tracing::error!("{task_name} panicked ({nr_panics} time(s) so far)");
    });

    let tick_count = Arc::new(AtomicU32::new(0));
    let task_function = worker;
    let handle = Handle::new(
        move |shutdown_rx| task_function(shutdown_rx, tick_count.clone()),
        get_crate_relative_function_path(task_function),
        on_panic,
    );

    tokio::time::sleep(Duration::from_secs(12)).await;

    tracing::info!("main: shutting down");
    handle
        .shutdown_with_timeout(Duration::from_secs(3))
        .await
        .expect("worker failed to shut down in time");
    tracing::info!("main: done");
}

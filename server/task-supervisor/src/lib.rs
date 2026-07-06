use std::{any::Any, sync::Arc, time::Duration};

pub type PanicCallback = Arc<dyn Fn(u32, &str) + Send + Sync + 'static>;

use futures::{future::Fuse, pin_mut, Future, FutureExt};

use tokio::{
    sync::{
        oneshot,
        watch::{self, Receiver},
    },
    time::{sleep, timeout},
};

/// Generic utility function for logging about tasks.
/// Takes the path to a function and removes the crate prefix and type parameter suffix.
pub fn get_crate_relative_function_path<F>(_: F) -> String
where
    F: Any,
{
    let type_name = std::any::type_name::<F>().to_string();
    let mut parts = type_name.split("<");
    let Some(type_name) = parts.next() else {
        return type_name.to_string();
    };
    let mut parts = type_name.split("::");
    let start = parts.nth(1).unwrap_or_default().to_string();
    parts.fold(start, |acc, part| acc + "::" + part)
}

pub struct ShutdownSignal {
    rx: Fuse<oneshot::Receiver<()>>,
}

impl ShutdownSignal {
    /// Completes if a shutdown is requested via Handle::shutdown()
    pub async fn is_requested(&mut self) {
        let rx = &mut self.rx;

        pin_mut!(rx);

        // Any result means shutdown
        let _ = rx.await; // panic because tx is gone
    }
}

/// A handle to a supervised background task.
///
/// The task restarts automatically on panic or unexpected exit.
/// Call [`Handle::shutdown`] or [`Handle::shutdown_with_timeout`] to stop it.
pub struct Handle {
    shutdown_sender: oneshot::Sender<()>,
    join: tokio::task::JoinHandle<()>,
    task_name: String,
}

impl Handle {
    /// Spawns a supervised task, calling `on_panic(panic_count)` whenever the task panics.
    ///
    /// `on_panic` receives the cumulative panic count (starting at 1) and must be cheaply
    /// cloneable (e.g. a closure capturing an `Arc` or `mpsc::Sender`).
    pub fn new<F, Fut>(async_func: F, task_name: impl Into<String>, on_panic: PanicCallback) -> Self
    where
        F: Fn(Receiver<()>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let task_name = task_name.into();
        let (tx, rx) = oneshot::channel();
        let mut shutdown_signal = ShutdownSignal { rx: rx.fuse() };
        tracing::info!("Spawning {task_name}");
        Handle {
            task_name: task_name.clone(),
            shutdown_sender: tx,
            join: tokio::spawn(async move {
                let mut nr_panics = 0;
                let mut shutdown_requested = false;
                let (inner_tx, _) = watch::channel(());
                let mut result = tokio::spawn(async_func(inner_tx.subscribe()));
                loop {
                    tokio::select! {
                        _ = shutdown_signal.is_requested() => {
                            shutdown_requested = true;
                            let _ = inner_tx.send(());
                            tracing::debug!("{task_name}: One shot: Shutdown requested");
                        },
                        res = &mut result => {
                            if shutdown_requested {
                                // At this point the task already shut down due to the watch channel message.
                                // Now we can break out of the loop.
                                tracing::debug!("{task_name}: Shutdown requested");
                                break;
                            }
                            match res {
                                Ok(_) => {}, // tasks without inner loop should be restarted (unless shutdown is requested)
                                Err(err) if err.is_panic() => {
                                    tracing::warn!("{task_name} panicked and will restart after 5 seconds. Error: {err}");
                                    nr_panics += 1;
                                    on_panic(nr_panics, &task_name);
                                    sleep(Duration::from_secs(5)).await;
                                }
                                Err(err) => {
                                    tracing::warn!("{task_name} stopped unexpectedly (no panic) and will restart after 5 seconds. Error: {err}");
                                    sleep(Duration::from_secs(5)).await;
                                },
                            }

                            // Delay to prevent busy loop when task is immediately restarting
                            sleep(Duration::from_millis(10)).await;
                            result = tokio::spawn(async_func(inner_tx.subscribe()));
                        }
                    }
                }
                tracing::info!("Exit {task_name}");
            }),
        }
    }
}

impl Handle {
    /// Gracefully shutdown the task and wait for the task to be shut down
    ///
    /// Note that this depends on the cooperation of the target task.
    /// If the target task is very busy or not properly monitoring its ShutdownSignal,
    /// this future might never complete.
    ///
    /// See [`shutdown_with_timeout()`](#method.shutdown_with_timeout) for a variant with a timeout.
    ///
    /// To create a Handle, see [`Handle::new()`](#method.new).
    pub async fn shutdown(self) -> Result<(), tokio::task::JoinError> {
        self.try_shutdown().await
    }

    fn try_shutdown(self) -> tokio::task::JoinHandle<()> {
        // Drop sender, this notifies the ShutdownSignal (channel closes)
        drop(self.shutdown_sender);

        self.join
    }

    /// Similar to `shutdown()` but with a timeout.
    /// The task is forced to abort if it has not completed after the timeout expires
    pub async fn shutdown_with_timeout(self, timeout_duration: Duration) -> anyhow::Result<()> {
        let task_name = self.task_name.clone();
        let mut join = self.try_shutdown();
        let jh = &mut join;
        pin_mut!(jh);

        match timeout(timeout_duration, jh).await {
            Ok(result) => Ok(result?),
            Err(_) => {
                // Kill the task so that at least it doesnt keep blocking
                // other tasks from shutting donwn
                join.abort();
                tracing::warn!("{}: Timeout while shutting down task!", &task_name);
                Err(anyhow::Error::msg(format!(
                    "{}: Timeout while shutting down task",
                    &task_name
                )))
            }
        }
    }
}

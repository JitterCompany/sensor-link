# task-supervisor

Supervised tokio background tasks. A `Handle` spawns an async task that:

- restarts automatically when it panics or exits unexpectedly (5s backoff),
  invoking a `PanicCallback` with the cumulative panic count;
- shuts down cooperatively: the task receives a `watch::Receiver<()>` and
  should exit when it fires; `Handle::shutdown_with_timeout()` aborts the
  task if it doesn't finish in time.

```rust
let handle = Handle::new(
    move |shutdown_rx| my_task(shutdown_rx, state.clone()),
    get_crate_relative_function_path(my_task),
    on_panic,
);
// ...
handle.shutdown_with_timeout(Duration::from_secs(5)).await?;
```

See [examples/supervised_task.rs](examples/supervised_task.rs) for a
runnable demo of the restart and shutdown behavior:

```sh
cargo run --example supervised_task
```

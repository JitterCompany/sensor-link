# sensor-link
Jitter Sensor Link Libraries

Reusable building blocks for Jitter sensor platforms, in three groups:

| Directory | Contents |
|---|---|
| [server/](server/) | Server-side crates |
| protocol/ | Device ↔ server protocol crates (planned) |
| firmware/ | Firmware crates (planned) |

## Crates

| Crate | Description |
|---|---|
| [server/task-supervisor](server/task-supervisor/) | Supervised tokio background tasks: auto-restart on panic, graceful shutdown with timeout |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

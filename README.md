# sensor-link
Jitter Sensor Link Libraries

Reusable building blocks for Jitter sensor platforms, in three groups:

| Directory | Contents |
|---|---|
| [server/](server/) | Server-side crates |
| sensor-link-protocol/ | Device ↔ server protocol crate (planned) |
| sensor-link-firmware/ | Firmware library crate (planned) |

## Crates

| Crate | Description |
|---|---|
| [server/task-supervisor](server/task-supervisor/) | Supervised tokio background tasks: auto-restart on panic, graceful shutdown with timeout |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

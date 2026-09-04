# sensor-link
Jitter Sensor Link Libraries

Reusable building blocks for Jitter sensor platforms, in three groups:

| Directory | Contents |
|---|---|
| [server/](server/) | Server-side crates |
| sensor-link-protocol/ | Device ↔ server protocol crate |
| sensor-link-firmware/ | Firmware library crate |

## Crates

| Crate | Description |
|---|---|
| [server/task-supervisor](server/task-supervisor/) | Supervised tokio background tasks: auto-restart on panic, graceful shutdown with timeout |

## Linting

The workspace carries a backlog of clippy findings.
Rather than fix them all at once, CI gates only files a pull request actually
changes, so the backlog shrinks as the code gets touched.
Run the script below to check what clippy warnings you must solve to get your PR accepted.

```bash
./scripts/clippy-changed.sh            # compare against origin/master
./scripts/clippy-changed.sh HEAD~1     # or any other base ref
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

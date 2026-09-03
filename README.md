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
| [sensor-link-provision](sensor-link-provision/) | Desktop provisioning tool: flashes bootloader, firmware and device config over J-Link, signs device certificates with a YubiKey-held CA |

## Linting

The workspace carries a backlog of clippy findings inherited from the code moved
in from the Frogwatch repos. Rather than fix them all at once, CI gates only on
findings in files a pull request actually changes, so the backlog shrinks as the
code gets touched. Touch a file, and you own its findings.

```bash
./scripts/clippy-changed.sh            # compare against origin/master
./scripts/clippy-changed.sh HEAD~1     # or any other base ref
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

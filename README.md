# Jitter sensor-link

The Jitter Sensor Link platform: the shared code behind Jitter sensor devices, from the
firmware on the device to the server it reports to. It also holds the tooling used
to produce devices.

## Libraries

| Crate | Contents |
|---|---|
| [sensor-link-firmware](sensor-link-firmware/) | Firmware building blocks: drivers, storage, bootloader etc |
| [sensor-link-protocol](sensor-link-protocol/) | Device ↔ server protocol |
| [server/sensor-link-server-core](server/sensor-link-server-core/) | Server core: devices, sensor data and time series, events, etc |
| [server/sensor-link-mqtt](server/sensor-link-mqtt/) | Server-side MQTT client |
| [server/sensor-link-notify](server/sensor-link-notify/) | Server-side Notifications: alarms, e-mail and SMS |
| [server/task-supervisor](server/task-supervisor/) | Supervised tokio background tasks: auto-restart on panic, graceful shutdown with timeout |

## Production tooling

Tools to help build & test hardware during manufacturing.

| Directory | Contents |
|---|---|
| [sensor-link-provision/](sensor-link-provision/) | Desktop provisioning tool: flashes bootloader, firmware and device config over J-Link, signs device certificates with a YubiKey-held CA |
| [factory-test/](factory-test/) | Factory test firmware for test jigs, each its own embedded workspace excluded from the root workspace |

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

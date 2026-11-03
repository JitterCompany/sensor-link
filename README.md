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

## Releases

Each library crate is released on its own, with a `<crate>-v<version>` tag such
as `sensor-link-firmware-v0.3.0`. To release:

1. Set `version` in the crate's `Cargo.toml`.
2. Rename the `## Unreleased` section of the crate's `CHANGELOG.md` to
   `## <version> - <date>`.
3. Merge to master, then tag that commit and push the tag.

The *Library release* workflow checks the tag against `Cargo.toml` and publishes
a GitHub Release. Its notes are the changelog section and it carries a
CycloneDX SBOM (`<tag>.cdx.json`). Downstream pins the release with
`tag = "<crate>-v<version>"` on the git dependency.

`sensor-link-provision` follows the same steps with its own
`sensor-link-provision-v*` tags, see its [README](sensor-link-provision/README.md).

## Dependency audit

The *Dependency audit* workflow runs `cargo audit` on every pull request and
weekly. Advisories that do not apply, or have no usable fix yet, are ignored in
[.cargo/audit.toml](.cargo/audit.toml), each with its reason.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

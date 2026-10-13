# Changelog

## Unreleased

- `--verify` resets an already provisioned board and runs the boot and MQTT
  checks, without a YubiKey. `--flash` first updates bootloader and firmware
  and keeps the device's config and certificate.

## 1.2.0 - 2026-10-11

- Optional checks: a *Checks* row in the session screen turns the SIM ICCID
  check and the MQTT connection check on or off. Defaults come from a new
  optional `[checks]` section in `provision.toml`.
- MQTT verify step: after the UID shows up in the boot log, the app keeps
  reading RTT for up to `mqtt_timeout_s` (default 60 s) until the firmware
  logs its connected marker. A miss marks the device UNVERIFIED. Needs
  firmware built against a sensor-link-firmware that logs the marker, so it
  is off by default.
- ICCID check accepts 19 digits as well as 20, still with the Luhn check.
- The issuance log path is remembered per project once a session starts.

## 1.1.0 - 2026-09-08

- Windows and Intel macOS builds.
- Probes are tested for WinUSB mode on Windows. A probe problem shows in a
  warning box above *Start session*, with a hint when the J-Link needs the
  WinUSB driver.

## 1.0.0 - 2026-09-04

- Initial release.

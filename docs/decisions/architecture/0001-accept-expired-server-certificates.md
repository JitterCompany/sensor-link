# ADR-0001: Accept expired MQTT server certificates on the device

## Status
**Current Status**: Accepted

**Date**: 2026-11-03

## Context
The modem's MQTT TLS context sets `ignorelocaltime` to 1, so the validity dates of the server certificate are not checked. The chain to the pinned CA is still verified (`seclevel` 2).

Turning the date check on has two failure modes:

- An expired server certificate takes the whole fleet offline. Firmware updates are announced over the same MQTT connection, so the fix cannot be delivered over the air.
- The modem clock is not set by the firmware. A wrong clock makes a valid certificate look "not yet valid" or expired. The modem reports a generic open error, so the firmware cannot tell this apart from a real rejection.

The gain is small. The device pins a private CA that only signs our own brokers. Date checks only help against an attacker holding an old broker key whose certificate has expired. Without revocation (no CRL or OCSP on the modem), the answer to a leaked key is rotating the CA anyway.

## Decision
Keep `ignorelocaltime` at 1. Certificate lifetime is handled on the server side:

- The device trusts the CA, never the broker certificate. Renewing the broker certificate needs no firmware change.
- The CA certificate is long-lived, longer than the expected life of the hardware.
- The CA certificate is compiled into the product firmware and written to the modem by `ensure_provisioned` when it differs. A CA rotation is a firmware update.
- Broker certificate expiry is monitored and alerted on well before the date.

CA rotation rule: ship firmware that trusts both the old and the new CA first. Switch the broker to a certificate from the new CA only once the fleet runs that firmware.

## Consequences
- An expired broker certificate does not disconnect devices.
- A leaked broker key stays usable against devices until the CA is rotated.
- Rotating the CA depends on the modem accepting a `cacert.pem` holding more than one certificate. Verify on each modem variant before relying on it.
- Device client certificates live in the provisioned config section, not in the firmware. They cannot be renewed over the air. Near their expiry the broker must accept them past their end date, or the devices must be re-provisioned.

## Alternatives Considered
- **Check dates only with a trusted clock.** Set the modem clock with NITZ or `AT+QNTP` before TLS, enable the check only when the clock is plausible, and fall back to `ignorelocaltime` 1 after repeated failures. Rejected for now: much firmware and test effort for the small gain described above.
- **Check dates unconditionally.** Rejected: a single lapsed certificate or a bad clock bricks the update path.

## Links
- **Issues**: https://github.com/JitterCompany/sensor-link/issues/52

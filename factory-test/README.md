# Factory test firmware

Standalone firmware images for test jigs. Each one checks hardware (typically at the PCBA factory)
and reports a verdict through LEDs and a parseable RTT log.

| Directory | Contents |
|---|---|
| [modem-test/](modem-test/) | LTE modems (mPCIe), on the SL23-modem-tester board |

## Ideas, not yet done

**A shared framework crate.** Part of `modem-test` is not specific to that jig:
- the `STEP/MEAS/CHECK/INFO` log grammar (`report.rs`)
- zones, verdicts and blink codes (`result.rs`)
- the LED rendering (`led.rs`)
- most of the modem steps (`modem.rs`)

The board bring-up, the board-level checks, the RTIC app and the BSP are per jig. When a
second factory test firmware is added, compare the two to see how much really overlaps. If
it's enough, move the shared part into `factory-test/sensor-link-factory-test/`, behind
traits (`OutputPin`, `embedded-io-async`, a current source) rather than a BSP. Until then,
a copy is cheaper than an abstraction built on one user.

**Driving the test from `sensor-link-provision`.** The provision tool already flashes
over J-Link. It could also flash a factory test image, capture the RTT log at the pinned
control block, and parse the results (today done by
`modem-test/scripts/factory-collect.py`). That would give the factory one tool
instead of a script.

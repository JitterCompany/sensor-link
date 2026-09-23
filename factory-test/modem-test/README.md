# Modem factory test

Factory test firmware for LTE modems, running on the SL23-modem-tester board: a jig with an
mPCIe slot for the modem under test. It is a standalone image. Each run checks the jig itself
and then the modem, and gives a red/green verdict for each, with blink codes for the operator
and a parseable RTT log for traceability.

Its scope is modems only. The jig also has an SL23 extension connector, but a test for a
sensor module on it needs sensor-specific knowledge, so it belongs with that sensor's own
firmware.

Currently supported:

| | |
|---|---|
| Modems | Quectel EC21 and EC25 on a mini-PCIe carrier (EC21 verified on hardware). Any other `AT+CGMM` model fails the test as "model not accepted". The list is `SUPPORTED_MODELS` in `src/factory/modem.rs`. |
| Hardware | The SL23-modem-tester board |

More modem types or hardware platforms can be added later, within the same modems-only scope.

## Reading the result

Press **Reset** to start a run. **Busy** blinks for about 15 s, and the result is final once it
stops. The step-by-step operator version is
[docs/factory-test-instruction.pdf](docs/factory-test-instruction.pdf).

There are two LED pairs. At power-up or reset, every LED lights for about 0.4 s as a lamp test.

**Status** (Busy / Error), next to the power connector: the jig itself.

| Status LEDs | Meaning |
|---|---|
| **Busy** blinking | Test running: don't read the result or swap the module yet |
| **Busy** off, **Error** off | Run finished, the jig is fine: read the Modem test pair |
| **Error** blinking | The jig failed its self-test: count the flashes |

| Error flashes | Jig fault |
|---|---|
| 2 | Processor: clock, board straps, or boot stats |
| 3 | Charger |
| 4 | Power supply rails |
| 5 | PCB temperature |
| 6 | Firmware fault (panic) |

**Modem test** (Pass / Fail), next to the module slot: the module under test.

| Modem test LEDs | Meaning |
|---|---|
| **Pass** blinking | Modem test running |
| **Pass** steady | Modem passed |
| **Fail** blinking | Modem failed: count the flashes |
| **Fail** steady | No verdict: the jig failed (see Status / Error), so the modem was not tested |

| Fail flashes | Modem fault |
|---|---|
| 2 | Module does not answer |
| 3 | SIM not detected |
| 4 | Module draws too much current |
| 5 | No module detected |
| 6 | Wrong module type |

On a jig error every Fail LED is on steadily, the unused **Sensor test** one included, and the
modem slot stays unpowered.

Flashes come in repeating groups; count one group. The lowest code is 2, so there's
never a single flash.

The codes are defined as `BoardFault` and `ModemFault` in
[src/factory/result.rs](src/factory/result.rs). What sets each one, and the gate values
behind it, are in [docs/factory-test.md](docs/factory-test.md).

## Building

This is a separate embedded workspace (thumbv7em-none-eabihf), excluded from the host
workspace at the repo root. It builds on a pinned commit of `sensor-link-firmware` (see
`Cargo.toml`), plus `bsp/`, a trimmed STM32L4R7 board support package.

```bash
cargo build --release --bin factory_test
probe-rs run --chip STM32L4R7ZITx target/thumbv7em-none-eabihf/release/factory_test
```

For a factory image, run the **Modem factory test release build** workflow (manual, in
GitHub Actions). It uploads `modem-test-<version>-<run>.elf` and `.bin`. Every push and PR
also runs `cargo check` on this workspace (the `modem-test` job in `rust.yml`).

## Documentation

- [docs/factory-test.md](docs/factory-test.md): engineering reference. Covers steps, gates and
  where each number came from, the log grammar, and building and flashing.
- [docs/factory-test-instruction.pdf](docs/factory-test-instruction.pdf): the operator
  instruction, to print.
- [docs/programming-guide.pdf](docs/programming-guide.pdf): how to program a board, needed
  once per board.

  Both are written in [typst](https://typst.app); the `.typ` sources sit next to the PDFs.
  Rebuild after an edit with `typst compile docs/<name>.typ docs/<name>.pdf`.
- Hardware: schematic, layout and the pin map this firmware follows
  ([`SL23-modem-tester/pinmap.toml`](https://github.com/JitterCompany/debug-tools/blob/master/SL23-modem-tester/pinmap.toml))
  are in [JitterCompany/debug-tools](https://github.com/JitterCompany/debug-tools).
- [scripts/factory-collect.py](scripts/factory-collect.py): collates RTT captures into CSV.

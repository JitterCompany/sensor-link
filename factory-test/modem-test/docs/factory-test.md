# Modem factory test

The board under test is **not** this board. The SL23-modem-tester is a jig, and this
firmware uses it to verify LTE modems in its mPCIe slot. A few jigs go to the PCBA factory
with this firmware so modules can be checked on arrival, with no network and no engineer
present.

**Scope is modems only, and stays that way.** The jig also carries an SL23 extension
connector and a UI connector. Testing a sensor module on them needs knowledge of that
specific sensor, so such tests belong with the sensor's own firmware, not in this repo.

The same binary runs unmodified on **v0** — the pin-compatible board it was developed on,
before any jig existed. Nothing that gates branches on which board it is running on.

The run is started by the `Reset` button; there is no other control.

## What the operator sees

One red/green pair per zone. The pair beside the DC jack is the jig's own, silkscreened
`Status` — green `Busy`, red `Error`; each module zone has a `Pass`/`Fail` pair beside the
connector it judges, under that zone's name. This firmware has one module zone, `Modem test`.
The `Sensor test` pair shows no result of its own: it joins the lamp test, and its Fail LED
lights steadily on a jig error like every other Fail LED.

| moment | main pair (`Status`) | zone pairs (`Pass`/`Fail`) |
|---|---|---|
| power-up, ~400 ms | both on | both on |
| jig self-test running | green blinks | off |
| **jig failed** | **red blinks the board code** | **red steady on** |
| zone test running | green blinks | that zone's green blinks |
| zone passed | green blinks until the run ends, then off | green steady |
| zone failed | as above | red blinks that zone's code |
| firmware panicked | records it, resets, then shows board fault 6 on the next boot | — |

Three invariants carry it:

- **Steady red anywhere means the jig failed.** Trust nothing on the board under test; the
  main pair blinks the reason. A zone's fault code is pure short blinks, so a red that goes
  dark is a code and a red that never does is the jig.
- **Main green blinking means the run is still going.** When it goes out the verdicts are
  final — that is the moment it is safe to read them.
- **Steady green means that zone passed**, and only module zones ever show it. The main
  green is never steady, so a green near the power input cannot be read as "the module is
  good".

The ~400 ms all-on lamp test at power-up is not decoration: without it a green LED with a
dry joint would make every good module read as a failure, forever, and nobody would
suspect the tester.

## Blink codes

Codes are scoped to a zone, so each starts at 2 and none needs more than a handful of
flashes. They are ordered by how often we expect to see them.

**Board** (the jig itself), main pair:

| code | meaning |
|---|---|
| 2 | MCU: wrong sysclk, unexpected board straps, or boot stats showing a fault loop |
| 3 | charger not answering on I2C3, wrong part, or latched faults |
| 4 | a rail outside its window |
| 5 | board temperature implausible |
| 6 | the firmware itself panicked on the previous run |

A board failure ends the run: the modem steps are skipped and report `NOT RUN`, so the
slot is never powered by a jig that failed its self-test. That also keeps the operator's
swap rule simple: when `Busy` is not blinking, the slot is off.

Code 6 covers both ways the firmware itself can fail: a panic, and a run that never
finishes. The deadline task waits `RUN_DEADLINE_MS` (120 s, against a 15 s run) and, if
the run is still going, marks every step that never reported as failed, fails the *board*
zone and publishes. It blames the jig rather than the step that happened to be executing:
a run that did not finish says nothing about the module in the slot, and attributing it
to, say, `Step::ModemCurrent` would bin a possibly-good part as over-current. Every zone
then shows steady red, which is right — no verdict from an unfinished run is worth
reading.

Verified with a `loop {}` in step 8 that never yields, so the step's own 5 s timeout
cannot fire either:

```text
  run deadline of 120000 ms expired
   1 boot           PASS   …   7 modem_sim      PASS
   8 modem_current  FAIL
  zone board    FAIL, blink code 6
FACTORY TEST: FAIL steps 8
```

All of it published while a task spins forever at priority 1 — the deadline task at
priority 3 and the LED task at priority 2 both preempt it, which is the whole point of
running the indicators off atomics rather than off the sequence.

Code 6 is not a pattern of its own. The panic handler sets the `Panic` flag in a backup
register, prints to RTT and resets; the boot that follows sees
`BootReason::Panic`, reports it and **deliberately runs no steps**. A deterministic panic
would otherwise reset forever and the operator would watch a flickering lamp test with no
verdict ever appearing. The flag is consumed by the read, so pressing `Reset` tries again:
a transient panic clears itself, a repeatable one shows code 6 every time and
`panic_total` climbs in the log.

The handler does **not** park the outputs first. The reset un-drives every pin a
microsecond later, leaving the board's own pulls to hold them, and parking would touch
port G, where `Pin::new` calls `pwr::enable_vddio2()` and busy-waits on `IOSV` with no
timeout: in a panic early enough that VDDIO2 is not valid, the handler would hang there
forever — no reset, no flag, no LEDs.

The message goes straight at RTT channel 0 rather than through the logger, which takes a
lock the interrupted code may be holding, and in `NoBlockTrim` so it cannot block on a
host that is not listening:

```text
FACTORY TEST: PANIC at step 8: panicked at modem-test/src/factory/modem.rs:489:5:
```

Reusing the board-fault vocabulary means a panic needs no pattern of its own, and nothing
extra in the operator's instructions.

**Modem**, pair beside the mPCIe slot:

| code | meaning | recognised by |
|---|---|---|
| 2 | module present, not talking | current rose normally, no AT response |
| 3 | SIM not detected or ICCID unreadable | `CPIN?` never READY, or `QCCID` fails |
| 4 | module drawing excessive current | step 5 delta above `MODULE_OVERCURRENT_MA`, *and* a later step failed |
| 5 | no module current | IBUS never rose — nothing fitted, or the 3V3_PCIE rail failed |
| 6 | model not accepted | `CGMM` not in `SUPPORTED_MODELS` (`src/factory/modem.rs`) |

**There is no code 1, in either zone, and there never will be.** A single flash
separated by a long gap is indistinguishable from a slow steady blink — the operator has
no reference for the rate, so "one flash every two seconds" and "a blink at half a hertz"
look the same. Starting at 2 means the eye always sees a *group*, and the grouping is
what makes a count readable at all. The cost is one extra flash on every code; the
flashes were slowed to 2 Hz (200 ms on, 300 ms off, 2 s gap) so that six of them are
still comfortable to count.

**Over-current arms a code; it does not fail a module.** Step 5 reports the delta against
its limits as a `MEAS` row, so an out-of-band reading is on record and machine-readable,
and then carries on to the AT handshake regardless. A module that answers, reports an
accepted model, reads its SIM and returns its ICCID passes, however much current it drew.
If communication does fail, the armed code 4 is already in place and wins over "does not
answer", which is the more useful of the two facts.

The reason is that the band comes from one good module and one bad one — and the bad one
does not answer AT either. There is therefore no evidence that a module can draw too much
and still work, and no basis for rejecting one that does. A threshold that has never had
to be right must not be able to fail a working part on its own. Once a batch has been
through and the log holds real distributions, re-arming it as a hard gate is a one-line
change: `arm_fault` back to `fail`.

The exception is *no* current, which stays a hard gate. An empty slot has nothing to bin
and there is nothing to wait fifteen seconds for.

Pass is defined by communication, never by current alone: the modem answers, the model is
accepted, the SIM reads READY and the ICCID comes back. Current is what *picks the code*
when communication fails, so the operator learns how a module is broken rather than only
that it is.

## What this does not prove

The slot is a mini-PCIe socket, but this board is **not** a mini-PCIe tester and cannot
become one without new hardware. Only four things are wired to it: the switched 3V3 rail,
USART3, `CELLULAR_RESET_n`/`DTR`, and the module's `LED_WWAN`. Specifically:

- **No USB.** The slot's `USB_D+`/`USB_D-` (J7 pins 38 and 36) go only to the debug
  header J6, which is DNP. They never reach the MCU, whose own USB is wired to the UI
  connector instead. A module whose USB is dead passes this test.
- **No PCIe.** Lane pin 33 is unconnected. Nothing here exercises it.
- **No RF and no network.** `AT+CFUN=4` puts the module in airplane mode on purpose — the
  factory may have no usable LTE coverage. Antenna, RF path and registration are all
  untested.
- **No SIM of the module's own.** The SIM holder is on the module; the test proves that
  holder and the module's SIM interface, using one card that stays at the station.

This matters for the name as much as the scope: the board tests *modems that happen to be
in mini-PCIe form factor*, over UART. It does not test mini-PCIe cards.

## The log

Every result is a tagged line, so a host script can parse a whole run without knowing what
any step measures:

```text
STEP  <code> <name> START
STEP  <code> <name> PASS|FAIL <ms>
MEAS  <code> <key> <value> <unit> <lo> <hi> PASS|FAIL|INFO
CHECK <code> <key> PASS|FAIL
INFO  <code> <key> <free text>
```

Absent fields are `-`, and every `MEAS` value is an integer in milli-units, so no tool has
to parse a float. `scripts/factory-collect.py` collates captures into CSV keyed by the MCU
unique ID.

The RTT control block is pinned at **0x2009FF00**:

```bash
JLinkRTTLogger -Device STM32L4R7ZI -If SWD -Speed 4000 \
               -RTTAddress 0x2009FF00 -RTTChannel 0 board1.log
./scripts/factory-collect.py board1.log > results.csv
```

`JLinkRTTLogger` needs a TTY on stdout; run it under a pty if scripting it.

## Building and flashing

```bash
cd factory-test/modem-test && cargo build --release --bin factory_test
```

Flashing needs a **reset and halt before the load**. The firmware on a board may be
asleep, and J-Link cannot run its flash routine on a sleeping core — the symptom is
"Failed to erase sectors" / "Failed to execute RAMCode":

```
device STM32L4R7ZI
SelectInterface swd
speed 4000
connect
r
h
loadfile firmware.elf
r
g
```

`probe-rs run --chip STM32L4R7ZITx <elf>` is the better choice while developing, because it
attaches before the firmware starts and so streams the whole run.

## The BSP

`bsp/` (crate `sl23-modem-tester-bsp`, imported as `hardware`) is a trimmed copy of an in-house
STM32L4R7 board support package, cut down to what this jig uses: clocks, GPIO and EXTI,
USART3 with DMA, I2C3, the ADC with VDDA, the RTC backup registers, the TIM5
monotonic, and the 5V5 power switch. It has one board init, `Board::init`,
for the SL23-modem-tester (and the pin-compatible v0). The `boot` step fails any board
whose `hw_v0_1`/`hw_v0_2` straps read other than low/high.

Peripherals the jig does not use (SPI flash, SD card, and v0's external SRAM) get no
driver, but init puts their pins in the idle state those drivers would leave them in. The copy is not kept in sync with the BSP it came from.
Everything else comes from `sensor-link-firmware`, by path.

## Current state

All eight steps are implemented and verified on a healthy v0 board, and **every gate
is now set from measured hardware**. Failure paths walked on real modules:

| case | run time | outcome |
|---|---|---|
| good module, SIM fitted | 14.3 s | PASS, all eight steps |
| slot empty | 2 s | modem blink code 5, steps 6–8 NOT RUN |
| module fitted, SIM removed | 14.6 s | modem blink code 3, step 7 the only failure |
| module that gets hot | 2 s | modem blink code 4, steps 6–8 NOT RUN |

The good-module pass was re-confirmed after the over-current band was tightened: 19 mA
against the 80 mA gate, all eight steps, 3 Mbaud link.

### Running it at the factory

One power connection in the morning; **NRST — the `Reset` button — is the start button**
from then on. A reset re-runs the whole sequence from `init`, with no reflash and no power
cycle, so the operator loop is: seat module, press `Reset`, wait for `Busy` to stop
blinking, read the verdict, swap. About 15 s of test in a ~30 s cycle.

That makes the swap a hot swap, so it is worth being precise about what the slot looks
like between runs. Every supply pin at J7 — 2, 24, 39, 41 and 52, 3.3Vaux included — is
downstream of U6, the load switch, and nothing else at the connector is fed from the
always-on +3V3. `power_off()` suspends the UART before dropping the rail, and
`park_all_outputs()` leaves `CELLULAR_PWR_EN`, `CELLULAR_RESET_n` and DTR driven low with
both UART pins as inputs pulled down. So after any ending, no jig pin can source current
into the slot.

U6 answers both questions that would otherwise be left open. The SLG59M301V discharges
its output through an internal 100–300 Ω when disabled, so the 205 µF downstream of it
(C93 and C95 at 100 µ, plus C94 and C96) decays with a 20–60 ms time constant: the slot
is at zero within roughly 300 ms of the rail being cut, long before an operator's hand
reaches the latch. **No bleeder resistor is needed**, and the charge-dump-into-the-next-
module concern does not apply. It also holds its own enable down through an internal
~4 MΩ, so the pin is never truly floating while the MCU is in reset.

That leaves one judgement call, not a defect. R50, the external pulldown on
`CELLULAR_PWR_EN`, is DNP. 4 MΩ is a weak hold: against a ~1 V enable threshold it takes
only about 250 nA to reach it, which is the order of I/O leakage at temperature and of
surface leakage across an uncoated board that gets handled all day in a factory. Fitting
R50 at 100 k makes the arithmetic uninteresting — you would need 10 µA — and costs 33 µA
while the rail is enabled. The footprint is already there, so it is nearly free
insurance for the one window that matters: the MCU in reset, just after the operator
pressed `Reset`, with their fingers still on the module.

There is no free card-detect to interlock against: no slot pin is grounded by the module
and left free on the base, and pin 44 `UIM_PRESENCE` is an input to the module (pulled to
1V8 through R8 on the carrier), not a presence output. The carrier's SIM holder has no
detect switch either. A seat interlock would need a mechanical switch and a spare GPIO;
worth it only if the button ever proves to be a problem.

### The over-current band, measured

The current thresholds were the last guesses in the test. Both populations are now
measured, on IBUS at the ~11.8 V input, as the mean of the step 5 burst:

| module | step 5 delta | step 8 delta | VSYS while powered |
|---|---|---|---|
| known good | 18 mA | 59 mA | 6547 mV (idle 6549) |
| known bad, gets hot | 141 mA | 166 mA | 6539 mV (idle 6552) |

141 mA at 11.8 V is 1.7 W going into a module that should be drawing 0.2 W — that is
the heat, and it is a factor of eight from the good part, so the gate does not have to
be clever. `MODULE_OVERCURRENT_MA` is **80 mA**: 4.4× the good module, 1.8× below the
bad one, biased towards never failing a good part. It gates the burst *mean*, so a
single sample landing on a boot transient cannot fail a good module by itself.

VSYS barely moves under the fault — 13 mV — so the load switch is not folding back and
the supply is comfortable. A module that did drag VSYS down would show up in
`system_mv_modem_on`, which is why that row is recorded.

Both figures are n=1. Every current is recorded on every run, pass or fail, so the band
can be tightened once a batch has been through.

The bad module was measured twice, with the bench supply limited to 0.4 A and then to
2 A: 141 mA and 140 mA, with the adapter rail at 11778 mV and 11773 mV. So the reading
is not a clamped supply — but note that a 0.4 A supply *is* audibly limiting at the
moment power is connected, on inrush, long before the test samples anything. The
operator instruction asks for 1 A or more for that reason.

Before the gate was measured, this module ran the full 35 s and came out as "present,
not talking" — true, but the less useful of the two facts about it, and it
cost the 15 s AT timeout to reach. Catching it at step 5 bins it in 2 s and tells the
factory *how* it is broken.

The no-SIM run is the one to read if you want to see the log do its job. The module
answers `+CME ERROR: 10` to `AT+CPIN?`, so the retry loop stops on the first attempt
rather than spending eight, and `AT+QCCID` and `AT+CIMI` then fail in their own right:

```text
INFO  7 sim not inserted (+CME ERROR: 10)
CHECK 7 sim_ready FAIL
  iccid: no value, module answered "+CME ERROR: 13"
CHECK 7 iccid FAIL
  imsi: no value, module answered "+CME ERROR: 3"
```

**An `INFO` row for a queried field appears only when the field is real.** A query that
errored produces no row at all — only a note, which is indented, logged at error level and
ignored by the parser. That matters more than it looks: the reply to a failed `AT+QCCID`
is `+CME ERROR: 13`, and stripping the `+CMD: ` echo the way a successful reply needs
would reduce it to `13`. Reported as a field value, a bare `13` is indistinguishable from
a card number to anything reading the log, so a run with no SIM could put a fake ICCID
into a traceability record. The machine-readable signal for the failure is the `CHECK`
row, which is where it belongs; `factory-collect.py` over this run emits exactly one
`iccid` row, `CHECK … FAIL` with an empty value.

Steps 5, 6 and 8 still pass with no SIM, which is correct: the SIM is not on the tester's
side of the interface.

### The board can measure its own supply

`bsp/src/adc.rs` samples the internal reference (ADC1_IN0, `VREFEN`) and
compares it against the factory calibration at `0x1FFF75AA`, which is the only
way to get an absolute voltage from an ADC whose every other reading is relative
to the supply being measured. `Measurements` gained `vdda_millivolts`, plus
`vrefint_raw` and `vrefint_cal` so a suspect reading can be diagnosed from a log.

It agrees with independent measurement to within half a percent: 3334 mV
measured, against 3348 mV read by the J-Link as VTref, against a multimeter.

This is the check that matters most on this board, because a sagging 3V3 is
invisible otherwise: the MCU runs fine down to 1.71 V, so the firmware keeps
working and every other ADC reading simply scales silently. What it looks like
instead is arbitrary firmware misbehaviour -- a board whose rail sat at 2 V
browned out mid-step and presented as every timer in the firmware stalling at
once, which cost a day of debugging the timer queue. Gate the supply.

### Measured values, healthy board against a failed one

Taken on two v0 boards, 2026-09-15. The second had a 3V3 rail that had
collapsed to 2 V; it is included because it is what the gates are set to catch.

| key | healthy | failed | gate |
|---|---|---|---|
| `vdda_mv` | 3334 | 2029 | 3150..3450 |
| `system_mv` (VSYS) | 6550 | 4364 | 6000..13000 |
| `pgood_3v3` | 5/5 high | 0/5 – 5/5, flapping | all samples high |
| `pgood_5v5` | asserts | never asserts | gated |
| `pcb_temp` | 23.4 C | 41.6 C | 5..45 C |
| `pcb_temp_spread` | 25 mC | 3900 mC | 0..500 mC |
| `adapter_mv` | 11775 | 11775 | informational |
| `ibus_idle_ma` | 34 | 17 | informational |

Every gate above fails the broken board, which is the point. Three earlier
conclusions drawn from that board alone were wrong and are corrected here:

- **The 5V5 rail does not need a battery.** It comes up and its power-good
  asserts with no pack fitted, because a healthy VSYS is 6550 mV -- above the
  6250 mV floor the charger driver programs. The failed board sat below it.
- **The 3V3 power-good is not too weak to gate.** The ~40 kOhm internal pull-up
  holds it solidly; it reads 5/5 high on a healthy board.
- **The temperature spread is not ADC noise.** 25 mC healthy against 3900 mC
  failed makes it a sensitive supply-health check, and it is gated as one.

### Modem steps, measured with a good module

A known-good EC21 on the bench, 2026-09-15. Whole run 14.3 s, of which the
module's own boot is 10.7 s.

| key | value | note |
|---|---|---|
| `baud` | 3000000 | this module had been provisioned, so it answers at the persisted rate |
| `boot_time` | 10729 ms | power-on to first `AT` answer |
| `model` / `modem_firmware` | EC21 / EC21EFAR06A03M4G | |
| `modem_idle_delta_ma` | 18 | over the board's idle baseline, rail up, before boot |
| `modem_running_delta_ma` | 65 | booted, RF off |
| `modem_off_delta_ma` | 0 | rail cut, current back to baseline |
| `iccid` / `imsi` | 89430103525273063623 / 232010879151802 | |

**Both baud rates are tried, and neither is persisted.** A factory-fresh module
answers at 115200; one that has been through provisioning answers at the rate
the application firmware persisted with `AT&W`. The test probes both and records
which one replied, so the operator never has to know and the module's
provisioning state ends up in the log. Setting or persisting a rate is the
application's job, not this test's. Answering at 3 Mbaud is worth something extra
on its own: it exercises the level shifters at full speed, where a marginal one
would show up.

`AT+QPOWD` is attempted in both its spellings and this firmware revision answers
`ERROR` to both, while a plain `AT` immediately before still returns `OK` -- so
the command is refused rather than the session being dead, most likely because
`CFUN=4` has the radio off. The test records which of the two it was and then
cuts the rail regardless; `modem_rail_off` gates on the current returning to the
idle baseline, which is the real proof that the module powered down and that the
load switch switches.

The present threshold (10 mA over baseline) is still provisional; the over-current one
is set from measured modules -- see *The over-current band, measured* above. Every current
is recorded on every run, so both can be tightened from real parts rather than a datasheet.

### Hardware notes

- Power-good: U3 (3V3) is on pin 117 = PD3, U2 (5V5) on pin 122 = PD6, which is what
  `pinmap.toml` says.
- The modem's TX and RX are crossed relative to USART3's default pin functions, and the
  UART driver undoes that with `CR2.SWAP` — kept from v0, so one binary serves both
  boards. It means `cellular_uart_tx` in the pin map is the pin the MCU *receives* on,
  which matters when parking them: the pin that can drive an unpowered module is the
  other one.
- Board init drives PD4/PD5 high as the SRAM's OE/WE standby state. On the tester
  those are the UI connector's LED lines, so both UI LEDs light during init.

### The backup registers need the RCC interrupt

`rcc::enable_rtc()` starts the LSE and leaves `RTCEN` to `rcc::isr()`, which runs when the
crystal is ready. A binary that binds no `RCC` interrupt never gets there: every boot finds
`RTCEN` clear, concludes the RTC clock is off and issues `BDRST`, wiping the backup domain
— silently, with `boot_total` stuck at 1 and `boot_reason` always the `Software` fallback.
This binary binds `RCC` and `TAMP_STAMP` for that reason. Both the boot statistics and the
panic flag live in those registers, so the panic handling above depends on it.

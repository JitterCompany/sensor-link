# ADR-0002: Quectel driver: detect the module, describe the host

## Status
**Current Status**: Proposed

**Date**: 2026-11-27

## Context
The SL modem interface spec (sensor-link-hardware, `docs/interfaces/sl-modem.md`) defines a
modem slot. Any card that meets the spec can sit in a host's slot. Today there are EC21 and EC25
cards (profile A). A new card has an EG915N module (profile B). Other boards have a module soldered
on, with no slot.

The driver picks the module at compile time with `ModemVariant`:

- `MiniPcie` expects an EC2x: model prefix `ec2`, 3 Mbaud, stored with `AT&W`. An EG915N card in
  the slot fails the model check after `AT+IPR`.
- `Eg915` drives PWRKEY and RESET and reads STATUS. It fits a board with a soldered module. On a
  slot it breaks the spec: the host must never drive PWRKEY, and a `~PERST` pulse turns the
  card's module off.
- `ModemVariant` mixes two things: how the board powers the module (pins, sequence), and what the
  module is (baud rate, timing).

The modules differ in:

| | EC21, EC25 | EG915N |
|---|---|---|
| Baud rate at boot | 115200 | 115200 |
| Baud rate in use | 3000000, stored with `AT&W` | 115200. 1 Mbaud, the module's maximum, failed on a test host. 921600 not tested. |
| Wait after `POWERED DOWN` | none required by the AT commands manual (v1.3 and v2.2): cut after STATUS low or the URC. The driver waits 1 s. Measured: about 2.5 s of activity after the URC. | 3 s after STATUS low and the URC (AT commands manual v1.5) |
| Power-on to `RDY` | 10.7 s (EC21-E, measured) | not measured |

Quectel's AT commands manual for the EG91xN series (v1.5, `AT+QPOWD`) says: after `AT+QPOWD`,
send no other AT command. The module outputs `POWERED DOWN` and sets STATUS low. Wait 3 s after
both before cutting the supply. Cut it anyway if `POWERED DOWN` has not arrived within 65 s.

On an EC21-E card, current stayed raised for about 2.5 s after the URC, in four runs. The card
then restarted by itself, about 9 s after the URC, because it holds PWRKEY low. `AT+QPOWD=1` was
refused with `ERROR` until about 9 s after `RDY`, so the driver retries it. The hardware design guides warn about
that: PWRKEY must be high after `AT+QPOWD`, or the module turns on again.

### Terms
- **Card, host, profile, extension:** as in the SL modem spec. A profile is the electrical contract
  (A, B, ...).
- **Module:** the Quectel part, on a card or on a board. The driver reads it with `AT+CGMM`.
- **Module parameters:** what the driver needs to know per module: baud rates, timings,
  capabilities. They belong to the module, not to the card.

## Decision
Split `ModemVariant` in two:

- **`ModemPower`** (board side): the pins and their sequence. `is_powered`, `power_on`,
  `power_off`. No module facts.
- **`Module`** (an enum: `Ec21`, `Ec25`, `Eg915n`, `Other`) with **`ModuleParams`**, a const table
  inside the library. Applications do not set module parameters.

Two `ModemPower` types:

- **`SlModemSlot<EN, PERST>`**: a host with an SL modem slot, `VIO` at 3.3 V, so profiles A and B.
  It implements the spec core: it releases `~PERST` together with `VCC_3V3`, never pulses it, and
  keeps `VCC_3V3` off for at least 2 s.
- **`DiscreteEg915<EN, PWRKEY, RESET, STATUS>`**: the current `Eg915`, for a board with the module
  soldered on.

Each power type sets which modules it accepts. `SlModemSlot` accepts every module the library
supports. `DiscreteEg915` accepts its own module only. The accepted set is a field, so a later
`.modules(&[..])` to narrow it (for example for type approval) only adds a method. It is not
implemented until a product needs it.

On every power-on the driver:

1. Probes `AT` at the last baud rate that worked, then at each probe rate of the accepted
   modules, 115200 first.
2. Reads `AT+CGMM` and maps it to a `Module`.
   - Not in the accepted set: `Error::WrongModule`.
   - `Other` on a slot: warn, and run fallback parameters: 115200, no `AT+IPR`, the longest
     waits of all known modules. The AT commands the driver uses are common to Quectel LTE
     modules, so the device most likely comes online. It can then report the model and receive
     the update that adds the module. Refusing would leave it offline with no update path.
3. Applies the module parameters: `AT+IPR` and `AT&W` where the module uses them.

Until the module is known, the driver uses the longest boot timeout and the longest wait after
`POWERED DOWN` of the accepted modules. `Quectel::module()` returns the detected module for logs
and telemetry.

To power off, the driver:

1. Sends `AT+QPOWD=1`. On `ERROR` or no reply it sends it again, once per second, and sends no
   other AT command in between.
2. Waits for `POWERED DOWN`, then 3 s more, then cuts the supply.
3. Cuts the supply anyway 65 s after the first `AT+QPOWD=1`, with or without `OK` or the URC.
   The 65 s is Quectel's limit for the EG91xN; the EC2x manual gives 60 s for leaving the
   network.

The host describes itself through `SlModemSlot`, in the spec's terms. Extensions are added later as
builder methods that take the pin, e.g. `.pps(pin)`. A host with `VIO` other than 3.3 V must read
`CARD_ID` before power-on. That constructor comes with the `CARD_ID` reader, not before.

`Quectel::new` keeps its signature. `MiniPcie` becomes a deprecated alias of `SlModemSlot` and
`Eg915` of `DiscreteEg915`, so existing applications build unchanged.

## Consequences
- A 3.3 V slot host runs the EG915N card after a sensor-link update, with no code change.
- The wait after `POWERED DOWN` is 3 s for every module, so EC2x hosts also wait 2 s longer. The
  extra cost is about 0.3 J per power cycle, small next to a boot and attach.
- Power-on on a slot host can take a few extra AT probes, until a baud rate answers.
- A device with a new card must never downgrade to firmware that does not support that card's
  module. It would lose its modem, and with it the update path. The CHANGELOG entry that adds a
  module says so.
- The factory tester can share `Module` and the `AT+CGMM` mapping with the driver.
- To verify on an EG915N: 921600 baud, `AT+QPOWD` under `AT+CFUN=4`, the `AT+QENG` reply format,
  the time from power-on to `RDY`.

## Alternatives Considered
- **Module as a type parameter.** Rejected. A board whose module differs per hardware revision
  needs two driver instances. The flash saved is small, because the AT, MQTT and TLS code is
  shared.
- **A list of modules in `Config`.** Rejected. It mixes network and hardware settings, and which
  modules work follows from the host, not from a list.
- **One firmware build per card.** Rejected. Swapping a card in the field then needs a different
  firmware.
- **Reuse `Eg915` for the slot.** Rejected. It drives PWRKEY and a reset pin, which the slot spec
  forbids.

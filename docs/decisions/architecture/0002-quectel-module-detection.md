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
| Wait after `POWERED DOWN` | 1 s | 3 s |
| Power-on to `RDY` | about 11 s | not measured |

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

The host describes itself through `SlModemSlot`, in the spec's terms. Extensions are added later as
builder methods that take the pin, e.g. `.pps(pin)`. A host with `VIO` other than 3.3 V must read
`CARD_ID` before power-on. That constructor comes with the `CARD_ID` reader, not before.

`Quectel::new` keeps its signature. `MiniPcie` becomes a deprecated alias of `SlModemSlot` and
`Eg915` of `DiscreteEg915`, so existing applications build unchanged.

## Consequences
- A 3.3 V slot host runs the EG915N card after a sensor-link update, with no code change.
- A board with a soldered EG915N waits 3 s instead of 1 s after `POWERED DOWN`.
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

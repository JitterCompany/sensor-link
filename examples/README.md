# Demo application

An end-to-end application on the sensor-link building blocks: device firmware,
its protocol and a server. It runs on the SL23-modem-tester board, the same
hardware as [factory-test/modem-test](../factory-test/modem-test/). Copy it as a
starting point for a new product.

```
 SL23-modem-tester                         server
 ┌──────────────────────┐   MQTT (mTLS)   ┌──────────────┐   ┌─────────────┐
 │ demo-firmware        │ ──────────────► │ Mosquitto    │ ─►│ demo-server │──► TimescaleDB
 │  └ demo-firmware-core│ ◄────────────── │              │ ◄─│             │      ▲
 └──────────────────────┘  settings, cmds └──────────────┘   └─────────────┘      │
   or demo-mock-device                                           db-admin ─────────┘
```

## What it does

- **Measures** four channels at 4 Hz: board temperature, and the charger's VBUS, IBUS and VSYS. Each is filtered and decimated to the configured data rate.
- **Syncs** on an interval (default 15 min). It also syncs early on an urgent event, a channel status change or a button press. Between syncs the modem is off and data queues in SPI flash.
- **Settings** come from the server: sync interval (0 = always online, or 3 min ... 4 h) and data rate. The device confirms them by echoing their hash.
- **Commands**: start, stop, reboot and blink, from db-admin or the RTT console.
- **Robustness**: watchdog fed through the orchestrator, network health checks, error recovery and boot-loop detection.
- **Updates**: signed images over the air, checked by the bootloader.

### LEDs

| Pair | LED | Meaning |
|---|---|---|
| Modem (by the mPCIe slot) | green dark | modem off |
| | green 1 Hz | powering up or connecting |
| | green steady | connected to the broker |
| | green fast, 10 s | `blink` command |
| Status (v1 only) | Busy short flash | measuring |
| | Error blinking | error recovery or error state, fast during a firmware update |

At power-up every LED lights for 0.4 s as a lamp test.

### Button

- Held at boot: resets the settings to their defaults.
- Pressed later: syncs right away and sends a `button_pressed` event.

## Crates

| Crate | Contents |
|---|---|
| [demo-protocol](demo-protocol/) | Topics, settings, status, events and channels on top of `sensor-link-protocol` |
| [demo-firmware-core](demo-firmware-core/) | Hardware-independent firmware logic: orchestrator, processing, storage, client. Host-tested |
| [demo-firmware](demo-firmware/) | Embedded workspace: BSP, RTIC app and bootloader for the board |
| [demo-firmware-signer](demo-firmware-signer/) | Signs firmware images |
| [demo-mock-device](demo-mock-device/) | Simulated devices on the host, running the real core |
| [demo-server](demo-server/) | Server, migrations, db-admin, dev and deploy stacks |

The demo uses path dependencies on the libraries, unlike modem-test, which pins
a revision. It exists to show the current building blocks, so CI catches any
library change that breaks it.

## Run it without hardware

```sh
# server and its database, see demo-server/README.md
cd examples/demo-server
docker compose -f dev/docker-compose.yml up -d
cp .cargo/config.toml.example .cargo/config.toml
cargo run

# three simulated devices, in another terminal
cargo run -p demo-mock-device -- -n 3 --port 1884

# change settings or send commands
tools/db-admin/db-admin.py   # http://127.0.0.1:8088
```

## Run it on the board

```sh
cd examples/demo-firmware
scripts/gen-dev-keys.sh [broker-ca.pem]   # signing key and broker CA, gitignored
DEMO_MQTT_HOST=<broker> scripts/release.sh 0.1.0
```

`dist/` then holds the bootloader, the signed application and `provision.toml`
for [sensor-link-provision](../sensor-link-provision/), which flashes both and
writes the device certificate. Copy the `.bin.sig.b64` into the server's
firmware directory to offer it as an update.

For a quick standalone image without the bootloader:

```sh
cargo build --release --features force-standalone
probe-rs run --chip STM32L4R7ZITx target/thumbv7em-none-eabihf/release/demo
```

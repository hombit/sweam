# sweam

Bridge an **original Steam Controller** to a **Nintendo Switch 1** using a Linux SBC in the middle: read the controller, present an emulated **Pro Controller** to the Switch over the Linux USB gadget API.

**Roadmap: read `PLAN.md` at the start of every session** — it is the living development + hardware-testing plan; check items off and amend it as work progresses.

## Hard constraints

- Rust only.
- First targets: Steam Controller via its **USB dongle** (28de:1142; Bluetooth later), Switch 1 via **USB** (Bluetooth and Switch 2 out of scope for now).
- Later goals: rumble forwarding to the Steam Controller haptics, gyro passthrough.

## Hardware

SSH addresses and bench wiring: see `TESTBED.md`.

| Role | Device | Notes |
|---|---|---|
| Target SBC | Radxa Zero 3E (RK3566) | UDC `fcc00000.usb`; USB-2.0 Type-C (power) port becomes peripheral via rsetup OTG overlay — see PLAN.md phase 0. Gigabit Ethernet (use it for SSH; frees the OTG port); no onboard WiFi/BT |
| Debug USB host | Raspberry Pi 3 | Stand-in for the Switch; kernel `hid-nintendo` speaks the same protocol and is the main pre-Switch validation tool |
| Final host | Nintendo Switch 1 | via USB (dock port or USB-C) |
| Dev machine | macOS | code compiles here for checks only; the binary is Linux-only |

## Architecture

```
src/main.rs            gadget/protocol wire-up + signal-safe shutdown
src/cli.rs             CLI: steam | manual | steamcheck | hostcheck; detect
                       everything, override anything (--udc --configfs
                       --skip-modprobe --config --evdev)
src/state.rs           ControllerState/Button — shared intermediate representation
src/manual.rs          `sweam manual`: Pro Controller inputs typed on stdin (testing)
src/steamcheck.rs      `sweam steamcheck`: print parsed Steam Controller inputs
src/hostcheck.rs       `sweam hostcheck`: run on the USB host — handshake over
                       hidraw + decode the 0x30 stream (hid-nintendo stand-in)
src/vdf.rs             minimal Valve KeyValues (VDF) parser
src/steam/             input side: InputSource trait; evdev via hid-steam first,
                       raw hidraw later (haptics, gyro); mapping.rs layout +
                       config.rs Steam-style VDF configs
src/switch/gadget.rs   configfs USB gadget (057e:2009), teardown on Drop,
                       stale-gadget cleanup at startup
src/switch/report.rs   real Pro Controller HID descriptor + 0x30 report packing
src/switch/protocol.rs handshake/subcommand state machine (0x80 → 0x21 → stream 0x30)
configs/               example mapping configs (default, face-labels, swapped-sticks)
```

Linux-only deps (`evdev`) are cfg-gated in `Cargo.toml` so host `cargo check` keeps working on macOS.

## Build & check

```sh
cargo check && cargo clippy          # on the dev machine (macOS)
cargo check --target aarch64-unknown-linux-gnu   # checks the Linux-only code paths
```

Deploy: build on the device (`cargo build --release` on the Radxa/Pi), or cross-compile from macOS with `cargo zigbuild --target aarch64-unknown-linux-gnu` (or `cross`). The binary needs **root** on the target (configfs writes + modprobe).

## Protocol references

- dekuNukem/Nintendo_Switch_Reverse_Engineering — canonical: report formats (`bluetooth_hid_notes.md`), USB specifics (`USB-HID-Notes.md`), SPI calibration layout (`spi_flash_notes.md`).
- mzyy94's Pro Controller USB gadget gist (descriptor + `simulate_procon.py` handshake/SPI responses): https://gist.github.com/mzyy94/60ae253a45e2759451789a117c59acf9
- Linux `drivers/hid/hid-nintendo.c` — what a Linux host demands; our phase-2 test oracle.
- Brikwerk/nxbt, mart1nro/joycontrol — Bluetooth emulators, useful for protocol code only (a clone of nxbt sits gitignored in `nxbt/`).
- Steam Controller side: kernel `hid-steam` driver source; `steam-devices` package for udev rules.

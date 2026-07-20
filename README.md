# sweam

Play a Nintendo Switch with an original **Steam Controller**: a small Linux
board (SBC) in the middle reads the controller and presents itself to the
Switch as a **Pro Controller** over USB.

```
Steam Controller ~~radio~~ USB dongle ─┐
                                       │  SBC running sweam
                                       └─ USB OTG port ──> Switch (or any USB host)
```

**Status**: the emulated Pro Controller enumerates, completes the Nintendo
handshake, and streams inputs correctly against a Linux USB host. Testing on
a real Switch is the next step ([PLAN.md](PLAN.md) phase 3). Rumble and gyro
forwarding are planned (phases 5–6).

## What you need

- A Linux SBC with a **USB device/OTG port** (developed on a Radxa Zero 3E;
  any board with a UDC in `/sys/class/udc` should do — Raspberry Pi Zero/4/5,
  etc.).
- Kernel with the **configfs USB gadget API** (`libcomposite`, `usb_f_hid` —
  modules or builtin) for the Switch side, and the **`hid-steam`** driver
  (in mainline since 4.18; some vendor kernels omit it — see
  Troubleshooting) for the controller side.
- An original **Steam Controller** with its USB dongle, plugged into the
  SBC's host port.
- A USB cable from the SBC's OTG port to the Switch (dock USB or USB-C).
- Root on the SBC (configfs writes and module loading).

## Build

Rust 1.85+ (edition 2024); on the SBC:

```sh
cargo build --release        # binary at target/release/sweam
```

or cross-compile from any machine (easiest with [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)):

```sh
rustup target add aarch64-unknown-linux-gnu
cargo zigbuild --release --target aarch64-unknown-linux-gnu
scp target/aarch64-unknown-linux-gnu/release/sweam user@sbc:
```

## Setup: put the OTG port in peripheral mode

The SBC's OTG port must expose a UDC (USB device controller):
`ls /sys/class/udc` should list one. If it doesn't, the port is in host mode:

- **Radxa boards** (Radxa OS): `sudo rsetup` → Overlays → enable the
  "USB OTG / peripheral mode" overlay, reboot. Disable `adbd` if it occupies
  the port (`sudo systemctl disable --now adbd`).
- **Raspberry Pi** (Raspberry Pi OS): add `dtoverlay=dwc2,dr_mode=peripheral`
  to `config.txt`, reboot.
- **Generic**: set the controller's `dr_mode` to `peripheral`/`otg` in the
  device tree.

Mind the power: on many boards (Radxa Zero 3E included) the OTG port is also
the power input, and the board runs bus-powered from the host while bridging.
If that's not enough, power the board another way (e.g. GPIO 5 V pins).

## Use it

```sh
sudo sweam steam                  # the bridge: Steam Controller → Switch
sudo sweam steam --config configs/swapped-sticks.vdf   # custom mapping
sweam help                        # full CLI reference
```

Turn the controller on (Steam button) whenever — sweam picks it up via the
dongle within a second, and survives it sleeping and coming back. Ctrl-C
exits cleanly and removes the USB gadget.

### Run at boot (headless, e.g. at the Switch)

```sh
sudo sweam install --config configs/default.vdf   # /opt/sweam + systemd unit
sudo sweam uninstall                              # stop and remove it all
```

`install` copies the binary (and the config) to `/opt/sweam` (`--prefix DIR`
to change), writes a `sweam.service` systemd unit running `sweam steam` at
boot with automatic restarts, and starts it. Re-run `install` to upgrade.
Logs: `sudo journalctl -u sweam -f`. On a Switch, also enable **System
Settings → Controllers and Sensors → Pro Controller Wired Communication** —
off by default, and without it the Switch ignores USB controllers.

### Test without a Switch, or without a controller

- `sudo sweam manual` — the same gadget, but you type the inputs:
  `press a`, `release a`, `stick l 0.5 -1`, `neutral`.
- `sweam steamcheck` — no gadget: prints every parsed controller input
  (with the active mapping applied) so you can check buttons and sticks.
  Needs root *or* membership in the `input` group (it only reads evdev).
- `sudo sweam hostcheck [/dev/hidrawN]` — run this **on a second Linux
  machine** connected to the SBC's OTG port: it performs the same USB
  handshake a Switch does and prints every decoded input it receives.
  `sweam manual` on the SBC + `sweam hostcheck` on the second machine
  verifies the whole pipe end to end.

### Environment-specific options

Everything is auto-detected where possible — the UDC (from
`/sys/class/udc`), the configfs mount (from `/proc/mounts`), the controller
(by vendor/name/capabilities), hostcheck's hidraw node (by USB IDs) — and
every detected value can be overridden:

| Flag | When you need it |
|---|---|
| `--udc NAME` | several UDCs in `/sys/class/udc` (default: first, with a warning) |
| `--configfs PATH` | configfs the detection can't see (default: per `/proc/mounts`) |
| `--skip-modprobe` | `libcomposite`/`usb_f_hid` are builtin, or no `modprobe` binary |
| `--evdev PATH` | force a specific `/dev/input/eventN` as the controller |
| `hostcheck DEVICE` | force a specific `/dev/hidrawN` on the host side |

## Custom button mappings

Mappings live in VDF files — the same Valve KeyValues format Steam stores
controller configurations in, with a simplified schema. Start from an
example in [`configs/`](configs/):

- `default.vdf` — the built-in layout: ABXY matched by *position* (Steam's
  bottom button acts as Switch B), left pad clicks = d-pad, joystick = left
  stick, right pad = right stick, full trigger pulls = ZL/ZR, grips =
  Capture/Home.
- `face-labels.vdf` — ABXY matched by printed label instead.
- `swapped-sticks.vdf` — joystick and right pad swap their Switch sticks.
- `touch-dpad.vdf` — the left pad's *touch* position drives the d-pad
  (no click needed), 8-way with diagonals and a center deadzone.
- `camera-rightpad.vdf` — the right pad becomes a mouse-like camera:
  finger *motion* deflects the right stick (velocity-based, recenters when
  the finger stops or lifts), with a tunable `sensitivity`.

Copy one, edit the `switch_button …` values (names: `A B X Y DPAD_UP
DPAD_DOWN DPAD_LEFT DPAD_RIGHT L R ZL ZR MINUS PLUS HOME CAPTURE LSTICK
RSTICK`, or `none` to unbind), and pass it with `--config`. Schema
reference: [`src/steam/config.rs`](src/steam/config.rs). Check the result
with `sweam steamcheck --config yourfile.vdf` before playing.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `No UDC found in /sys/class/udc` | OTG port is in host mode — see Setup above |
| `modprobe libcomposite failed` | builtin modules or missing modprobe: `--skip-modprobe` |
| `No Steam Controller input device found` | controller off, dongle unplugged, or the kernel lacks `hid-steam` (`modinfo hid_steam`); vendor kernels may need it built out-of-tree — grab `drivers/hid/hid-steam.c` + `hid-ids.h` matching your kernel version, `make -C /lib/modules/$(uname -r)/build M=$PWD modules` |
| Host sees the gadget but no inputs | the host must speak the Nintendo protocol: a Switch, `hid-nintendo` (Linux), or `sweam hostcheck` |
| `Removing stale gadget from a previous run…` | normal after a hard kill; sweam cleans up and continues |

## Development

`cargo test && cargo clippy` runs anywhere (Linux-only bits are cfg-gated).
Pre-commit hooks (fmt, clippy for both targets, tests): install
[pre-commit](https://pre-commit.com) (`pipx install pre-commit` or run via
`uvx pre-commit`), then `pre-commit install`.
Roadmap: [PLAN.md](PLAN.md) · protocol notes and hardware test bench:
[TESTBED.md](TESTBED.md) · license: [MIT](LICENSE).

## Acknowledgements

sweam contains no Nintendo or Valve code; the protocol implementation was
built from these community resources:

- [dekuNukem/Nintendo_Switch_Reverse_Engineering](https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering)
  (MIT) — the canonical Switch controller protocol documentation: report
  formats, USB handshake, SPI flash layout.
- [Brikwerk/nxbt](https://github.com/Brikwerk/nxbt) (MIT) — reference for
  subcommand replies a real Switch accepts, notably the NFC/IR MCU state
  report format.
- [mzyy94](https://mzyy94.com/blog/2020/03/20/nintendo-switch-pro-controller-usb-gadget/)'s
  Pro Controller USB gadget research — descriptor values and factory
  calibration data for the emulated SPI flash.
- The Linux kernel's `hid-steam` (Steam Controller input) and
  `hid-nintendo` (used strictly as a behavioral test oracle: what a
  Pro Controller host expects) drivers.

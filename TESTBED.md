# Test bench

Hardware test setup for sweam (roles in CLAUDE.md, roadmap in PLAN.md).

| Device | SSH | Role |
|---|---|---|
| Radxa Zero 3E | `ssh radxa@192.168.1.44` | Runs sweam. Steam Controller dongle attached; USB OTG (power) port plugged into the RPi3, acting as the gadget/peripheral. |
| Raspberry Pi 3 | `ssh root@192.168.1.43` | Debug USB host (stand-in for the Switch). Runs openSUSE Leap 16 — see the hid-nintendo caveat below. |

Both reachable over LAN; SSH key auth set up from the dev machine.

## One-time setup already done

- **Radxa: passwordless sudo** for user `radxa` via `/etc/sudoers.d/010-radxa-nopasswd`
  (`radxa ALL=(ALL:ALL) NOPASSWD: ALL`).
- **Radxa: `radxa` added to the `input` group** (2026-07-19), so the
  input-only tools (`steamcheck`) run without sudo; `sweam steam` still
  needs root for configfs/modprobe.
- **Radxa: OTG peripheral mode** enabled (PLAN.md phase 0a); UDC is
  `fcc00000.usb` (not the `.dwc3` name PLAN.md predicted).
- No Rust toolchain on either board (1 GB RAM on the Radxa); build on the
  dev machine instead — see below.
- Runtime setup (modprobe `libcomposite`/`usb_f_hid`, configfs gadget, UDC
  bind) is done by sweam itself in `UsbGadget::new`; nothing to pre-configure.

## Build & deploy (from the dev machine)

```sh
cargo zigbuild --release --target aarch64-unknown-linux-gnu
scp target/aarch64-unknown-linux-gnu/release/sweam radxa@192.168.1.44:
scp target/aarch64-unknown-linux-gnu/release/sweam root@192.168.1.43:   # for hostcheck
```

## Manual input test (no Steam Controller involved)

Gadget side (Radxa), interactive: `sudo ./sweam manual`, then type commands
(`press a`, `release a b`, `stick l 0.5 -1`, `neutral`).

For driving it from scripted/remote SSH calls, feed stdin through a FIFO held
open by a dummy writer (each bare `echo > fifo` would otherwise close stdin):

```sh
mkfifo /tmp/sweam.fifo
sleep 3600 > /tmp/sweam.fifo &
sudo ./sweam manual < /tmp/sweam.fifo > /tmp/sweam.log 2>&1 &
echo "press a" > /tmp/sweam.fifo    # inject from any later ssh call
```

Host side (RPi3): `sudo ./sweam hostcheck` — finds the gadget's hidraw node
by USB IDs (pass `/dev/hidrawN` to override), drives the hid-nintendo-style
USB handshake and prints every decoded button/stick change from the 0x30
stream.

Verified 2026-07-12: enumeration as 057e:2009, handshake, and all manual
inputs decoded correctly on the Pi at ~121 reports/s. Also verified:
stale-gadget cleanup at startup and clean teardown on SIGTERM/Ctrl-C.

## Steam Controller input test (no gadget involved)

On the Radxa: `./sweam steamcheck [--config configs/….vdf]` (no sudo needed,
see the `input` group above; exits with a hint if permissions are missing) —
waits for
the controller (turn it on), then prints every parsed input as mapped
Pro Controller state with button/stick names. Mapping configs are Steam-style
VDF files, examples in `configs/` (deployed to `~/configs` on the Radxa).

### hid-steam module (one-time, done 2026-07-12)

The Radxa vendor kernel (6.1.84-10-rk2410-nocsf) ships **no hid-steam**, so
the controller only appeared as its lizard-mode keyboard/mouse. Built it
out-of-tree — sources from stable v6.1.84 (`drivers/hid/hid-steam.c` +
`hid-ids.h`, Makefile with `obj-m := hid-steam.o`) live in `~/hid-steam/` on
the Radxa:

```sh
make -C /lib/modules/$(uname -r)/build M=$PWD modules
sudo cp hid-steam.ko /lib/modules/$(uname -r)/extra/ && sudo depmod -a
echo hid-steam | sudo tee /etc/modules-load.d/hid-steam.conf   # load at boot
```

Rebuild against the new headers after any kernel upgrade. (`linux-headers`,
`gcc`, `make` were already installed.)

## Headless service (for the Switch, no SSH)

`sudo ./sweam install [--config configs/….vdf] [--prefix DIR]` → binary and
config land in /opt/sweam (or DIR), systemd unit `sweam.service` runs
`sweam steam` at boot with `Restart=always`. `sudo ./sweam uninstall` removes
everything. Logs: `sudo journalctl -u sweam -f`. Verified 2026-07-19 on the
Radxa: install, reinstall over the running service, uninstall, and a full
cold-boot end-to-end test — service up 1 s after boot, gadget bound, and
Steam Controller button presses decoded on the Pi by `hostcheck`.

## Caveats

- **No hid-nintendo on the Pi**: openSUSE Leap 16 doesn't package it
  (upstream module reportedly fails to build there:
  <https://build.opensuse.org/package/live_build_log/home:sp1rit/hid-nintendo/16.0/aarch64>),
  and there are no gcc/kernel headers installed to build it. The gadget binds
  to `hid-generic`, which exposes `/dev/hidraw0` — that's what `sweam
  hostcheck` uses. The *independent* kernel-oracle test (PLAN.md phase 2 exit
  criterion) therefore still needs a Raspberry Pi OS image or another host
  with hid-nintendo.
- ~~Killing sweam leaks the gadget~~ fixed 2026-07-12: SIGINT/SIGTERM now
  tear the gadget down, and startup removes a stale `sweam_procon` gadget
  left by a `kill -9`. Only a hard crash *while the kernel wedges configfs*
  should ever need a reboot.
- The Radxa is bus-powered by the Pi through the OTG cable; if it browns out
  under load, power it via GPIO 5V pins instead (PLAN.md phase 0a).

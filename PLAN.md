# sweam development & hardware-testing plan

Living roadmap — check items off as they complete, amend freely. Phases are
ordered so every phase ends with something observable on real hardware.

## Phase 0 — hardware bring-up (no sweam code involved)

### 0a. Radxa Zero 3E: enable and verify USB OTG peripheral mode

The Zero 3E has two USB-C ports: the **USB 2.0 port (also the power input) is
the OTG port**; the USB 3.0 port is host-only. Peripheral mode is officially
supported via an overlay ([Radxa USB gadget docs](https://docs.radxa.com/en/zero/zero3/radxa-os/usbnet)).
Use the Gigabit Ethernet port for SSH access so the OTG port stays free for
gadget work.

- [ ] Flash current Radxa OS (Debian) image; boot; `sudo rsetup` → update, then
      **Overlays → Manage overlays → enable the "USB OTG / peripheral mode" overlay**; reboot.
- [x] Verify a UDC exists: `ls /sys/class/udc` → it is `fcc00000.usb`
      (not the `.dwc3` name originally expected).
      (Fallback check: `cat /sys/firmware/devicetree/base/usb@fcc00000/dr_mode`
      should read `peripheral` or `otg`.)
- [ ] If an Android debug bridge gadget occupies the port (official images may
      run `adbd`), disable it: `systemctl status adbd` / `sudo systemctl disable --now adbd`.
- [ ] Smoke test against the Pi 3 *before writing any code*: on the Radxa
      `sudo modprobe g_ether`, connect OTG port ↔ Pi 3 USB-A; on the Pi:
      `lsusb` shows a new device and `ip link` shows `usb0`. Then `sudo rmmod g_ether`.
- [ ] Power note: the OTG port is also the power port. When it is connected to
      the Pi 3, the Radxa is bus-powered by the Pi (~limited current) — if
      unstable, power the Radxa via its GPIO 5V pins (or the optional PoE HAT)
      instead. Same concern applies later with the Switch (which does power
      controllers).

### 0b. A Linux debug USB host (Switch stand-in)

Two options — try the loopback first, it makes the whole dev loop live on one
board; keep the Pi 3 as the fallback and as an occasional second opinion:

**Option A — Radxa self-loopback**: connect the Radxa's OTG port to its own
USB 3.0 host port with a short C-to-C cable. The host stack doesn't care that
the device on the bus is the same board; the gadget enumerates like any USB
device. Caveats:
- [ ] Power: with the OTG/power port occupied, power the board via the GPIO
      5V pins (or PoE HAT). Watch for VBUS backfeed weirdness from its own
      host port; if the board misbehaves, use a data-only setup or the Pi 3.
- [ ] Check the vendor kernel ships the test oracle: `modinfo hid-nintendo`.
      If it's missing, either build the module or fall back to the Pi 3.
- [ ] Downside to keep in mind: gadget bugs and kernel crashes take down the
      debug host too, and a wedged USB stack can't be debugged from itself.

**Option B — Raspberry Pi 3** (isolated host, needed anyway if A disappoints):

Reality check (2026-07-12): the bench Pi runs **openSUSE Leap 16**, which has
no hid-nintendo and no build tooling; `sweam hostcheck` covers input
verification for now (see TESTBED.md). The items below apply if/when it gets
reflashed with Raspberry Pi OS for the kernel-oracle test. (Option A is out:
the Radxa vendor kernel lacks hid-nintendo too.)

- [ ] Raspberry Pi OS (64-bit), current kernel. Install:
      `sudo apt install usbutils usbhid-dump evtest joystick wireshark`
      and `pipx install hid-tools` (for `hid-decode`/`hid-recorder`).
- [ ] Confirm the protocol oracle exists: `modinfo hid-nintendo` (in-tree
      since 5.16; Raspberry Pi OS kernels have it). This driver performs the
      same handshake/subcommand/SPI-calibration dance as the Switch — **a
      gadget that satisfies hid-nintendo very likely satisfies the Switch**.
- [ ] Learn to capture: `sudo modprobe usbmon` + Wireshark on `usbmonX` to
      watch enumeration and report traffic from our gadget.

### 0c. Steam Controller on the Radxa

- [x] hid-steam (2026-07-12): the vendor kernel ships none — built it
      out-of-tree from stable v6.1.84 sources and set it to load at boot;
      see TESTBED.md. Dongle binds, lizard mode suppressed. (steam-devices
      udev rules unneeded so far: sweam runs as root.)
- [x] Turn on the controller (2026-07-19): dmesg shows it connect, and
      `sweam steamcheck` (better than raw `evtest`: applies our mapping)
      shows buttons/sticks/pads. Also verified end-to-end: button presses
      decoded on the Pi through the full bridge after a cold boot.

## Phase 1 — gadget enumerates as a Pro Controller

- [ ] Run `sweam` on the Radxa (gadget setup exists in `src/switch/gadget.rs`).
- [ ] On the debug host: `lsusb -d 057e:2009 -v` matches a real Pro Controller
      (VID/PID, strings, single HID interface, 64-byte interrupt endpoints);
      `usbhid-dump` shows our 203-byte report descriptor.
- [ ] dmesg shows `hid-nintendo` binding (it will then time out on the
      handshake — expected until phase 2).

## Phase 2 — protocol state machine (the core of the project)

Implement `src/switch/protocol.rs` (see its doc comments and TODOs):

- [x] `0x80` USB commands: status (MAC + controller type), handshake ack,
      baud-rate ack, "USB HID only" → begin reporting.
- [x] `0x01` subcommand replies (`0x21` acks): device info (0x02), input
      report mode (0x03), shipment state (0x08), player lights (0x30),
      IMU enable (0x40), vibration enable (0x48).
- [x] SPI flash read (0x10) served from a baked calibration image
      (stick + IMU factory calibration, body colors; values lifted from
      mzyy94's Switch-proven `simulate_procon.py`, layout per dekuNukem
      `spi_flash_notes.md` and the `JC_CAL_*` addresses in hid-nintendo.c).
- [x] 0x30 report pump (8 ms interval, in `main.rs`) once streaming; parse
      and ignore `0x10` rumble reports for now.
- [x] Unit tests for report packing and the state machine, including a replay
      of hid-nintendo's exact `joycon_init()` sequence (`cargo test`).
- [x] Userspace verification (2026-07-12, see TESTBED.md): the bench Pi 3
      runs openSUSE with **no hid-nintendo** (not packaged, no headers to
      build it), so added two subcommands instead: `sweam manual` (type
      Pro Controller inputs on stdin, `src/manual.rs`) and `sweam hostcheck`
      (run on the USB host: drives the joycon_init() handshake over the
      hid-generic hidraw node and decodes the 0x30 stream, `src/hostcheck.rs`).
      Enumeration (057e:2009), handshake, and every manual button/stick input
      decoded correctly on the Pi at ~121 reports/s.
- [ ] **Exit criterion (needs hardware):** on a debug host with the real
      `hid-nintendo` (e.g. Raspberry Pi OS — the bench openSUSE lacks it),
      the driver completes setup and creates a working input device:
      `evtest`/`jstest` shows our synthetic button presses and stick
      movements. Debug failures with usbmon capture vs. a capture/trace of a
      real Pro Controller (dekuNukem repo has traces).
- [x] Robustness (2026-07-12): gadget teardown on SIGINT/SIGTERM and
      stale-gadget cleanup at startup — both verified on the bench (killed
      sweam, restarted over the leaked gadget; SIGTERM leaves configfs
      empty). Also fixed teardown's UDC unbind (a zero-byte write never
      reached the kernel's store callback).

## Phase 3 — real Switch over USB

- [x] Headless operation (2026-07-19): `sweam install [--config …] [--prefix …]`
      copies the binary (+ config) to /opt/sweam and enables a systemd service
      running `sweam steam` at boot (`Restart=always`; SIGTERM teardown);
      `sweam uninstall` reverses it. Verified on the Radxa, including
      reinstall over the running service. No SSH needed at the Switch.
- [x] Connect Radxa OTG port to the Switch dock USB (2026-07-19). Gotcha #1:
      **"Pro Controller Wired Communication" must be enabled** on the Switch
      (System Settings → Controllers and Sensors, off by default) — without
      it the Switch enumerates the gadget but only pokes it with 2-byte
      `00 00` reports and never starts the 0x80 handshake.
- [x] Controller appears in "Controllers → Change Grip/Order"; pairing and
      in-game play work (tested with The Entropy Centre).
- [ ] Gotcha #2 — periodic disconnects (~every 30–60 s of play, Switch asks
      to re-pair with L+R). **Fix deployed 2026-07-19 but not yet re-tested
      on the Switch.** Evidence from the journal (first Switch session):
      the Switch retried subcommand 0x21 (set NFC/IR MCU config) every
      32 ms, dozens of times in a row — our reply (short 8-byte ack lifted
      from simulate_procon.py) didn't satisfy it — then killed the port:
      hidg write failed with ENOTCONN ("transport endpoint shutdown"),
      sweam exited, systemd restarted it (Restart counter matched the
      number of disconnects), fresh enumeration → re-pair prompt. Fix:
      reply with the full 34-byte MCU state report (status bytes
      01 00 FF 00 08 00 1B 01, zero padding, trailing crc8 0xC8), ack
      0xA0, format per nxbt (MIT); plain-ack 0x22 (set MCU state) too.
      **To verify:** play ≥ 5 min; journal must show no "Host subcommand
      0x21" bursts and no service restarts (`journalctl -u sweam | grep -c
      Started`). If it still drops: log the 0x21 args (they carry an MCU
      sub-command; we may need state-dependent replies — busy/configured
      states), and check for 0x11 output reports (direct MCU requests) in
      the raw log. Body color also changed to dark blue (the baked SPI
      image's raspberry showed as a red controller).
- [ ] Direct USB-C to the Switch (handheld/tabletop) did **not** work on
      2026-07-19 (dock USB-A works fine). Suspects, in order: C-to-C role/CC
      negotiation (both ports are primarily sinks; the Radxa OTG port may
      not present device-mode CC correctly for the Switch to source VBUS),
      underpower (that port is also the Radxa's power input), or the Switch
      restricting the Pro Controller protocol to docked USB. To investigate:
      power the Radxa via GPIO 5 V + data-only C-to-C, watch
      `journalctl -u sweam -f` for USB state transitions (the watcher now
      logs enumeration/reset/suspend) — that alone separates "never
      enumerated" (power/CC) from "enumerated, no handshake" (Switch
      policy).
- [ ] Scripted input (hardcoded sequence) navigates the Switch UI.

## Phase 4 — Steam Controller end-to-end

- [x] Implement `steam::EvdevSteamController` (enumerate evdev, vendor 0x28de,
      name "…Steam Controller"; non-blocking poll wired into the report pump
      in `main.rs`; runs with neutral inputs when no controller is present).
- [x] Initial mapping (`steam/mapping.rs`, pure + unit-tested, event
      vocabulary from hid-steam.c): positional ABXY swap, left-pad click
      quadrants → d-pad, joystick → left stick, right pad → right stick
      (re-centered on touch release), full trigger pulls → ZL/ZR, grips →
      Capture/Home (both BTN_GRIPL/R and pre-6.11 BTN_GEAR_DOWN/UP codes).
- [x] Mapping configuration (2026-07-12): layouts are configurable via
      Steam-style VDF files (`--config`, parser in `src/vdf.rs`, schema in
      `src/steam/config.rs`, examples in `configs/`). `sweam steamcheck`
      prints parsed controller inputs for mapping work.
- [ ] Hardware tuning: pad/stick feel and deadzones, grip mapping, and
      analog triggers (currently unmapped — needs new modes in the config
      schema, e.g. Steam's `joystick_move` on left_trackpad). First Switch
      session feedback (2026-07-19): right-pad camera feel needs work for
      first-person games (sensitivity/curve options, maybe a trackball-style
      mode instead of recenter-on-touch).
- [ ] Left-pad **touch position** (ABS_HAT0X/Y) as d-pad instead of click
      quadrants — requested after the first Switch session.
- [ ] Investigate importing real Steam controller configs (the client's
      exported VDF layouts) for the subset that binds to gamepad outputs.
- [x] Controller hotplug in bridge mode (2026-07-19): `sweam steam` now
      retries once a second, survives disconnects (resets to neutral), and
      no longer needs the controller at startup.
- [x] Play something on the Switch (2026-07-19, The Entropy Centre) —
      latency subjectively fine.

## Phase 5 — rumble forwarding

- [ ] Parse HD rumble data from `0x10`/`0x01` output reports (frequency +
      amplitude encoding in dekuNukem notes).
- [ ] Drive Steam Controller haptic actuators via raw hidraw feature reports
      (this is why phase 5 likely also migrates input from evdev to raw
      hidraw — hid-steam may claim the device; use `hidraw` + lizard-mode
      disable, see the old `hid_main.rs` experiment in git history for the
      dongle feature-report shape).

## Phase 6 — gyro passthrough

- [ ] Read Steam Controller IMU (raw hidraw input reports).
- [ ] Fill the 3×12-byte IMU sample fields of the 0x30 report (bytes 13..49),
      with axis remapping and scale per dekuNukem `imu_sensor_notes.md`.

## Appendix — Bluetooth: assessed, deferred

Verdict: **USB first is the right call.** Two independent reasons:

1. **The Radxa Zero 3E has no onboard Bluetooth at all.** BT would require a
   USB dongle on the host port, adding an adapter-compatibility variable on
   top of everything else.
2. **Switch BT controller emulation is intrinsically fragile**: joycontrol/
   nxbt need BlueZ run with the input plugin disabled (`-P input`), BT
   address/name spoofing, and break across BlueZ versions; USB gadget
   behavior is deterministic and debuggable with usbmon.

If/when BT becomes interesting, evaluate in this order:
- [ ] Prototype BT emulation on the **Pi 3** (built-in BT, and it is nxbt's
      reference platform) — isolates protocol work from dongle/driver
      variables entirely.
- [ ] Pick a BlueZ-friendly USB BT dongle for the Radxa and check its health:
      `rfkill list`, `bluetoothctl show`, stability under sustained HID
      traffic.
- [ ] Only then port: the protocol layer (`switch/protocol.rs`) is transport-
      agnostic by design; a BT transport would replace `switch/gadget.rs`.

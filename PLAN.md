# sweam development & hardware-testing plan

Living roadmap — check items off as they complete, amend freely. Phases are
ordered so every phase ends with something observable on real hardware.

## ⚠ Current focus (2026-08-03): the pump is exonerated; gyro back on

**Read this before touching the Switch side.** Three sessions in a row, the
answer came from one measurement and none of the theorising. Measure first —
the tooling below exists precisely so you can.

### What the hardware says (2026-08-03, live Switch session)

- **No disconnects, at all.** `journalctl -u sweam --since today | grep -cE
  "Host disconnected|os error 108"` → **0**, across 24 boots and a played
  session, confirmed from the couch too. Whatever July's drops were, they do
  not reproduce on `cadf5ab` (full-speed enumeration + reference-matched
  descriptors). Treat the disconnect hunt as closed until it recurs, and if
  it does, get the trace before touching code.
- **The 16 ms report period belongs to the host; the pump is not involved.**
  Decomposing the gadget trace into "host consumed a report → we armed the
  next" and "we armed it → the host came back":

  | | median | min | max |
  |---|---|---|---|
  | our loop, arming the next report | **0.09 ms** | 0.07 | 0.38 |
  | waiting for the host's next poll | **15.84 ms** | 15.57 | 16.07 |

  A report is sitting on the endpoint 0.09 ms after every single poll. There
  is nothing left in userspace to speed up.
- **Why every pacing change so far was a no-op** — the thing to remember:
  f_hid's `write()` blocks until the *previous* request completes and queues
  the new one at that instant, so a `sleep()` before the write overlaps the
  wait rather than adding to it. Tested directly, not reasoned: a build with
  the pre-write sleep deleted measured **16.00 ms, exactly as with it** (and
  armed 0.23 ms after each poll instead of 0.09, i.e. marginally worse). It
  was reverted; only the comment in `main.rs` changed. This also retires the
  "sleep-then-block doubles the period" story that `8316911` was built on.
- **The board's own reboots are bench wiring, not a bug.** 24 boots today,
  9–27 minutes apart, none with a clean-shutdown marker. The board is
  bus-powered from the dock and a *sleeping* Switch cycles VBUS. The tell:
  of those 24 boots only **3** ever reached `USB state: configured` — the
  rest came up, found no host enumerating, and died again. From the couch
  this is indistinguishable from a disconnect, and it poisoned earlier
  sessions' data. Fix: power the Radxa from its GPIO 5 V pins and leave the
  OTG port for data only.
- **Re-enumeration costs a registration gesture.** Restarting the service
  (or swapping binaries) drops the Switch to "Press L+R", and L+R alone is
  not enough — it needs **A a couple of seconds later** to leave Change
  Grip/Order. Verified end-to-end on 2026-08-03 with `sweam manual`: L+R
  drew `0x48/0x21/0x30/0x22` out of the Switch, and after A the left stick
  moved the home-menu cursor.

### State at the end of 2026-08-03

Deployed on the Radxa: still `cadf5ab` — today's only source change is a
comment, so /opt/sweam was left alone. Gyro still parked (`enabled "0"`),
tracing armed, desktop disabled (`multi-user.target` — GNOME was
crash-looping a session every ~35 s on a 1 GB board).

Score for the session, honestly: one code change attempted and reverted for
doing nothing, which is the correct outcome for a theory that measurement
killed. What it bought was three facts that stop future work going in
circles — the host's 16 ms is the host's, f_hid writes overlap sleeps, and
the board's reboots are the sleeping Switch. Everything comparable in the
descriptors still matches the SN30 reference; the only known divergence left
is `bInterval` (reference 8, we publish 10 at full speed).

### Reference descriptors (2026-08-02) — what the Switch accepts

An 8BitDo SN30 Pro in Switch mode enumerates as **057e:2009**, i.e. it
presents itself as a Pro Controller and the Switch accepts it. Plugged into
the Radxa's host port, `lsusb -v -d 057e:2009` gives the reference we never
had. Differences from what we present:

| field | reference | ours | |
|---|---|---|---|
| speed | full-speed | full-speed | ✓ since `cadf5ab` (configfs `max_speed`, written before the UDC bind) |
| `bcdUSB` | 2.00 | 2.00 | ✓ fell out of full speed — LPM is a high-speed feature, so the composite driver stopped bumping us to 2.01 |
| `bInterval` (EP1) | 8 | **10** | **the last divergence.** f_hid hardcodes it per speed with a FIXME; measured consequence is the Switch polling us every 16.00 ms instead of 8 |
| `bcdHID` | 1.11 | **1.01** | f_hid hardcodes 1.01; no observed consequence |
| `bmAttributes` | 0xa0 (bus powered, remote wakeup) | 0xa0 | matched again — briefly 0x80, see below |
| `MaxPower` | 500 mA | 500 mA | ✓ |
| HID `wDescriptorLength` | 203 | 203 | ✓ our report descriptor length is right |
| endpoints | EP1 IN + EP2 OUT, 64 B interrupt | same | ✓ |
| IDs/strings | 057e:2009, Nintendo Co., Ltd. / Pro Controller / 000000000001 | identical | ✓ |

Everything we can reach from userspace now matches. `bInterval` is the one
field left, and configfs does not expose it — hence the FunctionFS/patched
f_hid item above. The device-tree route once planned for speed and LPM
(`maximum-speed = "full-speed"`, `snps,usb2-lpm-disable`) turned out to be
unnecessary: `max_speed` in configfs did both.

### Next, in order

- [x] **Verify the paced interval on the Switch** (2026-08-03): 16.00 ms,
      and the decomposition above proves it is the host's schedule, not our
      loop. Method, for reuse: clear `/sys/kernel/debug/tracing/trace`, wait
      a few seconds, then pair `ep1in: Transfer In Progress` (host consumed)
      with the following `ep1in: cmd 'Update Transfer'` (we armed the next).
- [ ] **Re-enable gyro** — the one open item with a user-visible payoff.
      `configs/touch-dpad.vdf` and `default.vdf` carry `settings { "enabled"
      "0" }` on the gyro group, parked for a bisect that has since been
      resolved by other means. Flip to `"1"`. Motion is verified on the
      controller side; only Switch-side axis tuning remains.
- [ ] **A/B the right-pad modes** (phase 4 below): the relative stick landed
      2026-08-03 and is staged on the board but has never been touched by a
      human. One session decides which of the two survives.
- [ ] **Power the Radxa from GPIO 5 V**, so a sleeping Switch stops
      power-cycling the board mid-experiment (see above). Bench task, no
      code, but it makes every future measurement trustworthy.
- [ ] **8 ms polling, only if latency demands it.** It can now come from
      exactly one place: the descriptor. f_hid hardcodes the full-speed
      `bInterval` to 10 (with a FIXME in the kernel); the reference SN30
      declares 8. Two routes — **FunctionFS** (`usb_f_fs.ko` is present on
      the board) lets userspace supply the whole descriptor set, endpoints
      included, at the cost of a second backend in `switch/gadget.rs` and
      moving report I/O to the `ep` files; or build a patched `usb_f_hid`
      out-of-tree the way `hid-steam` already was (TESTBED.md). **Have a
      reason first**: no disconnects are outstanding, so the case for 8 ms
      is ~8 ms of average input lag, not stability.

### Retired by the 2026-08-03 measurement

All three were aimed at a pump that arms its next report 0.09 ms after each
poll, so none of them can buy anything. Do them as cleanup if at all, never
as a disconnect theory:

- ~~One `poll()` loop over `/dev/hidg0` instead of two threads.~~ Still a
  defensible simplification — it deletes three mutexes, a lock order and the
  reply queue, and it is the shape a no-std port would need (see the
  appendix) — but the 80 ms starvation that motivated it was fixed by
  `b34de32`, and nothing is starving now. Note if it is ever done:
  `b34de32` made replies *displace* `0x30` reports on the theory that a real
  controller sends one instead of the other. **That theory is unsupported**
  — hid-nintendo reads state from `0x30` and treats `0x21` as a synchronous
  reply — so a single loop should send both.
- ~~Get logging out of the hot path.~~ The reader thread's per-subcommand
  `println!` is not costing us reports; the pump's arming latency maxes at
  0.38 ms.
- ~~`SCHED_FIFO` on the pump thread.~~ No deadline is being missed.

### Do not repeat these

- **Async/tokio**: does not help. The constraint is that the host polls on
  its own schedule and the write completes when it does; no concurrency
  model changes that. It would earn its place only with many concurrent
  transports (Bluetooth, several controllers).
- **Driving the report timer from wall-clock time**: tried in `209a45b`
  because nxbt (`int(delta_t * 4)`, "Joy-Con uses 4.96ms as the timer tick
  rate") and mzyy94 both do it. On hardware it made teardown ~15x faster
  (2 s vs 23–43 s) and was reverted in `561400d`. `Protocol::set_elapsed`
  survives, unused, with the reasoning. Do not re-try without a trace
  explaining why it should be different.
- **Touching the report pump's pacing.** Sleep, deadline, no sleep at all —
  all three measure 16.00 ms, because f_hid's write blocks on the previous
  request and swallows whatever you did before it. Three commits have now
  been spent here. The pump arms its next report 0.09 ms after each poll;
  there is no headroom to reclaim.
- **Confusing a board reboot with a Switch disconnect** — the trap that
  wasted the most time. They look identical from the couch and different in
  the journal: a host teardown says `Host disconnected (os error 108)`,
  while a power cut leaves no shutdown marker at all and starts a fresh
  boot. Separate them *first*, with
  `journalctl -u sweam --since today | grep -c "Host disconnected"` versus
  counting `Started sweam.service`. A sleeping Switch cycles VBUS on the
  dock port and the board is bus-powered from it, so idle time against a
  sleeping console manufactures "disconnects" by the dozen.

### Tooling that now exists for this

- `sweam trace start|snapshot|dump|stop` — dwc3 gadget tracepoints; usbmon
  cannot see this traffic (it captures on a *host* controller and we are the
  peripheral). `sweam install --trace` wires `snapshot` into the unit's
  `ExecStopPost`, so every disconnect dumps the preceding USB events into
  the journal with nobody at the keyboard. See TESTBED.md.
- `Report pump stalled for N ms` — logged when our own stream gaps. Note the
  first version of this (`825b343`) reported false stalls of up to 2112 ms
  because it skipped its clock update while not streaming; fixed in
  `8316911`. Trust the current one.
- `sweam --version` prints the git commit (`build.rs`), and it heads every
  run in the journal — so a log always names the build that wrote it.

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
- [x] Gotcha #2 — periodic disconnects (~every 30–60 s of play, Switch asks
      to re-pair with L+R). **Fix verified on the Switch 2026-07-20: no
      disconnects.** Evidence from the journal (first Switch session):
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
- [x] Right pad in **camera mode in the shipped configs** (2026-08-02):
      configs/default.vdf, face-labels.vdf and touch-dpad.vdf ask for
      `joystick_camera` — a thumb-sized pad reaches much further as a
      velocity camera than as an absolute stick. The code default
      (`Mapping::default`, no `--config`) stays absolute, as does
      swapped-sticks.vdf where the pad drives movement;
      configs/absolute-rightpad.vdf is the opt-out (was camera-rightpad.vdf).
      Deployed to the Radxa the same day; still needs a Switch session to
      confirm the feel.
- [x] Left-pad **touch position** (ABS_HAT0X/Y) as d-pad instead of click
      quadrants (2026-07-19) — `settings { requires_click 0 }` on the
      left_trackpad group, example in configs/touch-dpad.vdf; deployed as
      the active config on the Radxa.
- [x] **Relative right-pad stick ("centre on touch")** — implemented
      2026-08-03 as `RightPadMode::RelativeStick`, selected by
      `joystick_move` + `settings { "center_on_touch" "1" }` with the drag
      gain as `settings { "scale" "N" }` (default 2.0 = full deflection
      after dragging half the pad's radius). Spelled as a setting on
      `joystick_move` rather than a mode name because the evidence that
      Valve calls this "Joystick Camera" is a forum post, not documentation.
      Shipped as `configs/relative-rightpad.vdf`, which is `touch-dpad.vdf`
      with *only* the pad mode changed — a test asserts that, so the A/B
      compares one variable.
      What forced it: the velocity camera cannot sustain a turn. Measured
      2026-08-03 at the pump's real 16 ms tick, its deflection peaks at the
      same 85% of full scale for every swipe speed and is *identical* at
      sensitivity 24 and 72 — it saturates on the first sample, so the
      4 → 12 → 24 treadmill was always going to come back "still too slow".
      The stick deflects only while the finger moves, so one swipe turns you
      exactly as far as that swipe's travel.
- [ ] **A/B the two right-pad modes in one session, then delete the loser**
      — three modes is one too many. Both are staged on the Radxa:
      `sudo ~/sweam install --config ~/configs/relative-rightpad.vdf --trace`
      switches to the new one, the same command with `touch-dpad.vdf` goes
      back. Judge it on sustained turning (can you complete a 180° without
      lifting?) and on aim precision, and tune `scale` before concluding —
      unlike the camera's `sensitivity`, it genuinely moves the output.
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

- [x] Switch/gadget half (2026-07-20, committed, **not yet bench-verified**):
      `ImuSample` ring in `ControllerState`; 0x30 reports carry the 3 IMU
      frames once the host enables the IMU (subcommand 0x40, gated in
      `Protocol`); `sweam manual` gained `gyro x y z` (dps) / `accel x y z`
      (g); `sweam hostcheck` sends IMU-enable and prints decoded motion.
      **Next: bench-verify** — Radxa: stop the service, run `sweam manual`
      (FIFO trick in TESTBED.md); Pi: `sweam hostcheck`; inject
      `gyro 100 0 0` and expect an `imu … gyro=(+100.0,…)` line. Deployed to
      the Radxa 2026-08-02 (the 07-20 attempt died mid-scp when the board
      went offline — same power symptom as the 08-02 outage), so this now
      only needs a USB host: the bench Pi (currently off) or a Switch.
- [x] Steam Controller half — **implemented and hardware-verified
      2026-08-02**: `src/steam/packet.rs` decodes the 64-byte packets into
      the same evdev vocabulary `mapping.rs` already speaks (so every VDF
      config works unchanged) plus an `ImuSample`; `src/steam/hidraw.rs`
      holds every dongle slot open, sends the 0x87 settings report and pumps
      packets. Selected by a `gyro` group in the config (`--hidraw` forces
      it); `configs/default.vdf` asks for it, `configs/no-gyro.vdf` opts out.
      Verified on the Radxa against a live controller: **all 18 buttons**,
      joystick full range, right-pad camera mode (deflects with finger
      motion, snaps to centre one sample after it stops), gyro to ±170 dps,
      and 45-60 s runs with zero disconnects.
      Three findings that cost a session between them:
      1. **Raw accel *does* cross the wireless dongle** (IMU mode 0x1C),
         contradicting hid-steam's field table ("not sent through wireless")
         and SDL's FIXME. Gravity reads +1.00 g at rest, which also
         validates the ±2 g → ±8 g rescale by physics. Mode is now 0x1C.
      2. **The controller migrates between dongle slots.** It was on slot 1,
         dropped, and came back on slot 2 — while we kept reading slot 1 and
         saw silence. Worse, the dongle **acks the settings report for a slot
         with no controller on it**, so "the slot that answers" is not a
         valid probe. Fixed: hold all four slots open, let the one that
         delivers packets identify itself, and forget it on disconnect.
      3. **Camera mode was frozen at centre** because `Mapping` treated every
         `BTN_THUMB2` as a touch transition and reset the tracked position.
         evdev only sends transitions; raw HID repeats the bit every packet,
         so the delta was never computed. Fixed + regression test.
- [x] Bridge-level verification (2026-08-02): `sweam steam` runs the hidraw
      source alongside the gadget (bound, /dev/hidg0, Switch enumerates it),
      and a controller power-cycle mid-run gives the full
      `streaming → disconnected (neutral) → connected (motion re-enabled) →
      streaming` cycle. Also fixed here: the pump used to `continue` before
      polling the controller whenever the host wasn't streaming, so packets
      queued in the kernel buffers and a session would open by replaying a
      stale backlog (and connect/disconnect events were missed entirely).
      Deployed as the active service config: touch-dpad.vdf + gyro group.
- [ ] Remaining: IMU axis order/signs are passed through unchanged and need
      tuning against a real Switch (motion game or the gyro calibration
      screen); consider exposing `imu_mode` in the gyro group's settings.
      Original research (byte-exact) in Notes.md. Highlights:
      enable via unnumbered feature report 0x87, register 0x30 = IMU mode
      bitmask (0x14 = quat+gyro is the proven wireless combo; raw accel over
      the dongle needs hardware verification — SDL has a FIXME); input
      packet type 0x01: accel s16le at offsets 28/30/32, gyro 34/36/38,
      quat 40-46; ±2000 dps, ±2 g full scale. **Architecture constraint:
      opening the hid-steam hidraw node makes hid-steam unregister its
      evdev device** — so the hidraw input source must parse buttons AND
      IMU from the raw 0x01 packets (evdev + hidraw concurrently is
      impossible). Implement as a new `InputSource` (raw hidraw) replacing
      the evdev one when gyro is requested; scale to Pro units
      (`ImuSample::ACCEL_PER_G`/`GYRO_PER_DPS`), axis remap needs hardware
      tuning. License-safe bases: ynsta/steamcontroller (MIT), SDL steam
      driver (zlib); hid-steam.c/sc-controller are GPL — facts only.
- [ ] Wire the Switch side end-to-end on the real Switch (motion game or
      the Switch's own gyro calibration screen).

## Appendix — someday: a no-std build

Not now, and nothing should be contorted for it. Recorded only so design
choices lean the right way when the simpler option is otherwise a wash: a
no-std sweam (running on a microcontroller rather than an SBC) is of
distant interest, so prefer designs that do not need threads, a runtime or
much allocation when a plain loop does the same job.

The single `poll()` loop in the current focus above is an example of that
happening to align — it is the right call for starvation reasons today, and
it is also the shape that would port.

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

### Resources

#### Linux USB Gadget API docs

https://www.kernel.org/doc/html/v6.11/driver-api/usb/gadget.html

#### `joycontrol`: Python Nintendo Controller Emulator Library

https://github.com/mart1nro/joycontrol
https://github.com/Poohl/joycontrol

#### `nxbt`: Python Nintendo Controller Emulator Library

https://github.com/Brikwerk/nxbt

### Packages

#### `steam-devices` on Debian for evdev

#### Kernel modules

- libcomposite
- usb_f_hid
## Steam config import feasibility (2026-07-19)

Question: can sweam re-use real Steam controller configurations (the community
layouts Steam users make in the Steam Input configurator)? Short answer: yes for
a useful subset — an importer is feasible and worth doing; full fidelity is not.

### 1. The real format

Same VDF/KeyValues syntax we already parse (`src/vdf.rs`), root block
`"controller_mappings"`, but a richer schema than our `src/steam/config.rs`
subset. A good public example: GoldRenard/GTAVSteamControllerNative
`controller.vdf` on GitHub (search "controller_mappings" "version" vdf for more;
`controller_neptune*.vdf` files in SteamOS repos are the same schema for the Deck).

- Top level: `version` ("2" old flat style, "3" current), `revision`, `title`,
  `description`, `creator`, `controller_type` (e.g.
  `controller_steamcontroller_gordon` for the original SC — configs are
  per-controller-type, so we should filter/prefer these), `actions` +
  `action_layers` (action sets), `localization`, many `group` blocks, `preset`
  blocks, `settings`.
- `group`: `id` (number), `mode` — `four_buttons`, `dpad`, `joystick_move`,
  `joystick_camera`, `joystick_mouse`, `mouse_joystick`, `absolute_mouse`,
  `scrollwheel`, `trigger`, `touch_menu`, `radial_menu`, `mouse_region`,
  `switches` — plus `bindings`/`inputs` and `settings` (deadzone, sensitivity,
  haptic_intensity, curve_exponent, ...).
  - v2 bindings are flat: `"button_a" "xinput_button A, , "` (we already
    tolerate the trailing activator commas).
  - v3 nests activators: `inputs { button_a { activators { Full_Press {
    bindings { binding "xinput_button A" } } } } }` with activator types
    Full_Press / Double_Press / Long_Press / Start_Press / chords / toggles.
- Binding value vocabulary: `xinput_button A|B|X|Y|SHOULDER_LEFT|...|
  TRIGGER_LEFT|DPAD_UP|JOYSTICK_LEFT|SELECT|START|GUIDE`, `key_press W`,
  `mouse_button LEFT`, `mouse_wheel ...`, `controller_action` (change_preset
  etc.), `game_action` (Steam Input API in-game actions).
- Groups are wired to physical controls not by a `source` key (our
  simplification) but via `preset { name "Default" group_source_bindings {
  "<group id>" "button_diamond active" ... } }`; entries may be `active` or
  `... modeshift`, and mode shifts add e.g.
  `"left_trigger_threshold" "mode_shift right_trigger 41"`. One preset per
  action set; the `Default` preset is the layout that matters to us.

### 2. Obtainability and licensing

- User's own local files: trivially fine. Live under
  `Steam/userdata/<uid>/241100/remote/controller_config/<game>/*.vdf`
  (241100 = Steam Controller Configs app) and downloaded community ones under
  `Steam/userdata/<uid>/config/controller_configs/workshop/<publishedfileid>/`.
  Easiest supported path: "point sweam at a .vdf you exported/copied".
- Without a Steam client: controller configs are legacy RemoteStorage workshop
  UGC; the (deprecated but still working) anonymous
  `ISteamRemoteStorage/GetPublishedFileDetails/v1` POST endpoint returns a
  `file_url` on Steam's CDN for a publishedfileid — this is how tools like
  nikop/SteamControllerConfigDownloader (MIT) and its forks work. steamcmd can
  fetch some items too. Deprecated API = don't build core features on it; an
  optional "fetch by workshop id" convenience is reasonable.
- Licensing: workshop configs are user-generated content licensed to Valve and
  to other users *for use within Steam* (Steam Subscriber Agreement §6). We may
  not redistribute config files with sweam. The format itself is factual and
  parsing it for interoperability is fine. Conclusion: ship an importer, never
  ship imported configs; users import their own files or fetch by id themselves.

### 3. What maps onto sweam's model

Clean (covers the practical majority of gamepad-style SC configs):
- `button_diamond` + `four_buttons` → face buttons (position-wise:
  xinput A→Switch B, B→A, X→Y, Y→X, since xinput names are positional on the
  other diamond convention — decide once, document it).
- `switch` group → bumpers, SELECT/START/GUIDE → Minus/Plus/Home, grips.
- `left_trigger`/`right_trigger` `trigger` mode, Full_Press/click → ZL/ZR
  (soft-pull and threshold settings ignored — Switch triggers are digital).
- `left_trackpad` mode `dpad` → dpad (click-required vs touch `requires_click`
  setting: we can honor click-only today; note the difference).
- `joystick` + `joystick_move` → left stick; `right_trackpad` +
  `joystick_camera`/`joystick_move` → right stick (`output_joystick` already
  understood by our loader).

Cannot map (ignore, with one warning each):
- `key_press`/`mouse_*`/`game_action` bindings — no keyboard/mouse on the
  Switch side.
- Action sets/layers beyond `Default`, `controller_action` preset switching.
- Mode shifts, chords, Double/Long/Start_Press activators, toggles, turbo.
- `touch_menu`/`radial_menu`/`mouse_region`/`absolute_mouse` groups.
- Gyro groups (until gyro passthrough lands — then `joystick_camera`-on-gyro
  could become real gyro), haptic/sensitivity/curve settings.

### 4. Recommendation

Extend `src/steam/config.rs` to accept native configs alongside our dialect:
1. Detect `version` 2/3 + presence of `preset` blocks → native path.
2. Build group-id → group table; take the `Default` preset's
   `group_source_bindings`, keep only `... active` entries (skip `modeshift`).
3. Per source, translate the group by mode as above; v2 flat `bindings` and v3
   `inputs/activators/Full_Press` both funnel into the existing
   `xinput_button → Button` translation (add the xinput name table next to
   `switch_button`).
4. Everything unrecognized: `eprintln!` warning + skip, never a hard error —
   matches our current philosophy. Warn once if `controller_type` is not the SC.
5. Optional later: `sweam import --workshop-id N` via GetPublishedFileDetails.

Effort: parser/translator ~300–500 lines + tests with one or two real
(hand-reconstructed, not redistributed) fixture configs — a few evenings.
No GPL concerns: format knowledge only; the one MIT downloader is reference,
not a dependency.

## Tech-debt audit (2026-07-19)

Whole-repo review; every finding checked against the code. Lock ordering in
`src/main.rs` was audited and is *consistent* (protocol → state, both released
before writer is taken, in both the reader thread and the pump loop) — no
deadlock finding there.

### High

- **high — src/switch/protocol.rs:264–269 + 310: host-controlled SPI read
  length panics the service.** `0x10` takes `len = args[4] as usize` (up to
  255) with no upper bound; the reply payload is `5 + len` bytes and
  `subcommand_reply` does `r[15..15 + payload.len()]` on a 64-byte report —
  any request with `len > 44` is a slice out-of-bounds panic. It fires in the
  reader thread *while holding the protocol and state mutexes*, so both get
  poisoned, the pump loop's `lock().unwrap()` then panics, and under the
  installed service (`Restart=always`) a buggy or hostile host can hold sweam
  in a crash loop. Known hosts request ≤ 24 bytes, which is why it hasn't
  fired. Fix: clamp `len` (a real controller caps SPI reads at 0x1D) or
  truncate the payload to what fits, and add a test with `len = 0xFF`.

### Medium

- **med — src/main.rs:163: `Ok(0) => continue` can busy-spin.** If the hidg
  read ever returns 0 (some gadget kernels do this at disconnect instead of
  an error), the reader thread spins at 100% CPU forever and never sets
  `RUNNING = false`, so the bridge never notices the host is gone. Fix: treat
  `Ok(0)` as disconnect (log + break) like the `Err` arm.

- **med — src/main.rs:171–176, 198–216: mutex-poison `unwrap()` cascade.**
  All six `lock().unwrap()` calls turn any panic-while-locked (e.g. the SPI
  finding above) into secondary `PoisonError` panics with misleading journal
  output on the Radxa. The shared data (`Protocol`, `ControllerState`) is
  valid after any partial update, so poisoning is safe to ignore here. Fix:
  `lock().unwrap_or_else(std::sync::PoisonError::into_inner)` via a small
  helper.

- **med — src/main.rs:138–143: `open_controller` swallows the error,
  including PermissionDenied.** The closure ends in `.ok()`, so in `steam`
  mode every open failure is silent: run `sweam steam` with configfs access
  but without `/dev/input` access (or with a typo'd `--evdev`) and it streams
  neutral inputs forever, retrying once a second, with only the one generic
  "No controller yet" line. `steamcheck.rs:26–34` already detects
  PermissionDenied and exits with the sudo/input-group hint — `steam` mode
  should reuse that check and at minimum log the first failure's reason.

- **med — src/install.rs:25–27, 41–49, 57: ExecStart breaks with relative
  paths or spaces in `--prefix`.** The unit line is
  `ExecStart={binary_path} steam --config {config_path}` with no quoting:
  `--prefix ./sweam-dir` yields a non-absolute ExecStart (systemd rejects the
  unit), and a prefix or config path containing a space splits into extra
  argv words. Fix: canonicalize the prefix, require it absolute, and quote
  the paths in the unit (systemd understands double quotes in ExecStart).

- **med — src/install.rs:95–98: `uninstall` recursively deletes whatever
  `--prefix` names.** `remove_dir_all(prefix)` with e.g.
  `sweam uninstall --prefix /opt` wipes all of `/opt`. Fix: refuse unless the
  directory looks like a sweam install (contains the `sweam` binary), or
  remove only the known files (`sweam`, `config.vdf`) and then
  `remove_dir`.

- **med — src/main.rs:165–183 vs 212–216: host-disconnect exit status is a
  race.** When the host goes away, the reader thread's read error path sets
  `RUNNING = false` → clean exit 0, but if the pump loop's `write_all` hits
  the dead hidg first it returns `Err` → exit 1. Same event, coin-flip
  status; `Restart=always` restarts either way, but the journal shows
  spurious "failed" states for an expected event (Switch sleeping/unplugged).
  Fix: route both paths through one place; treat
  `ESHUTDOWN`/`ENOTCONN`/`EPIPE`-class write errors as the same clean
  "host gone" shutdown the read path already gets.

### Medium-low

- **med-low — src/main.rs:103–120, 157–185: threads never joined; hidg fd
  outlives gadget teardown.** The reader thread holds a clone of the hidg fd
  and a `writer` Arc, and is never joined, so `/dev/hidgN` is still open when
  `UsbGadget::drop` removes the configfs function on shutdown. TESTBED
  (2026-07-12) shows teardown empirically works, but the ordering is
  accidental; the USB-state watcher also keeps polling the removed UDC state
  file during teardown and can log a spurious "(unreadable)" transition.
  Fix: give the loops a shutdown signal (check `RUNNING`), close/shutdown the
  reader fd, and join both threads before `main` returns (dropping the
  gadget last stays correct because locals drop in reverse order).

- **med-low — src/install.rs:41–49: `--config` is installed without being
  validated.** `install` copies the VDF verbatim; if it doesn't parse,
  `sweam steam --config …` exits at startup and `Restart=always` crash-loops
  the service from boot, headless. One `steam::config::load(config)?` before
  copying turns this into an immediate install-time error.

### Low

- **low — src/switch/protocol.rs:157–165, 182–187: raw-log repeat counter is
  only flushed by a *different* report.** If the last pre-streaming report
  repeats and then streaming starts (0x80 0x04) — or traffic just stops — the
  "… repeated N more times" line is never printed, so the journal undercounts
  exactly the traffic the log exists to capture. Fix: call
  `flush_raw_repeats()` when `streaming` flips on (protocol.rs:216–219).

- **low — src/cli.rs:126: `--skip-modprobe=anything` silently ignores the
  inline value.** The boolean arm never looks at `inline`, so
  `--skip-modprobe=false` still *enables* skipping. Fix: reject an inline
  value on boolean flags.

- **low — src/cli.rs:135–224: per-command flag validation is duplicated four
  ways** (`no_gadget_flags`, `no_prefix`, plus two ad-hoc loops for
  `--config`/`--evdev` repeated in the `manual`, `hostcheck`, and
  `install`/`uninstall` arms). A single table of allowed flags per command
  (`&[(&str, bool)]` checked in one loop) would collapse ~50 lines and make
  the next flag additions one-line changes.

- **low — src/steamcheck.rs:46–62 / src/hostcheck.rs:69–93: duplicated
  "describe-on-change" loop.** Both keep a `last` state and print
  `[{elapsed:8.3}s] state.describe()` on change; the "sort, take first, warn
  if several" detection pattern also appears three times
  (gadget.rs:219–235 `autodetect_udc`, hostcheck.rs:99–129 `detect_device`,
  and steam/mod.rs enumeration). Small shared helpers would keep the three
  check tools in lockstep.

- **low — docs drift.**
  - README.md:82 says `sudo sweam steamcheck`, but TESTBED.md:16–18 and the
    hint in src/steam/mod.rs:81–84 establish that steamcheck runs without
    sudo via the `input` group; README.md:30 ("Root on the SBC") deserves the
    same nuance.
  - README.md has no mention of `sweam install`/`uninstall`/the systemd
    service at all — it exists in the CLI help (cli.rs:16–20) and
    TESTBED.md:84–92 but not in the user-facing doc.
  - CLAUDE.md architecture block lists every module except `src/install.rs`,
    and its cli.rs line still reads "steam | manual | steamcheck |
    hostcheck".
  - PLAN.md:73 "udev rules unneeded so far: sweam runs as root" predates the
    input-group setup (TESTBED 2026-07-19).

- **low — test gaps.**
  - `0x80 0x05` (stream off) has no test — protocol.rs:221–225 flips
    `streaming` off but no test covers stream-on → stream-off → reports stop.
  - The raw-log dedupe counters (protocol.rs:134–135, 157–165) are untested;
    they'd need `Protocol` to expose the log lines or count instead of
    printing directly.
  - The hotplug retry (main.rs:193–196) and the whole pump loop live inside
    `main()` and are untestable; extracting a `run_bridge(input, protocol,
    state, writer)` function (and a `RetryTimer`) would let the
    retry/neutral-inputs behavior be tested with a fake `InputSource`.
  - install.rs builds the unit string inline (install.rs:51–64); extracting
    `fn unit_contents(exec_start: &str) -> String` (plus an
    `exec_start(prefix, config)` helper) would make the quoting fixes above
    testable on any platform.

### Quick wins

1. Clamp the SPI read length (protocol.rs:265) — one line + one test; removes
   the only host-triggerable panic.
2. `Ok(0) => break` in the reader thread (main.rs:163).
3. Flush the raw-repeat counter when streaming starts (protocol.rs:218).
4. Validate `--config` with `steam::config::load` before installing
   (install.rs:43).
5. Guard `uninstall`'s `remove_dir_all` behind a "contains the sweam binary"
   check (install.rs:95).
6. Add install.rs + cli.rs to the CLAUDE.md architecture block; fix
   README's `sudo steamcheck` and add an install/service section.

## Steam Controller IMU over hidraw (2026-07-20)

### 1. Enabling IMU data (feature report 0x87, SET_SETTINGS_VALUES)

The SC streams IMU samples only after being asked. All control traffic is one
unnumbered 64-byte HID feature report: `type len payload...`, zero-padded.
`0x87` = SET_SETTINGS_VALUES; payload is repeated triplets `reg u16le`:

```
87 <len=3*n> ( <reg> <val_lo> <val_hi> ){n}    ; pad to 64 with 0x00
```

Relevant setting registers (numeric ids from hid-steam.c, GPL, facts only):

| reg  | name                       | values |
|------|----------------------------|--------|
| 0x07 | LEFT_TRACKPAD_MODE         | 7 = TRACKPAD_NONE (mouse emu off) |
| 0x08 | RIGHT_TRACKPAD_MODE        | 7 = TRACKPAD_NONE |
| 0x30 | IMU_MODE ("gyro mode")     | bitmask, below |
| 0x31 | WIRELESS_PACKET_VERSION    | 2 (SDL and Steam set this first) |
| 0x32 | SLEEP_INACTIVITY_TIMEOUT   | seconds (0x0384 = 900) |

IMU_MODE bitmask: `0`=OFF, `1`=STEERING, `2`=TILT, `4`=SEND_ORIENTATION
(quaternion), `8`=SEND_RAW_ACCEL, `16`=SEND_RAW_GYRO. SDL uses `0x18`
(accel+gyro); sc-controller uses `0x14` (quat+gyro, no accel); `0x1C` = all
three. Known-good full packet (ynsta/steamcontroller, MIT; gyro value patched
to 0x1C at byte 18):

```
87 15  32 84 03  18 00 00  31 02 00  08 07 00  07 07 00  30 1C 00  2F 01 00
      (0x32=900) (0x18=0)  (0x31=2)  (0x08=7)  (0x07=7)  (IMU=0x1C)(0x2F=1)
```

(0x18 = MOMENTUM_MAXIMUM_VELOCITY here per the id table; 0x2F=ENABLE_FAST_SCAN.)

Per-controller addressing through the dongle: each of the 4 slots is its own
USB HID interface (1-4) with its own hidraw node; send the feature report on
that slot's node — no extra addressing byte. (ynsta, using libusb instead,
sent SET_REPORT wValue=0x0300 wIndex=interface.) An empty slot NAKs SET_REPORT
(EPIPE); hid-steam retries up to 50x with 20 ms sleeps — do the same.
After 0x87 do a GET_REPORT: a lingering reply can otherwise be read back later.

Lizard mode / hid-steam interaction (hid-steam.c facts):
- lizard OFF = `0x81` (CLEAR_DIGITAL_MAPPINGS) + 0x87 setting trackpads to 7;
  lizard ON = `0x85` (SET_DEFAULT_DIGITAL_MAPPINGS) + `0x8E`
  (LOAD_DEFAULT_SETTINGS). 0x8E resets *all* settings — kills IMU_MODE too.
- hid-steam (re)sends lizard config on: evdev open/close, wireless connect
  event (type 0x03), hidraw-client close, and writes to the lizard_mode module
  param. There is no suspend/resume hook, and no periodic resend.
- **Gotcha: the hidraw node for the SC interface is hid-steam's virtual
  "client" device.** open() on it sets client_opened → hid-steam *unregisters
  the evdev input (and battery stays, sensors are Deck-only)* and stops all
  its own configuration writes until close. So "buttons via hid-steam evdev +
  IMU via hidraw" cannot coexist: while we hold hidraw open we own the device
  completely and must parse buttons from the same raw packets (they're all in
  the one input report anyway). On close, hid-steam restores lizard mode and
  re-registers evdev.

### 2. Input report layout (64 bytes, interrupt IN / hidraw read)

Header: bytes 0-1 = report version `01 00`; byte 2 = type; byte 3 = payload
len. Types: `0x01` input (60 B), `0x03` wireless event (1 B), `0x04` status/
battery (11 B), (`0x09` = Steam Deck only). All fields little-endian.

Type 0x01 payload (offsets in the 64-byte packet; per SDL controller_structs.h
ValveControllerStatePacket_t, zlib, cross-checked vs ynsta MIT + hid-steam):

| off | type | field |
|-----|------|-------|
| 4   | u32  | packet/sequence number |
| 8-10| u24  | buttons (b8 bit7=A ... b10 bit4=rpad-click, part of a u64 field) |
| 11  | u8   | left trigger 0-255 |
| 12  | u8   | right trigger 0-255 |
| 13-15| -   | pad |
| 16  | s16  | lpad_x (joystick X when b10 bit3 clear) |
| 18  | s16  | lpad_y |
| 20  | s16  | rpad_x |
| 22  | s16  | rpad_y |
| 24  | u16  | trigger L 16-bit (wired only, "redundant") |
| 26  | u16  | trigger R 16-bit |
| 28  | s16  | accel X |
| 30  | s16  | accel Y |
| 32  | s16  | accel Z |
| 34  | s16  | gyro X (pitch) |
| 36  | s16  | gyro Y (roll) |
| 38  | s16  | gyro Z (yaw) |
| 40  | s16  | quat W |
| 42  | s16  | quat X |
| 44  | s16  | quat Y |
| 46  | s16  | quat Z |
| 48-63| -   | unused |

Scales (SDL facts): gyro full scale ±2000 dps over ±32768 (≈16.4 LSB/dps,
MPU-6500 style); accel ±2 g over ±32768 (16384 LSB/g); quat components are
unit-normalized over ±32768. SDL FIXME comment hints accel may not arrive over
the wireless dongle — verify on hardware; quat+gyro (0x14) is the known-good
wireless combo (sc-controller shipped exactly that).

Packet rate: query feature 0x83 GET_ATTRIBUTES_VALUES → attribute
ATTRIB_CONNECTION_INTERVAL_IN_US (SDL defaults to 9000 µs ≈ 111 Hz when
absent). Wireless idle: input packets stop; the slot sends periodic type 0x04
status packets instead — u16 mV at offset 12, charge % at byte 14. Wireless
events: type 0x03, byte 4 = 1 disconnected / 2 connected / 3 newly paired.
Empty-slot interfaces simply produce no input packets.

### 3. Sources / licenses

- ynsta/steamcontroller (MIT) — packet struct, working 0x87 packet, endpoints.
- SDL `src/joystick/hidapi/steam/` (zlib) — C structs with accel, IMU_MODE
  constants, scales, attributes query. Best base for Rust translation.
- hid-steam.c (GPL-2) and kozec/sc-controller (GPL-2), rodrigorc/steamctrl
  (GPL-2): behavioral facts only (setting ids, lizard/client behavior, battery
  offsets, retry quirk) — no code copied.

### 4. Practical notes & plan

- Finding the node: for each `/sys/class/hidraw/hidraw*/device`, the resolved
  path contains `.../<bus>-<port>:1.<iface>/0003:28DE:1142.XXXX/hidraw/hidrawN`;
  parse the `:1.<iface>` USB-interface segment (or read `../../bInterfaceNumber`).
  Dongle iface 0 = keyboard emu, 1-4 = controller slots. Wired 28DE:1102:
  iface 2 is the gamepad.
- Plain `open(O_RDWR)` + `read()` works while hid-steam is bound (it forwards
  every raw packet to the client node), but see the §1 gotcha: it disables the
  evdev device. Feature reports: use HIDIOCSFEATURE/HIDIOCGFEATURE with a
  leading report-id byte 0x00 + 64 bytes (SDL does `buf[0]=0`, 65 total).
- Implementation order: (1) sysfs scan → hidraw open; (2) send 0x87 with
  IMU_MODE=0x14, EPIPE-retry, readback; (3) parse type 0x01: buttons+pads+
  triggers (drop evdev path while hidraw is open) and gyro/quat; (4) handle
  0x03 connect (resend 0x87) and 0x04 idle; (5) try 0x18/0x1C on hardware to
  see whether raw accel survives the dongle; fall back to deriving tilt from
  the quaternion if not.

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

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

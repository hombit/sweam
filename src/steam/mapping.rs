//! Mapping from Steam Controller evdev events to [`ControllerState`].
//!
//! The event vocabulary is defined by the kernel `hid-steam` driver
//! (`steam_input_register()` and `steam_do_input_event()` in
//! drivers/hid/hid-steam.c):
//!
//! - buttons as `EV_KEY` with `BTN_*` codes (left-pad click quadrants arrive
//!   as `BTN_DPAD_*`);
//! - joystick as `ABS_X`/`ABS_Y` and right pad as `ABS_RX`/`ABS_RY`, all in
//!   -32767..=32767 with up = negative (the driver negates raw Y for us),
//!   and 0 when released;
//! - left-pad touch position as `ABS_HAT0X/Y` (with `BTN_THUMB` = left pad
//!   touched) — used by [`LeftPadMode::TouchDpad`], ignored in the default
//!   click mode; analog triggers as `ABS_HAT2X/Y` — unmapped (we use full
//!   trigger pulls instead).
//!
//! [`Mapping::default`] is the built-in layout; `steam::config` builds
//! custom [`Mapping`]s from Steam-style VDF files. This module works on raw
//! `(code, value)` pairs so it has no Linux-only dependency and its tests
//! run on any platform.

use crate::state::{Button, ControllerState, StickState};
use std::collections::HashMap;

// Event codes from include/uapi/linux/input-event-codes.h. Names follow
// hid-steam's usage; note the kernel's positional aliases are misleading for
// this layout (BTN_X is 0x133 "BTN_NORTH" but sits west on the controller).
pub(crate) const BTN_A: u16 = 0x130; // bottom (south)
pub(crate) const BTN_B: u16 = 0x131; // right (east)
pub(crate) const BTN_X: u16 = 0x133; // left (west)
pub(crate) const BTN_Y: u16 = 0x134; // top (north)
pub(crate) const BTN_TL: u16 = 0x136;
pub(crate) const BTN_TR: u16 = 0x137;
pub(crate) const BTN_TL2: u16 = 0x138; // left trigger fully pressed
pub(crate) const BTN_TR2: u16 = 0x139; // right trigger fully pressed
pub(crate) const BTN_SELECT: u16 = 0x13A; // menu left
pub(crate) const BTN_START: u16 = 0x13B; // menu right
pub(crate) const BTN_MODE: u16 = 0x13C; // Steam logo
pub(crate) const BTN_THUMBL: u16 = 0x13D; // joystick clicked
pub(crate) const BTN_THUMBR: u16 = 0x13E; // right-pad clicked
pub(crate) const BTN_THUMB: u16 = 0x121; // left-pad touched
pub(crate) const BTN_THUMB2: u16 = 0x122; // right-pad touched
pub(crate) const BTN_DPAD_UP: u16 = 0x220; // left-pad click quadrants
pub(crate) const BTN_DPAD_DOWN: u16 = 0x221;
pub(crate) const BTN_DPAD_LEFT: u16 = 0x222;
pub(crate) const BTN_DPAD_RIGHT: u16 = 0x223;
// Back grip levers: modern kernels emit BTN_GRIPL/R, pre-6.11 hid-steam used
// BTN_GEAR_DOWN/UP — accept both.
pub(crate) const BTN_GRIPL: u16 = 0x224;
pub(crate) const BTN_GRIPR: u16 = 0x225;
pub(crate) const BTN_GEAR_DOWN: u16 = 0x150;
pub(crate) const BTN_GEAR_UP: u16 = 0x151;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_HAT0X: u16 = 0x10; // left-pad touch position
const ABS_HAT0Y: u16 = 0x11;

/// evdev axis full scale (`hid-steam` reports symmetric -32767..=32767).
const AXIS_MAX: i32 = 32767;

/// Touch-dpad center deadzone radius: ~30% of full scale. Touches closer to
/// the pad center press nothing, so a thumb resting mid-pad doesn't trigger
/// directions.
const TOUCH_DPAD_DEADZONE: i32 = AXIS_MAX * 3 / 10;

// tan(22.5°) ≈ 0.41421 as an integer ratio: the slope of the 8-way sector
// boundaries (each cardinal direction owns ±67.5° around its axis, so
// adjacent directions overlap in 45° diagonal zones — Steam's dpad feel).
const SECTOR_TAN_NUM: i64 = 27146;
const SECTOR_TAN_DEN: i64 = 65536;

/// Which Switch stick a physical analog input drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickTarget {
    None,
    LeftStick,
    RightStick,
}

/// How the left trackpad produces d-pad presses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftPadMode {
    /// Click quadrants (`BTN_DPAD_*` from hid-steam) press directions —
    /// the default.
    ClickDpad,
    /// Touch position (`ABS_HAT0X/Y`) presses directions, no click needed:
    /// Steam's dpad mode with "requires click" off.
    TouchDpad,
}

/// A complete Steam Controller → Pro Controller layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    /// evdev `BTN_*` code → Switch button; unlisted codes are unbound.
    buttons: HashMap<u16, Button>,
    /// What the physical joystick (`ABS_X/Y`) drives.
    pub joystick: StickTarget,
    /// What the right pad (`ABS_RX/RY`) drives.
    pub right_pad: StickTarget,
    /// How the left pad drives the d-pad.
    pub left_pad: LeftPadMode,
    /// Last seen left-pad touch position (`ABS_HAT0X/Y`), tracked so a
    /// single-axis event can re-sectorize with both coordinates. Runtime
    /// state, only meaningful in [`LeftPadMode::TouchDpad`].
    left_pad_touch: (i32, i32),
}

impl Mapping {
    /// A mapping with nothing bound; config files start from this.
    pub fn empty() -> Self {
        Self {
            buttons: HashMap::new(),
            joystick: StickTarget::None,
            right_pad: StickTarget::None,
            left_pad: LeftPadMode::ClickDpad,
            left_pad_touch: (0, 0),
        }
    }

    pub fn bind_button(&mut self, code: u16, target: Button) {
        self.buttons.insert(code, target);
    }

    #[cfg(test)]
    pub(crate) fn button(&self, code: u16) -> Option<Button> {
        self.buttons.get(&code).copied()
    }

    /// Apply one `EV_KEY` event.
    pub fn apply_key(&mut self, state: &mut ControllerState, code: u16, pressed: bool) {
        // Right pad released: snap its stick back to center in case the
        // last position event didn't return to 0.
        if code == BTN_THUMB2 {
            if !pressed {
                match self.right_pad {
                    StickTarget::LeftStick => state.left_stick = StickState::default(),
                    StickTarget::RightStick => state.right_stick = StickState::default(),
                    StickTarget::None => {}
                }
            }
            return;
        }
        // Left pad released in touch mode: lift all four directions in case
        // the last position event didn't return to center.
        if code == BTN_THUMB {
            if self.left_pad == LeftPadMode::TouchDpad && !pressed {
                self.left_pad_touch = (0, 0);
                self.set_touch_dpad(state);
            }
            return;
        }
        // In touch mode the pad click is not a separate input — ignore the
        // click quadrants so a click-release while still touching doesn't
        // lift a held direction.
        if self.left_pad == LeftPadMode::TouchDpad
            && matches!(
                code,
                BTN_DPAD_UP | BTN_DPAD_DOWN | BTN_DPAD_LEFT | BTN_DPAD_RIGHT
            )
        {
            return;
        }
        if let Some(&button) = self.buttons.get(&code) {
            state.set_button(button, pressed);
        }
    }

    /// Apply one `EV_ABS` event.
    pub fn apply_abs(&mut self, state: &mut ControllerState, code: u16, value: i32) {
        // Left-pad touch position: drives the d-pad in touch mode only.
        if code == ABS_HAT0X || code == ABS_HAT0Y {
            if self.left_pad == LeftPadMode::TouchDpad {
                if code == ABS_HAT0X {
                    self.left_pad_touch.0 = value;
                } else {
                    self.left_pad_touch.1 = value;
                }
                self.set_touch_dpad(state);
            }
            return;
        }
        let (target, x_axis) = match code {
            ABS_X => (self.joystick, true),
            ABS_Y => (self.joystick, false),
            ABS_RX => (self.right_pad, true),
            ABS_RY => (self.right_pad, false),
            // ABS_HAT2X/Y (analog triggers) are intentionally unmapped,
            // see module docs.
            _ => return,
        };
        let stick = match target {
            StickTarget::LeftStick => &mut state.left_stick,
            StickTarget::RightStick => &mut state.right_stick,
            StickTarget::None => return,
        };
        if x_axis {
            stick.x = scale_x(value);
        } else {
            stick.y = scale_y(value);
        }
    }

    /// Press/release whatever the four `BTN_DPAD_*` codes are bound to
    /// according to the current touch position.
    fn set_touch_dpad(&self, state: &mut ControllerState) {
        let (x, y) = self.left_pad_touch;
        let (up, down, left, right) = touch_dpad_directions(x, y);
        for (code, pressed) in [
            (BTN_DPAD_UP, up),
            (BTN_DPAD_DOWN, down),
            (BTN_DPAD_LEFT, left),
            (BTN_DPAD_RIGHT, right),
        ] {
            if let Some(&button) = self.buttons.get(&code) {
                state.set_button(button, pressed);
            }
        }
    }
}

impl Default for Mapping {
    /// The built-in layout: positional ABXY swap, left-pad click quadrants →
    /// d-pad, joystick → left stick, right pad → right stick, full trigger
    /// pulls → ZL/ZR, grips → Capture/Home.
    fn default() -> Self {
        let mut mapping = Self::empty();
        mapping.joystick = StickTarget::LeftStick;
        mapping.right_pad = StickTarget::RightStick;
        // ABXY are swapped positionally: Steam uses the Xbox layout (A
        // bottom, B right, X left, Y top), Switch puts B bottom, A right,
        // Y left, X top.
        for (code, button) in [
            (BTN_A, Button::B),
            (BTN_B, Button::A),
            (BTN_X, Button::Y),
            (BTN_Y, Button::X),
            (BTN_TL, Button::L),
            (BTN_TR, Button::R),
            (BTN_TL2, Button::ZL),
            (BTN_TR2, Button::ZR),
            (BTN_SELECT, Button::Minus),
            (BTN_START, Button::Plus),
            (BTN_MODE, Button::Home),
            (BTN_THUMBL, Button::LStick),
            (BTN_THUMBR, Button::RStick),
            (BTN_DPAD_UP, Button::Up),
            (BTN_DPAD_DOWN, Button::Down),
            (BTN_DPAD_LEFT, Button::Left),
            (BTN_DPAD_RIGHT, Button::Right),
            (BTN_GRIPL, Button::Capture),
            (BTN_GEAR_DOWN, Button::Capture),
            // TODO(phase 4, hardware): revisit; Home doubles with the logo.
            (BTN_GRIPR, Button::Home),
            (BTN_GEAR_UP, Button::Home),
        ] {
            mapping.bind_button(code, button);
        }
        mapping
    }
}

/// 8-way sectorization of a left-pad touch, Steam-dpad style: inside the
/// [`TOUCH_DPAD_DEADZONE`] circle nothing is pressed; outside, each cardinal
/// direction is active within ±67.5° of its axis, so adjacent directions
/// overlap in 45° diagonal zones. Returns `(up, down, left, right)`; evdev
/// up is negative y. `(0, 0)` (untouched) presses nothing.
fn touch_dpad_directions(x: i32, y: i32) -> (bool, bool, bool, bool) {
    let (x, y) = (x as i64, y as i64);
    let deadzone = TOUCH_DPAD_DEADZONE as i64;
    if x * x + y * y < deadzone * deadzone {
        return (false, false, false, false);
    }
    // Active when the component along the direction is positive and at
    // least tan(22.5°) times the perpendicular one.
    let active = |along: i64, across: i64| {
        along > 0 && along * SECTOR_TAN_DEN >= across.abs() * SECTOR_TAN_NUM
    };
    (active(-y, x), active(y, x), active(-x, y), active(x, y))
}

/// evdev -32767..=32767 → Switch 12-bit, right = max.
fn scale_x(value: i32) -> u16 {
    let center = StickState::CENTER as i32;
    let scaled = center + value.clamp(-AXIS_MAX, AXIS_MAX) * (center - 1) / AXIS_MAX;
    scaled as u16
}

/// evdev -32767..=32767 (up = negative) → Switch 12-bit, up = max.
fn scale_y(value: i32) -> u16 {
    let center = StickState::CENTER as i32;
    let scaled = center - value.clamp(-AXIS_MAX, AXIS_MAX) * (center - 1) / AXIS_MAX;
    scaled as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abxy_are_swapped_positionally() {
        let mut mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_A, true); // Steam bottom → Switch bottom (B)
        assert_ne!(state.buttons & (1 << Button::B as u32), 0);
        assert_eq!(state.buttons & (1 << Button::A as u32), 0);

        mapping.apply_key(&mut state, BTN_A, false);
        mapping.apply_key(&mut state, BTN_Y, true); // Steam top → Switch top (X)
        assert_ne!(state.buttons & (1 << Button::X as u32), 0);
    }

    #[test]
    fn left_pad_clicks_are_dpad() {
        let mut mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_DPAD_LEFT, true);
        assert_ne!(state.buttons & (1 << Button::Left as u32), 0);
    }

    /// Default layout with the left pad switched to touch mode.
    fn touch_mapping() -> Mapping {
        Mapping {
            left_pad: LeftPadMode::TouchDpad,
            ..Mapping::default()
        }
    }

    fn pressed(state: &ControllerState, button: Button) -> bool {
        state.buttons & (1 << button as u32) != 0
    }

    #[test]
    fn touch_dpad_cardinals_press_one_direction() {
        let mut mapping = touch_mapping();
        let mut state = ControllerState::default();
        // Full right, slightly off-axis: still a plain Right.
        mapping.apply_abs(&mut state, ABS_HAT0X, AXIS_MAX);
        mapping.apply_abs(&mut state, ABS_HAT0Y, -5000);
        assert!(pressed(&state, Button::Right));
        for button in [Button::Up, Button::Down, Button::Left] {
            assert!(!pressed(&state, button), "{button:?}");
        }
        // Sliding to full up (evdev up = negative) hands over cleanly.
        mapping.apply_abs(&mut state, ABS_HAT0X, 0);
        mapping.apply_abs(&mut state, ABS_HAT0Y, -AXIS_MAX);
        assert!(pressed(&state, Button::Up));
        assert!(!pressed(&state, Button::Right));
    }

    #[test]
    fn touch_dpad_diagonals_press_two_directions() {
        let mut mapping = touch_mapping();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_HAT0X, 20000);
        mapping.apply_abs(&mut state, ABS_HAT0Y, -20000);
        assert!(pressed(&state, Button::Up));
        assert!(pressed(&state, Button::Right));
        assert!(!pressed(&state, Button::Down));
        assert!(!pressed(&state, Button::Left));
    }

    #[test]
    fn touch_dpad_deadzone_presses_nothing() {
        let mut mapping = touch_mapping();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_HAT0X, TOUCH_DPAD_DEADZONE - 1);
        assert_eq!(state.buttons, 0);
        // Leaving the deadzone presses, retreating into it releases.
        mapping.apply_abs(&mut state, ABS_HAT0X, TOUCH_DPAD_DEADZONE);
        assert!(pressed(&state, Button::Right));
        mapping.apply_abs(&mut state, ABS_HAT0X, 100);
        assert_eq!(state.buttons, 0);
    }

    #[test]
    fn touch_dpad_releases_all_on_untouch() {
        let mut mapping = touch_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB, true);
        mapping.apply_abs(&mut state, ABS_HAT0X, 20000);
        mapping.apply_abs(&mut state, ABS_HAT0Y, 20000);
        assert!(pressed(&state, Button::Down));
        // Untouch without the axes returning to 0 first.
        mapping.apply_key(&mut state, BTN_THUMB, false);
        assert_eq!(state.buttons, 0);
        // The stale position was also forgotten: a later single-axis event
        // sectorizes against center, not the old coordinate.
        mapping.apply_abs(&mut state, ABS_HAT0X, AXIS_MAX);
        assert!(pressed(&state, Button::Right));
        assert!(!pressed(&state, Button::Down));
    }

    #[test]
    fn touch_dpad_ignores_click_quadrants() {
        let mut mapping = touch_mapping();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_HAT0X, -AXIS_MAX);
        // A click-release while still touching must not lift the direction.
        mapping.apply_key(&mut state, BTN_DPAD_LEFT, true);
        mapping.apply_key(&mut state, BTN_DPAD_LEFT, false);
        assert!(pressed(&state, Button::Left));
    }

    #[test]
    fn click_mode_ignores_touch_position() {
        let mut mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_HAT0X, AXIS_MAX);
        mapping.apply_key(&mut state, BTN_THUMB, true);
        assert_eq!(state.buttons, 0);
    }

    #[test]
    fn both_grip_encodings_map_the_same() {
        let mut mapping = Mapping::default();
        for code in [BTN_GRIPL, BTN_GEAR_DOWN] {
            let mut state = ControllerState::default();
            mapping.apply_key(&mut state, code, true);
            assert_ne!(
                state.buttons & (1 << Button::Capture as u32),
                0,
                "{code:#x}"
            );
        }
    }

    #[test]
    fn press_and_release_round_trips() {
        let mut mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_TL2, true);
        mapping.apply_key(&mut state, BTN_TL2, false);
        assert_eq!(state.buttons, 0);
    }

    #[test]
    fn stick_scaling_covers_full_range() {
        assert_eq!(scale_x(0), 2048);
        assert_eq!(scale_x(AXIS_MAX), 4095);
        assert_eq!(scale_x(-AXIS_MAX), 1);
        // evdev up (negative) must become Switch up (max).
        assert_eq!(scale_y(-AXIS_MAX), 4095);
        assert_eq!(scale_y(AXIS_MAX), 1);
        // Out-of-range values (shouldn't happen, but) are clamped.
        assert_eq!(scale_x(i32::MAX), 4095);
    }

    #[test]
    fn joystick_moves_left_stick() {
        let mut mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_X, AXIS_MAX);
        mapping.apply_abs(&mut state, ABS_Y, -AXIS_MAX);
        assert_eq!((state.left_stick.x, state.left_stick.y), (4095, 4095));
        assert_eq!(state.right_stick.x, 2048, "right stick untouched");
    }

    #[test]
    fn right_pad_release_recenters_right_stick() {
        let mut mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, 12345);
        mapping.apply_key(&mut state, BTN_THUMB2, false);
        assert_eq!(state.right_stick.x, StickState::CENTER);
        assert_eq!(state.buttons, 0, "touch itself is not a button");
    }

    #[test]
    fn swapped_sticks_recenter_the_left_stick_on_pad_release() {
        let mut mapping = Mapping::empty();
        mapping.right_pad = StickTarget::LeftStick;
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_RX, AXIS_MAX);
        assert_eq!(state.left_stick.x, 4095, "pad drives the left stick");
        mapping.apply_key(&mut state, BTN_THUMB2, false);
        assert_eq!(state.left_stick.x, StickState::CENTER);
    }

    #[test]
    fn unbound_inputs_do_nothing() {
        let mut mapping = Mapping::empty();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_A, true);
        mapping.apply_abs(&mut state, ABS_X, AXIS_MAX);
        mapping.apply_abs(&mut state, 0x10 /* ABS_HAT0X */, AXIS_MAX);
        assert_eq!(state, ControllerState::default());
    }
}

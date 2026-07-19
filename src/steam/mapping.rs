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
//! - left-pad touch position as `ABS_HAT0X/Y` and analog triggers as
//!   `ABS_HAT2X/Y` — both currently unmapped (we use pad clicks and full
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

/// evdev axis full scale (`hid-steam` reports symmetric -32767..=32767).
const AXIS_MAX: i32 = 32767;

/// Which Switch stick a physical analog input drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickTarget {
    None,
    LeftStick,
    RightStick,
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
}

impl Mapping {
    /// A mapping with nothing bound; config files start from this.
    pub fn empty() -> Self {
        Self {
            buttons: HashMap::new(),
            joystick: StickTarget::None,
            right_pad: StickTarget::None,
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
    pub fn apply_key(&self, state: &mut ControllerState, code: u16, pressed: bool) {
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
        if let Some(&button) = self.buttons.get(&code) {
            state.set_button(button, pressed);
        }
    }

    /// Apply one `EV_ABS` event.
    pub fn apply_abs(&self, state: &mut ControllerState, code: u16, value: i32) {
        let (target, x_axis) = match code {
            ABS_X => (self.joystick, true),
            ABS_Y => (self.joystick, false),
            ABS_RX => (self.right_pad, true),
            ABS_RY => (self.right_pad, false),
            // ABS_HAT0X/Y (left-pad touch) and ABS_HAT2X/Y (analog triggers)
            // are intentionally unmapped, see module docs.
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
        let mapping = Mapping::default();
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
        let mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_DPAD_LEFT, true);
        assert_ne!(state.buttons & (1 << Button::Left as u32), 0);
    }

    #[test]
    fn both_grip_encodings_map_the_same() {
        let mapping = Mapping::default();
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
        let mapping = Mapping::default();
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
        let mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_X, AXIS_MAX);
        mapping.apply_abs(&mut state, ABS_Y, -AXIS_MAX);
        assert_eq!((state.left_stick.x, state.left_stick.y), (4095, 4095));
        assert_eq!(state.right_stick.x, 2048, "right stick untouched");
    }

    #[test]
    fn right_pad_release_recenters_right_stick() {
        let mapping = Mapping::default();
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
        let mapping = Mapping::empty();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_A, true);
        mapping.apply_abs(&mut state, ABS_X, AXIS_MAX);
        mapping.apply_abs(&mut state, 0x10 /* ABS_HAT0X */, AXIS_MAX);
        assert_eq!(state, ControllerState::default());
    }
}

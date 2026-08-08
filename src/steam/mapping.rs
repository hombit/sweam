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
//!   click mode; right-pad touch is `BTN_THUMB2` — used by
//!   [`RightPadMode::CameraStick`] and [`RightPadMode::RelativeStick`];
//!   analog triggers as `ABS_HAT2X/Y` — unmapped (we use full trigger pulls
//!   instead).
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

pub(crate) const ABS_X: u16 = 0x00;
pub(crate) const ABS_Y: u16 = 0x01;
pub(crate) const ABS_RX: u16 = 0x03;
pub(crate) const ABS_RY: u16 = 0x04;
pub(crate) const ABS_HAT0X: u16 = 0x10; // left-pad touch position
pub(crate) const ABS_HAT0Y: u16 = 0x11;
pub(crate) const ABS_HAT2X: u16 = 0x12; // right trigger, analog
pub(crate) const ABS_HAT2Y: u16 = 0x13; // left trigger, analog

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

/// Default camera-mode sensitivity: virtual stick deflection gained per unit
/// of finger travel (both in evdev units). See [`CAMERA_DECAY`] for how
/// velocity turns into a sustained deflection.
///
/// Raised 4.0 → 12.0 → 24.0 over two Switch sessions (2026-08-02); all three
/// came back "still too slow", and a fourth would too. **This knob stops
/// doing anything above ~4**: simulated on 2026-08-03 at the pump's real
/// 16 ms tick, a rightward swipe peaks at the same 85% of full deflection
/// for every speed from a slow drag to a flick, and identically at 24 and
/// 72 — the accumulated deflection saturates on the first sample and clamps.
/// What limits the mode is *duration*, not gain: the stick deflects only
/// while the finger moves, so a swipe turns you as far as that swipe's
/// travel. [`RightPadMode::RelativeStick`] exists because of this.
///
/// (The older note here blamed a sample rate below the tick rate. That was
/// wrong twice over: the pump ticks at 62.5 Hz, not the 125 Hz it assumed,
/// and the controller's ~111 packets/s therefore *outrun* it.)
/// Tune per config with `settings { "sensitivity" "N" }`.
pub(crate) const CAMERA_SENSITIVITY_DEFAULT: f32 = 24.0;

/// Fraction of the camera deflection kept per [`Mapping::tick`] (~8 ms), an
/// exponential decay with time constant ~49 ms (-8 ms / ln 0.85): the stick
/// recenters ~50 ms after the finger stops. During steady motion the
/// deflection settles at `velocity_per_tick * sensitivity / (1 - decay)`.
const CAMERA_DECAY: f32 = 0.85;

/// Decayed deflection below this snaps straight to 0 (well inside host-side
/// stick deadzones) so the exponential tail doesn't linger forever.
const CAMERA_STOP: f32 = 64.0;

/// Default [`RightPadMode::RelativeStick`] gain: full deflection after
/// dragging half the pad's radius from wherever the finger landed. Unlike
/// the camera sensitivity this genuinely changes the output — the mode is
/// displacement-based, so the stick is proportional to the drag until it
/// clamps, rather than saturating on the first sample.
pub(crate) const RELATIVE_SCALE_DEFAULT: f32 = 2.0;

/// Pass the controller's IMU axes through unchanged.
pub(crate) const IMU_AXES_IDENTITY: [(usize, bool); 3] = [(0, false), (1, false), (2, false)];

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

/// How the right trackpad drives its target stick.
// The shared `…Stick` suffix is the point, and parallels
// `LeftPadMode::{ClickDpad, TouchDpad}`: each variant names the thing it
// drives, which is what distinguishes these modes from the left pad's.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightPadMode {
    /// Touch position maps directly to stick deflection — the default.
    AbsoluteStick,
    /// Finger *motion* deflects the stick (mouse-like camera): each touch
    /// delta accumulates into a virtual deflection that decays back to
    /// center, so the stick follows finger velocity and stops when the
    /// finger stops. Steam's `joystick_camera` mode.
    CameraStick,
    /// Touch anywhere, and the stick equals the vector dragged from wherever
    /// the finger landed; it holds for as long as the finger holds it and
    /// recenters on lift.
    ///
    /// The point is *sustained* deflection, which is what neither other mode
    /// offers on a thumb-sized pad: [`Self::AbsoluteStick`] can hold a
    /// direction but only if the thumb finds the pad's true center first,
    /// and [`Self::CameraStick`] deflects only while the finger is actually
    /// moving — measured 2026-08-03, its deflection saturates at every swipe
    /// speed and every sensitivity, so a swipe turns you exactly as far as
    /// that swipe's travel and no gain can extend it. Here, holding a small
    /// offset turns continuously, with no decay to lag behind the finger.
    RelativeStick,
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
    /// How the right pad drives its stick.
    pub right_pad_mode: RightPadMode,
    /// Camera-mode gain: stick deflection per unit of finger travel.
    pub camera_sensitivity: f32,
    /// [`RightPadMode::RelativeStick`] gain: how far the drag from the touch
    /// origin is amplified. 1.0 means a drag of the pad's full radius gives
    /// full deflection; the default 2.0 asks for half that, which is a
    /// comfortable thumb reach from wherever the finger happens to land.
    pub relative_scale: f32,
    /// Whether this layout forwards the controller's motion to the Switch.
    ///
    /// This is a *requirement on the input source*, not a binding: the IMU
    /// only exists in the raw HID packets, so a layout that asks for motion
    /// can only be served by the hidraw source (see `steam::hidraw`).
    pub gyro: bool,
    /// How the controller's IMU axes map onto the Pro Controller's, as
    /// `(source axis, negate)` per output axis. Identity by default.
    ///
    /// The two devices do not share a body frame, and the mapping between
    /// them is a hardware question, so it is configurable rather than baked
    /// in: `settings { "axes" "-z,x,y" }` on the gyro group.
    pub imu_axes: [(usize, bool); 3],
    /// Extra gain on gyro rates, on top of the unit conversion. The Switch
    /// integrates three IMU frames per report assuming 5 ms spacing, which
    /// this controller cannot match (it samples at ~111 Hz), so the felt
    /// speed may need trimming.
    pub gyro_scale: f32,
    /// Last seen left-pad touch position (`ABS_HAT0X/Y`), tracked so a
    /// single-axis event can re-sectorize with both coordinates. Runtime
    /// state, only meaningful in [`LeftPadMode::TouchDpad`].
    left_pad_touch: (i32, i32),
    /// Whether the right pad is currently touched (`BTN_THUMB2`). Runtime
    /// state for [`RightPadMode::CameraStick`] and
    /// [`RightPadMode::RelativeStick`].
    right_pad_touched: bool,
    /// Right-pad reference position per axis, `None` right after a touch (or
    /// lift) so the first sample can't be measured against a stale position.
    /// What it references depends on the mode: the *previous sample* in
    /// [`RightPadMode::CameraStick`] (which wants per-sample deltas), the
    /// *touch origin* in [`RightPadMode::RelativeStick`] (which wants the
    /// whole drag).
    right_pad_reference: (Option<i32>, Option<i32>),
    /// Virtual right-pad deflection in evdev units, written out to the stick
    /// by [`Mapping::set_right_pad_stick`]. Accumulated from finger velocity
    /// and decayed by [`Mapping::tick`] in [`RightPadMode::CameraStick`];
    /// recomputed from the drag on every sample, and never decayed, in
    /// [`RightPadMode::RelativeStick`].
    right_pad_deflection: (f32, f32),
}

impl Mapping {
    /// A mapping with nothing bound; config files start from this.
    pub fn empty() -> Self {
        Self {
            buttons: HashMap::new(),
            joystick: StickTarget::None,
            right_pad: StickTarget::None,
            left_pad: LeftPadMode::ClickDpad,
            right_pad_mode: RightPadMode::AbsoluteStick,
            camera_sensitivity: CAMERA_SENSITIVITY_DEFAULT,
            relative_scale: RELATIVE_SCALE_DEFAULT,
            gyro: false,
            imu_axes: IMU_AXES_IDENTITY,
            gyro_scale: 1.0,
            left_pad_touch: (0, 0),
            right_pad_touched: false,
            right_pad_reference: (None, None),
            right_pad_deflection: (0.0, 0.0),
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
        // Right pad touched/released. In camera mode a lift zeroes the
        // deflection immediately (no drift after the finger leaves) and
        // forgets the tracked position (no stale delta on the next touch);
        // in absolute mode a lift snaps the stick back to center in case
        // the last position event didn't return to 0.
        if code == BTN_THUMB2 {
            if matches!(
                self.right_pad_mode,
                RightPadMode::CameraStick | RightPadMode::RelativeStick
            ) {
                // Only a *transition* may reset the tracked position. Sources
                // are allowed to repeat the current state on every sample —
                // the raw HID one does, once per packet — and resetting on
                // each repeat would drop `right_pad_reference` before the next
                // position could ever be compared against it, freezing the
                // camera at center (and, in relative mode, re-originating the
                // drag under the finger so it could never deflect).
                if pressed != self.right_pad_touched {
                    self.right_pad_touched = pressed;
                    self.right_pad_reference = (None, None);
                    if !pressed {
                        self.right_pad_deflection = (0.0, 0.0);
                        self.set_right_pad_stick(state);
                    }
                }
            } else if !pressed {
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
        // Right-pad position: both non-absolute modes measure it against a
        // reference rather than mapping it directly.
        if code == ABS_RX || code == ABS_RY {
            match self.right_pad_mode {
                RightPadMode::CameraStick => {
                    self.apply_camera_abs(state, code == ABS_RX, value);
                    return;
                }
                RightPadMode::RelativeStick => {
                    self.apply_relative_abs(state, code == ABS_RX, value);
                    return;
                }
                RightPadMode::AbsoluteStick => {}
            }
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

    /// Re-frame one IMU sample into the Pro Controller's axes and apply the
    /// gyro gain. Accel is remapped the same way (same physical frame) but
    /// never scaled — it carries gravity, whose magnitude must stay 1 g.
    pub fn remap_imu(&self, sample: crate::state::ImuSample) -> crate::state::ImuSample {
        let mut out = crate::state::ImuSample::default();
        for (axis, &(source, negate)) in self.imu_axes.iter().enumerate() {
            let sign = if negate { -1 } else { 1 };
            out.accel[axis] = sample.accel[source].saturating_mul(sign);
            let scaled = sample.gyro[source] as f32 * self.gyro_scale * sign as f32;
            out.gyro[axis] = scaled.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
        out
    }

    /// Advance time-based behavior by one pump iteration (~8 ms): in
    /// [`RightPadMode::CameraStick`] decay the virtual deflection toward
    /// center (see [`CAMERA_DECAY`]). Cheap no-op in every other mode and
    /// when the deflection is already centered.
    pub fn tick(&mut self, state: &mut ControllerState) {
        if self.right_pad_mode != RightPadMode::CameraStick
            || self.right_pad_deflection == (0.0, 0.0)
        {
            return;
        }
        for deflection in [
            &mut self.right_pad_deflection.0,
            &mut self.right_pad_deflection.1,
        ] {
            *deflection *= CAMERA_DECAY;
            if f32::abs(*deflection) < CAMERA_STOP {
                *deflection = 0.0;
            }
        }
        self.set_right_pad_stick(state);
    }

    /// One right-pad position sample in camera mode: the delta from the
    /// previous sample (scaled by the sensitivity) accumulates into the
    /// virtual deflection. Samples while untouched are ignored; the first
    /// sample after a touch only records the position.
    fn apply_camera_abs(&mut self, state: &mut ControllerState, x_axis: bool, value: i32) {
        if !self.right_pad_touched {
            return;
        }
        let (prev, deflection) = if x_axis {
            (
                &mut self.right_pad_reference.0,
                &mut self.right_pad_deflection.0,
            )
        } else {
            (
                &mut self.right_pad_reference.1,
                &mut self.right_pad_deflection.1,
            )
        };
        if let Some(prev_value) = *prev {
            let delta = (value - prev_value) as f32 * self.camera_sensitivity;
            *deflection = f32::clamp(*deflection + delta, -(AXIS_MAX as f32), AXIS_MAX as f32);
        }
        *prev = Some(value);
        self.set_right_pad_stick(state);
    }

    /// One right-pad position sample in relative mode: the stick is the
    /// offset from the touch origin, scaled and clamped. The first sample
    /// after a touch *is* the origin and deflects nothing, so the stick
    /// never jumps to wherever the thumb happened to land.
    ///
    /// No decay and no accumulation: the deflection is a pure function of
    /// where the finger is now versus where it started, so it holds while
    /// held and cannot lag behind the finger.
    fn apply_relative_abs(&mut self, state: &mut ControllerState, x_axis: bool, value: i32) {
        if !self.right_pad_touched {
            return;
        }
        let (origin, deflection) = if x_axis {
            (
                &mut self.right_pad_reference.0,
                &mut self.right_pad_deflection.0,
            )
        } else {
            (
                &mut self.right_pad_reference.1,
                &mut self.right_pad_deflection.1,
            )
        };
        match *origin {
            Some(origin) => {
                let dragged = (value - origin) as f32 * self.relative_scale;
                *deflection = f32::clamp(dragged, -(AXIS_MAX as f32), AXIS_MAX as f32);
            }
            None => *origin = Some(value),
        }
        self.set_right_pad_stick(state);
    }

    /// Write the camera deflection out to whatever stick the right pad
    /// drives.
    fn set_right_pad_stick(&self, state: &mut ControllerState) {
        let stick = match self.right_pad {
            StickTarget::LeftStick => &mut state.left_stick,
            StickTarget::RightStick => &mut state.right_stick,
            StickTarget::None => return,
        };
        stick.x = scale_x(self.right_pad_deflection.0 as i32);
        stick.y = scale_y(self.right_pad_deflection.1 as i32);
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

    /// Default layout with the right pad switched to camera mode.
    fn camera_mapping() -> Mapping {
        Mapping {
            right_pad_mode: RightPadMode::CameraStick,
            ..Mapping::default()
        }
    }

    #[test]
    fn camera_motion_deflects_toward_the_motion() {
        let mut mapping = camera_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        // Rightward finger motion → stick right; upward (evdev negative Y)
        // → stick up (Switch max).
        mapping.apply_abs(&mut state, ABS_RX, 1000);
        mapping.apply_abs(&mut state, ABS_RY, 2000);
        mapping.apply_abs(&mut state, ABS_RX, 4000);
        mapping.apply_abs(&mut state, ABS_RY, -3000);
        assert!(state.right_stick.x > StickState::CENTER);
        assert!(state.right_stick.y > StickState::CENTER);
        assert_eq!(
            state.left_stick.x,
            StickState::CENTER,
            "left stick untouched"
        );
        // Leftward motion pulls the deflection back down.
        let was = state.right_stick.x;
        mapping.apply_abs(&mut state, ABS_RX, 3000);
        assert!(state.right_stick.x < was);
    }

    #[test]
    fn camera_tolerates_the_touch_flag_repeated_on_every_sample() {
        // Regression: the raw HID source reports the touch bit in *every*
        // packet, where evdev only reports transitions. Resetting the
        // tracked position on each repeat froze the camera at center —
        // the pad clicked fine but never moved the stick (hardware,
        // 2026-08-02).
        let mut mapping = camera_mapping();
        let mut state = ControllerState::default();
        for x in [0, 1000, 2000, 3000, 4000] {
            // Packet order: positions first, then the button bits.
            mapping.apply_abs(&mut state, ABS_RX, x);
            mapping.apply_key(&mut state, BTN_THUMB2, true);
        }
        assert!(
            state.right_stick.x > StickState::CENTER,
            "steady rightward motion must deflect the stick, got {}",
            state.right_stick.x
        );
    }

    #[test]
    fn right_pad_deflection_decays_to_center() {
        let mut mapping = camera_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, 0);
        mapping.apply_abs(&mut state, ABS_RX, 5000);
        let mut last = state.right_stick.x;
        assert!(last > StickState::CENTER);
        // Finger holds still: each tick moves the stick monotonically back
        // toward center, reaching it exactly (snap-to-zero) within ~1 s.
        for _ in 0..125 {
            mapping.tick(&mut state);
            assert!(state.right_stick.x <= last);
            last = state.right_stick.x;
        }
        assert_eq!(state.right_stick.x, StickState::CENTER);
        assert_eq!(state.right_stick.y, StickState::CENTER);
    }

    #[test]
    fn camera_release_zeroes_immediately_without_stale_delta() {
        let mut mapping = camera_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, 0);
        mapping.apply_abs(&mut state, ABS_RX, 20000);
        assert!(state.right_stick.x > StickState::CENTER);
        // Lift: centered right away, no decay needed.
        mapping.apply_key(&mut state, BTN_THUMB2, false);
        assert_eq!(state.right_stick, StickState::default());
        // Retouch far from the old position: the first sample must not
        // replay the jump from the stale coordinate.
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, -20000);
        assert_eq!(state.right_stick, StickState::default());
        // But the second sample moves as usual.
        mapping.apply_abs(&mut state, ABS_RX, -15000);
        assert!(state.right_stick.x > StickState::CENTER);
    }

    #[test]
    fn camera_ignores_position_while_untouched() {
        let mut mapping = camera_mapping();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_RX, 10000);
        mapping.apply_abs(&mut state, ABS_RX, 20000);
        assert_eq!(state.right_stick, StickState::default());
    }

    #[test]
    fn absolute_mode_is_unaffected_by_camera_machinery() {
        let mut mapping = Mapping::default();
        let mut state = ControllerState::default();
        mapping.apply_abs(&mut state, ABS_RX, AXIS_MAX);
        assert_eq!(state.right_stick.x, 4095, "position maps absolutely");
        // tick is a no-op outside camera mode.
        mapping.tick(&mut state);
        assert_eq!(state.right_stick.x, 4095);
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

    fn relative_mapping() -> Mapping {
        Mapping {
            right_pad_mode: RightPadMode::RelativeStick,
            ..Mapping::default()
        }
    }

    /// Touch down, drag, and the stick is the drag — not the landing spot.
    #[test]
    fn relative_stick_measures_the_drag_not_the_touch_position() {
        let mut mapping = relative_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        // Landing far from the pad center must deflect nothing: this is the
        // whole point of the mode over AbsoluteStick.
        mapping.apply_abs(&mut state, ABS_RX, 20000);
        mapping.apply_abs(&mut state, ABS_RY, -20000);
        assert_eq!(state.right_stick.x, StickState::CENTER);
        assert_eq!(state.right_stick.y, StickState::CENTER);
        // A quarter-radius drag right at the default scale of 2.0 is half
        // deflection, measured from where the finger landed.
        mapping.apply_abs(&mut state, ABS_RX, 20000 + AXIS_MAX / 4);
        assert_eq!(state.right_stick.x, scale_x(AXIS_MAX / 2));
        assert!(state.right_stick.x > StickState::CENTER);
    }

    /// The property the velocity camera cannot offer: a held finger keeps
    /// the stick deflected indefinitely, so the turn continues.
    #[test]
    fn relative_stick_holds_deflection_while_the_finger_holds_still() {
        let mut mapping = relative_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, 0);
        mapping.apply_abs(&mut state, ABS_RX, 8000);
        let deflected = state.right_stick.x;
        assert!(deflected > StickState::CENTER);
        // Two seconds of pump iterations with no new motion.
        for _ in 0..250 {
            mapping.tick(&mut state);
        }
        assert_eq!(
            state.right_stick.x, deflected,
            "relative mode must not decay: the finger is still holding"
        );
    }

    #[test]
    fn relative_stick_recenters_on_lift_and_reorigins_on_the_next_touch() {
        let mut mapping = relative_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, 0);
        mapping.apply_abs(&mut state, ABS_RX, 8000);
        assert!(state.right_stick.x > StickState::CENTER);
        mapping.apply_key(&mut state, BTN_THUMB2, false);
        assert_eq!(state.right_stick.x, StickState::CENTER);
        // Touching down again elsewhere must not resurrect the old origin
        // (which would fling the stick to the difference between touches).
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, -25000);
        assert_eq!(state.right_stick.x, StickState::CENTER);
    }

    /// Same regression the camera mode hit: the raw HID source repeats the
    /// touch bit on every packet, and re-originating on each repeat would
    /// pin the stick to center forever.
    #[test]
    fn relative_stick_survives_a_repeated_touch_bit() {
        let mut mapping = relative_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, 0);
        for step in 1..=4 {
            // The source re-asserts "touched" alongside every position.
            mapping.apply_key(&mut state, BTN_THUMB2, true);
            mapping.apply_abs(&mut state, ABS_RX, step * 2000);
        }
        assert_eq!(state.right_stick.x, scale_x(8000 * 2));
    }

    #[test]
    fn relative_stick_clamps_at_full_deflection() {
        let mut mapping = relative_mapping();
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RY, 0);
        // Drag up (evdev up is negative) far past what the scale needs.
        mapping.apply_abs(&mut state, ABS_RY, -AXIS_MAX);
        assert_eq!(state.right_stick.y, 4095, "up is full deflection");
    }

    /// A layout that puts the pad on the left stick must recenter that one.
    #[test]
    fn relative_stick_follows_the_configured_target() {
        let mut mapping = relative_mapping();
        mapping.right_pad = StickTarget::LeftStick;
        let mut state = ControllerState::default();
        mapping.apply_key(&mut state, BTN_THUMB2, true);
        mapping.apply_abs(&mut state, ABS_RX, 0);
        mapping.apply_abs(&mut state, ABS_RX, 9000);
        assert!(state.left_stick.x > StickState::CENTER);
        assert_eq!(state.right_stick.x, StickState::CENTER);
        mapping.apply_key(&mut state, BTN_THUMB2, false);
        assert_eq!(state.left_stick.x, StickState::CENTER);
    }
}

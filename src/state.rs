//! Controller-agnostic intermediate state.
//!
//! The Steam Controller side writes into [`ControllerState`], the Switch side
//! packs it into Pro Controller input reports. Axis ranges and button bit
//! positions follow the Pro Controller "standard input report" (0x30) layout,
//! see dekuNukem/Nintendo_Switch_Reverse_Engineering,
//! `bluetooth_hid_notes.md` → "Standard input report format".

/// One analog stick. 12-bit values, `0..=4095`, center is `2048`.
///
/// The Switch applies per-controller calibration on top of these raw values;
/// we emulate the calibration data it reads from "SPI flash" (see
/// `switch::protocol`), so the effective range is whatever that calibration
/// blob declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StickState {
    pub x: u16,
    pub y: u16,
}

impl StickState {
    pub const CENTER: u16 = 2048;
}

impl Default for StickState {
    fn default() -> Self {
        Self {
            x: Self::CENTER,
            y: Self::CENTER,
        }
    }
}

/// Buttons as bit indices into the 3 button bytes of the 0x30 input report
/// (byte 0 = right buttons, byte 1 = shared, byte 2 = left buttons).
///
/// SL/SR exist in the report layout (they are Joy-Con buttons) but a Pro
/// Controller never sets them, so they are omitted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[rustfmt::skip]
pub enum Button {
    // Byte 0: right side
    Y = 0, X = 1, B = 2, A = 3, R = 6, ZR = 7,
    // Byte 1: shared
    Minus = 8, Plus = 9, RStick = 10, LStick = 11, Home = 12, Capture = 13,
    // Byte 2: left side
    Down = 16, Up = 17, Right = 18, Left = 19, L = 22, ZL = 23,
}

impl Button {
    /// Every button, for iterating (e.g. decoding a report back to names).
    #[rustfmt::skip]
    pub const ALL: [Button; 18] = [
        Button::Y, Button::X, Button::B, Button::A, Button::R, Button::ZR,
        Button::Minus, Button::Plus, Button::RStick, Button::LStick,
        Button::Home, Button::Capture,
        Button::Down, Button::Up, Button::Right, Button::Left,
        Button::L, Button::ZL,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Button::Y => "Y",
            Button::X => "X",
            Button::B => "B",
            Button::A => "A",
            Button::R => "R",
            Button::ZR => "ZR",
            Button::Minus => "Minus",
            Button::Plus => "Plus",
            Button::RStick => "RStick",
            Button::LStick => "LStick",
            Button::Home => "Home",
            Button::Capture => "Capture",
            Button::Down => "Down",
            Button::Up => "Up",
            Button::Right => "Right",
            Button::Left => "Left",
            Button::L => "L",
            Button::ZL => "ZL",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerState {
    /// Bitmask indexed by [`Button`]; only the low 24 bits are used.
    pub buttons: u32,
    pub left_stick: StickState,
    pub right_stick: StickState,
    // TODO(phase 6): IMU state (gyro/accel samples) for motion passthrough.
}

impl ControllerState {
    pub fn set_button(&mut self, button: Button, pressed: bool) {
        let bit = 1 << (button as u32);
        if pressed {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
    }

    /// The three button bytes of the standard input report, in wire order.
    pub fn button_bytes(&self) -> [u8; 3] {
        [
            (self.buttons & 0xFF) as u8,
            ((self.buttons >> 8) & 0xFF) as u8,
            ((self.buttons >> 16) & 0xFF) as u8,
        ]
    }

    /// Human-readable one-liner for the check subcommands, e.g.
    /// `buttons=A+ZL L=(2048,2048) R=(2048,2048)`.
    pub fn describe(&self) -> String {
        let names: Vec<&str> = Button::ALL
            .iter()
            .filter(|&&button| self.buttons & (1 << button as u32) != 0)
            .map(|button| button.name())
            .collect();
        let buttons = if names.is_empty() {
            "none".to_owned()
        } else {
            names.join("+")
        };
        format!(
            "buttons={buttons} L=({},{}) R=({},{})",
            self.left_stick.x, self.left_stick.y, self.right_stick.x, self.right_stick.y
        )
    }
}

//! Pro Controller HID report descriptor and input report packing.
//!
//! References:
//! - dekuNukem/Nintendo_Switch_Reverse_Engineering: `bluetooth_hid_notes.md`
//!   (report layouts are the same over USB, minus the extra 0x80 framing)
//!   and `USB-HID-Notes.md`.
//! - mzyy94's known-good USB gadget setup:
//!   <https://gist.github.com/mzyy94/60ae253a45e2759451789a117c59acf9>

use crate::state::ControllerState;

/// The real Pro Controller USB HID report descriptor (203 bytes), taken
/// verbatim from mzyy94's gadget script (see module docs). Declares input
/// reports 0x30/0x21/0x81/0x3F and output reports 0x01/0x10/0x80/0x82.
#[rustfmt::skip]
pub const HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x15, 0x00, 0x09, 0x04, 0xA1, 0x01, 0x85, 0x30, 0x05, 0x01,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x0A, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01,
    0x95, 0x0A, 0x55, 0x00, 0x65, 0x00, 0x81, 0x02, 0x05, 0x09, 0x19, 0x0B,
    0x29, 0x0E, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x04, 0x81, 0x02,
    0x75, 0x01, 0x95, 0x02, 0x81, 0x03, 0x0B, 0x01, 0x00, 0x01, 0x00, 0xA1,
    0x00, 0x0B, 0x30, 0x00, 0x01, 0x00, 0x0B, 0x31, 0x00, 0x01, 0x00, 0x0B,
    0x32, 0x00, 0x01, 0x00, 0x0B, 0x35, 0x00, 0x01, 0x00, 0x15, 0x00, 0x27,
    0xFF, 0xFF, 0x00, 0x00, 0x75, 0x10, 0x95, 0x04, 0x81, 0x02, 0xC0, 0x0B,
    0x39, 0x00, 0x01, 0x00, 0x15, 0x00, 0x25, 0x07, 0x35, 0x00, 0x46, 0x3B,
    0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x02, 0x05, 0x09, 0x19,
    0x0F, 0x29, 0x12, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x04, 0x81,
    0x02, 0x75, 0x08, 0x95, 0x34, 0x81, 0x03, 0x06, 0x00, 0xFF, 0x85, 0x21,
    0x09, 0x01, 0x75, 0x08, 0x95, 0x3F, 0x81, 0x03, 0x85, 0x81, 0x09, 0x02,
    0x75, 0x08, 0x95, 0x3F, 0x81, 0x03, 0x85, 0x01, 0x09, 0x03, 0x75, 0x08,
    0x95, 0x3F, 0x91, 0x83, 0x85, 0x10, 0x09, 0x04, 0x75, 0x08, 0x95, 0x3F,
    0x91, 0x83, 0x85, 0x80, 0x09, 0x05, 0x75, 0x08, 0x95, 0x3F, 0x91, 0x83,
    0x85, 0x82, 0x09, 0x06, 0x75, 0x08, 0x95, 0x3F, 0x91, 0x83, 0xC0,
];

/// All USB reports are 64 bytes.
pub const REPORT_LENGTH: usize = 64;

/// One 64-byte report, padded with zeros.
pub type Report = [u8; REPORT_LENGTH];

/// Pack a 12-bit stick pair into the 3-byte wire format.
fn pack_stick(x: u16, y: u16) -> [u8; 3] {
    [
        (x & 0xFF) as u8,
        (((x >> 8) & 0x0F) | ((y & 0x0F) << 4)) as u8,
        ((y >> 4) & 0xFF) as u8,
    ]
}

/// Inverse of [`pack_stick`], as `hid_field_extract` does it in
/// hid-nintendo.c: x = 12 bits from byte 0, y = 12 bits from byte 1 bit 4.
pub fn unpack_stick(data: &[u8]) -> (u16, u16) {
    let x = data[0] as u16 | ((data[1] as u16 & 0x0F) << 8);
    let y = (data[1] as u16 >> 4) | ((data[2] as u16) << 4);
    (x, y)
}

/// The 11-byte input-state block shared by 0x30 and 0x21 reports (bytes
/// 2..13): battery/connection, buttons, sticks, vibrator ack
/// (`joycon_input_report` in hid-nintendo.c, minus id and timer).
pub fn input_state_bytes(state: &ControllerState) -> [u8; 11] {
    let mut bytes = [0u8; 11];
    // Battery/connection: high 3 bits = level (4 = full), bit 0 = host
    // powered. 0x81 is what mzyy94's Switch-proven simulator reports.
    bytes[0] = 0x81;
    bytes[1..4].copy_from_slice(&state.button_bytes());
    bytes[4..7].copy_from_slice(&pack_stick(state.left_stick.x, state.left_stick.y));
    bytes[7..10].copy_from_slice(&pack_stick(state.right_stick.x, state.right_stick.y));
    bytes[10] = 0x00; // Vibrator input report (rumble ack heuristic)
    bytes
}

/// Build a standard full input report (ID 0x30).
///
/// `timer` is a free-running counter the real controller increments per
/// report (wrapping); the Switch uses it to detect stalls.
///
/// TODO(phase 6): fill IMU sample fields (bytes 13..49) — zeros mean
/// "no motion", which is fine until then.
pub fn standard_input_report(state: &ControllerState, timer: u8) -> Report {
    let mut report = [0u8; REPORT_LENGTH];
    report[0] = 0x30; // Report ID
    report[1] = timer;
    report[2..13].copy_from_slice(&input_state_bytes(state));
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Button, StickState};

    #[test]
    fn stick_packing_round_trips() {
        for (x, y) in [(2048, 2048), (0, 4095), (4095, 0), (123, 3210)] {
            assert_eq!(unpack_stick(&pack_stick(x, y)), (x, y));
        }
    }

    #[test]
    fn centered_stick_wire_format() {
        assert_eq!(pack_stick(2048, 2048), [0x00, 0x08, 0x80]);
    }

    #[test]
    fn standard_report_layout() {
        let mut state = ControllerState::default();
        state.set_button(Button::A, true); // right-buttons byte, bit 3
        state.set_button(Button::ZL, true); // left-buttons byte, bit 7
        state.left_stick = StickState { x: 0, y: 4095 };

        let report = standard_input_report(&state, 42);
        assert_eq!(report[0], 0x30);
        assert_eq!(report[1], 42);
        assert_eq!(report[3], 0x08, "A in right-buttons byte");
        assert_eq!(report[4], 0x00, "shared byte empty");
        assert_eq!(report[5], 0x80, "ZL in left-buttons byte");
        assert_eq!(unpack_stick(&report[6..9]), (0, 4095));
        assert_eq!(unpack_stick(&report[9..12]), (2048, 2048));
        assert!(report[13..].iter().all(|&b| b == 0), "IMU zeroed for now");
    }
}

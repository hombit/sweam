//! Steam Controller raw HID packet decoding (dongle 28de:1142, wired
//! 28de:1102).
//!
//! Every packet is 64 bytes: `01 00 <type> <payload len> <payload…>`. This
//! module turns a type-`0x01` input packet into exactly the `EV_KEY`/`EV_ABS`
//! events the kernel `hid-steam` driver would have emitted for it, so
//! [`mapping::Mapping`] — and therefore every VDF config — works the same
//! whether input arrives over evdev or raw hidraw.
//!
//! Provenance: the packet layout, scales and IMU-mode constants come from
//! the permissively licensed descriptions of this hardware — SDL's
//! `controller_structs.h` (zlib) and ynsta/steamcontroller (MIT) — as
//! recorded in `Notes.md` ("Steam Controller IMU over hidraw"). The byte
//! offsets and button bit numbers were additionally confirmed against the
//! field table in GPL-2 `hid-steam.c`; those are interface facts about the
//! device, and no driver code or text is reproduced here. sweam links
//! nothing from the driver — it only reads the device nodes the kernel
//! exposes.
//!
//! Two quirks are inherited from that table deliberately:
//!
//! - **X/Y at offsets 16/18 are multiplexed.** They carry either the joystick
//!   or the left-pad touch position, selected by two bits: `lpad_touched`
//!   (10.3) says the values are pad coordinates, `lpad_and_joy` (10.7) says
//!   both controls are in use at once. The idle control is reported as
//!   centered so a mapping never sees a stale position.
//! - **Y axes are negated** (evdev up is negative, the controller reports up
//!   as positive), matching what the driver hands userspace.
//!
//! Wireless caveat: the `hid-steam` table marks accelerometer values (and the
//! 16-bit trigger/uncalibrated fields) as "not sent through wireless", while
//! gyro and quaternion are unmarked — expect gyro over the dongle and treat
//! accel as unverified until hardware says otherwise.

use super::mapping;
use crate::state::ImuSample;

/// Every Steam Controller HID packet is this long, in both directions.
pub const PACKET_LEN: usize = 64;

const TYPE_INPUT: u8 = 0x01;
const TYPE_CONNECT: u8 = 0x03;
const TYPE_BATTERY: u8 = 0x04;

/// Gyro full scale: ±2000 dps mapped onto a full `i16`, i.e. LSB per dps.
const SC_GYRO_PER_DPS: f32 = 32768.0 / 2000.0;
/// Accelerometer full scale: ±2 g mapped onto a full `i16`, i.e. LSB per g.
const SC_ACCEL_PER_G: f32 = 32768.0 / 2.0;

/// One decoded event, in the kernel's `input-event-codes.h` vocabulary —
/// the same `(code, value)` pairs [`mapping::Mapping`] consumes from evdev.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Key(u16, bool),
    Abs(u16, i32),
}

/// What a wireless slot reports when the controller comes or goes
/// (type `0x03`, payload byte 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectEvent {
    Disconnected,
    Connected,
    Paired,
    Unknown(u8),
}

/// A decoded packet; anything we don't model is [`Packet::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet {
    Input(Input),
    Connect(ConnectEvent),
    Battery { millivolts: u16, percent: u8 },
    Other,
}

/// A type-`0x01` input packet: the whole controller state in one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Input {
    /// Packet bytes 8, 9 and 10 — the 24 button bits.
    pub buttons: [u8; 3],
    pub left_trigger: u8,
    pub right_trigger: u8,
    /// Bytes 16/18: joystick *or* left-pad position, see [`Input::lpad_touched`].
    pub xy: (i16, i16),
    pub right_pad: (i16, i16),
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
}

impl Input {
    /// Whether bytes 16/18 hold a left-pad touch position rather than the
    /// joystick (bit 10.3).
    pub fn lpad_touched(&self) -> bool {
        self.buttons[2] & (1 << 3) != 0
    }

    /// Whether pad and joystick are being used simultaneously (bit 10.7) —
    /// then 16/18 are the joystick and the pad position is elsewhere.
    pub fn lpad_and_joy(&self) -> bool {
        self.buttons[2] & (1 << 7) != 0
    }

    /// The events `hid-steam` would emit for this packet, in its order:
    /// analog triggers, the multiplexed X/Y pair, the right pad, then keys.
    pub fn events(&self) -> Vec<Event> {
        let (lpad_touched, lpad_and_joy) = (self.lpad_touched(), self.lpad_and_joy());
        let (x, y) = (self.xy.0 as i32, -(self.xy.1 as i32));
        let mut events = vec![
            Event::Abs(mapping::ABS_HAT2Y, self.left_trigger as i32),
            Event::Abs(mapping::ABS_HAT2X, self.right_trigger as i32),
        ];

        // One pair of coordinates, two possible owners: report it to the
        // active one and re-center the other, exactly as the driver does.
        if lpad_touched {
            events.push(Event::Abs(mapping::ABS_HAT0X, x));
            events.push(Event::Abs(mapping::ABS_HAT0Y, y));
            if !lpad_and_joy {
                events.push(Event::Abs(mapping::ABS_X, 0));
                events.push(Event::Abs(mapping::ABS_Y, 0));
            }
        } else {
            events.push(Event::Abs(mapping::ABS_X, x));
            events.push(Event::Abs(mapping::ABS_Y, y));
            if !lpad_and_joy {
                events.push(Event::Abs(mapping::ABS_HAT0X, 0));
                events.push(Event::Abs(mapping::ABS_HAT0Y, 0));
            }
        }

        events.push(Event::Abs(mapping::ABS_RX, self.right_pad.0 as i32));
        events.push(Event::Abs(mapping::ABS_RY, -(self.right_pad.1 as i32)));

        let [b8, b9, b10] = self.buttons;
        for (code, byte, bit) in [
            (mapping::BTN_TR2, b8, 0),
            (mapping::BTN_TL2, b8, 1),
            (mapping::BTN_TR, b8, 2),
            (mapping::BTN_TL, b8, 3),
            (mapping::BTN_Y, b8, 4),
            (mapping::BTN_B, b8, 5),
            (mapping::BTN_X, b8, 6),
            (mapping::BTN_A, b8, 7),
            (mapping::BTN_DPAD_UP, b9, 0),
            (mapping::BTN_DPAD_RIGHT, b9, 1),
            (mapping::BTN_DPAD_LEFT, b9, 2),
            (mapping::BTN_DPAD_DOWN, b9, 3),
            (mapping::BTN_SELECT, b9, 4),
            (mapping::BTN_MODE, b9, 5),
            (mapping::BTN_START, b9, 6),
            (mapping::BTN_GEAR_DOWN, b9, 7),
            (mapping::BTN_GEAR_UP, b10, 0),
            (mapping::BTN_THUMBR, b10, 2),
            (mapping::BTN_THUMB2, b10, 4),
            (mapping::BTN_THUMBL, b10, 6),
        ] {
            events.push(Event::Key(code, byte & (1 << bit) != 0));
        }
        // Left-pad touch is the one key that isn't a plain bit: either bit
        // means a finger is on the pad.
        events.push(Event::Key(mapping::BTN_THUMB, lpad_touched || lpad_and_joy));
        events
    }

    /// This packet's motion in Pro Controller raw units, ready for
    /// [`crate::state::ControllerState::imu`].
    ///
    /// Both devices use ±2000 dps for gyro, so that conversion is nearly a
    /// no-op; accel differs (±2 g here, ±8 g there) and scales by 1/4.
    ///
    /// TODO(phase 6, hardware): axis order/signs are passed through
    /// unchanged. The Pro Controller's frame almost certainly differs from
    /// the Steam Controller's — needs a real Switch to tune.
    pub fn imu_sample(&self) -> ImuSample {
        ImuSample {
            accel: self
                .accel
                .map(|raw| rescale(raw, SC_ACCEL_PER_G, ImuSample::ACCEL_PER_G)),
            gyro: self
                .gyro
                .map(|raw| rescale(raw, SC_GYRO_PER_DPS, ImuSample::GYRO_PER_DPS)),
        }
    }
}

/// Reinterpret a raw reading from one full-scale range into another,
/// saturating rather than wrapping at the edges.
fn rescale(raw: i16, from_per_unit: f32, to_per_unit: f32) -> i16 {
    let converted = raw as f32 / from_per_unit * to_per_unit;
    converted.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

/// Decode one 64-byte packet. Returns `None` if it is too short or does not
/// carry the `01 00` report-version header.
pub fn parse(data: &[u8]) -> Option<Packet> {
    if data.len() < PACKET_LEN || data[0] != 0x01 || data[1] != 0x00 {
        return None;
    }
    let le16 = |offset: usize| i16::from_le_bytes([data[offset], data[offset + 1]]);
    match data[2] {
        TYPE_INPUT => Some(Packet::Input(Input {
            buttons: [data[8], data[9], data[10]],
            left_trigger: data[11],
            right_trigger: data[12],
            xy: (le16(16), le16(18)),
            right_pad: (le16(20), le16(22)),
            accel: [le16(28), le16(30), le16(32)],
            gyro: [le16(34), le16(36), le16(38)],
        })),
        TYPE_CONNECT => Some(Packet::Connect(match data[4] {
            0x01 => ConnectEvent::Disconnected,
            0x02 => ConnectEvent::Connected,
            0x03 => ConnectEvent::Paired,
            other => ConnectEvent::Unknown(other),
        })),
        TYPE_BATTERY => Some(Packet::Battery {
            millivolts: u16::from_le_bytes([data[12], data[13]]),
            percent: data[14],
        }),
        _ => Some(Packet::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal type-0x01 packet with the given field writes applied.
    fn input_packet(writes: &[(usize, u8)]) -> [u8; PACKET_LEN] {
        let mut packet = [0u8; PACKET_LEN];
        packet[0..4].copy_from_slice(&[0x01, 0x00, TYPE_INPUT, 60]);
        for &(offset, value) in writes {
            packet[offset] = value;
        }
        packet
    }

    fn parse_input(packet: &[u8]) -> Input {
        match parse(packet) {
            Some(Packet::Input(input)) => input,
            other => panic!("expected an input packet, got {other:?}"),
        }
    }

    #[test]
    fn rejects_short_or_foreign_packets() {
        assert_eq!(parse(&[0x01, 0x00, TYPE_INPUT]), None);
        let mut packet = input_packet(&[]);
        packet[0] = 0x02;
        assert_eq!(parse(&packet), None);
    }

    #[test]
    fn buttons_decode_to_the_hid_steam_bits() {
        // 8.7 = A, 9.5 = Steam logo, 10.6 = joystick click.
        let input = parse_input(&input_packet(&[(8, 1 << 7), (9, 1 << 5), (10, 1 << 6)]));
        let events = input.events();
        assert!(events.contains(&Event::Key(mapping::BTN_A, true)));
        assert!(events.contains(&Event::Key(mapping::BTN_MODE, true)));
        assert!(events.contains(&Event::Key(mapping::BTN_THUMBL, true)));
        assert!(events.contains(&Event::Key(mapping::BTN_B, false)));
    }

    #[test]
    fn triggers_and_right_pad_are_reported() {
        let mut packet = input_packet(&[(11, 200), (12, 100)]);
        packet[20..24].copy_from_slice(&[0x00, 0x40, 0x00, 0x40]); // 16384, 16384
        let events = parse_input(&packet).events();
        assert!(events.contains(&Event::Abs(mapping::ABS_HAT2Y, 200)));
        assert!(events.contains(&Event::Abs(mapping::ABS_HAT2X, 100)));
        assert!(events.contains(&Event::Abs(mapping::ABS_RX, 16384)));
        // Y is negated on the way out, like the driver does.
        assert!(events.contains(&Event::Abs(mapping::ABS_RY, -16384)));
    }

    #[test]
    fn xy_goes_to_the_joystick_when_the_pad_is_untouched() {
        let mut packet = input_packet(&[]);
        packet[16..20].copy_from_slice(&[0x00, 0x40, 0x00, 0x40]);
        let events = parse_input(&packet).events();
        assert!(events.contains(&Event::Abs(mapping::ABS_X, 16384)));
        assert!(events.contains(&Event::Abs(mapping::ABS_Y, -16384)));
        // …and the pad is re-centered so a stale touch can't linger.
        assert!(events.contains(&Event::Abs(mapping::ABS_HAT0X, 0)));
        assert!(events.contains(&Event::Key(mapping::BTN_THUMB, false)));
    }

    #[test]
    fn xy_goes_to_the_left_pad_when_touched() {
        let mut packet = input_packet(&[(10, 1 << 3)]); // lpad_touched
        packet[16..20].copy_from_slice(&[0x00, 0x40, 0x00, 0x40]);
        let events = parse_input(&packet).events();
        assert!(events.contains(&Event::Abs(mapping::ABS_HAT0X, 16384)));
        assert!(events.contains(&Event::Abs(mapping::ABS_HAT0Y, -16384)));
        assert!(events.contains(&Event::Abs(mapping::ABS_X, 0)));
        assert!(events.contains(&Event::Key(mapping::BTN_THUMB, true)));
    }

    #[test]
    fn pad_and_joystick_together_leave_both_alone() {
        // lpad_and_joy without lpad_touched: X/Y are the joystick, and the
        // pad must NOT be re-centered (its position arrives separately).
        let mut packet = input_packet(&[(10, 1 << 7)]);
        packet[16..20].copy_from_slice(&[0x00, 0x40, 0x00, 0x00]);
        let events = parse_input(&packet).events();
        assert!(events.contains(&Event::Abs(mapping::ABS_X, 16384)));
        assert!(!events.contains(&Event::Abs(mapping::ABS_HAT0X, 0)));
        assert!(events.contains(&Event::Key(mapping::BTN_THUMB, true)));
    }

    #[test]
    fn gyro_converts_to_pro_units_nearly_one_to_one() {
        let mut packet = input_packet(&[]);
        // +100 dps on X at the Steam Controller's 16.384 LSB/dps.
        let raw = (100.0 * SC_GYRO_PER_DPS) as i16;
        packet[34..36].copy_from_slice(&raw.to_le_bytes());
        let sample = parse_input(&packet).imu_sample();
        let dps = sample.gyro[0] as f32 / ImuSample::GYRO_PER_DPS;
        assert!((dps - 100.0).abs() < 0.5, "got {dps} dps");
    }

    #[test]
    fn accel_rescales_from_2g_to_8g_range() {
        let mut packet = input_packet(&[]);
        // 1 g on Z: 16384 raw here should read as 4096 in Pro units.
        packet[32..34].copy_from_slice(&(SC_ACCEL_PER_G as i16).to_le_bytes());
        let sample = parse_input(&packet).imu_sample();
        assert_eq!(sample.accel[2], ImuSample::ACCEL_PER_G as i16);
    }

    #[test]
    fn extreme_imu_values_saturate_instead_of_wrapping() {
        let mut packet = input_packet(&[]);
        packet[34..36].copy_from_slice(&i16::MAX.to_le_bytes());
        let sample = parse_input(&packet).imu_sample();
        assert!(sample.gyro[0] > 32000, "got {}", sample.gyro[0]);
    }

    #[test]
    fn connect_and_battery_packets_decode() {
        let mut packet = [0u8; PACKET_LEN];
        packet[0..4].copy_from_slice(&[0x01, 0x00, TYPE_CONNECT, 1]);
        packet[4] = 0x02;
        assert_eq!(
            parse(&packet),
            Some(Packet::Connect(ConnectEvent::Connected))
        );

        let mut packet = [0u8; PACKET_LEN];
        packet[0..4].copy_from_slice(&[0x01, 0x00, TYPE_BATTERY, 11]);
        packet[12..14].copy_from_slice(&3000u16.to_le_bytes());
        packet[14] = 88;
        assert_eq!(
            parse(&packet),
            Some(Packet::Battery {
                millivolts: 3000,
                percent: 88,
            })
        );
    }
}

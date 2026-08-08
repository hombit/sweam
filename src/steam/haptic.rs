//! Driving the Steam Controller's haptic actuators.
//!
//! The controller has one actuator under each trackpad. They are *piezo*
//! transducers, not rumble motors, and the command drives them with a square
//! wave: `count` cycles of `on_us` microseconds energised followed by
//! `off_us` microseconds idle. Pitch is `1 / (on_us + off_us)`; loudness
//! comes from the duty cycle, peaking near 50%.
//!
//! Command format (feature report `0x8f`), packed as
//! `<BBBHHH` — id, payload length, side, then the three u16 fields:
//!
//! ```text
//! byte 0    0x8f   TRIGGER_HAPTIC_PULSE
//! byte 1    0x07   payload length
//! byte 2    side   0 = right, 1 = left
//! bytes 3-4 on_us   u16 LE, microseconds energised
//! bytes 5-6 off_us  u16 LE, microseconds idle
//! bytes 7-8 count   u16 LE, cycles to play
//! ```
//!
//! **The field names are ours, and they are the point.** ynsta's
//! steamcontroller (MIT), which is where the packing comes from, calls the
//! first u16 `amplitude` and the second `period`. Read that way the command
//! makes no sense, and following it produced silence: a full-scale
//! "amplitude" of 65535 is 65 ms of DC, which neither oscillates nor can be
//! felt. Measured on hardware 2026-08-08 by pitch — `on=250 off=250` sounds
//! two octaves above `on=2500 off=2500`, and swapping the two changes
//! nothing — the fields are two half-periods in microseconds. See Notes.md.
//!
//! Sent the same way as the settings report in [`super::hidraw`]: padded to
//! 64 bytes behind a `0x00` report-id byte, via `HIDIOCSFEATURE`. The
//! controller acks by echoing `8F 00` into the control pipe, which says it
//! parsed the command and nothing about whether it moved.

use super::packet;
use crate::switch::rumble::Band;
use std::time::Duration;

/// Feature report id: trigger a haptic pulse train.
const CMD_TRIGGER_HAPTIC: u8 = 0x8F;
/// Payload length byte the command carries (side + on + off + count = 7).
const HAPTIC_PAYLOAD_LEN: u8 = 0x07;

/// Duty cycle a full-strength rumble is played at.
///
/// A square wave delivers the most energy — and the piezo the most output —
/// at 50%, and past that it is just the same waveform mirrored. We stay well
/// under that: at ~50% a *sustained* sequence of bursts knocked the
/// controller off the dongle three times in as many minutes (2026-08-08),
/// while isolated bursts at the same duty were fine. Whether that is the
/// radio browning out under the actuator's draw or simply tired batteries is
/// not yet established — see Notes.md — so this is deliberately cautious
/// until a game session says otherwise. Raising it is the first thing to try
/// if rumble is too faint, and the first suspect if the controller drops.
pub const MAX_DUTY: f32 = 0.25;

/// Shortest on-time worth sending. Below roughly this the piezo has not
/// moved before it is released, so a very quiet rumble would silently become
/// no rumble; clamping keeps it faint instead of absent.
const MIN_ON_US: u16 = 20;

/// Longest on-time we will ever ask for, whatever the duty cycle works out
/// to.
///
/// Hardware, 2026-08-08: a burst of 2.5 ms on-times dropped the controller
/// off the dongle mid-test. A piezo held energised is a near short circuit,
/// so long pulses are both a power draw and pointless — the element has
/// finished moving within a fraction of a millisecond and the rest is heat.
/// At low frequencies this caps the duty below [`MAX_DUTY`], turning the
/// waveform into a train of clicks, which is what these actuators do
/// anyway.
const MAX_ON_US: u16 = 600;

/// Which actuator. Values are the wire encoding, not our choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Right = 0,
    Left = 1,
}

/// One burst: `count` cycles of `on_us` energised, `off_us` idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HapticPulse {
    pub side: Side,
    pub on_us: u16,
    pub off_us: u16,
    pub count: u16,
}

impl HapticPulse {
    /// Translate one decoded rumble band into a burst lasting `duration`.
    ///
    /// Frequency sets the cycle length and amplitude sets the duty cycle:
    /// the piezo cannot be driven harder than full swing, so "quieter" means
    /// a narrower pulse within the same cycle. Returns `None` for silence,
    /// which is the common case — the Switch streams neutral rumble in every
    /// report, and a zero-width pulse is not worth a USB transfer.
    ///
    /// Silence is *not* an active stop, and cannot be: a burst already
    /// running plays to its end. Keeping `duration` near the tick that
    /// re-arms it is what bounds the overhang.
    pub fn from_band(side: Side, band: Band, duration: Duration) -> Option<Self> {
        if band.amplitude <= 0.0 || band.freq_hz <= 0.0 {
            return None;
        }
        // One full cycle, split into the energised and idle halves. Both
        // fields are u16 microseconds, so the cycle cannot exceed ~131 ms
        // total; the Switch's own range (41..1252 Hz) is far inside that.
        let cycle_us = f32::clamp(1_000_000.0 / band.freq_hz, 2.0, 2.0 * f32::from(u16::MAX));
        let duty = f32::clamp(band.amplitude, 0.0, 1.0) * MAX_DUTY;
        let on_us = f32::clamp(cycle_us * duty, 0.0, f32::from(MAX_ON_US)).round() as u16;
        let on_us = u16::clamp(on_us, MIN_ON_US, MAX_ON_US);
        let off_us = f32::clamp(cycle_us - f32::from(on_us), 1.0, f32::from(u16::MAX)) as u16;
        // How many whole cycles fill the requested duration. At least one: a
        // burst of zero cycles is a command that does nothing.
        let cycle_us = u128::from(on_us) + u128::from(off_us);
        let count = u16::try_from(duration.as_micros() / cycle_us).unwrap_or(u16::MAX);
        Some(Self {
            side,
            on_us,
            off_us,
            count: count.max(1),
        })
    }

    /// The 64-byte feature report to hand to `HIDIOCSFEATURE`.
    pub fn feature_report(&self) -> [u8; packet::PACKET_LEN] {
        let mut report = [0u8; packet::PACKET_LEN];
        report[0] = CMD_TRIGGER_HAPTIC;
        report[1] = HAPTIC_PAYLOAD_LEN;
        report[2] = self.side as u8;
        report[3..5].copy_from_slice(&self.on_us.to_le_bytes());
        report[5..7].copy_from_slice(&self.off_us.to_le_bytes());
        report[7..9].copy_from_slice(&self.count.to_le_bytes());
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(freq_hz: f32, amplitude: f32) -> Band {
        Band { freq_hz, amplitude }
    }

    #[test]
    fn report_layout_matches_the_reference_packing() {
        let pulse = HapticPulse {
            side: Side::Left,
            on_us: 0x1234,
            off_us: 0x5678,
            count: 0x9ABC,
        };
        let report = pulse.feature_report();
        assert_eq!(
            &report[..9],
            &[0x8F, 0x07, 0x01, 0x34, 0x12, 0x78, 0x56, 0xBC, 0x9A]
        );
        assert!(report[9..].iter().all(|&b| b == 0));
    }

    #[test]
    fn sides_use_the_wire_encoding() {
        assert_eq!(Side::Right as u8, 0);
        assert_eq!(Side::Left as u8, 1);
    }

    /// The property hardware taught us: pitch is 1/(on + off).
    #[test]
    fn cycle_length_is_the_requested_frequency() {
        for freq in [50.0, 160.0, 320.0, 1000.0] {
            let pulse =
                HapticPulse::from_band(Side::Left, band(freq, 1.0), Duration::from_millis(50))
                    .unwrap();
            let cycle_us = f32::from(pulse.on_us) + f32::from(pulse.off_us);
            let played = 1_000_000.0 / cycle_us;
            assert!(
                (played - freq).abs() / freq < 0.02,
                "asked {freq} Hz, cycle plays {played} Hz"
            );
        }
    }

    #[test]
    fn amplitude_narrows_the_pulse_rather_than_raising_it() {
        let loud = HapticPulse::from_band(Side::Left, band(200.0, 1.0), Duration::from_millis(50))
            .unwrap();
        let quiet =
            HapticPulse::from_band(Side::Left, band(200.0, 0.25), Duration::from_millis(50))
                .unwrap();
        // Same pitch…
        assert_eq!(
            loud.on_us + loud.off_us,
            quiet.on_us + quiet.off_us,
            "amplitude must not change the pitch"
        );
        // …but a narrower energised fraction, and never past half.
        assert!(quiet.on_us < loud.on_us);
        assert!(f32::from(loud.on_us) / f32::from(loud.on_us + loud.off_us) <= MAX_DUTY + 0.01);
        assert!(loud.on_us <= MAX_ON_US);
    }

    #[test]
    fn duration_becomes_a_cycle_count() {
        let pulse =
            HapticPulse::from_band(Side::Right, band(200.0, 1.0), Duration::from_millis(50))
                .unwrap();
        // 200 Hz = 5000 µs per cycle; 50 ms holds ten.
        assert_eq!(pulse.count, 10);
        assert_eq!(pulse.side, Side::Right);
    }

    #[test]
    fn silence_produces_no_command() {
        assert_eq!(
            HapticPulse::from_band(Side::Left, band(160.0, 0.0), Duration::from_millis(50)),
            None
        );
        assert_eq!(
            HapticPulse::from_band(Side::Left, band(0.0, 1.0), Duration::from_millis(50)),
            None
        );
    }

    /// A rumble too quiet to round into a pulse must stay faint, not vanish.
    #[test]
    fn very_quiet_rumble_keeps_a_minimum_pulse() {
        let pulse =
            HapticPulse::from_band(Side::Left, band(160.0, 1e-4), Duration::from_millis(50))
                .unwrap();
        assert_eq!(pulse.on_us, MIN_ON_US);
        assert!(pulse.off_us > 0);
    }

    /// No frequency or amplitude may produce a pulse long enough to drop
    /// the controller off the dongle — see [`MAX_ON_US`].
    #[test]
    fn on_time_is_capped_however_low_the_frequency() {
        for freq in [1.0, 20.0, 50.0, 160.0, 1000.0] {
            let pulse =
                HapticPulse::from_band(Side::Left, band(freq, 1.0), Duration::from_millis(50))
                    .unwrap();
            assert!(
                pulse.on_us <= MAX_ON_US,
                "{freq} Hz asked for {} µs on",
                pulse.on_us
            );
        }
    }

    /// Every field is a u16 on the wire; nothing may wrap into a tiny value.
    #[test]
    fn extreme_inputs_clamp_instead_of_wrapping() {
        // 1 Hz wants a 1-second cycle, far past two u16 microsecond fields.
        let slow =
            HapticPulse::from_band(Side::Left, band(1.0, 1.0), Duration::from_millis(50)).unwrap();
        assert_eq!(slow.on_us, MAX_ON_US);
        assert_eq!(slow.off_us, u16::MAX);
        assert_eq!(slow.count, 1, "a cycle longer than the burst plays once");
        // 100 kHz wants a 10 µs cycle, below the minimum on-time.
        let fast = HapticPulse::from_band(Side::Left, band(100_000.0, 1.0), Duration::from_secs(1))
            .unwrap();
        assert_eq!(fast.on_us, MIN_ON_US);
        assert!(fast.off_us >= 1);
        // A long burst at a short cycle overflows the count field.
        let long = HapticPulse::from_band(Side::Left, band(1000.0, 1.0), Duration::from_secs(3600))
            .unwrap();
        assert_eq!(long.count, u16::MAX);
    }
}

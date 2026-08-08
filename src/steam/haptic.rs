//! Driving the Steam Controller's haptic actuators.
//!
//! The controller has one actuator under each trackpad. They are *piezo*
//! transducers, not rumble motors: they click, and a train of clicks at an
//! audio-rate period is what passes for vibration. So there is no "set
//! intensity" — you ask for `count` pulses of `period_us` at `amplitude`,
//! and the burst runs to completion on its own.
//!
//! Command format (feature report `0x8f`), from ynsta/steamcontroller (MIT),
//! which packs it as `struct('<BBBHHH', 0x8f, 0x07, position, amplitude,
//! period, count)`:
//!
//! ```text
//! byte 0    0x8f   TRIGGER_HAPTIC_PULSE
//! byte 1    0x07   payload length
//! byte 2    side   0 = right, 1 = left
//! bytes 3-4 amplitude  u16 LE
//! bytes 5-6 period     u16 LE, microseconds
//! bytes 7-8 count      u16 LE, pulses to play
//! ```
//!
//! Sent the same way as the settings report in [`super::hidraw`]: padded to
//! 64 bytes behind a `0x00` report-id byte, via `HIDIOCSFEATURE`.
//!
//! **Unverified by feel.** [`FULL_SCALE_AMPLITUDE`] is the one number here
//! that hardware has to settle — see its docs.

use super::packet;
use crate::switch::rumble::Band;
use std::time::Duration;

/// Feature report id: trigger a haptic pulse train.
const CMD_TRIGGER_HAPTIC: u8 = 0x8F;
/// Payload length byte the command carries (side + amplitude + period +
/// count = 7 bytes).
const HAPTIC_PAYLOAD_LEN: u8 = 0x07;

/// What a Switch amplitude of 1.0 becomes in the actuator's 0..=65535
/// amplitude field.
///
/// **This is the tunable unknown.** The field is documented as full-range,
/// but ynsta's own default is 128, three orders of magnitude below the top —
/// so the useful scale is somewhere in between and nobody has written down
/// where. Half scale is a deliberately unadventurous starting point: loud
/// enough to feel, short of driving a piezo at its limit for minutes at a
/// time. Find the real number by feel with `sweam buzz --amplitude N`, then
/// change it here.
pub const FULL_SCALE_AMPLITUDE: u16 = 0x8000;

/// Which actuator. Values are the wire encoding, not our choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Right = 0,
    Left = 1,
}

/// One burst: `count` pulses of `period_us`, at `amplitude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HapticPulse {
    pub side: Side,
    pub amplitude: u16,
    pub period_us: u16,
    pub count: u16,
}

impl HapticPulse {
    /// Shortest period we will ask for (≈10 kHz) — past this the pulses stop
    /// being distinguishable and start being an unpleasant whine.
    const MIN_PERIOD_US: u16 = 100;

    /// Translate one decoded rumble band into a burst lasting `duration`.
    ///
    /// Returns `None` for silence, which is the overwhelmingly common case:
    /// the Switch streams neutral rumble in every report, and sending a
    /// zero-amplitude burst 25 times a second would be pure bus traffic.
    ///
    /// Silence is *not* an active stop, and cannot be: a burst already
    /// running plays to its end. Keeping `duration` near the tick that
    /// re-arms it is what bounds the overhang.
    pub fn from_band(side: Side, band: Band, duration: Duration) -> Option<Self> {
        if band.amplitude <= 0.0 || band.freq_hz <= 0.0 {
            return None;
        }
        let amplitude = (band.amplitude * f32::from(FULL_SCALE_AMPLITUDE)).round();
        let amplitude = f32::clamp(amplitude, 0.0, f32::from(u16::MAX)) as u16;
        if amplitude == 0 {
            return None;
        }
        let period_us = (1_000_000.0 / band.freq_hz).round();
        let period_us = u16::max(
            Self::MIN_PERIOD_US,
            f32::clamp(period_us, 0.0, f32::from(u16::MAX)) as u16,
        );
        // How many of those pulses fill the requested duration. At least
        // one: a burst of zero pulses is a command that does nothing.
        let count = duration.as_micros() / u128::from(period_us);
        let count = u16::try_from(count).unwrap_or(u16::MAX).max(1);
        Some(Self {
            side,
            amplitude,
            period_us,
            count,
        })
    }

    /// The 64-byte feature report to hand to `HIDIOCSFEATURE`.
    pub fn feature_report(&self) -> [u8; packet::PACKET_LEN] {
        let mut report = [0u8; packet::PACKET_LEN];
        report[0] = CMD_TRIGGER_HAPTIC;
        report[1] = HAPTIC_PAYLOAD_LEN;
        report[2] = self.side as u8;
        report[3..5].copy_from_slice(&self.amplitude.to_le_bytes());
        report[5..7].copy_from_slice(&self.period_us.to_le_bytes());
        report[7..9].copy_from_slice(&self.count.to_le_bytes());
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_layout_matches_the_reference_packing() {
        let pulse = HapticPulse {
            side: Side::Left,
            amplitude: 0x1234,
            period_us: 0x5678,
            count: 0x9ABC,
        };
        let report = pulse.feature_report();
        assert_eq!(
            &report[..9],
            &[0x8F, 0x07, 0x01, 0x34, 0x12, 0x78, 0x56, 0xBC, 0x9A]
        );
        // Everything past the payload is padding.
        assert!(report[9..].iter().all(|&b| b == 0));
        assert_eq!(report.len(), packet::PACKET_LEN);
    }

    #[test]
    fn sides_use_the_wire_encoding() {
        assert_eq!(Side::Right as u8, 0);
        assert_eq!(Side::Left as u8, 1);
    }

    #[test]
    fn silence_produces_no_command() {
        let silent = Band {
            freq_hz: 160.0,
            amplitude: 0.0,
        };
        assert_eq!(
            HapticPulse::from_band(Side::Left, silent, Duration::from_millis(50)),
            None
        );
        // A band with an amplitude too small to survive rounding to the
        // integer field is silence too, not a zero-amplitude command.
        let inaudible = Band {
            freq_hz: 160.0,
            amplitude: 1e-9,
        };
        assert_eq!(
            HapticPulse::from_band(Side::Left, inaudible, Duration::from_millis(50)),
            None
        );
    }

    #[test]
    fn frequency_becomes_period_and_duration_becomes_count() {
        let band = Band {
            freq_hz: 200.0,
            amplitude: 1.0,
        };
        let pulse = HapticPulse::from_band(Side::Right, band, Duration::from_millis(50)).unwrap();
        // 200 Hz = 5000 µs per pulse; 50 ms holds ten of them.
        assert_eq!(pulse.period_us, 5000);
        assert_eq!(pulse.count, 10);
        assert_eq!(pulse.amplitude, FULL_SCALE_AMPLITUDE);
        assert_eq!(pulse.side, Side::Right);
    }

    #[test]
    fn amplitude_scales_linearly_and_saturates() {
        let half = Band {
            freq_hz: 160.0,
            amplitude: 0.5,
        };
        let pulse = HapticPulse::from_band(Side::Left, half, Duration::from_millis(40)).unwrap();
        assert_eq!(pulse.amplitude, FULL_SCALE_AMPLITUDE / 2);
        // Above full scale the field must clamp, never wrap.
        let loud = Band {
            freq_hz: 160.0,
            amplitude: 100.0,
        };
        let pulse = HapticPulse::from_band(Side::Left, loud, Duration::from_millis(40)).unwrap();
        assert_eq!(pulse.amplitude, u16::MAX);
    }

    /// Every field is a u16 on the wire; nothing may wrap into a tiny value.
    #[test]
    fn extreme_inputs_clamp_instead_of_wrapping() {
        // 1 Hz would want a 1-second period, far past the field.
        let slow = Band {
            freq_hz: 1.0,
            amplitude: 1.0,
        };
        let pulse = HapticPulse::from_band(Side::Left, slow, Duration::from_millis(40)).unwrap();
        assert_eq!(pulse.period_us, u16::MAX);
        assert_eq!(pulse.count, 1, "a period longer than the burst plays once");
        // 100 kHz would want a period of 10 µs, below the floor.
        let fast = Band {
            freq_hz: 100_000.0,
            amplitude: 1.0,
        };
        let pulse = HapticPulse::from_band(Side::Left, fast, Duration::from_millis(40)).unwrap();
        assert_eq!(pulse.period_us, HapticPulse::MIN_PERIOD_US);
        // A long burst at a short period overflows the count field.
        let pulse = HapticPulse::from_band(Side::Left, fast, Duration::from_secs(3600)).unwrap();
        assert_eq!(pulse.count, u16::MAX);
    }
}

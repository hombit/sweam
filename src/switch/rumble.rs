//! Decoding the Switch's HD rumble data.
//!
//! Every output report the host sends carries 8 bytes of rumble — 4 per
//! side, left first — whether or not it carries a subcommand. Each side
//! describes *two* sine bands (a low one and a high one), each with its own
//! frequency and amplitude, which is what makes Joy-Con rumble expressive
//! and what makes it awkward to replay on hardware that has one actuator per
//! side.
//!
//! Layout and formulas from dekuNukem/Nintendo_Switch_Reverse_Engineering,
//! `rumble_data_table.md`:
//!
//! ```text
//! byte[0] = HF & 0xFF
//! byte[1] = HF_AMP + ((HF >> 8) & 0xFF)
//! byte[2] = LF + ((LF_AMP >> 8) & 0xFF)
//! byte[3] = LF_AMP & 0xFF
//! ```
//!
//! with `encoded_freq = log2(hz / 10) * 32`, `HF = (encoded_freq - 0x60) * 4`
//! and `LF = encoded_freq - 0x40`; amplitudes encode as
//! `log2(8.7 * amp) * 32` (or `log2(17 * amp) * 16` in the quiet range),
//! stored as `HF_AMP = encoded * 2` and `LF_AMP = (encoded >> 1) + 0x40`.
//!
//! Decoding inverts all of that back into Hz and a 0.0..=1.0 amplitude,
//! because that is what a *different* actuator can be driven from. The
//! neutral frame `00 01 40 40` decodes to zero amplitude on both bands,
//! which is the cheapest check that the arithmetic is right — there is a
//! test for exactly that.

/// The rumble the host is currently asking for, shared between the thread
/// that hears it and the thread that plays it.
///
/// Latest-wins on purpose. Rumble is a *level*, not a stream of events: the
/// host restates it in every output report (~66/s), and the actuators are
/// re-armed on a slower tick, so intermediate values are worth dropping. A
/// queue here would only build a backlog of stale effects to work through.
#[derive(Debug, Default)]
pub struct RumbleMailbox {
    latest: std::sync::Mutex<Option<(RumbleFrame, std::time::Instant)>>,
}

impl RumbleMailbox {
    /// Post what the host just asked for.
    pub fn set(&self, frame: RumbleFrame) {
        *self.lock() = Some((frame, std::time::Instant::now()));
    }

    /// What the host most recently asked for, ignoring age — tests only, so
    /// that asserting on what was posted does not depend on a clock. The
    /// bridge always reads through [`Self::get_fresh`].
    ///
    /// `None` means the host has never said anything, which is not the same
    /// as silence: a host that has sent no rumble at all should not have us
    /// touching the actuators.
    #[cfg(test)]
    pub fn get(&self) -> Option<RumbleFrame> {
        self.lock().map(|(frame, _)| frame)
    }

    /// Same, but only if it is recent enough to still be meant.
    ///
    /// A host normally ends an effect by streaming neutral frames, so the
    /// mailbox goes quiet on its own. If it instead stops sending *anything*
    /// — it crashed, unplugged, suspended mid-rumble — the last loud frame
    /// would otherwise sit here and be re-armed forever, and a controller
    /// buzzing until its battery dies is the worst way this can fail.
    pub fn get_fresh(&self, max_age: std::time::Duration) -> Option<RumbleFrame> {
        self.lock()
            .filter(|(_, posted)| posted.elapsed() <= max_age)
            .map(|(frame, _)| frame)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(RumbleFrame, std::time::Instant)>> {
        // A panic elsewhere must not silence rumble forever; the value is a
        // plain Copy struct, so there is no torn state to recover from.
        self.latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One sine band: what to play, and how hard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub freq_hz: f32,
    /// 0.0 = silent, 1.0 = full scale.
    pub amplitude: f32,
}

impl Band {
    const SILENT: Self = Self {
        freq_hz: 0.0,
        amplitude: 0.0,
    };
}

/// The two bands driving one side's actuator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SideRumble {
    pub low: Band,
    pub high: Band,
}

impl SideRumble {
    pub const SILENT: Self = Self {
        low: Band::SILENT,
        high: Band::SILENT,
    };

    /// The band actually worth playing on a single-actuator device: the
    /// louder of the two. Ties go to the low band, which carries the body of
    /// a rumble effect — the high band is usually the detail on top.
    pub fn dominant(&self) -> Band {
        if self.high.amplitude > self.low.amplitude {
            self.high
        } else {
            self.low
        }
    }

    /// Loudest amplitude across both bands, for "is anything happening".
    pub fn amplitude(&self) -> f32 {
        f32::max(self.low.amplitude, self.high.amplitude)
    }
}

/// One report's worth of rumble: both sides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RumbleFrame {
    pub left: SideRumble,
    pub right: SideRumble,
}

impl RumbleFrame {
    pub const SILENT: Self = Self {
        left: SideRumble::SILENT,
        right: SideRumble::SILENT,
    };

    /// Decode the 8 rumble bytes (left side first).
    pub fn decode(data: &[u8; 8]) -> Self {
        Self {
            left: decode_side(&[data[0], data[1], data[2], data[3]]),
            right: decode_side(&[data[4], data[5], data[6], data[7]]),
        }
    }

    /// Whether this frame asks for nothing at all. The host streams neutral
    /// frames continuously, so this is the common case by far.
    pub fn is_silent(&self) -> bool {
        self.left.amplitude() == 0.0 && self.right.amplitude() == 0.0
    }
}

fn decode_side(data: &[u8; 4]) -> SideRumble {
    // HF is a 16-bit multiple of 4 whose top byte can only be 0 or 1, so it
    // shares byte 1 with the amplitude: bit 0 is HF's high byte, bits 1..=7
    // are HF_AMP (always even, being `encoded * 2`).
    let hf = u16::from(data[0]) | (u16::from(data[1] & 0x01) << 8);
    let hf_amp_encoded = (data[1] & 0xFE) >> 1;
    // LF_AMP is 9 bits: the top one rides in byte 2's spare high bit, above
    // the 7-bit frequency.
    let lf = data[2] & 0x7F;
    let lf_amp_raw = (u16::from(data[2] & 0x80) << 1) | u16::from(data[3]);

    SideRumble {
        low: Band {
            freq_hz: decode_freq(u16::from(lf) + 0x40),
            amplitude: decode_amplitude(lf_amp_from_raw(lf_amp_raw)),
        },
        high: Band {
            freq_hz: decode_freq(hf / 4 + 0x60),
            amplitude: decode_amplitude(f32::from(hf_amp_encoded)),
        },
    }
}

/// Invert `LF_AMP = (encoded >> 1) + 0x40`. The `>> 1` threw away the low
/// bit on the way in, so this recovers `encoded` to within 1 — sub-step
/// precision no actuator will notice. Anything below the 0x40 baseline is a
/// malformed frame; clamp rather than wrap into a huge amplitude.
fn lf_amp_from_raw(raw: u16) -> f32 {
    f32::from(raw.saturating_sub(0x40)) * 2.0
}

/// `encoded = log2(hz / 10) * 32`, inverted. The Switch's own range is
/// 40.9..1252.6 Hz; values outside it mean a frame we misread.
fn decode_freq(encoded: u16) -> f32 {
    10.0 * f32::exp2(f32::from(encoded) / 32.0)
}

/// Invert the amplitude curve. Three ranges, per the table: above 32 the
/// encoding is `log2(8.7 * amp) * 32`, from 16 to 32 it is
/// `log2(17 * amp) * 16`, and below 16 the table says "TBD" — there we
/// interpolate linearly up to where the 16..32 curve starts, which keeps the
/// quietest effects monotonic instead of guessing at a formula nobody
/// published.
fn decode_amplitude(encoded: f32) -> f32 {
    /// Where the documented 16..32 curve lands at its low end, i.e.
    /// `2^(16/16) / 17`.
    const QUIET_CEILING: f32 = 2.0 / 17.0;
    let amp = if encoded <= 0.0 {
        0.0
    } else if encoded < 16.0 {
        QUIET_CEILING * (encoded / 16.0)
    } else if encoded < 32.0 {
        f32::exp2(encoded / 16.0) / 17.0
    } else {
        f32::exp2(encoded / 32.0) / 8.7
    };
    // The table notes amplitudes above 1.0 exist but are unsafe for the
    // hardware; we are driving a different actuator, but clamping keeps
    // every consumer's arithmetic in a known range.
    f32::clamp(amp, 0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame the Switch streams when nothing is rumbling. If this is
    /// wrong, the bridge buzzes forever.
    const NEUTRAL: [u8; 8] = [0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40];

    #[test]
    fn stale_rumble_expires_so_the_pads_cannot_stick_on() {
        use std::time::Duration;
        let mailbox = RumbleMailbox::default();
        assert_eq!(mailbox.get_fresh(Duration::from_millis(250)), None);
        mailbox.set(RumbleFrame::decode(&[
            0x00, 0x01, 0x64, 0x72, 0x00, 0x01, 0x40, 0x40,
        ]));
        // Fresh right after posting, gone once it is older than the window.
        assert!(mailbox.get_fresh(Duration::from_millis(250)).is_some());
        assert_eq!(mailbox.get_fresh(Duration::ZERO), None);
        // The unfiltered read still sees it — that is what the tests of the
        // protocol layer assert against.
        assert!(mailbox.get().is_some());
    }

    #[test]
    fn neutral_frame_is_silent() {
        let frame = RumbleFrame::decode(&NEUTRAL);
        assert!(frame.is_silent(), "{frame:?}");
        assert_eq!(frame.left.low.amplitude, 0.0);
        assert_eq!(frame.left.high.amplitude, 0.0);
        assert_eq!(frame.right, frame.left);
    }

    /// Neutral still names the frequencies the Switch idles at: the encoded
    /// values 0x80 (low) and 0xA0 (high) are 160 Hz and 320 Hz.
    #[test]
    fn neutral_frame_carries_the_idle_frequencies() {
        let frame = RumbleFrame::decode(&NEUTRAL);
        assert!((frame.left.low.freq_hz - 160.0).abs() < 0.5, "{frame:?}");
        assert!((frame.left.high.freq_hz - 320.0).abs() < 1.0, "{frame:?}");
    }

    /// Round-trip against the encoder in the notes: build bytes for a known
    /// frequency/amplitude and check we get them back.
    fn encode_side(low_hz: f32, low_amp: f32, high_hz: f32, high_amp: f32) -> [u8; 4] {
        let encode_freq = |hz: f32| (f32::log2(hz / 10.0) * 32.0).round() as u16;
        let encode_amp = |amp: f32| (f32::log2(8.7 * amp) * 32.0).round() as u16;
        let hf = (encode_freq(high_hz) - 0x60) * 4;
        let lf = encode_freq(low_hz) - 0x40;
        let hf_amp = encode_amp(high_amp) * 2;
        let lf_amp = (encode_amp(low_amp) >> 1) + 0x40;
        [
            (hf & 0xFF) as u8,
            (hf_amp + ((hf >> 8) & 0xFF)) as u8,
            (lf + ((lf_amp >> 8) & 0xFF)) as u8,
            (lf_amp & 0xFF) as u8,
        ]
    }

    #[test]
    fn round_trips_a_known_effect() {
        // A typical effect: strong 160 Hz body, quieter 320 Hz detail.
        let side = encode_side(160.0, 1.0, 320.0, 0.5);
        let decoded = decode_side(&side);
        assert!((decoded.low.freq_hz - 160.0).abs() < 4.0, "{decoded:?}");
        assert!((decoded.high.freq_hz - 320.0).abs() < 8.0, "{decoded:?}");
        // The low band's amplitude loses a bit to the `>> 1` in the
        // encoding, so it is only good to a few percent.
        assert!((decoded.low.amplitude - 1.0).abs() < 0.1, "{decoded:?}");
        assert!((decoded.high.amplitude - 0.5).abs() < 0.05, "{decoded:?}");
        assert_eq!(decoded.dominant().amplitude, decoded.low.amplitude);
    }

    #[test]
    fn sides_decode_independently() {
        let mut data = NEUTRAL;
        data[4..].copy_from_slice(&encode_side(160.0, 0.8, 320.0, 0.0));
        let frame = RumbleFrame::decode(&data);
        assert!(frame.left.amplitude() == 0.0, "{frame:?}");
        assert!(frame.right.amplitude() > 0.5, "{frame:?}");
        assert!(!frame.is_silent());
    }

    #[test]
    fn dominant_band_is_the_louder_one() {
        let side = decode_side(&encode_side(160.0, 0.2, 320.0, 0.9));
        assert_eq!(side.dominant(), side.high);
        assert!((side.dominant().freq_hz - 320.0).abs() < 8.0);
    }

    /// Amplitude must rise monotonically across all three ranges of the
    /// curve, including the undocumented quiet one — a discontinuity there
    /// would feel like the rumble cutting in and out.
    #[test]
    fn amplitude_curve_is_monotonic_and_bounded() {
        let mut previous = -1.0;
        for encoded in 0..=100 {
            let amp = decode_amplitude(encoded as f32);
            assert!(amp >= previous, "dropped at {encoded}: {amp} < {previous}");
            assert!((0.0..=1.0).contains(&amp), "out of range at {encoded}");
            previous = amp;
        }
        assert_eq!(decode_amplitude(0.0), 0.0);
    }

    /// Garbage in must not become a huge amplitude out: a malformed LF_AMP
    /// below the 0x40 baseline saturates at silence instead of wrapping.
    #[test]
    fn malformed_frames_stay_in_range() {
        let frame = RumbleFrame::decode(&[0xFF; 8]);
        assert!(frame.left.amplitude() <= 1.0);
        assert!(frame.right.amplitude() <= 1.0);
        let below_baseline = decode_side(&[0x00, 0x01, 0x40, 0x00]);
        assert_eq!(below_baseline.low.amplitude, 0.0);
    }
}

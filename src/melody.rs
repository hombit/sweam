//! Note sequences for the haptics.
//!
//! The actuators play a square wave at `1/(on+off)` (see
//! `steam::haptic`), which makes them a one-voice tone generator — so once
//! rumble worked, playing a tune was almost free. This module is only the
//! text-to-pitch part, kept out of `buzz.rs` so it compiles and is tested on
//! any platform.
//!
//! Spec syntax: whitespace- or comma-separated `NOTE[:BEATS]`, where NOTE is
//! a letter `A`–`G`, an optional `#`/`b`, and an octave digit — or `r` for a
//! rest. `C5:2 E5 G5:0.5` is a half-length C, a beat of E, half a beat of G.

/// One note: a pitch and how long to hold it. `freq_hz == 0.0` is a rest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    pub freq_hz: f32,
    pub beats: f32,
}

impl Note {
    pub fn is_rest(&self) -> bool {
        self.freq_hz <= 0.0
    }
}

/// Shift every pitch by `semitones`, leaving rests alone.
pub fn transpose(notes: &mut [Note], semitones: i32) {
    if semitones == 0 {
        return;
    }
    let ratio = f32::exp2(semitones as f32 / 12.0);
    for note in notes.iter_mut().filter(|note| !note.is_rest()) {
        note.freq_hz *= ratio;
    }
}

/// Parse a spec into notes, or say which token was wrong.
pub fn parse(spec: &str) -> Result<Vec<Note>, String> {
    let mut notes = Vec::new();
    for token in spec.split([' ', ',', '\t', '\n']).filter(|t| !t.is_empty()) {
        let (name, beats) = match token.split_once(':') {
            Some((name, beats)) => {
                let beats: f32 = beats
                    .parse()
                    .map_err(|_| format!("bad beat count in {token:?}"))?;
                if !(beats.is_finite() && beats > 0.0) {
                    return Err(format!("beat count in {token:?} must be positive"));
                }
                (name, beats)
            }
            None => (token, 1.0),
        };
        notes.push(Note {
            freq_hz: note_hz(name).ok_or_else(|| format!("bad note {name:?}"))?,
            beats,
        });
    }
    if notes.is_empty() {
        return Err("no notes".to_owned());
    }
    Ok(notes)
}

/// A scientific-pitch note name to Hz, equal temperament with A4 = 440.
/// `r` (or `-`) is a rest, which comes back as 0 Hz.
fn note_hz(name: &str) -> Option<f32> {
    if name.eq_ignore_ascii_case("r") || name == "-" {
        return Some(0.0);
    }
    let mut chars = name.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let class = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave) = match rest.strip_prefix('#') {
        Some(octave) => (1, octave),
        None => match rest.strip_prefix('b') {
            Some(octave) => (-1, octave),
            None => (0, rest),
        },
    };
    let octave: i32 = octave.parse().ok()?;
    // MIDI numbering: C-1 is 0, so C4 (middle C) is 60 and A4 is 69.
    let midi = (octave + 1) * 12 + class + accidental;
    Some(440.0 * f32::exp2((midi - 69) as f32 / 12.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hz(name: &str) -> f32 {
        note_hz(name).unwrap()
    }

    #[test]
    fn concert_pitch_and_octaves() {
        assert!((hz("A4") - 440.0).abs() < 0.01);
        assert!((hz("A5") - 880.0).abs() < 0.01);
        assert!((hz("A3") - 220.0).abs() < 0.01);
        // Middle C, the other number everyone knows.
        assert!((hz("C4") - 261.63).abs() < 0.01, "{}", hz("C4"));
    }

    #[test]
    fn accidentals_are_a_semitone_either_way() {
        // Enharmonic equivalents must land on the same pitch.
        assert!((hz("A#4") - hz("Bb4")).abs() < 0.01);
        // And a semitone is the twelfth root of two.
        assert!((hz("A#4") / hz("A4") - f32::exp2(1.0 / 12.0)).abs() < 1e-4);
    }

    #[test]
    fn parses_beats_and_defaults_to_one() {
        let notes = parse("C5:2 E5 r:0.5").unwrap();
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0].beats, 2.0);
        assert_eq!(notes[1].beats, 1.0);
        assert!(notes[2].is_rest());
        assert!(!notes[1].is_rest());
    }

    #[test]
    fn separators_are_flexible() {
        assert_eq!(parse("C5,E5  G5").unwrap().len(), 3);
    }

    #[test]
    fn bad_input_says_what_was_wrong() {
        for bad in ["H5", "C5:x", "C5:0", "C5:-1", "", "   ", "5C"] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn transposing_moves_pitches_and_leaves_rests() {
        let mut notes = parse("A4 r C5").unwrap();
        transpose(&mut notes, 12);
        assert!((notes[0].freq_hz - 880.0).abs() < 0.01, "an octave up");
        assert!(notes[1].is_rest(), "a rest has no pitch to shift");
        transpose(&mut notes, -24);
        assert!((notes[0].freq_hz - 220.0).abs() < 0.01, "two octaves down");
    }
}

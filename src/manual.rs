//! Manual input source: Pro Controller inputs typed on stdin.
//!
//! Speaks the Nintendo vocabulary — it writes the Pro-Controller-shaped
//! [`ControllerState`] directly, bypassing the Steam Controller mapping.
//! For hardware testing without a Steam Controller (PLAN.md phase 2 exit
//! criterion): run `sweam manual`, then type or pipe lines like `press a`,
//! `release a b`, `stick l 0.5 -1`, `neutral` and watch them arrive on the
//! debug host with `evtest`.

use crate::state::{Button, ControllerState, ImuSample, StickState};
use crate::steam::InputSource;
use std::sync::mpsc;

/// Reads commands from stdin on a background thread; [`InputSource::poll`]
/// drains them into the shared state. If stdin closes, the last state holds.
pub struct ManualInput {
    lines: mpsc::Receiver<String>,
}

impl ManualInput {
    pub fn new() -> Self {
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        println!(
            "Manual input: press|release <button…> | stick <l|r> <x> <y> (-1..1) | \
             gyro <x> <y> <z> (dps) | accel <x> <y> <z> (g) | neutral\n\
             Buttons: a b x y up down left right l r zl zr plus minus home capture lstick rstick"
        );
        Self { lines }
    }
}

impl InputSource for ManualInput {
    fn poll(&mut self, state: &mut ControllerState) -> anyhow::Result<()> {
        loop {
            match self.lines.try_recv() {
                Ok(line) => {
                    if let Err(err) = apply_line(state, &line) {
                        eprintln!("Manual input: {err}");
                    }
                }
                // Disconnected == stdin closed (e.g. piped script ended):
                // keep streaming the last state rather than erroring out.
                Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => {
                    return Ok(());
                }
            }
        }
    }
}

/// Apply one command line to the state. Empty lines are no-ops.
fn apply_line(state: &mut ControllerState, line: &str) -> Result<(), String> {
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else {
        return Ok(());
    };
    match command {
        "press" | "release" => {
            let pressed = command == "press";
            let mut any = false;
            for name in words {
                state.set_button(button_by_name(name)?, pressed);
                any = true;
            }
            if !any {
                return Err(format!("{command}: expected at least one button name"));
            }
            Ok(())
        }
        "stick" => {
            let side = words.next().ok_or("stick: expected side l or r")?;
            let x = parse_axis(words.next())?;
            let y = parse_axis(words.next())?;
            let stick = match side {
                "l" => &mut state.left_stick,
                "r" => &mut state.right_stick,
                other => return Err(format!("stick: side must be l or r, got {other:?}")),
            };
            *stick = StickState { x, y };
            Ok(())
        }
        // IMU test inputs: a constant rate/acceleration held until changed
        // (the sample ring is filled so all three report frames carry it).
        "gyro" | "accel" => {
            let mut values = [0f32; 3];
            for value in &mut values {
                *value = parse_float(words.next(), command)?;
            }
            let mut sample = state.imu[2];
            if command == "gyro" {
                sample.gyro = values.map(|dps| scale_imu(dps, ImuSample::GYRO_PER_DPS));
            } else {
                sample.accel = values.map(|g| scale_imu(g, ImuSample::ACCEL_PER_G));
            }
            state.imu = [sample; 3];
            Ok(())
        }
        "neutral" => {
            *state = ControllerState::default();
            Ok(())
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

/// Physical units → clamped raw i16 IMU value.
fn scale_imu(value: f32, per_unit: f32) -> i16 {
    f32::clamp(value * per_unit, f32::from(i16::MIN), f32::from(i16::MAX)).round() as i16
}

fn parse_float(word: Option<&str>, command: &str) -> Result<f32, String> {
    let word = word.ok_or_else(|| format!("{command}: expected three values"))?;
    let value: f32 = word
        .parse()
        .map_err(|_| format!("{command}: bad value {word:?}"))?;
    if !value.is_finite() {
        return Err(format!("{command}: bad value {word:?}"));
    }
    Ok(value)
}

fn button_by_name(name: &str) -> Result<Button, String> {
    Ok(match name {
        "a" => Button::A,
        "b" => Button::B,
        "x" => Button::X,
        "y" => Button::Y,
        "up" => Button::Up,
        "down" => Button::Down,
        "left" => Button::Left,
        "right" => Button::Right,
        "l" => Button::L,
        "r" => Button::R,
        "zl" => Button::ZL,
        "zr" => Button::ZR,
        "plus" => Button::Plus,
        "minus" => Button::Minus,
        "home" => Button::Home,
        "capture" => Button::Capture,
        "lstick" => Button::LStick,
        "rstick" => Button::RStick,
        other => return Err(format!("unknown button {other:?}")),
    })
}

/// Parse a `-1..=1` float into the 12-bit stick range (0..=4095, center 2048).
fn parse_axis(word: Option<&str>) -> Result<u16, String> {
    let word = word.ok_or("stick: expected two axis values in -1..1")?;
    let value: f32 = word
        .parse()
        .map_err(|_| format!("stick: bad axis value {word:?}"))?;
    // "nan" parses as a float but would silently become 0 (full left/down)
    // through the cast below; reject it and the infinities explicitly.
    if !value.is_finite() {
        return Err(format!("stick: bad axis value {word:?}"));
    }
    let value = f32::clamp(value, -1.0, 1.0);
    Ok(((value + 1.0) / 2.0 * 4095.0).round() as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_and_release_buttons() {
        let mut state = ControllerState::default();
        apply_line(&mut state, "press a zl").unwrap();
        assert_eq!(
            state.buttons,
            (1 << Button::A as u32) | (1 << Button::ZL as u32)
        );
        apply_line(&mut state, "release a").unwrap();
        assert_eq!(state.buttons, 1 << Button::ZL as u32);
    }

    #[test]
    fn stick_maps_unit_range_to_12_bits() {
        let mut state = ControllerState::default();
        apply_line(&mut state, "stick l -1 1").unwrap();
        assert_eq!(state.left_stick.x, 0);
        assert_eq!(state.left_stick.y, 4095);
        apply_line(&mut state, "stick r 0 0").unwrap();
        assert_eq!(state.right_stick.x, 2048);
        assert_eq!(state.right_stick.y, 2048);
    }

    #[test]
    fn axis_values_are_clamped() {
        let mut state = ControllerState::default();
        apply_line(&mut state, "stick l -5 7").unwrap();
        assert_eq!(state.left_stick.x, 0);
        assert_eq!(state.left_stick.y, 4095);
    }

    #[test]
    fn neutral_resets_everything() {
        let mut state = ControllerState::default();
        apply_line(&mut state, "press home").unwrap();
        apply_line(&mut state, "stick r 1 1").unwrap();
        apply_line(&mut state, "neutral").unwrap();
        assert_eq!(state.buttons, 0);
        assert_eq!(state.right_stick.x, StickState::CENTER);
    }

    #[test]
    fn errors_on_nonsense() {
        let mut state = ControllerState::default();
        assert!(apply_line(&mut state, "press").is_err());
        assert!(apply_line(&mut state, "press warp").is_err());
        assert!(apply_line(&mut state, "stick m 0 0").is_err());
        assert!(apply_line(&mut state, "stick l zero 0").is_err());
        assert!(apply_line(&mut state, "stick l nan 0").is_err());
        assert!(apply_line(&mut state, "stick l inf 0").is_err());
        assert!(apply_line(&mut state, "flip").is_err());
        assert_eq!(state.buttons, 0);
        assert_eq!(state.left_stick, StickState::default());
    }

    #[test]
    fn gyro_and_accel_fill_the_sample_ring() {
        let mut state = ControllerState::default();
        apply_line(&mut state, "gyro 100 -100 0").unwrap();
        apply_line(&mut state, "accel 0 0 1").unwrap();
        let expected = ImuSample {
            accel: [0, 0, 4096],
            gyro: [1640, -1640, 0],
        };
        assert_eq!(state.imu, [expected; 3]);
        // Values clamp instead of wrapping.
        apply_line(&mut state, "gyro 99999 0 0").unwrap();
        assert_eq!(state.imu[2].gyro[0], i16::MAX);
        // Errors: wrong arity and non-finite values.
        assert!(apply_line(&mut state, "gyro 1 2").is_err());
        assert!(apply_line(&mut state, "accel nan 0 0").is_err());
    }

    #[test]
    fn empty_lines_are_ignored() {
        let mut state = ControllerState::default();
        apply_line(&mut state, "").unwrap();
        apply_line(&mut state, "   ").unwrap();
    }
}

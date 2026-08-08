//! `sweam buzz`: fire the Steam Controller's haptics directly.
//!
//! No Switch, no gadget, no rumble decoding — just the actuator command from
//! `steam/haptic.rs`, with every field on the command line. This exists
//! because the haptic protocol is the one part of rumble forwarding that no
//! test can settle: whether a burst is felt at all, and what amplitude is
//! pleasant rather than nothing or a shriek, are answered by holding the
//! controller.
//!
//! ```sh
//! sudo sweam buzz                          # both pads, half a second
//! sudo sweam buzz --side left --amplitude 4096
//! sudo sweam buzz --period-us 2000 --seconds 2
//! ```
//!
//! Whatever amplitude turns out to feel right is the number that belongs in
//! `haptic::FULL_SCALE_AMPLITUDE`.

/// `--duty` is in the units a person thinks in (fraction of the cycle
/// energised); the mapping takes a 0..1 rumble amplitude that tops out at
/// [`crate::steam::haptic::MAX_DUTY`]. Asking for more than the cap gets the
/// cap, not an error — the point of the flag is exploring, and the limit
/// exists to protect the controller.
#[cfg(target_os = "linux")]
fn duty_to_amplitude(duty: f32) -> f32 {
    f32::clamp(duty / crate::steam::haptic::MAX_DUTY, 0.0, 1.0)
}

#[cfg(target_os = "linux")]
pub fn run(args: &crate::cli::BuzzOpts) -> anyhow::Result<()> {
    use crate::state::ControllerState;
    use crate::steam::InputSource;
    use crate::steam::haptic::{HapticPulse, Side};
    use crate::steam::hidraw::HidrawSteamController;
    use crate::steam::mapping::Mapping;
    use crate::switch::rumble::Band;
    use anyhow::Context;
    use std::time::{Duration, Instant};

    /// How long to wait for a packet that says which slot holds the
    /// controller. Sending blind works, but naming the slot is a better
    /// error message when nothing is felt.
    const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(2);

    let mut controller = HidrawSteamController::open(Mapping::empty(), args.device.as_deref())?;

    // Poll briefly so the active slot identifies itself; the controller has
    // to be awake for a burst to land anyway.
    let mut state = ControllerState::default();
    let deadline = Instant::now() + IDENTIFY_TIMEOUT;
    while Instant::now() < deadline && !controller.is_streaming() {
        controller.poll(&mut state)?;
        std::thread::sleep(Duration::from_millis(8));
    }
    if !controller.is_streaming() {
        eprintln!(
            "No packets yet — is the controller on? Sending to every slot anyway; \
             press the Steam button and try again if nothing is felt."
        );
    }

    // A tune takes over the whole run: the pads are a one-voice tone
    // generator, so notes are just bursts at the right pitch back to back.
    let tune = args
        .notes
        .as_deref()
        .map(|spec| crate::melody::parse(spec).map_err(anyhow::Error::msg))
        .transpose()?;
    if let Some(mut notes) = tune {
        crate::melody::transpose(&mut notes, args.transpose);
        let side = match args.side {
            Some(crate::cli::BuzzSide::Right) => Side::Right,
            _ => Side::Left,
        };
        let beat = Duration::from_secs_f32(60.0 / f32::max(args.bpm, 1.0));
        println!(
            "Playing {} notes at {:.0} bpm on {side:?}…",
            notes.len(),
            args.bpm
        );
        for note in notes {
            let length = beat.mul_f32(note.beats);
            if note.is_rest() {
                std::thread::sleep(length);
                continue;
            }
            let band = Band {
                freq_hz: note.freq_hz,
                amplitude: duty_to_amplitude(args.duty),
            };
            // Leave real silence at the end of each note. Measured
            // 2026-08-08: with only a 15% gap a whole tune came back as
            // "just one vzzhhhh" — the actuator rings on, and consecutive
            // notes merge into a single buzz. The same four pitches with
            // long pauses were clearly distinguishable.
            let sounding = length.mul_f32(1.0 - f32::clamp(args.gap, 0.0, 0.9));
            if let Some(pulse) = HapticPulse::from_band(side, band, sounding) {
                controller.send_haptic(&pulse)?;
            }
            std::thread::sleep(length);
        }
        println!("Done.");
        return Ok(());
    }

    let sides: &[Side] = match args.side {
        Some(crate::cli::BuzzSide::Left) => &[Side::Left],
        Some(crate::cli::BuzzSide::Right) => &[Side::Right],
        None => &[Side::Left, Side::Right],
    };
    let duration = Duration::from_secs_f32(args.seconds);
    for &side in sides {
        // Raw µs if given, otherwise the same frequency/duty mapping the
        // bridge uses — so what is felt here is what a game will feel like.
        let mut pulse = match (args.on_us, args.off_us) {
            (Some(on_us), Some(off_us)) => HapticPulse {
                side,
                on_us,
                off_us,
                count: 1,
            },
            _ => HapticPulse::from_band(
                side,
                Band {
                    freq_hz: args.freq_hz,
                    amplitude: duty_to_amplitude(args.duty),
                },
                duration,
            )
            .context("Nothing to play — check --freq-hz and --duty")?,
        };
        if let Some(count) = args.count {
            pulse.count = count;
        } else if args.on_us.is_some() {
            let cycle_us = u128::from(pulse.on_us) + u128::from(pulse.off_us);
            pulse.count = u16::try_from(duration.as_micros() / cycle_us.max(1))
                .unwrap_or(u16::MAX)
                .max(1);
        }
        let cycle_us = f32::from(pulse.on_us) + f32::from(pulse.off_us);
        println!(
            "{side:?}: on {} µs, off {} µs ({:.0} Hz, duty {:.0}%), {} cycles ≈ {:.2} s",
            pulse.on_us,
            pulse.off_us,
            1e6 / cycle_us,
            100.0 * f32::from(pulse.on_us) / cycle_us,
            pulse.count,
            f32::from(pulse.count) * cycle_us / 1e6,
        );
        controller.send_haptic(&pulse)?;
        // The command's only other feedback is a hand on the pads, so show
        // whatever the controller leaves in the control pipe — an echo, an
        // error code, or nothing at all are all informative.
        match controller.read_feature() {
            Some(Ok(reply)) => {
                let head: Vec<String> = reply[..12].iter().map(|b| format!("{b:02X}")).collect();
                println!("  readback: {}", head.join(" "));
            }
            Some(Err(err)) => println!("  readback failed: {err}"),
            None => println!("  readback: (no slot identified)"),
        }
        // Let one side finish before starting the other, so "only the left
        // one works" is a real observation rather than an artifact of both
        // firing at once.
        std::thread::sleep(duration + Duration::from_millis(150));
    }
    println!("Nothing felt? Try --duty 0.5 with --freq-hz 60..300, or raw --on-us/--off-us.");
    Ok(())
}

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

#[cfg(target_os = "linux")]
pub fn run(args: &crate::cli::BuzzOpts) -> anyhow::Result<()> {
    use crate::state::ControllerState;
    use crate::steam::InputSource;
    use crate::steam::haptic::{HapticPulse, Side};
    use crate::steam::hidraw::HidrawSteamController;
    use crate::steam::mapping::Mapping;
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

    let sides: &[Side] = match args.side {
        Some(crate::cli::BuzzSide::Left) => &[Side::Left],
        Some(crate::cli::BuzzSide::Right) => &[Side::Right],
        None => &[Side::Left, Side::Right],
    };
    let duration = Duration::from_secs_f32(args.seconds);
    for &side in sides {
        // count is what makes the burst last: the actuator plays `count`
        // pulses of `period_us` and stops on its own.
        let count = (duration.as_micros() / u128::from(args.period_us)).max(1);
        let pulse = HapticPulse {
            side,
            amplitude: args.amplitude,
            period_us: args.period_us,
            count: u16::try_from(count).unwrap_or(u16::MAX),
        };
        println!(
            "{side:?}: amplitude {}, period {} µs ({:.0} Hz), {} pulses ≈ {:.2} s",
            pulse.amplitude,
            pulse.period_us,
            1_000_000.0 / f32::from(pulse.period_us),
            pulse.count,
            f32::from(pulse.count) * f32::from(pulse.period_us) / 1e6,
        );
        controller.send_haptic(&pulse)?;
        // Let one side finish before starting the other, so "only the left
        // one works" is a real observation rather than an artifact of both
        // firing at once.
        std::thread::sleep(duration + Duration::from_millis(150));
    }
    println!("Sent. Felt nothing? Try a larger --amplitude, or --period-us 1000..20000.");
    Ok(())
}

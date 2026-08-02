//! `sweam steamcheck`: print parsed inputs from the actual Steam Controller.
//!
//! Runs on the gadget board (the Radxa) with no USB gadget involved: opens
//! the controller and applies the exact same mapping the bridge uses
//! (`steam/mapping.rs`), printing every mapped Pro Controller state change
//! with button and stick names. The input-side counterpart of
//! `sweam hostcheck`. Survives the controller connecting late and
//! disconnecting: the gamepad node only exists while the controller is on.
//!
//! With `--hidraw` it reads raw HID packets instead, which also prints
//! motion — the way to check gyro without a Switch attached.

#[cfg(target_os = "linux")]
pub fn run(
    mapping: crate::steam::mapping::Mapping,
    input: &crate::cli::InputOpts,
) -> anyhow::Result<()> {
    use crate::state::{ControllerState, ImuSample};
    use std::time::{Duration, Instant};

    /// Motion changes on every packet; printing each one would bury the
    /// state changes, so summarize it a couple of times a second.
    const IMU_PRINT_INTERVAL: Duration = Duration::from_millis(500);

    println!("Printing mapped Pro Controller state changes (Ctrl-C to stop)…");
    let started = Instant::now();
    loop {
        // Wait for the controller to show up (or come back).
        let mut controller = {
            let mut waiting = false;
            loop {
                match crate::steam::open_source(
                    mapping.clone(),
                    input.hidraw || mapping.gyro,
                    input.hidraw_device.as_deref(),
                    input.evdev.as_deref(),
                ) {
                    Ok(controller) => break controller,
                    // Waiting can't fix missing permissions — exit instead.
                    Err(err) if crate::steam::is_permission_error(&err) => return Err(err),
                    Err(err) => {
                        if !waiting {
                            println!("Waiting for the Steam Controller (turn it on?): {err:#}");
                            waiting = true;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        };

        // Fresh neutral state per connection; report it as the baseline.
        let mut state = ControllerState::default();
        let mut last = state;
        let mut last_imu_print = Instant::now();
        let elapsed = started.elapsed().as_secs_f64();
        println!("[{elapsed:8.3}s] {}", state.describe());
        loop {
            std::thread::sleep(Duration::from_millis(8));
            if let Err(err) = controller.poll(&mut state) {
                println!("Controller lost ({err:#}); waiting for it to return…");
                break;
            }
            // Buttons and sticks only: motion is far too chatty to treat as
            // a state change, and gets its own throttled line below.
            let changed = state.buttons != last.buttons
                || state.left_stick != last.left_stick
                || state.right_stick != last.right_stick;
            if changed {
                let elapsed = started.elapsed().as_secs_f64();
                println!("[{elapsed:8.3}s] {}", state.describe());
            }
            let motion = state.imu[2];
            if motion != ImuSample::default() && last_imu_print.elapsed() >= IMU_PRINT_INTERVAL {
                let elapsed = started.elapsed().as_secs_f64();
                let gyro = motion.gyro.map(|raw| raw as f32 / ImuSample::GYRO_PER_DPS);
                let accel = motion.accel.map(|raw| raw as f32 / ImuSample::ACCEL_PER_G);
                println!(
                    "[{elapsed:8.3}s] imu gyro=({:+7.1},{:+7.1},{:+7.1}) dps \
                     accel=({:+5.2},{:+5.2},{:+5.2}) g",
                    gyro[0], gyro[1], gyro[2], accel[0], accel[1], accel[2],
                );
                last_imu_print = Instant::now();
            }
            last = state;
        }
    }
}

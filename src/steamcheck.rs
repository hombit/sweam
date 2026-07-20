//! `sweam steamcheck`: print parsed inputs from the actual Steam Controller.
//!
//! Runs on the gadget board (the Radxa) with no USB gadget involved: opens
//! the hid-steam evdev device and applies the exact same mapping the bridge
//! uses (`steam/mapping.rs`), printing every mapped Pro Controller state
//! change with button and stick names. The input-side counterpart of
//! `sweam hostcheck`. Survives the controller connecting late and
//! disconnecting: the gamepad node only exists while the controller is on.

#[cfg(target_os = "linux")]
pub fn run(mapping: crate::steam::mapping::Mapping, evdev: Option<String>) -> anyhow::Result<()> {
    use crate::state::ControllerState;
    use crate::steam::{EvdevSteamController, InputSource};
    use std::time::{Duration, Instant};

    println!("Printing mapped Pro Controller state changes (Ctrl-C to stop)…");
    let started = Instant::now();
    loop {
        // Wait for the controller to show up (or come back).
        let mut controller = {
            let mut waiting = false;
            loop {
                match EvdevSteamController::open(mapping.clone(), evdev.as_deref()) {
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
        let elapsed = started.elapsed().as_secs_f64();
        println!("[{elapsed:8.3}s] {}", state.describe());
        loop {
            std::thread::sleep(Duration::from_millis(8));
            if let Err(err) = controller.poll(&mut state) {
                println!("Controller lost ({err:#}); waiting for it to return…");
                break;
            }
            if state != last {
                let elapsed = started.elapsed().as_secs_f64();
                println!("[{elapsed:8.3}s] {}", state.describe());
                last = state;
            }
        }
    }
}

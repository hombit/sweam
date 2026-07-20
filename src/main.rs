//! sweam: Steam Controller → emulated Switch Pro Controller bridge.
//!
//! See README.md for user instructions, CLAUDE.md for the project overview
//! and PLAN.md for the roadmap.

// The non-Linux build is a check-only stub whose main() uses little, so
// everything would warn; keep dead-code analysis active for the real target.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod cli;
mod hostcheck;
mod install;
mod manual;
mod state;
mod steam;
mod steamcheck;
mod switch;
mod vdf;

/// The mapping from `--config`, or the built-in default layout.
fn load_mapping(config: Option<&str>) -> anyhow::Result<steam::mapping::Mapping> {
    match config {
        Some(path) => {
            let mapping = steam::config::load(path)?;
            println!("Loaded mapping config {path}");
            Ok(mapping)
        }
        None => Ok(steam::mapping::Mapping::default()),
    }
}

fn parse_command_line() -> cli::Command {
    match cli::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("sweam: {err}\nRun \"sweam help\" for usage.");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// hid-nintendo treats input report deltas of 8–17 ms as valid; a real
    /// Pro Controller streams every 8 ms over USB.
    const REPORT_INTERVAL: Duration = Duration::from_millis(8);

    /// Cleared by SIGINT/SIGTERM (and by I/O errors) so the pump loop exits
    /// and the gadget is torn down on Drop instead of leaking in configfs.
    static RUNNING: AtomicBool = AtomicBool::new(true);

    let (gadget_args, manual_mode) = match parse_command_line() {
        cli::Command::Help => {
            println!("{}", cli::HELP);
            return Ok(());
        }
        cli::Command::Version => {
            println!("sweam {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        cli::Command::Hostcheck { device } => return hostcheck::run(device.as_deref()),
        cli::Command::Install { config, prefix } => {
            return install::install(config.as_deref(), prefix.as_deref())
        }
        cli::Command::Uninstall { prefix } => return install::uninstall(prefix.as_deref()),
        cli::Command::Steamcheck { config, evdev } => {
            return steamcheck::run(load_mapping(config.as_deref())?, evdev)
        }
        cli::Command::Steam(args) => (args, false),
        cli::Command::Manual(args) => (args, true),
    };

    extern "C" fn stop(_signal: libc::c_int) {
        RUNNING.store(false, Ordering::SeqCst);
    }
    let handler = stop as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }

    // Full gadget setup: load kernel modules (unless --skip-modprobe),
    // create the configfs gadget (removing a stale one first), bind the UDC.
    let gadget = switch::gadget::UsbGadget::new(switch::gadget::GadgetOptions {
        udc: gadget_args.udc,
        configfs_root: gadget_args.configfs.map(Into::into),
        skip_modprobe: gadget_args.skip_modprobe,
    })
    .context("Failed to set up the Pro Controller USB gadget")?;
    let hidg_path = gadget.hidg_device()?;
    println!("Gadget configured; HID device at {}", hidg_path.display());

    // USB connection-state watcher: log every transition the UDC reports
    // (not attached → default → addressed → configured → suspended). This
    // is how the journal tells "the host never enumerated us" apart from
    // "enumerated but silent", and shows resets/reconnect loops.
    std::thread::spawn({
        let state_path = gadget.udc_state_path();
        move || {
            let mut last = String::new();
            loop {
                let state = std::fs::read_to_string(&state_path).unwrap_or_default();
                let state = state.trim().to_owned();
                if state != last {
                    println!(
                        "USB state: {}",
                        if state.is_empty() { "(unreadable)" } else { &state }
                    );
                    last = state;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    });

    let hidg = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&hidg_path)
        .with_context(|| format!("Failed to open {hidg_path:?}"))?;
    let mut reader = hidg.try_clone().context("Failed to clone hidg handle")?;
    let writer = Arc::new(Mutex::new(hidg));
    let protocol = Arc::new(Mutex::new(switch::protocol::Protocol::new()));
    let state = Arc::new(Mutex::new(state::ControllerState::default()));

    // In steam mode the controller can connect late or drop out (it sleeps
    // after idle); keep the mapping around and (re)open it from the pump
    // loop, streaming neutral inputs whenever it is gone.
    let mapping = (!manual_mode)
        .then(|| load_mapping(gadget_args.config.as_deref()))
        .transpose()?;
    let open_controller = || -> anyhow::Result<Option<Box<dyn steam::InputSource>>> {
        let Some(mapping) = mapping.clone() else {
            return Ok(None);
        };
        match steam::EvdevSteamController::open(mapping, gadget_args.evdev.as_deref()) {
            Ok(controller) => Ok(Some(Box::new(controller) as Box<dyn steam::InputSource>)),
            // Retrying can't fix permissions — fail the whole bridge.
            Err(err) if steam::is_permission_error(&err) => Err(err),
            Err(_) => Ok(None),
        }
    };
    let mut input: Option<Box<dyn steam::InputSource>> = if manual_mode {
        Some(Box::new(manual::ManualInput::new()))
    } else {
        let controller = open_controller()?;
        if controller.is_none() {
            eprintln!("No controller yet; streaming neutral inputs until it appears");
        }
        controller
    };

    // Poison-tolerant lock: our shared state is valid after any panic (all
    // updates are single writes), and unwrapping a PoisonError would turn
    // one panic into a cascade of misleading secondary crashes.
    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // Reader thread: host output reports in, protocol replies out. On I/O
    // errors it stops the pump loop below so main returns and Drop tears the
    // gadget down. Lock order everywhere: protocol, then state, then writer.
    std::thread::spawn({
        let (protocol, state, writer) = (protocol.clone(), state.clone(), writer.clone());
        move || {
            let mut buf = [0u8; switch::report::REPORT_LENGTH];
            loop {
                let n = match reader.read(&mut buf) {
                    // 0 bytes = EOF-like: treat as the host going away, not
                    // a value to retry (that would spin at 100% CPU).
                    Ok(0) => {
                        eprintln!("hidg read returned 0 bytes (host gone?)");
                        break;
                    }
                    Ok(n) => n,
                    Err(err) => {
                        eprintln!("Failed to read from hidg (host gone?): {err}");
                        break;
                    }
                };
                let replies = {
                    let mut protocol = lock(&protocol);
                    let state = lock(&state);
                    protocol.handle_output_report(&buf[..n], &state)
                };
                for reply in replies {
                    if let Err(err) = lock(&writer).write_all(&reply) {
                        eprintln!("Failed to write reply to hidg: {err}");
                        RUNNING.store(false, Ordering::SeqCst);
                        return;
                    }
                }
            }
            RUNNING.store(false, Ordering::SeqCst);
        }
    });

    // Input report pump.
    println!("Waiting for the host handshake…");
    let mut last_retry = std::time::Instant::now();
    while RUNNING.load(Ordering::SeqCst) {
        std::thread::sleep(REPORT_INTERVAL);
        // Hotplug: while no controller is open, retry once a second.
        if input.is_none() && !manual_mode && last_retry.elapsed() >= Duration::from_secs(1) {
            input = open_controller()?;
            last_retry = std::time::Instant::now();
        }
        let report = {
            let mut protocol = lock(&protocol);
            if !protocol.streaming() {
                continue;
            }
            let mut state = lock(&state);
            if let Some(controller) = input.as_mut() {
                if let Err(err) = controller.poll(&mut state) {
                    eprintln!("Controller lost, streaming neutral inputs: {err:#}");
                    input = None;
                    *state = state::ControllerState::default();
                }
            }
            protocol.next_input_report(&state)
        };
        if let Err(err) = lock(&writer).write_all(&report) {
            // The host cutting the connection (unplug, Switch sleeping) is
            // expected operation, not a failure: exit cleanly so the journal
            // stays green and systemd restarts us for a fresh enumeration.
            if matches!(
                err.raw_os_error(),
                Some(libc::ENOTCONN | libc::ESHUTDOWN | libc::EPIPE)
            ) {
                eprintln!("Host disconnected ({err}); exiting for a fresh enumeration");
                break;
            }
            return Err(err).context("Failed to write input report to hidg");
        }
    }
    println!("Shutting down; removing the gadget…");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // Keep help working on the dev machine; everything else needs Linux.
    match parse_command_line() {
        cli::Command::Help => println!("{}", cli::HELP),
        cli::Command::Version => println!("sweam {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!(
                "sweam only runs on Linux (USB gadget API); this build is for development checks."
            );
            std::process::exit(1);
        }
    }
}

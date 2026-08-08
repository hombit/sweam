//! sweam: Steam Controller → emulated Switch Pro Controller bridge.
//!
//! See README.md for user instructions, CLAUDE.md for the project overview
//! and PLAN.md for the roadmap.

// The non-Linux build is a check-only stub whose main() uses little, so
// everything would warn; keep dead-code analysis active for the real target.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod buzz;
mod cli;
mod hostcheck;
mod install;
mod manual;
mod state;
mod steam;
mod steamcheck;
mod switch;
mod trace;
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

/// Parse argv; clap itself prints help/version/usage errors and exits.
fn parse_command_line() -> cli::Command {
    use clap::Parser;
    cli::Cli::parse().command
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

    /// Cap on queued subcommand replies, drained one per report interval.
    /// Deep enough for the Switch's init burst, shallow enough that a host
    /// which stopped polling cannot grow it without bound.
    const MAX_QUEUED_REPLIES: usize = 32;

    /// Cleared by SIGINT/SIGTERM (and by I/O errors) so the pump loop exits
    /// and the gadget is torn down on Drop instead of leaking in configfs.
    static RUNNING: AtomicBool = AtomicBool::new(true);

    // Parse first: `--version`/`--help` exit inside clap, and printing
    // before that would duplicate their output.
    let command = parse_command_line();
    // First line of every run, so a journal always names the build that
    // wrote it — deploys are a bare `scp` with nothing else recording what
    // landed.
    println!("sweam {}", cli::VERSION);

    let (input_args, gadget_args, manual_mode) = match command {
        cli::Command::Hostcheck { device } => return hostcheck::run(device.as_deref()),
        cli::Command::Trace { action } => return trace::run(action),
        cli::Command::Install {
            config,
            prefix,
            trace,
        } => {
            return install::install(config.as_deref(), prefix.prefix.as_deref(), trace);
        }
        cli::Command::Uninstall { prefix } => return install::uninstall(prefix.prefix.as_deref()),
        cli::Command::Steamcheck { input } => {
            let mapping = load_mapping(input.config.as_deref())?;
            return steamcheck::run(mapping, &input);
        }
        cli::Command::Buzz { opts } => return buzz::run(&opts),
        cli::Command::Steam { input, gadget } => (input, gadget, false),
        cli::Command::Manual { gadget } => (cli::InputOpts::default(), gadget, true),
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
        max_power: gadget_args.max_power,
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
                        if state.is_empty() {
                            "(unreadable)"
                        } else {
                            &state
                        }
                    );
                    last = state;
                    // Dump the USB trace here, while it still describes the
                    // transition. Waiting for ExecStopPost no longer works:
                    // sweam now survives the host's reset instead of
                    // exiting, so the service never stops, and the board is
                    // power-cycled between the Switch and the bench, which
                    // wipes the ring buffer. The journal is the only thing
                    // that survives both. Errors are ignored — tracing is
                    // optional and must never disturb the bridge.
                    let _ = trace::run(cli::TraceAction::Snapshot);
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
    // Replies queue here instead of being written by the reader thread.
    // Two threads writing to /dev/hidg0 meant the reader's blocking writes
    // held the writer mutex while waiting for the host's poll (~8 ms each),
    // starving the report pump for the length of a subcommand burst — a
    // measured 80 ms against a host that tolerates 17. One writer also
    // matches the protocol: a 0x21 reply carries the input state itself, so
    // a real controller sends a reply *instead of* a 0x30, never both.
    let replies: Arc<Mutex<std::collections::VecDeque<switch::report::Report>>> =
        Arc::new(Mutex::new(std::collections::VecDeque::new()));

    // In steam mode the controller can connect late or drop out (it sleeps
    // after idle); keep the mapping around and (re)open it from the pump
    // loop, streaming neutral inputs whenever it is gone.
    let mapping = (!manual_mode)
        .then(|| load_mapping(input_args.config.as_deref()))
        .transpose()?;
    // A layout with no motion must not claim motion: keep the IMU region
    // zeroed even if the host enables it, which is how the bridge behaved
    // before phase 6 — a shape known to survive a full Switch session.
    let forwards_motion = manual_mode || mapping.as_ref().is_some_and(|m| m.gyro);
    protocol
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_imu_allowed(forwards_motion);

    // Rumble: the reader thread decodes what the host asks for into this
    // mailbox, and the controller's haptics worker plays it. Created even in
    // manual mode, where nothing reads it — one less conditional.
    let rumble = Arc::new(switch::rumble::RumbleMailbox::default());
    protocol
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_rumble_mailbox(rumble.clone());

    let open_controller = || -> anyhow::Result<Option<Box<dyn steam::InputSource>>> {
        let Some(mapping) = mapping.clone() else {
            return Ok(None);
        };
        match steam::open_source(
            mapping,
            input_args.hidraw_device.as_deref(),
            Some(rumble.clone()),
        ) {
            Ok(controller) => Ok(Some(controller)),
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

    // Reader thread: host output reports in, replies onto the queue for the
    // pump to send — it never touches the hidg fd, so it cannot block the
    // report stream. On I/O errors it stops the pump loop below so main
    // returns and Drop tears the gadget down. Lock order: protocol, then
    // state, then replies.
    std::thread::spawn({
        let (protocol, state, replies) = (protocol.clone(), state.clone(), replies.clone());
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
                let new_replies = {
                    let mut protocol = lock(&protocol);
                    let state = lock(&state);
                    protocol.handle_output_report(&buf[..n], &state)
                };
                let mut queue = lock(&replies);
                for reply in new_replies {
                    // A host that stopped polling must not grow this without
                    // bound; the oldest reply is the least useful.
                    if queue.len() >= MAX_QUEUED_REPLIES {
                        queue.pop_front();
                    }
                    queue.push_back(reply);
                }
            }
            RUNNING.store(false, Ordering::SeqCst);
        }
    });

    // Input report pump.
    println!("Waiting for the host handshake…");
    let mut last_retry = std::time::Instant::now();
    // A stalled pump looks to the host exactly like a dead controller —
    // hid-nintendo accepts report gaps of 8–17 ms and the Switch is no more
    // patient — so measure the gap and say so. Without this, a stall and a
    // protocol rejection are indistinguishable in the journal, which is what
    // made the disconnect hunt so slow.
    let mut last_report = std::time::Instant::now();
    let mut worst_gap = Duration::ZERO;
    let mut next_report = std::time::Instant::now();
    while RUNNING.load(Ordering::SeqCst) {
        // Pace against a fixed deadline, not "sleep 8 ms then work" — though
        // measurement says this loop is not what sets the rate, and the
        // sleep costs nothing either way. Gadget trace against the Switch,
        // 2026-08-03: the next report is armed on the endpoint a median
        // 0.09 ms after the host consumed the previous one, and the host
        // then takes 15.84 ms to poll again. f_hid's write blocks until the
        // *previous* request completes and queues the new one at that
        // instant, so any sleep here overlaps the wait instead of adding to
        // it — removing it entirely changed the period by nothing (16.00 ms
        // either way). The 16 ms is the host's schedule for our endpoint;
        // only the descriptor can move it (see PLAN.md: f_hid hardcodes
        // bInterval 10 at full speed, the real controller declares 8).
        next_report += REPORT_INTERVAL;
        match next_report.checked_duration_since(std::time::Instant::now()) {
            Some(delay) => std::thread::sleep(delay),
            // Already late: resync instead of firing a catch-up burst.
            None => next_report = std::time::Instant::now(),
        }
        // Hotplug: while no controller is open, retry once a second. This
        // runs *in the pump*, so anything slow inside open_controller()
        // stalls the report stream (the hidraw probe sleeps between its
        // settings-report retries — up to 60 ms per slot).
        if input.is_none() && !manual_mode && last_retry.elapsed() >= Duration::from_secs(1) {
            input = open_controller()?;
            last_retry = std::time::Instant::now();
        }
        let report = {
            let mut protocol = lock(&protocol);
            let mut state = lock(&state);
            // Poll even when the host isn't streaming yet. Skipping it would
            // leave packets queueing in the kernel buffers, so a session
            // would open by replaying a backlog of stale input (a stale
            // camera deflection among it), and wireless connect/disconnect
            // events would arrive late or not at all.
            if let Some(controller) = input.as_mut()
                && let Err(err) = controller.poll(&mut state)
            {
                eprintln!("Controller lost, streaming neutral inputs: {err:#}");
                input = None;
                *state = state::ControllerState::default();
            }
            // Replies first — they are answers the host is waiting on, and
            // they carry the input state anyway. Note this runs before the
            // streaming check: the handshake happens before streaming.
            if let Some(reply) = lock(&replies).pop_front() {
                Some(reply)
            } else if protocol.streaming() {
                Some(protocol.next_input_report(&state))
            } else {
                None
            }
        };
        let Some(report) = report else {
            // Keep the stall clock honest — without this the first report
            // after the handshake measures the whole pre-streaming wait and
            // reports a stall that never happened.
            last_report = std::time::Instant::now();
            continue;
        };
        // Report the gap since the previous report, but only when it is bad
        // enough to matter and worse than anything reported so far — a stall
        // storm must not itself become a logging storm.
        let gap = last_report.elapsed();
        if gap > Duration::from_millis(17) && gap > worst_gap {
            worst_gap = gap;
            eprintln!(
                "Report pump stalled for {} ms (host tolerates ~17)",
                gap.as_millis()
            );
        }
        last_report = std::time::Instant::now();
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
    // clap makes help/version work on the dev machine (it prints them and
    // exits inside parse); every actual subcommand needs Linux.
    let _ = parse_command_line();
    eprintln!("sweam only runs on Linux (USB gadget API); this build is for development checks.");
    std::process::exit(1);
}

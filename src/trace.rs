//! `sweam trace`: record the gadget-side USB conversation.
//!
//! When the Switch tears the connection down, sweam's own log shows what the
//! host *asked* for but never what happened on the wire — whether the host
//! reset the port, suspended it, or simply stopped talking. That gap is what
//! made the disconnect hunt so slow.
//!
//! `usbmon` cannot fill it: it captures on a USB *host* controller, and this
//! board is the peripheral (the host is the Switch, which we cannot
//! instrument). The gadget-side equivalent is the UDC driver's own ftrace
//! tracepoints — `dwc3_event` for bus events (reset/disconnect/suspend),
//! `dwc3_ctrl_req` for control requests the gadget layer answers without
//! ever telling us, and the `gadget:` group for connect/disconnect.
//!
//! Recording is a ring buffer, so nothing has to be caught live: after a
//! disconnect it still holds the events leading up to it. `sweam install
//! --trace` wires `snapshot` into the service's `ExecStopPost`, so every
//! disconnect leaves its trace in the journal with no one at the keyboard —
//! which matters because the board is plugged into a Switch, not a desk.

#[cfg(target_os = "linux")]
pub use linux::run;

#[cfg(target_os = "linux")]
mod linux {
    use crate::cli::TraceAction;
    use anyhow::{Context, bail};
    use std::path::Path;

    const TRACING: &str = "/sys/kernel/debug/tracing";
    /// Enough to cover the seconds before a teardown without burying the
    /// journal on a board that reconnects every few seconds.
    const SNAPSHOT_LINES: usize = 400;

    /// Bus events, control requests, endpoint commands, and the gadget
    /// layer's own connect/disconnect.
    const EVENTS: &[&str] = &[
        "dwc3/dwc3_event",
        "dwc3/dwc3_ctrl_req",
        "dwc3/dwc3_gadget_ep_cmd",
        "gadget/usb_gadget_connect",
        "gadget/usb_gadget_disconnect",
    ];

    pub fn run(action: TraceAction) -> anyhow::Result<()> {
        let tracing = Path::new(TRACING);
        if !tracing.exists() {
            bail!(
                "{TRACING} is missing — mount debugfs first: \
                 sudo mount -t debugfs none /sys/kernel/debug"
            );
        }
        match action {
            TraceAction::Start => start(),
            TraceAction::Snapshot => snapshot(),
            TraceAction::Dump => dump(),
            TraceAction::Stop => stop(),
        }
    }

    fn start() -> anyhow::Result<()> {
        write("tracing_on", "0")?;
        write("trace", "")?;
        write("buffer_size_kb", "16384")?;
        let mut enabled = 0;
        for event in EVENTS {
            // A kernel without a given tracepoint is not fatal: the others
            // still record, and which exist depends on the UDC driver.
            if write(&format!("events/{event}/enable"), "1").is_ok() {
                enabled += 1;
            } else {
                eprintln!("trace: no {event} tracepoint on this kernel");
            }
        }
        if enabled == 0 {
            bail!("None of the USB tracepoints exist — is this a dwc3 UDC?");
        }
        write("tracing_on", "1")?;
        println!("trace: recording {enabled} event source(s) into a 16 MB ring");
        Ok(())
    }

    /// Print the tail of the buffer and clear it, so each snapshot covers
    /// only what happened since the last one.
    fn snapshot() -> anyhow::Result<()> {
        let trace = read("trace")?;
        let lines: Vec<&str> = trace
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .collect();
        // Ordinary restarts record nothing; staying quiet keeps the journal
        // readable.
        if !lines.is_empty() {
            println!("trace: last USB events before exit");
            for line in lines.iter().rev().take(SNAPSHOT_LINES).rev() {
                println!("{line}");
            }
        }
        write("trace", "")?;
        Ok(())
    }

    fn dump() -> anyhow::Result<()> {
        print!("{}", read("trace")?);
        Ok(())
    }

    fn stop() -> anyhow::Result<()> {
        write("tracing_on", "0")?;
        for event in EVENTS {
            let _ = write(&format!("events/{event}/enable"), "0");
        }
        println!("trace: stopped");
        Ok(())
    }

    fn write(relative: &str, value: &str) -> anyhow::Result<()> {
        let path = format!("{TRACING}/{relative}");
        std::fs::write(&path, value).with_context(|| format!("Failed to write {path} (need root?)"))
    }

    fn read(relative: &str) -> anyhow::Result<String> {
        let path = format!("{TRACING}/{relative}");
        std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {path} (need root?)"))
    }
}

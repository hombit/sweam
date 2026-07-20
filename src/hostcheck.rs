//! `sweam hostcheck`: userspace stand-in for hid-nintendo on the debug host.
//!
//! Runs on the *host* side of the cable (the Pi 3 on our bench, where
//! openSUSE ships no hid-nintendo — see TESTBED.md), against the hidraw node
//! hid-generic exposes for the gadget. Drives the same USB handshake as
//! hid-nintendo's `joycon_init()`, then decodes the 0x30 stream and prints
//! every button/stick change. It reuses the project's own report layout
//! code, so it verifies the wire format matches what we think we send —
//! the independent-oracle test still needs a real hid-nintendo host.

use crate::state::{ControllerState, ImuSample, StickState};
use crate::switch::report::{REPORT_LENGTH, unpack_stick};
use anyhow::{Context, bail};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const READ_TIMEOUT: Duration = Duration::from_secs(3);

pub fn run(device: Option<&str>) -> anyhow::Result<()> {
    let path = match device {
        Some(path) => path.to_owned(),
        None => detect_device()?,
    };
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("Failed to open {path} (root? gadget attached?)"))?;
    println!("Opened {path}");

    // Blocking reads live on their own thread so the handshake can time out.
    let reports = {
        let mut file = file.try_clone().context("Failed to clone hidraw handle")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; REPORT_LENGTH];
            while let Ok(n) = file.read(&mut buf) {
                if n > 0 && tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        rx
    };

    // USB handshake, same order as hid-nintendo's joycon_init(): status,
    // handshake, then "USB HID only" — which has no reply and starts the
    // 0x30 stream instead.
    for command in [0x01u8, 0x02, 0x04] {
        file.write_all(&[0x80, command])
            .with_context(|| format!("Failed to send USB command {command:#04x}"))?;
        if command == 0x04 {
            break;
        }
        match reports.recv_timeout(READ_TIMEOUT) {
            Ok(reply) if reply.starts_with(&[0x81, command]) => {
                let shown = usize::min(reply.len(), 12);
                println!(
                    "0x80 {command:#04x} -> {reply:02x?}",
                    reply = &reply[..shown]
                );
            }
            Ok(reply) => bail!("USB command {command:#04x}: unexpected reply {reply:02x?}"),
            Err(_) => bail!("USB command {command:#04x}: no reply within {READ_TIMEOUT:?}"),
        }
    }

    // Enable the IMU (subcommand 0x40 arg 1, as hid-nintendo does) so the
    // gadget fills the motion sample fields of its 0x30 reports.
    let mut enable_imu = [0u8; 12];
    enable_imu[0] = 0x01; // subcommand output report
    enable_imu[10] = 0x40;
    enable_imu[11] = 0x01;
    file.write_all(&enable_imu)
        .context("Failed to send the IMU-enable subcommand")?;

    println!("Streaming; waiting for input changes (Ctrl-C to stop)…");
    let started = Instant::now();
    let mut last = None;
    let mut count = 0u64;
    let mut last_imu_print = Instant::now();
    loop {
        let report = match reports.recv_timeout(READ_TIMEOUT) {
            Ok(report) => report,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                println!("Stream stalled: no report within {READ_TIMEOUT:?}");
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("hidraw closed (gadget gone?)"),
        };
        if report.first() == Some(&0x21) {
            // Subcommand ack; for IMU enable just confirm it.
            if report.len() > 14 && report[14] == 0x40 {
                println!("IMU enable acked (0x21, ack {:#04x})", report[13]);
            }
            continue;
        }
        if report.first() != Some(&0x30) || report.len() < 49 {
            println!("Non-0x30 report: {report:02x?}");
            continue;
        }
        count += 1;
        let state = decode(&report);
        // Buttons/sticks: print on every change (IMU excluded — it changes
        // with every report once motion streams).
        let mut inputs = state;
        inputs.imu = Default::default();
        if last != Some(inputs) {
            let elapsed = started.elapsed().as_secs_f64();
            println!("[{elapsed:8.3}s #{count:6}] {}", inputs.describe());
            last = Some(inputs);
        }
        // Motion: newest frame, converted to physical units, at most 2/s.
        let newest = state.imu[2];
        if newest != ImuSample::default() && last_imu_print.elapsed() >= Duration::from_millis(500)
        {
            let elapsed = started.elapsed().as_secs_f64();
            let acc = newest.accel.map(|v| f32::from(v) / ImuSample::ACCEL_PER_G);
            let gyro = newest.gyro.map(|v| f32::from(v) / ImuSample::GYRO_PER_DPS);
            println!(
                "[{elapsed:8.3}s #{count:6}] imu accel=({:+.2},{:+.2},{:+.2}) g \
                 gyro=({:+7.1},{:+7.1},{:+7.1}) dps",
                acc[0], acc[1], acc[2], gyro[0], gyro[1], gyro[2]
            );
            last_imu_print = Instant::now();
        }
    }
}

/// Find the Pro Controller's hidraw node by USB IDs: scan
/// `/sys/class/hidraw/*/device/uevent` for `HID_ID=….VID:PID`. An explicit
/// device argument overrides this.
fn detect_device() -> anyhow::Result<String> {
    let mut found: Vec<String> = std::fs::read_dir("/sys/class/hidraw")
        .context("Failed to list /sys/class/hidraw — no HID devices at all?")?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let uevent =
                std::fs::read_to_string(entry.path().join("device/uevent")).unwrap_or_default();
            let name = entry.file_name().into_string().ok()?;
            is_pro_controller(&uevent).then_some(name)
        })
        .collect();
    found.sort();
    if found.is_empty() {
        bail!(
            "No Pro Controller ({:04x}:{:04x}) hidraw device found — is the sweam \
             gadget attached and enumerated? (or pass its /dev/hidrawN explicitly)",
            crate::switch::gadget::NINTENDO_VID,
            crate::switch::gadget::PRO_CONTROLLER_PID,
        );
    }
    if found.len() > 1 {
        eprintln!(
            "Warning: several Pro Controller hidraw nodes ({}); using the first — \
             pass one explicitly to override",
            found.join(", ")
        );
    }
    let path = format!("/dev/{}", found[0]);
    println!("Detected Pro Controller at {path}");
    Ok(path)
}

/// Match a hidraw uevent's `HID_ID=<bus>:<vendor>:<product>` line against
/// the Pro Controller USB IDs.
fn is_pro_controller(uevent: &str) -> bool {
    let suffix = format!(
        ":{:08X}:{:08X}",
        crate::switch::gadget::NINTENDO_VID,
        crate::switch::gadget::PRO_CONTROLLER_PID
    );
    uevent.lines().any(|line| {
        line.strip_prefix("HID_ID=")
            .is_some_and(|id| id.to_ascii_uppercase().ends_with(&suffix))
    })
}

/// Decode the input-state block of a 0x30 report back into a
/// [`ControllerState`] (the inverse of `report::standard_input_report`),
/// including the three IMU sample frames (bytes 13..49, oldest first).
fn decode(report: &[u8]) -> ControllerState {
    let buttons = u32::from(report[3]) | u32::from(report[4]) << 8 | u32::from(report[5]) << 16;
    let (lx, ly) = unpack_stick(&report[6..9]);
    let (rx, ry) = unpack_stick(&report[9..12]);
    let mut imu = [ImuSample::default(); 3];
    for (frame, sample) in imu.iter_mut().enumerate() {
        let base = 13 + frame * 12;
        let read = |i: usize| i16::from_le_bytes([report[base + i * 2], report[base + i * 2 + 1]]);
        sample.accel = [read(0), read(1), read(2)];
        sample.gyro = [read(3), read(4), read(5)];
    }
    ControllerState {
        buttons,
        left_stick: StickState { x: lx, y: ly },
        right_stick: StickState { x: rx, y: ry },
        imu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Button;
    use crate::switch::report::standard_input_report;

    /// Packing (gadget side) and decoding (hostcheck side) must agree.
    #[test]
    fn decode_inverts_report_packing() {
        let mut state = ControllerState::default();
        state.set_button(Button::A, true);
        state.set_button(Button::Home, true);
        state.set_button(Button::ZL, true);
        state.left_stick = StickState { x: 0, y: 4095 };
        state.right_stick = StickState { x: 123, y: 3210 };
        state.imu[2] = ImuSample {
            accel: [1, -2, 4096],
            gyro: [-164, 0, 32767],
        };

        assert_eq!(decode(&standard_input_report(&state, 0, true)), state);
        assert_eq!(
            state.describe(),
            "buttons=A+Home+ZL L=(0,4095) R=(123,3210)"
        );

        // With the IMU disabled the frames decode as zeros.
        let decoded = decode(&standard_input_report(&state, 0, false));
        assert_eq!(decoded.imu, [ImuSample::default(); 3]);
    }

    #[test]
    fn pro_controller_uevent_matching() {
        let ours = "DRIVER=hid-generic\n\
                    HID_ID=0003:0000057E:00002009\n\
                    HID_NAME=Nintendo Co., Ltd. Pro Controller\n";
        assert!(is_pro_controller(ours));
        // Different product (a real Steam Controller dongle).
        assert!(!is_pro_controller("HID_ID=0003:000028DE:00001142\n"));
        assert!(!is_pro_controller(""));
        // Lowercase hex, as some kernels format it.
        assert!(is_pro_controller("HID_ID=0003:0000057e:00002009\n"));
    }

    #[test]
    fn neutral_state_decodes_to_none() {
        let decoded = decode(&standard_input_report(
            &ControllerState::default(),
            0,
            false,
        ));
        assert_eq!(
            decoded.describe(),
            "buttons=none L=(2048,2048) R=(2048,2048)"
        );
    }
}

//! Pro Controller protocol state machine.
//!
//! Over USB the conversation with the Switch (or the kernel `hid-nintendo`
//! driver, which speaks the same protocol and is our pre-Switch test host) is:
//!
//! 1. **USB handshake**: the host sends `0x80`-prefixed commands; we reply
//!    with `0x81`-prefixed responses. After `RequireUsbHidOnly` (0x04) we
//!    start streaming input reports.
//! 2. **Subcommands**: the host sends `0x01` output reports (rumble data +
//!    subcommand at byte 10); we reply with `0x21` input reports (current
//!    input state + ack + payload). This includes `SpiFlashRead` requests for
//!    factory calibration, served from a baked SPI image.
//! 3. **Steady state**: we stream 0x30 reports every ~8 ms and keep parsing
//!    incoming `0x10` rumble reports (phase 5 forwards those to the Steam
//!    Controller haptics).
//!
//! Byte-level sources (do not tweak values without a trace to justify it):
//! - mzyy94's `simulate_procon.py` — drove a real Switch; response table and
//!   SPI values are lifted from it verbatim.
//! - Linux `drivers/hid/hid-nintendo.c` — `joycon_init()` defines the exact
//!   USB init sequence and which SPI ranges are read; `joycon_input_report`
//!   defines the reply layout it parses.
//! - dekuNukem/Nintendo_Switch_Reverse_Engineering for field meanings.

use crate::state::ControllerState;
use crate::switch::report::{self, REPORT_LENGTH, Report};

/// Locally-administered MAC in the IANA documentation range (from mzyy94).
pub const MAC_ADDRESS: [u8; 6] = [0x00, 0x00, 0x5E, 0x00, 0x53, 0x5E];

/// Largest SPI read a real controller serves per request (hid-nintendo's
/// `JC_SPI_MAX_TRANSFER`); also what fits a 64-byte `0x21` reply.
const MAX_SPI_READ: usize = 0x1D;

/// `0x80` USB command IDs (second byte of a `0x80 ..` output report).
/// Names follow `JC_USB_CMD_*` in hid-nintendo.c.
///
/// Documentation of the wire values handled in `handle_usb_command` (u8
/// literals can't be matched against enum variants without casts).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UsbCommand {
    ConnStatus = 0x01,
    Handshake = 0x02,
    Baudrate3M = 0x03,
    /// "No timeout / force USB HID" — host expects the report stream to start.
    NoTimeout = 0x04,
    EnableTimeout = 0x05,
}

/// Subcommand IDs carried in `0x01` output reports (byte 10).
/// Names follow `JC_SUBCMD_*` in hid-nintendo.c.
///
/// Documentation of the wire values handled in `handle_subcommand`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Subcommand {
    ControllerState = 0x00,
    ManualBtPairing = 0x01,
    RequestDeviceInfo = 0x02,
    SetInputReportMode = 0x03,
    TriggersElapsed = 0x04,
    LowPowerMode = 0x08,
    SpiFlashRead = 0x10,
    SetMcuConfig = 0x21,
    SetPlayerLights = 0x30,
    SetHomeLight = 0x38,
    EnableImu = 0x40,
    EnableVibration = 0x48,
}

/// Emulated SPI flash: sparse regions over a 0xFF-filled address space
/// (0xFF = "never written", which is how a real controller marks absent
/// user calibration). Values are from a real Pro Controller via mzyy94,
/// except the factory IMU calibration region (0x6020), which reuses the
/// values his script served as *user* IMU calibration (0x8028).
///
/// Layout reference: dekuNukem `spi_flash_notes.md`; the ranges hid-nintendo
/// actually reads are `JC_CAL_*`/`JC_IMU_CAL_*` in hid-nintendo.c:
/// factory stick 0x603D/0x6046, factory IMU 0x6020, user magics 0x8010/
/// 0x801B/0x8026 (we leave those 0xFF → host falls back to factory data).
#[rustfmt::skip]
const SPI_REGIONS: &[(usize, &[u8])] = &[
    // Serial number: unset.
    (0x6000, &[0xFF; 16]),
    // Factory IMU calibration.
    (0x6020, &[
        0xBE, 0xFF, 0x3E, 0x00, 0xF0, 0x01, 0x00, 0x40, 0x00, 0x40, 0x00, 0x40,
        0xFE, 0xFF, 0xFE, 0xFF, 0x08, 0x00, 0xE7, 0x3B, 0xE7, 0x3B, 0xE7, 0x3B,
    ]),
    // Factory stick calibration: left (9 bytes at 0x603D), right (9 at 0x6046).
    (0x603D, &[0xBA, 0x15, 0x62, 0x11, 0xB8, 0x7F, 0x29, 0x06, 0x5B]),
    (0x6046, &[0xFF, 0xE7, 0x7E, 0x0E, 0x36, 0x56, 0x9E, 0x85, 0x60]),
    // Body/button colors: dark blue body (#191970), white buttons.
    (0x6050, &[0x19, 0x19, 0x70, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
    // Factory sensor/stick device parameters.
    (0x6080, &[
        0x50, 0xFD, 0x00, 0x00, 0xC6, 0x0F, 0x0F, 0x30, 0x61, 0x96, 0x30, 0xF3,
        0xD4, 0x14, 0x54, 0x41, 0x15, 0x54, 0xC7, 0x79, 0x9C, 0x33, 0x36, 0x63,
    ]),
    // Factory stick device parameters 2.
    (0x6098, &[
        0x0F, 0x30, 0x61, 0x96, 0x30, 0xF3, 0xD4, 0x14, 0x54, 0x41, 0x15, 0x54,
        0xC7, 0x79, 0x9C, 0x33, 0x36, 0x63,
    ]),
];

/// Read `len` bytes at `addr` from the emulated SPI flash.
fn spi_read(addr: usize, len: usize) -> Vec<u8> {
    let mut data = vec![0xFF; len];
    let read_end = addr + len;
    for &(start, region) in SPI_REGIONS {
        let region_end = start + region.len();
        // Overlap of [addr, read_end) with [start, region_end).
        let from = usize::max(addr, start);
        let to = usize::min(read_end, region_end);
        if from < to {
            data[from - addr..to - addr].copy_from_slice(&region[from - start..to - start]);
        }
    }
    data
}

/// Protocol driver: owns the handshake/subcommand state and produces reply
/// and input-stream reports. I/O (reading/writing `/dev/hidg0`) lives in the
/// caller so this stays trivially unit-testable.
#[derive(Debug, Default)]
pub struct Protocol {
    /// Free-running counter stamped into every input report.
    timer: u8,
    /// Set once the host asked for the input stream (USB 0x04, or input
    /// report mode 0x30); before that we only answer requests.
    streaming: bool,
    /// Whether the host enabled the IMU (subcommand 0x40 arg 1); until then
    /// the 0x30 IMU sample fields stay zero, like a real controller.
    imu_enabled: bool,
    /// Dedupe for the raw pre-streaming traffic log: the last report seen
    /// and how many times it repeated without being printed.
    last_raw: Vec<u8>,
    last_raw_repeats: u32,
}

impl Protocol {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the steady-state 0x30 stream should be pumped (~every 8 ms;
    /// hid-nintendo considers report deltas of 8–17 ms valid).
    pub fn streaming(&self) -> bool {
        self.streaming
    }

    /// Handle one output report from the host, returning the reports to send
    /// back, in order. `state` is embedded in `0x21` replies, which carry the
    /// current input state alongside the subcommand ack.
    pub fn handle_output_report(&mut self, data: &[u8], state: &ControllerState) -> Vec<Report> {
        // Until streaming starts, log every host report verbatim (deduped) —
        // cheap then, and it captures malformed/unexpected traffic that the
        // match arms below would silently drop. Once streaming, the host
        // sends rumble at full rate, so raw logging must stop.
        if !self.streaming {
            if data == self.last_raw {
                self.last_raw_repeats += 1;
            } else {
                self.flush_raw_repeats();
                println!("Host report: {data:02X?}");
                self.last_raw = data.to_vec();
            }
        }
        match data.first() {
            Some(0x80) if data.len() >= 2 => self.handle_usb_command(data[1]),
            // Subcommand: bytes 1..10 are packet counter + rumble data.
            Some(0x01) if data.len() >= 11 => self.handle_subcommand(data[10], &data[11..], state),
            // Rumble-only report: no reply. TODO(phase 5): forward to haptics.
            Some(0x10) => vec![],
            Some(_) | None => {
                if self.streaming {
                    eprintln!("Unhandled output report: {data:02X?}");
                }
                vec![]
            }
        }
    }

    /// If the last raw-logged report repeated silently, say how many times.
    fn flush_raw_repeats(&mut self) {
        if self.last_raw_repeats > 0 {
            println!("  … repeated {} more times", self.last_raw_repeats);
            self.last_raw_repeats = 0;
        }
    }

    /// Next 0x30 standard input report for the current controller state.
    pub fn next_input_report(&mut self, state: &ControllerState) -> Report {
        report::standard_input_report(state, self.next_timer(), self.imu_enabled)
    }

    fn next_timer(&mut self) -> u8 {
        let timer = self.timer;
        self.timer = self.timer.wrapping_add(1);
        timer
    }

    fn handle_usb_command(&mut self, command: u8) -> Vec<Report> {
        // Log handshake progress: invaluable in the journal when debugging
        // against a host we can't observe (the Switch).
        match command {
            // Status: 0x00, type (0x03 = Pro Controller), MAC.
            0x01 => {
                println!("Host USB command 0x01 (status request)");
                let mut payload = vec![0x00, 0x03];
                payload.extend_from_slice(&MAC_ADDRESS);
                vec![usb_response(command, &payload)]
            }
            0x02 | 0x03 => {
                println!("Host USB command {command:#04x} (handshake/baud)");
                vec![usb_response(command, &[])]
            }
            // No response; the host now expects the input stream.
            0x04 => {
                self.flush_raw_repeats();
                println!("Host USB command 0x04 (HID-only) — input stream on");
                self.streaming = true;
                vec![]
            }
            0x05 => {
                println!("Host USB command 0x05 — input stream off");
                self.streaming = false;
                vec![]
            }
            _ => {
                eprintln!("Unhandled USB command: {command:#04X}");
                vec![]
            }
        }
    }

    fn handle_subcommand(
        &mut self,
        subcommand: u8,
        args: &[u8],
        state: &ControllerState,
    ) -> Vec<Report> {
        println!("Host subcommand {subcommand:#04x}");
        // ACK byte: MSB set = OK; lower bits are a data-type tag when the
        // reply carries a payload. Values mirror simulate_procon.py.
        let (ack, payload): (u8, Vec<u8>) = match subcommand {
            // Bluetooth manual pairing: not relevant over USB, ack with 0x03.
            0x01 => (0x81, vec![0x03]),
            // Device info: firmware 3.48, type 0x03 (Pro Controller), 0x02,
            // MAC, 0x03, 0x01 (= colors are stored in SPI).
            0x02 => {
                let mut payload = vec![0x03, 0x48, 0x03, 0x02];
                payload.extend_from_slice(&MAC_ADDRESS);
                payload.extend_from_slice(&[0x03, 0x01]);
                (0x82, payload)
            }
            // Set input report mode: 0x30 = standard full mode.
            0x03 => {
                if args.first() == Some(&0x30) {
                    self.flush_raw_repeats();
                    self.streaming = true;
                }
                (0x80, vec![])
            }
            // Trigger buttons elapsed time.
            0x04 => (0x83, vec![]),
            // SPI flash read: args are addr (LE u32) + length; reply echoes
            // them followed by the data. Clamp the length to the real
            // controller's per-read cap: an unclamped 255 would overflow
            // the 64-byte reply report (a host-triggerable panic).
            0x10 if args.len() >= 5 => {
                let addr = u32::from_le_bytes([args[0], args[1], args[2], args[3]]) as usize;
                let len = usize::min(args[4] as usize, MAX_SPI_READ);
                let mut payload = args[..5].to_vec();
                payload[4] = len as u8;
                payload.extend_from_slice(&spi_read(addr, len));
                (0x90, payload)
            }
            // Set NFC/IR MCU configuration: the reply must be a full 34-byte
            // MCU state report — status bytes, zero padding, and a crc8 of
            // the payload as its last byte (0xC8 for this constant payload).
            // The Switch retries 0x21 every 32 ms and eventually drops the
            // controller if the reply doesn't parse; values per nxbt (MIT).
            0x21 => {
                let mut payload = vec![0u8; 34];
                payload[..8].copy_from_slice(&[0x01, 0x00, 0xFF, 0x00, 0x08, 0x00, 0x1B, 0x01]);
                payload[33] = 0xC8;
                (0xA0, payload)
            }
            // Enable/disable the IMU: gates whether 0x30 reports carry
            // motion samples.
            0x40 => {
                self.imu_enabled = args.first().is_some_and(|&on| on != 0);
                println!(
                    "IMU {} by host",
                    if self.imu_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                (0x80, vec![])
            }
            // Plain acks: low power (0x08), set MCU state (0x22), player
            // lights (0x30), home light (0x38), vibration enable (0x48),
            // and — to stay permissive — anything we don't know.
            0x08 | 0x22 | 0x30 | 0x38 | 0x48 => (0x80, vec![]),
            _ => {
                eprintln!("Generic ack for unhandled subcommand: {subcommand:#04X} {args:02X?}");
                (0x80, vec![])
            }
        };
        vec![self.subcommand_reply(ack, subcommand, &payload, state)]
    }

    /// Build a `0x21` subcommand-reply report: input state prefix, then
    /// ack byte, subcommand ID, and payload (`joycon_subcmd_reply` layout).
    fn subcommand_reply(
        &mut self,
        ack: u8,
        subcommand: u8,
        payload: &[u8],
        state: &ControllerState,
    ) -> Report {
        let mut r = [0u8; REPORT_LENGTH];
        r[0] = 0x21;
        r[1] = self.next_timer();
        r[2..13].copy_from_slice(&report::input_state_bytes(state));
        r[13] = ack;
        r[14] = subcommand;
        r[15..15 + payload.len()].copy_from_slice(payload);
        r
    }
}

/// Build a `0x81` response to a `0x80` USB command.
fn usb_response(command: u8, payload: &[u8]) -> Report {
    let mut r = [0u8; REPORT_LENGTH];
    r[0] = 0x81;
    r[1] = command;
    r[2..2 + payload.len()].copy_from_slice(payload);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Button;

    /// Pad an output report to the 64 bytes the host actually sends.
    fn out(bytes: &[u8]) -> Vec<u8> {
        let mut v = bytes.to_vec();
        v.resize(REPORT_LENGTH, 0);
        v
    }

    /// A `0x01` subcommand output report as hid-nintendo builds it:
    /// id, packet counter, 8 bytes rumble, subcommand, args.
    fn subcmd(id: u8, args: &[u8]) -> Vec<u8> {
        let mut v = vec![0x01, 0x00];
        v.extend_from_slice(&[0; 8]);
        v.push(id);
        v.extend_from_slice(args);
        out(&v)
    }

    fn handle(protocol: &mut Protocol, data: &[u8]) -> Vec<Report> {
        protocol.handle_output_report(data, &ControllerState::default())
    }

    #[test]
    fn oversized_spi_read_is_clamped_not_a_panic() {
        // A hostile/buggy host may request up to 255 bytes; unclamped this
        // overflowed the 64-byte reply report and panicked the service.
        let mut p = Protocol::new();
        let replies = handle(&mut p, &subcmd(0x10, &[0x00, 0x60, 0x00, 0x00, 0xFF]));
        assert_eq!(replies.len(), 1);
        let r = &replies[0];
        assert_eq!(r[13], 0x90);
        assert_eq!(r[14], 0x10);
        assert_eq!(r[19], MAX_SPI_READ as u8, "echoed length is clamped");
    }

    #[test]
    fn usb_stream_off_stops_streaming() {
        let mut p = Protocol::new();
        handle(&mut p, &out(&[0x80, 0x04]));
        assert!(p.streaming());
        let replies = handle(&mut p, &out(&[0x80, 0x05]));
        assert!(replies.is_empty());
        assert!(!p.streaming());
    }

    #[test]
    fn imu_enable_gates_motion_samples() {
        let mut p = Protocol::new();
        let mut state = ControllerState::default();
        state.imu[2] = crate::state::ImuSample {
            accel: [1, 2, 3],
            gyro: [4, 5, 6],
        };

        // Off by default: sample fields stay zero.
        assert!(p.next_input_report(&state)[13..49].iter().all(|&b| b == 0));

        // 0x40 arg 1 enables, arg 0 disables again.
        handle(&mut p, &subcmd(0x40, &[0x01]));
        assert!(p.next_input_report(&state)[13..49].iter().any(|&b| b != 0));
        handle(&mut p, &subcmd(0x40, &[0x00]));
        assert!(p.next_input_report(&state)[13..49].iter().all(|&b| b == 0));
    }

    #[test]
    fn mcu_config_reply_is_a_full_state_report() {
        // The Switch retries 0x21 forever (and then drops the controller)
        // unless the reply is a full 34-byte MCU state report with the crc
        // byte at report offset 48.
        let mut p = Protocol::new();
        let replies = handle(&mut p, &subcmd(0x21, &[0x21, 0x00]));
        assert_eq!(replies.len(), 1);
        let r = &replies[0];
        assert_eq!(r[0], 0x21);
        assert_eq!(r[13], 0xA0); // ack, MCU-data tag
        assert_eq!(r[14], 0x21);
        assert_eq!(r[15..23], [0x01, 0x00, 0xFF, 0x00, 0x08, 0x00, 0x1B, 0x01]);
        assert_eq!(r[23..48], [0; 25]); // zero padding
        assert_eq!(r[48], 0xC8); // crc8 of the payload
    }

    #[test]
    fn usb_handshake_sequence() {
        // The exact joycon_init() sequence from hid-nintendo.c.
        let mut p = Protocol::new();

        let replies = handle(&mut p, &out(&[0x80, 0x02]));
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0][..2], [0x81, 0x02]);

        let replies = handle(&mut p, &out(&[0x80, 0x03]));
        assert_eq!(replies[0][..2], [0x81, 0x03]);

        let replies = handle(&mut p, &out(&[0x80, 0x02]));
        assert_eq!(replies[0][..2], [0x81, 0x02]);

        assert!(!p.streaming());
        let replies = handle(&mut p, &out(&[0x80, 0x04]));
        assert!(replies.is_empty(), "0x04 must not be acked");
        assert!(p.streaming(), "0x04 must start the input stream");
    }

    #[test]
    fn usb_status_reports_pro_controller_mac() {
        let mut p = Protocol::new();
        let replies = handle(&mut p, &out(&[0x80, 0x01]));
        assert_eq!(replies[0][..4], [0x81, 0x01, 0x00, 0x03]);
        assert_eq!(replies[0][4..10], MAC_ADDRESS);
    }

    #[test]
    fn device_info_reply_layout() {
        // hid-nintendo reads type from subcmd_reply.data[2] (report byte 17)
        // and MAC from data[4..10] (report bytes 19..25).
        let mut p = Protocol::new();
        let replies = handle(&mut p, &subcmd(0x02, &[]));
        let r = &replies[0];
        assert_eq!(r[0], 0x21);
        assert_eq!(r[13], 0x82, "device info ack");
        assert_eq!(r[14], 0x02, "subcommand echo");
        assert_eq!(r[17], 0x03, "controller type must be Pro Controller");
        assert_eq!(r[19..25], MAC_ADDRESS);
    }

    #[test]
    fn spi_read_reply_echoes_request_and_serves_calibration() {
        let mut p = Protocol::new();
        // Factory left stick calibration, exactly as hid-nintendo requests it.
        let replies = handle(&mut p, &subcmd(0x10, &[0x3D, 0x60, 0x00, 0x00, 9]));
        let r = &replies[0];
        assert_eq!(r[13], 0x90, "SPI read ack");
        assert_eq!(r[14], 0x10);
        assert_eq!(r[15..20], [0x3D, 0x60, 0x00, 0x00, 9], "addr + len echo");
        // Read data starts at subcmd_reply.data[5] (report byte 20).
        assert_eq!(
            r[20..29],
            [0xBA, 0x15, 0x62, 0x11, 0xB8, 0x7F, 0x29, 0x06, 0x5B]
        );
    }

    #[test]
    fn spi_read_spanning_regions_and_gaps() {
        // 0x603D..0x6055: left cal + right cal + a 0xFF gap byte + colors.
        let data = spi_read(0x603D, 25);
        assert_eq!(
            data[..9],
            [0xBA, 0x15, 0x62, 0x11, 0xB8, 0x7F, 0x29, 0x06, 0x5B]
        );
        assert_eq!(
            data[9..18],
            [0xFF, 0xE7, 0x7E, 0x0E, 0x36, 0x56, 0x9E, 0x85, 0x60]
        );
        assert_eq!(data[18], 0xFF, "unmapped gap byte");
        assert_eq!(data[19..22], [0x19, 0x19, 0x70], "body color");
    }

    #[test]
    fn user_calibration_is_absent() {
        // Magics at 0x8010 (left stick), 0x801B (right), 0x8026 (IMU) must
        // read 0xFF so the host falls back to factory calibration.
        for addr in [0x8010, 0x801B, 0x8026] {
            assert_eq!(spi_read(addr, 2), vec![0xFF, 0xFF], "addr {addr:#06X}");
        }
    }

    #[test]
    fn imu_factory_calibration_is_served() {
        let data = spi_read(0x6020, 24);
        assert_eq!(data[..4], [0xBE, 0xFF, 0x3E, 0x00]);
        assert_eq!(data[18..], [0xE7, 0x3B, 0xE7, 0x3B, 0xE7, 0x3B]);
    }

    #[test]
    fn input_report_mode_starts_streaming() {
        let mut p = Protocol::new();
        let replies = handle(&mut p, &subcmd(0x03, &[0x30]));
        assert_eq!(replies[0][13], 0x80);
        assert_eq!(replies[0][14], 0x03);
        assert!(p.streaming());
    }

    #[test]
    fn rumble_only_report_has_no_reply() {
        let mut p = Protocol::new();
        let mut frame = vec![0x10, 0x00];
        frame.extend_from_slice(&[0; 8]);
        assert!(handle(&mut p, &out(&frame)).is_empty());
    }

    #[test]
    fn full_hid_nintendo_init_succeeds() {
        // Replay joycon_init() end to end; every request the driver waits on
        // must get a reply with the ack it matches against.
        let mut p = Protocol::new();
        let state = ControllerState::default();

        for (frame, ack_prefix) in [
            (out(&[0x80, 0x02]), vec![0x81, 0x02]),
            (out(&[0x80, 0x03]), vec![0x81, 0x03]),
            (out(&[0x80, 0x02]), vec![0x81, 0x02]),
            (out(&[0x80, 0x04]), vec![]),
            (subcmd(0x02, &[]), vec![0x21]), // device info
            (subcmd(0x10, &[0x10, 0x80, 0x00, 0x00, 2]), vec![0x21]), // user cal magic
            (subcmd(0x10, &[0x3D, 0x60, 0x00, 0x00, 9]), vec![0x21]), // factory left
            (subcmd(0x10, &[0x46, 0x60, 0x00, 0x00, 9]), vec![0x21]), // factory right
            (subcmd(0x10, &[0x26, 0x80, 0x00, 0x00, 2]), vec![0x21]), // IMU user magic
            (subcmd(0x10, &[0x20, 0x60, 0x00, 0x00, 24]), vec![0x21]), // IMU factory
            (subcmd(0x03, &[0x30]), vec![0x21]), // report mode
            (subcmd(0x48, &[0x01]), vec![0x21]), // vibration
            (subcmd(0x40, &[0x01]), vec![0x21]), // IMU enable
            (subcmd(0x30, &[0x01]), vec![0x21]), // player lights
        ] {
            let replies = p.handle_output_report(&frame, &state);
            if ack_prefix.is_empty() {
                assert!(replies.is_empty());
            } else {
                assert_eq!(replies.len(), 1);
                assert_eq!(replies[0][..ack_prefix.len()], ack_prefix[..]);
                // Every 0x21 reply must have the OK bit set in the ack byte.
                if replies[0][0] == 0x21 {
                    assert_ne!(replies[0][13] & 0x80, 0, "NACKed: {frame:02X?}");
                }
            }
        }
        assert!(p.streaming());
    }

    #[test]
    fn reply_carries_current_button_state() {
        let mut p = Protocol::new();
        let mut state = ControllerState::default();
        state.set_button(Button::A, true);
        let replies = p.handle_output_report(&subcmd(0x02, &[]), &state);
        // Button bytes are report bytes 3..6; A is bit 3 of the first.
        assert_ne!(replies[0][3] & 0x08, 0);
    }
}

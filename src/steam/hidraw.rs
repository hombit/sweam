//! Steam Controller over raw hidraw — the bridge's only input source.
//!
//! The kernel `hid-steam` evdev device carries buttons, pads and sticks but
//! never the IMU, so gyro passthrough (PLAN phase 6) has to read the
//! controller's own 64-byte packets, and haptics (phase 5) have to write
//! feature reports back. Decoding lives in [`super::packet`]; this module is
//! the Linux plumbing around it: find the right hidraw node, ask the
//! controller to send motion, and pump packets into the shared
//! [`ControllerState`] through the usual [`mapping::Mapping`].
//!
//! **This takes the device over.** Opening the hidraw node sets hid-steam's
//! `client_opened`, which makes the driver unregister its evdev device and
//! stop configuring the controller until we close. Buttons and IMU therefore
//! *must* both come from these packets — which is why the evdev source that
//! once read them was removed rather than kept as a fallback. On close,
//! hid-steam restores lizard mode and re-registers evdev. (See `Notes.md`,
//! "Steam Controller IMU over hidraw".)

use super::haptic::{HapticPulse, Side};
use super::{ControllerState, InputSource, mapping, packet};
use crate::switch::rumble::RumbleMailbox;
use anyhow::{Context, bail};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const STEAM_VENDOR_ID: u16 = 0x28DE;
const DONGLE_PRODUCT_ID: u16 = 0x1142;
const WIRED_PRODUCT_ID: u16 = 0x1102;

/// Feature report `0x87` = SET_SETTINGS_VALUES: payload is `len` followed by
/// `reg u16le` triplets.
const CMD_SET_SETTINGS: u8 = 0x87;

// Setting register ids (from hid-steam.c / SDL — numeric facts).
const REG_LPAD_MODE: u8 = 0x07;
const REG_RPAD_MODE: u8 = 0x08;
const REG_MOMENTUM_MAX_VELOCITY: u8 = 0x18;
const REG_GYRO_MODE: u8 = 0x30;
const REG_WIRELESS_PACKET_VERSION: u8 = 0x31;
const REG_SLEEP_INACTIVITY_TIMEOUT: u8 = 0x32;
const REG_ENABLE_FAST_SCAN: u8 = 0x2F;

/// Trackpad mode 7 = "none": no mouse/scroll emulation, just raw positions.
const TRACKPAD_NONE: u16 = 7;
/// Idle timeout the known-good packet uses, in seconds.
const SLEEP_TIMEOUT_SECS: u16 = 900;

/// Gyro-mode bitmask: `4` = quaternion, `8` = raw accel, `16` = raw gyro.
///
/// We ask for all three. Quat+gyro (`0x14`) is the combination sc-controller
/// proved over the wireless dongle; raw accel is the doubtful one — SDL
/// flags it as unverified there and hid-steam's field table marks
/// accelerometer values "not sent through wireless" — but the Switch expects
/// gravity in its reports and we cannot derive that from gyro alone, so it
/// is worth asking for. If the dongle drops it, accel simply stays zero.
const IMU_MODE_ALL: u16 = 0x04 | 0x08 | 0x10;

/// An empty dongle slot NAKs the settings report; hid-steam retries this
/// often (20 ms apart) before giving up on a controller that is merely busy.
/// We keep the retry short because `open()` runs in a once-a-second hotplug
/// loop and has three other slots to try.
const SETTINGS_RETRIES: u32 = 3;
const SETTINGS_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// Sentinel for [`HidrawSteamController::active_shared`]: no slot has
/// identified itself yet.
const NO_ACTIVE_SLOT: usize = usize::MAX;

/// How often the haptics worker re-arms the actuators.
///
/// The actuators play a *finite* burst and stop, so sustaining a rumble
/// means re-sending. 40 ms is a compromise between two costs: each re-arm is
/// a USB control transfer to the dongle, and everything between the host's
/// rumble changing and the next tick is latency the player feels. It is also
/// deliberately slower than the host's ~66 rumble updates a second — those
/// restate a level, so dropping most of them loses nothing.
const HAPTIC_TICK: Duration = Duration::from_millis(40);

/// How long each burst is asked to last.
///
/// Longer than the tick, so consecutive bursts overlap rather than leaving
/// audible gaps if a tick runs late. The overhang is the price: when rumble
/// stops, whatever is already playing finishes, so the pads keep buzzing for
/// up to `HAPTIC_BURST - HAPTIC_TICK` after silence.
const HAPTIC_BURST: Duration = Duration::from_millis(50);

/// How long a posted rumble frame stays worth playing.
///
/// The host restates rumble in every output report, so anything older than a
/// few ticks means the host stopped talking rather than asked for silence —
/// see [`RumbleMailbox::get_fresh`]. Generous enough to survive a hiccup in
/// the host's stream, short enough that nobody has to power-cycle a
/// controller that got stuck buzzing.
const RUMBLE_TTL: Duration = Duration::from_millis(250);

/// One dongle slot (or a wired controller): a node we hold open and read.
struct Slot {
    path: PathBuf,
    device: File,
}

/// Steam Controller read through its raw HID packets.
///
/// The dongle has four slots and the controller can be on any of them — and
/// it *moves* between them across reconnects (observed 2026-08-02: after a
/// drop it came back on slot 2 having been on slot 1). Asking a slot whether
/// it has a controller doesn't work either: the dongle acks the settings
/// report for empty slots. So we hold every candidate open and let the one
/// that actually delivers packets identify itself.
pub struct HidrawSteamController {
    slots: Vec<Slot>,
    /// Index into `slots` of the one that has produced input packets.
    active: Option<usize>,
    /// The same index, shared with the haptics worker so it can address the
    /// controller without reaching into `self` across threads.
    /// [`NO_ACTIVE_SLOT`] until a slot identifies itself.
    active_shared: Arc<AtomicUsize>,
    /// Stops the haptics worker when this source is dropped — which happens
    /// on every reconnect, since the bridge reopens the controller.
    haptics_stop: Option<Arc<AtomicBool>>,
    mapping: mapping::Mapping,
    /// Kept so a wireless reconnect can re-apply it: the controller forgets
    /// its settings (IMU mode included) when it drops off and comes back.
    imu_mode: u16,
    /// Last reported battery, so the periodic status packets only print when
    /// something changes.
    last_battery: Option<(u16, u8)>,
}

impl HidrawSteamController {
    /// Open the controller: just `path` if given, otherwise every hidraw
    /// node that belongs to a Steam Controller dongle slot (or a wired
    /// controller). Motion is enabled on all of them; which one carries the
    /// controller is decided later, by whichever sends packets.
    pub fn open(mapping: mapping::Mapping, path: Option<&str>) -> anyhow::Result<Self> {
        let imu_mode = IMU_MODE_ALL;
        let candidates = match path {
            Some(path) => vec![PathBuf::from(path)],
            None => {
                let found = candidate_nodes().context("Failed to scan /sys/class/hidraw")?;
                if found.is_empty() {
                    bail!(
                        "No Steam Controller hidraw node found — is the dongle plugged in \
                         and hid-steam loaded?"
                    );
                }
                found
            }
        };

        let mut slots = Vec::new();
        let mut last_error = None;
        for path in candidates {
            let device = match open_node(&path) {
                Ok(device) => device,
                // Permission problems are fatal for every node, not just
                // this one — report immediately so the hint reaches the user.
                Err(err) if super::is_permission_error(&err) => return Err(err),
                Err(err) => {
                    last_error = Some(err);
                    continue;
                }
            };
            // A slot that refuses the settings report is kept anyway: it may
            // still deliver packets, and it costs one idle file descriptor.
            if let Err(err) = enable_imu(&device, imu_mode) {
                last_error = Some(err);
            }
            slots.push(Slot { path, device });
        }
        if slots.is_empty() {
            let detail = last_error
                .map(|err| format!(": {err:#}"))
                .unwrap_or_default();
            bail!("No Steam Controller hidraw node could be opened{detail}");
        }
        println!(
            "Watching {} Steam Controller slot(s): {}; waiting for packets…",
            slots.len(),
            slots
                .iter()
                .map(|slot| slot.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(Self {
            slots,
            active: None,
            active_shared: Arc::new(AtomicUsize::new(NO_ACTIVE_SLOT)),
            haptics_stop: None,
            mapping,
            imu_mode,
            last_battery: None,
        })
    }

    /// Start playing whatever rumble the host posts to `mailbox` on the
    /// controller's actuators.
    ///
    /// The work happens on its own thread, and that is the whole point.
    /// Sending a burst is a USB control transfer to the dongle; doing it
    /// from the report pump would put an unpredictable stall between the
    /// host's poll and our next report, on the one axis this project has
    /// already lost three sessions to. Here the worst case is a late buzz.
    ///
    /// The thread holds its own duplicated descriptors, so it keeps working
    /// regardless of what the pump is doing, and stops when this source is
    /// dropped (i.e. on every reconnect, when the bridge reopens).
    pub fn start_haptics(&mut self, mailbox: Arc<RumbleMailbox>) -> anyhow::Result<()> {
        let mut devices = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            devices.push(
                slot.device
                    .try_clone()
                    .with_context(|| format!("Failed to duplicate {:?}", slot.path))?,
            );
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.haptics_stop = Some(stop.clone());
        let active = self.active_shared.clone();
        std::thread::spawn(move || {
            // Only re-arm when something is actually asked for. Silence is
            // not a command we can send — a burst already playing runs to
            // its end — so it simply means sending nothing.
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(HAPTIC_TICK);
                let Some(frame) = mailbox.get_fresh(RUMBLE_TTL) else {
                    continue;
                };
                if frame.is_silent() {
                    continue;
                }
                let target = active.load(Ordering::Relaxed);
                for (side, rumble) in [(Side::Left, frame.left), (Side::Right, frame.right)] {
                    let Some(pulse) = HapticPulse::from_band(side, rumble.dominant(), HAPTIC_BURST)
                    else {
                        continue;
                    };
                    let report = pulse.feature_report();
                    // Before a slot identifies itself there is nothing
                    // sensible to address, and blind-firing at four slots
                    // 25 times a second is not it — buzz only once we know.
                    if let Some(device) = devices.get(target) {
                        let _ = set_feature(device, &report);
                    }
                }
            }
        });
        Ok(())
    }

    /// Whether a slot has actually delivered packets, i.e. a controller is
    /// on and talking. Until then we do not know which slot to address.
    pub fn is_streaming(&self) -> bool {
        self.active.is_some()
    }

    /// Fire one haptic burst at the controller.
    ///
    /// Before any slot has identified itself the command goes to all of
    /// them: the dongle acks for empty slots regardless (see the struct
    /// docs), so there is no cheaper way to reach a controller whose slot we
    /// have not learned yet, and the ones that miss cost an ioctl each.
    pub fn send_haptic(&self, pulse: &super::haptic::HapticPulse) -> anyhow::Result<()> {
        let report = pulse.feature_report();
        let targets: Vec<usize> = match self.active {
            Some(index) => vec![index],
            None => (0..self.slots.len()).collect(),
        };
        let mut last_error = None;
        for index in targets {
            if let Err(err) = set_feature(&self.slots[index].device, &report) {
                last_error = Some((self.slots[index].path.clone(), err));
            }
        }
        match last_error {
            // A failure on one speculative slot is not a failure overall;
            // only report when we knew the target and it refused.
            Some((path, err)) if self.active.is_some() => {
                Err(anyhow::Error::new(err).context(format!("Haptic report rejected by {path:?}")))
            }
            _ => Ok(()),
        }
    }

    /// Read back whatever the controller left in the control pipe, for
    /// diagnosing commands whose only other feedback is a hand on the pads.
    /// `None` before a slot has identified itself.
    pub fn read_feature(&self) -> Option<std::io::Result<[u8; packet::PACKET_LEN + 1]>> {
        let index = self.active?;
        Some(get_feature(&self.slots[index].device))
    }

    /// Re-apply settings to one slot after a wireless reconnect; a failure
    /// here is worth reporting but not worth dropping the connection over,
    /// since the next connect event will try again.
    fn reenable(&self, index: usize) {
        let slot = &self.slots[index];
        if let Err(err) = enable_imu(&slot.device, self.imu_mode) {
            eprintln!(
                "Steam Controller reconnected but re-enabling motion failed: {err:#} ({})",
                slot.path.display()
            );
        }
    }
}

impl Drop for HidrawSteamController {
    fn drop(&mut self) {
        // The worker holds duplicated descriptors, so it would happily keep
        // buzzing a controller this source no longer owns.
        if let Some(stop) = &self.haptics_stop {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

impl InputSource for HidrawSteamController {
    fn poll(&mut self, state: &mut ControllerState) -> anyhow::Result<()> {
        for index in 0..self.slots.len() {
            self.poll_slot(index, state)?;
        }
        // One time step per poll (~8 ms pump cadence): decays camera-mode
        // deflection, a no-op in other modes.
        self.mapping.tick(state);
        Ok(())
    }
}

impl HidrawSteamController {
    /// Drain one slot's pending packets.
    fn poll_slot(&mut self, index: usize, state: &mut ControllerState) -> anyhow::Result<()> {
        let mut buf = [0u8; packet::PACKET_LEN];
        loop {
            match self.slots[index].device.read(&mut buf) {
                // A short read is not a packet we can trust; skip it.
                Ok(n) if n < packet::PACKET_LEN => continue,
                Ok(_) => match packet::parse(&buf) {
                    Some(packet::Packet::Input(input)) => {
                        // The settings report is acked by the dongle even for
                        // a slot with no controller on it, so "opened" proves
                        // nothing — this is the first evidence of a live
                        // controller, and worth saying out loud.
                        if self.active != Some(index) {
                            self.active = Some(index);
                            self.active_shared.store(index, Ordering::Relaxed);
                            println!(
                                "Steam Controller streaming on {}",
                                self.slots[index].path.display()
                            );
                        }
                        for event in input.events() {
                            match event {
                                packet::Event::Key(code, pressed) => {
                                    self.mapping.apply_key(state, code, pressed)
                                }
                                packet::Event::Abs(code, value) => {
                                    self.mapping.apply_abs(state, code, value)
                                }
                            }
                        }
                        push_imu(state, self.mapping.remap_imu(input.imu_sample()));
                    }
                    Some(packet::Packet::Connect(event)) => match event {
                        // The slot keeps its node when the controller sleeps
                        // or wanders off, so nothing else would tell us to
                        // stop holding the last inputs down.
                        packet::ConnectEvent::Disconnected => {
                            println!(
                                "Steam Controller disconnected from {}; inputs back to neutral",
                                self.slots[index].path.display()
                            );
                            // Forget the active slot: after a reconnect the
                            // controller may come back on a different one.
                            if self.active == Some(index) {
                                self.active = None;
                            }
                            *state = ControllerState::default();
                        }
                        packet::ConnectEvent::Connected | packet::ConnectEvent::Paired => {
                            println!(
                                "Steam Controller connected on {}; re-enabling motion",
                                self.slots[index].path.display()
                            );
                            self.reenable(index);
                        }
                        packet::ConnectEvent::Unknown(code) => {
                            eprintln!("Unknown Steam Controller connect event 0x{code:02x}");
                        }
                    },
                    // An idle wireless slot sends these instead of input
                    // packets, so they double as the answer to "is the
                    // controller alive but bored, or gone?".
                    Some(packet::Packet::Battery {
                        millivolts,
                        percent,
                    }) if self.last_battery != Some((millivolts, percent)) => {
                        self.last_battery = Some((millivolts, percent));
                        println!("Steam Controller battery: {percent}% ({millivolts} mV)");
                    }
                    _ => {}
                },
                // Nothing pending: the normal case between packets.
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("Failed to read from {}", self.slots[index].path.display())
                    });
                }
            }
        }
        Ok(())
    }
}

/// Shift a new sample into the report's 3-frame ring, oldest first.
fn push_imu(state: &mut ControllerState, sample: crate::state::ImuSample) {
    state.imu[0] = state.imu[1];
    state.imu[1] = state.imu[2];
    state.imu[2] = sample;
}

/// Open a hidraw node read-write (feature reports need write) and
/// non-blocking (the report pump must never stall on it).
fn open_node(path: &PathBuf) -> anyhow::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("Failed to open {}", path.display()))
}

/// Ask the controller to stream motion, retrying the NAK an idle or busy
/// slot answers with.
fn enable_imu(device: &File, imu_mode: u16) -> anyhow::Result<()> {
    let report = settings_report(imu_mode);
    let mut attempt = 0;
    loop {
        match set_feature(device, &report) {
            Ok(()) => break,
            Err(err) if attempt < SETTINGS_RETRIES => {
                attempt += 1;
                let _ = err;
                std::thread::sleep(SETTINGS_RETRY_DELAY);
            }
            Err(err) => return Err(err).context("Settings report rejected"),
        }
    }
    // A reply left sitting in the control pipe would otherwise surface as a
    // stale read later; drain it and ignore what it says.
    let _ = get_feature(device);
    Ok(())
}

/// The SET_SETTINGS_VALUES report: trackpads to raw mode, wireless packet
/// version 2, and the requested IMU mode. Register set and order follow the
/// known-good packet in `Notes.md`.
fn settings_report(imu_mode: u16) -> [u8; packet::PACKET_LEN] {
    let settings = [
        (REG_SLEEP_INACTIVITY_TIMEOUT, SLEEP_TIMEOUT_SECS),
        (REG_MOMENTUM_MAX_VELOCITY, 0),
        (REG_WIRELESS_PACKET_VERSION, 2),
        (REG_RPAD_MODE, TRACKPAD_NONE),
        (REG_LPAD_MODE, TRACKPAD_NONE),
        (REG_GYRO_MODE, imu_mode),
        (REG_ENABLE_FAST_SCAN, 1),
    ];
    let mut report = [0u8; packet::PACKET_LEN];
    report[0] = CMD_SET_SETTINGS;
    report[1] = (settings.len() * 3) as u8;
    for (index, (register, value)) in settings.iter().enumerate() {
        let offset = 2 + index * 3;
        report[offset] = *register;
        report[offset + 1..offset + 3].copy_from_slice(&value.to_le_bytes());
    }
    report
}

// hidraw ioctls, asm-generic encoding: dir(2) | size(14) | type(8) | nr(8).
// Both feature ioctls are read+write (dir = 3) on type 'H'.
const IOC_READ_WRITE: u32 = 3;
const HID_IOC_TYPE: u32 = b'H' as u32;
const HIDIOCSFEATURE_NR: u32 = 0x06;
const HIDIOCGFEATURE_NR: u32 = 0x07;

fn feature_ioctl(nr: u32, len: usize) -> libc::c_ulong {
    ((IOC_READ_WRITE << 30) | ((len as u32) << 16) | (HID_IOC_TYPE << 8) | nr) as libc::c_ulong
}

/// HIDIOCSFEATURE with the leading report-id byte these unnumbered reports
/// need (`0x00`), i.e. 65 bytes on the wire for a 64-byte report.
fn set_feature(device: &File, report: &[u8; packet::PACKET_LEN]) -> std::io::Result<()> {
    let mut buf = [0u8; packet::PACKET_LEN + 1];
    buf[1..].copy_from_slice(report);
    let result = unsafe {
        libc::ioctl(
            device.as_raw_fd(),
            feature_ioctl(HIDIOCSFEATURE_NR, buf.len()),
            buf.as_ptr(),
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// HIDIOCGFEATURE, used only to drain a pending reply.
fn get_feature(device: &File) -> std::io::Result<[u8; packet::PACKET_LEN + 1]> {
    let mut buf = [0u8; packet::PACKET_LEN + 1];
    let result = unsafe {
        libc::ioctl(
            device.as_raw_fd(),
            feature_ioctl(HIDIOCGFEATURE_NR, buf.len()),
            buf.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(buf)
}

/// Hidraw nodes belonging to a Steam Controller *gamepad* interface.
///
/// The dongle exposes one USB interface per slot: 0 is keyboard emulation,
/// 1-4 are the four controller slots. A wired controller puts its gamepad on
/// interface 2. Anything else (the keyboard node especially) would enumerate
/// but never produce input packets.
fn candidate_nodes() -> anyhow::Result<Vec<PathBuf>> {
    let mut nodes = Vec::new();
    let entries = match std::fs::read_dir("/sys/class/hidraw") {
        Ok(entries) => entries,
        // No hidraw class at all: no nodes, not an error worth failing on.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(nodes),
        Err(err) => return Err(err.into()),
    };
    for entry in entries.flatten() {
        let sysfs = entry.path();
        let Some(name) = sysfs.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((vendor, product)) = hid_ids(&sysfs) else {
            continue;
        };
        if vendor != STEAM_VENDOR_ID {
            continue;
        }
        let interface = interface_number(&sysfs);
        let is_gamepad = matches!(
            (product, interface),
            (DONGLE_PRODUCT_ID, Some(1..=4)) | (WIRED_PRODUCT_ID, Some(2))
        );
        if is_gamepad {
            nodes.push(PathBuf::from("/dev").join(name));
        }
    }
    // Slot order is stable and meaningful (slot 1 first), node order is not.
    nodes.sort();
    Ok(nodes)
}

/// `HID_ID=0003:000028DE:00001142` out of the node's uevent file.
fn hid_ids(sysfs: &std::path::Path) -> Option<(u16, u16)> {
    let uevent = std::fs::read_to_string(sysfs.join("device/uevent")).ok()?;
    let line = uevent
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?;
    let mut fields = line.split(':').skip(1);
    let vendor = u16::from_str_radix(fields.next()?.trim(), 16).ok()?;
    let product = u16::from_str_radix(fields.next()?.trim(), 16).ok()?;
    Some((vendor, product))
}

/// The USB interface number the HID device hangs off (sysfs prints it hex).
fn interface_number(sysfs: &std::path::Path) -> Option<u8> {
    let text = std::fs::read_to_string(sysfs.join("device/../bInterfaceNumber")).ok()?;
    u8::from_str_radix(text.trim(), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_report_matches_the_known_good_packet() {
        let report = settings_report(IMU_MODE_ALL);
        // 87 15 | 32 84 03 | 18 00 00 | 31 02 00 | 08 07 00 | 07 07 00 |
        // 30 1C 00 | 2F 01 00, zero-padded to 64 — the known-good packet
        // from Notes.md with the IMU mode set to quat+accel+gyro.
        #[rustfmt::skip]
        let expected: [u8; 23] = [
            0x87, 0x15,
            0x32, 0x84, 0x03,
            0x18, 0x00, 0x00,
            0x31, 0x02, 0x00,
            0x08, 0x07, 0x00,
            0x07, 0x07, 0x00,
            0x30, 0x1C, 0x00,
            0x2F, 0x01, 0x00,
        ];
        assert_eq!(&report[..expected.len()], &expected);
        assert!(report[expected.len()..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn feature_ioctl_encoding() {
        // HIDIOCSFEATURE(65) as the kernel's _IOC macro computes it.
        assert_eq!(feature_ioctl(HIDIOCSFEATURE_NR, 65), 0xC0414806);
        assert_eq!(feature_ioctl(HIDIOCGFEATURE_NR, 65), 0xC0414807);
    }

    #[test]
    fn imu_ring_keeps_the_newest_sample_last() {
        let mut state = ControllerState::default();
        for gyro_x in 1..=4i16 {
            push_imu(
                &mut state,
                crate::state::ImuSample {
                    gyro: [gyro_x, 0, 0],
                    ..Default::default()
                },
            );
        }
        assert_eq!(
            state.imu.map(|sample| sample.gyro[0]),
            [2, 3, 4],
            "oldest first, newest last"
        );
    }
}

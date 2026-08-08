//! Steam Controller side: read inputs into [`ControllerState`].
//!
//! One source: [`HidrawSteamController`], the controller's own HID packets,
//! decoded by [`packet`] into the evdev vocabulary [`mapping::Mapping`]
//! speaks. Raw is the only path that carries the IMU, and the only one that
//! can reach the haptics, so the bridge always takes the device over —
//! `hid-steam` withdraws its evdev device while we hold the node open, which
//! means buttons, pads and sticks must come from the same packets.
//!
//! There was a second source until 2026-08-08, reading `hid-steam`'s evdev
//! device: fewer lines, no root, no motion. It went because every shipped
//! config asks for motion, so it was already unreachable in practice, and
//! because nothing in `cargo test` could exercise it — evdev needs a real
//! device node, so the code could only ever be verified by plugging in
//! hardware. `git log` has it if a driver-managed fallback is ever wanted.
//!
//! Dongle USB IDs: 28de:1142 (wired controller: 28de:1102).

pub mod config;
pub mod mapping;
pub mod packet;

use crate::state::ControllerState;

/// Anything that can feed the bridge with controller input.
pub trait InputSource {
    /// Pump pending input events into `state`. Non-blocking.
    fn poll(&mut self, state: &mut ControllerState) -> anyhow::Result<()>;
}

#[cfg(target_os = "linux")]
pub mod hidraw;

#[cfg(target_os = "linux")]
pub use hidraw::HidrawSteamController;

/// Open the controller. `hidraw` pins a specific `/dev/hidrawN`; without it
/// every dongle slot is opened and the one that delivers packets wins.
#[cfg(target_os = "linux")]
pub fn open_source(
    mapping: mapping::Mapping,
    hidraw: Option<&str>,
) -> anyhow::Result<Box<dyn InputSource>> {
    Ok(Box::new(HidrawSteamController::open(mapping, hidraw)?))
}

/// Whether an error from [`HidrawSteamController::open`] is a permission
/// problem (its chain carries an `io::Error` of kind `PermissionDenied`).
/// Waiting/retrying can't fix those — callers should exit with the hint.
#[cfg(target_os = "linux")]
pub fn is_permission_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

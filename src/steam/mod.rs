//! Steam Controller side: read inputs into [`ControllerState`].
//!
//! Uses the kernel `hid-steam` driver's evdev interface (needs the
//! `steam-devices` udev rules; the driver also disables "lizard mode" for
//! us). Raw hidraw access comes later for haptics (phase 5) and gyro
//! (phase 6). Dongle USB IDs: 28de:1142 (wired controller: 28de:1102).

pub mod config;
pub mod mapping;

use crate::state::ControllerState;

/// Anything that can feed the bridge with controller input.
pub trait InputSource {
    /// Pump pending input events into `state`. Non-blocking.
    fn poll(&mut self, state: &mut ControllerState) -> anyhow::Result<()>;
}

#[cfg(target_os = "linux")]
pub use evdev_input::EvdevSteamController;

/// Whether an error from [`EvdevSteamController::open`] is a permission
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

#[cfg(target_os = "linux")]
mod evdev_input {
    use super::{ControllerState, InputSource, mapping};
    use anyhow::{Context, bail};

    const STEAM_VENDOR_ID: u16 = 0x28DE;

    /// Steam Controller via the kernel `hid-steam` evdev device.
    pub struct EvdevSteamController {
        device: evdev::Device,
        mapping: mapping::Mapping,
    }

    impl EvdevSteamController {
        /// Open the Steam Controller: `path` if given, otherwise find it
        /// among `/dev/input/event*`.
        ///
        /// hid-steam names its gamepad node "Steam Controller" ("Wireless
        /// Steam Controller" via the dongle), but the "lizard mode"
        /// keyboard node of the same device can also end up named
        /// "… Steam Controller" — so additionally require ABS axes, which
        /// only the gamepad node has. The gamepad node exists only while
        /// the controller is on and connected.
        pub fn open(mapping: mapping::Mapping, path: Option<&str>) -> anyhow::Result<Self> {
            if let Some(path) = path {
                let device =
                    evdev::Device::open(path).with_context(|| format!("Failed to open {path}"))?;
                device
                    .set_nonblocking(true)
                    .with_context(|| format!("Failed to set {path} non-blocking"))?;
                println!("Steam Controller (per --evdev) at {path}");
                return Ok(Self { device, mapping });
            }
            for (path, device) in evdev::enumerate() {
                let matches = device.input_id().vendor() == STEAM_VENDOR_ID
                    && device
                        .name()
                        .is_some_and(|name| name.ends_with("Steam Controller"))
                    && device
                        .supported_absolute_axes()
                        .is_some_and(|axes| axes.contains(evdev::AbsoluteAxisCode::ABS_X));
                if matches {
                    device
                        .set_nonblocking(true)
                        .with_context(|| format!("Failed to set {path:?} non-blocking"))?;
                    println!("Steam Controller at {}", path.display());
                    return Ok(Self { device, mapping });
                }
            }
            // enumerate() silently skips nodes it can't open, so without
            // root it comes up empty even with the controller present —
            // point at permissions instead of the hardware in that case.
            // Keep the io::Error as the source so callers can tell this
            // unrecoverable case apart from "controller not connected yet".
            if any_event_node_permission_denied() {
                return Err(anyhow::Error::new(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                ))
                .context(
                    "Permission denied opening /dev/input/event* devices — \
                     run sweam as root (sudo) or join the `input` group",
                ));
            }
            bail!(
                "No Steam Controller input device found — is the dongle plugged in, \
                 the controller on, and hid-steam active (steam-devices udev rules)?"
            )
            // TODO(phase 4, hardware): support hotplug — retry open() while
            // running instead of requiring the controller at startup.
        }
    }

    /// Whether any `/dev/input/event*` node exists that we can't open —
    /// the signature of running unprivileged (nodes are root:input).
    fn any_event_node_permission_denied() -> bool {
        let Ok(entries) = std::fs::read_dir("/dev/input") else {
            return false;
        };
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("event"))
                && matches!(
                    std::fs::File::open(entry.path()),
                    Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied
                )
        })
    }

    impl InputSource for EvdevSteamController {
        fn poll(&mut self, state: &mut ControllerState) -> anyhow::Result<()> {
            match self.device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        let event_type = event.event_type();
                        if event_type == evdev::EventType::KEY {
                            self.mapping
                                .apply_key(state, event.code(), event.value() != 0);
                        } else if event_type == evdev::EventType::ABSOLUTE {
                            self.mapping.apply_abs(state, event.code(), event.value());
                        }
                    }
                }
                // No pending events is the common case — still tick below,
                // time-based behavior must advance while the input is idle.
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(err).context("Failed to fetch evdev events"),
            }
            // One time step per poll (~8 ms pump cadence): decays the
            // camera-mode deflection; a no-op in other modes.
            self.mapping.tick(state);
            Ok(())
        }
    }
}

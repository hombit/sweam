//! configfs USB HID gadget that presents us to the Switch as a Pro Controller.
//!
//! Descriptor values mirror a real Pro Controller and mzyy94's known-good
//! gadget script (<https://gist.github.com/mzyy94/60ae253a45e2759451789a117c59acf9>).
//!
//! Requires root, the `libcomposite`/`usb_f_hid` kernel modules, and a UDC in
//! peripheral mode (on the Radxa Zero 3E: `fcc00000.usb`, enabled via the
//! rsetup OTG-peripheral overlay — see PLAN.md phase 0).

use anyhow::{bail, Context};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const NINTENDO_VID: u16 = 0x057E;
pub const PRO_CONTROLLER_PID: u16 = 0x2009;

const GADGET_NAME: &str = "sweam_procon";
const DEFAULT_CONFIGFS_ROOT: &str = "/sys/kernel/config/usb_gadget";

/// Environment-specific knobs, exposed on the command line so sweam is not
/// tied to one SBC or distro (defaults match the Radxa Zero 3E / Debian).
#[derive(Debug, Default)]
pub struct GadgetOptions {
    /// UDC name to bind; `None` = autodetect from `/sys/class/udc`.
    pub udc: Option<String>,
    /// configfs `usb_gadget` directory, if mounted somewhere unusual.
    pub configfs_root: Option<PathBuf>,
    /// Skip `modprobe libcomposite usb_f_hid` (modules builtin or preloaded).
    pub skip_modprobe: bool,
}

/// A configured (and, after [`UsbGadget::bind`], UDC-bound) USB gadget.
/// Unbinds and removes itself on drop.
#[derive(Debug)]
pub struct UsbGadget {
    gadget_path: PathBuf,
    udc: String,
}

impl UsbGadget {
    /// Create the gadget in configfs and bind it to the UDC.
    pub fn new(options: GadgetOptions) -> anyhow::Result<Self> {
        let configfs_root = options.configfs_root.unwrap_or_else(detect_configfs_root);
        if !options.skip_modprobe {
            for module in ["libcomposite", "usb_f_hid"] {
                if let Err(err) = modprobe(module) {
                    // Builtin modules and distros without modprobe in PATH
                    // land here; if the gadget API is usable anyway, go on.
                    if configfs_root.exists() {
                        eprintln!(
                            "Warning: modprobe {module} failed ({err:#}); continuing \
                             since {configfs_root:?} exists (builtin? try --skip-modprobe)"
                        );
                    } else {
                        return Err(err).with_context(|| {
                            format!(
                                "Failed to load the {module} kernel module and \
                                 {configfs_root:?} does not exist"
                            )
                        });
                    }
                }
            }
        }
        if !configfs_root.exists() {
            bail!(
                "{configfs_root:?} does not exist — is configfs mounted and the \
                 USB gadget API enabled in this kernel? (--configfs PATH to override)"
            );
        }

        let gadget_path = configfs_root.join(GADGET_NAME);
        let mut slf = Self {
            gadget_path,
            udc: String::new(),
        };
        // A killed sweam (no chance to run Drop) leaves its gadget bound in
        // configfs, which would make every write below fail — remove it.
        if slf.gadget_path.exists() {
            eprintln!("Removing stale gadget from a previous run…");
            slf.teardown()
                .context("Failed to remove a stale gadget (reboot to clear it)")?;
        }
        slf.setup().context("Failed to set up configfs gadget")?;

        let udc = match options.udc {
            Some(udc) => udc,
            None => autodetect_udc()?,
        };
        println!("Binding gadget to UDC {udc}");
        write(slf.gadget_path.join("UDC"), &udc)
            .with_context(|| format!("Failed to bind gadget to UDC {udc:?}"))?;
        slf.udc = udc;

        Ok(slf)
    }

    /// Sysfs file holding the USB device state as seen by our UDC:
    /// "not attached", "default", "addressed", "configured", "suspended".
    pub fn udc_state_path(&self) -> PathBuf {
        Path::new("/sys/class/udc").join(&self.udc).join("state")
    }

    /// The HID device node to exchange reports through, e.g. `/dev/hidg0`.
    pub fn hidg_device(&self) -> anyhow::Result<PathBuf> {
        let dev = fs::read_to_string(self.function_path().join("dev"))
            .context("Failed to read hid function dev number")?;
        let minor = dev
            .trim()
            .split(':')
            .nth(1)
            .context("Malformed hid function dev number")?;
        Ok(PathBuf::from(format!("/dev/hidg{minor}")))
    }

    fn function_path(&self) -> PathBuf {
        self.gadget_path.join("functions/hid.usb0")
    }

    fn setup(&self) -> anyhow::Result<()> {
        let g = &self.gadget_path;
        fs::create_dir_all(g).with_context(|| format!("Failed to create {g:?}"))?;

        write(g.join("idVendor"), format!("{NINTENDO_VID:#06x}"))?;
        write(g.join("idProduct"), format!("{PRO_CONTROLLER_PID:#06x}"))?;
        write(g.join("bcdDevice"), "0x0200")?;
        write(g.join("bcdUSB"), "0x0200")?;
        write(g.join("bDeviceClass"), "0x00")?;
        write(g.join("bDeviceSubClass"), "0x00")?;
        write(g.join("bDeviceProtocol"), "0x00")?;

        let strings = g.join("strings/0x409");
        fs::create_dir_all(&strings)?;
        write(strings.join("serialnumber"), "000000000001")?;
        write(strings.join("manufacturer"), "Nintendo Co., Ltd.")?;
        write(strings.join("product"), "Pro Controller")?;

        let function = self.function_path();
        fs::create_dir_all(&function)?;
        write(function.join("protocol"), "0")?;
        write(function.join("subclass"), "0")?;
        write(
            function.join("report_length"),
            super::report::REPORT_LENGTH.to_string(),
        )?;
        fs::write(
            function.join("report_desc"),
            super::report::HID_REPORT_DESCRIPTOR,
        )
        .context("Failed to write HID report descriptor")?;

        let config = g.join("configs/c.1");
        fs::create_dir_all(config.join("strings/0x409"))?;
        write(
            config.join("strings/0x409/configuration"),
            "Nintendo Switch Pro Controller",
        )?;
        write(config.join("MaxPower"), "500")?;
        write(config.join("bmAttributes"), "0xa0")?;

        let link = config.join("hid.usb0");
        if !link.exists() {
            std::os::unix::fs::symlink(&function, &link)
                .with_context(|| format!("Failed to symlink {function:?} -> {link:?}"))?;
        }

        Ok(())
    }

    fn teardown(&self) -> anyhow::Result<()> {
        let g = &self.gadget_path;
        if !g.exists() {
            return Ok(());
        }
        // Unbind from the UDC first, then unwind the configfs tree in
        // reverse creation order (configfs only removes empty directories).
        // A zero-byte write would never reach the kernel's store callback,
        // so write a newline; skip when already unbound (the write errors).
        let udc_file = g.join("UDC");
        let bound = fs::read_to_string(&udc_file).is_ok_and(|udc| !udc.trim().is_empty());
        if bound {
            write(udc_file, "\n")?;
        }
        for path in [g.join("configs/c.1/hid.usb0")] {
            if path.exists() {
                fs::remove_file(&path).with_context(|| format!("Failed to remove {path:?}"))?;
            }
        }
        for dir in [
            g.join("configs/c.1/strings/0x409"),
            g.join("configs/c.1"),
            g.join("functions/hid.usb0"),
            g.join("strings/0x409"),
            g.to_owned(),
        ] {
            if dir.exists() {
                fs::remove_dir(&dir).with_context(|| format!("Failed to remove {dir:?}"))?;
            }
        }
        Ok(())
    }
}

impl Drop for UsbGadget {
    fn drop(&mut self) {
        if let Err(err) = self.teardown() {
            eprintln!("Failed to tear down USB gadget: {err:?}");
        }
    }
}

fn write(path: PathBuf, contents: impl AsRef<[u8]>) -> anyhow::Result<()> {
    fs::write(&path, contents).with_context(|| format!("Failed to write {path:?}"))
}

/// The UDC to bind when the user didn't pick one: unambiguous when there is
/// exactly one (the usual SBC case); with several, take the first and warn.
fn autodetect_udc() -> anyhow::Result<String> {
    let mut names: Vec<String> = fs::read_dir("/sys/class/udc")
        .context("Failed to list /sys/class/udc — is the OTG port in peripheral mode?")?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    if names.is_empty() {
        bail!("No UDC found in /sys/class/udc — is the OTG port in peripheral mode?");
    }
    if names.len() > 1 {
        eprintln!(
            "Warning: several UDCs found ({}); using the first — override with --udc NAME",
            names.join(", ")
        );
    }
    Ok(names.remove(0))
}

/// Where configfs is mounted (usually `/sys/kernel/config`), per
/// `/proc/mounts`; the `usb_gadget` directory lives inside it.
fn detect_configfs_root() -> PathBuf {
    let detected = fs::read_to_string("/proc/mounts")
        .ok()
        .and_then(|mounts| configfs_mountpoint(&mounts));
    match detected {
        Some(mountpoint) => mountpoint.join("usb_gadget"),
        None => PathBuf::from(DEFAULT_CONFIGFS_ROOT),
    }
}

fn configfs_mountpoint(mounts: &str) -> Option<PathBuf> {
    mounts.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let _device = fields.next()?;
        let mountpoint = fields.next()?;
        let fstype = fields.next()?;
        (fstype == "configfs").then(|| PathBuf::from(mountpoint))
    })
}

/// Load a kernel module by shelling out to `modprobe`. Not using libkmod
/// bindings keeps cross-compilation dependency-free. Tries the sbin paths
/// too, since root shells on some distros don't have them in PATH.
fn modprobe(module: &str) -> anyhow::Result<()> {
    let mut not_found = None;
    for command in ["modprobe", "/sbin/modprobe", "/usr/sbin/modprobe"] {
        match Command::new(command).arg(module).status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => bail!("{command} {module} exited with {status}"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                not_found = Some(err);
            }
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to run {command} {module}"))
            }
        }
    }
    Err(not_found.expect("loop ran")).context("modprobe not found in PATH or /sbin, /usr/sbin")
}

#[cfg(test)]
mod tests {
    use super::configfs_mountpoint;
    use std::path::Path;

    #[test]
    fn configfs_found_in_proc_mounts() {
        let mounts = "sysfs /sys sysfs rw 0 0\n\
                      configfs /sys/kernel/config configfs rw,relatime 0 0\n\
                      tmpfs /tmp tmpfs rw 0 0\n";
        assert_eq!(
            configfs_mountpoint(mounts).as_deref(),
            Some(Path::new("/sys/kernel/config"))
        );
        assert_eq!(configfs_mountpoint("tmpfs /tmp tmpfs rw 0 0\n"), None);
        assert_eq!(configfs_mountpoint(""), None);
    }
}

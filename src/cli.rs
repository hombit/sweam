//! Command-line parsing and help text. Hand-rolled to keep the binary
//! dependency-light; pure, so its tests run on any platform.

pub const HELP: &str = concat!(
    "sweam ",
    env!("CARGO_PKG_VERSION"),
    " — Steam Controller → Switch Pro Controller USB bridge

Usage:
  sweam steam [options]           bridge a real Steam Controller (needs root)
  sweam manual [options]          bridge with inputs typed on stdin (testing)
  sweam steamcheck [options]      print parsed Steam Controller inputs
  sweam hostcheck [DEVICE]        verify a sweam gadget from the USB host side
                                  (DEVICE = its hidraw node; default: detected
                                  by USB IDs among /dev/hidraw*)
  sweam install [--config FILE]   install this binary (and the config) to
                                  /opt/sweam and enable a systemd service
                                  running \"sweam steam\" at boot (needs root)
  sweam uninstall                 stop the service and remove /opt/sweam
  sweam help | version            this help / the version

Options for install/uninstall:
  --prefix DIR      install directory; default /opt/sweam (the systemd unit
                    always goes to /etc/systemd/system/sweam.service)

Everything is auto-detected where possible; every detected value can be
overridden with a flag.

Options for the gadget side (steam, manual):
  --udc NAME        USB device controller to bind; default: detected from
                    /sys/class/udc (first one, with a warning if several)
  --configfs PATH   configfs gadget root; default: detected from /proc/mounts
                    (usually /sys/kernel/config/usb_gadget)
  --skip-modprobe   don't load the libcomposite/usb_f_hid kernel modules
                    first — use when they are built into your kernel or
                    already loaded

Controller input (steam, steamcheck):
  --config FILE     Steam-style VDF controller mapping; see configs/ for
                    commented examples. Default: built-in positional layout.
  --evdev PATH      the controller's /dev/input/eventN; default: detected by
                    vendor ID, name, and capabilities

All flags accept both \"--flag value\" and \"--flag=value\".

Manual mode commands (typed on stdin):
  press <button…> | release <button…> | stick <l|r> <x> <y> | neutral
  buttons: a b x y up down left right l r zl zr plus minus home capture
           lstick rstick;  stick x/y in -1..1

Docs: README.md (usage), configs/ (mappings), PLAN.md (roadmap)."
);

/// What `main` should do, fully validated.
#[derive(Debug, PartialEq)]
pub enum Command {
    Steam(GadgetArgs),
    Manual(GadgetArgs),
    Steamcheck {
        config: Option<String>,
        evdev: Option<String>,
    },
    Hostcheck {
        device: Option<String>,
    },
    Install {
        config: Option<String>,
        prefix: Option<String>,
    },
    Uninstall {
        prefix: Option<String>,
    },
    Help,
    Version,
}

/// Gadget-side options shared by `steam` and `manual`.
#[derive(Debug, PartialEq, Default)]
pub struct GadgetArgs {
    /// Mapping config (`steam` only; `manual` doesn't read a controller).
    pub config: Option<String>,
    /// Controller evdev node (`steam` only), overriding auto-detection.
    pub evdev: Option<String>,
    pub udc: Option<String>,
    pub configfs: Option<String>,
    pub skip_modprobe: bool,
}

/// Parse the arguments after the program name. `Err` is a user-facing
/// message; the caller appends a "try sweam help" hint.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let command = match args.next() {
        None => return Ok(Command::Help),
        Some(mode) => mode,
    };

    let mut config = None;
    let mut evdev = None;
    let mut udc = None;
    let mut configfs = None;
    let mut skip_modprobe = false;
    let mut prefix = None;
    let mut positional = None;
    while let Some(arg) = args.next() {
        // Split "--flag=value" into flag and inline value.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg, None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            match inline.clone().or_else(|| args.next()) {
                Some(value) => Ok(value),
                None => Err(format!("{name} needs a value")),
            }
        };
        match flag.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            "--config" => set_once(&mut config, value("--config")?, "--config")?,
            "--evdev" => set_once(&mut evdev, value("--evdev")?, "--evdev")?,
            "--udc" => set_once(&mut udc, value("--udc")?, "--udc")?,
            "--configfs" => set_once(&mut configfs, value("--configfs")?, "--configfs")?,
            "--skip-modprobe" => skip_modprobe = true,
            "--prefix" => set_once(&mut prefix, value("--prefix")?, "--prefix")?,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other:?}"));
            }
            _ => set_once(&mut positional, flag, "positional argument")?,
        }
    }

    let no_gadget_flags = |mode: &str| -> Result<(), String> {
        for (name, given) in [
            ("--udc", udc.is_some()),
            ("--configfs", configfs.is_some()),
            ("--skip-modprobe", skip_modprobe),
        ] {
            if given {
                return Err(format!("{name} does not apply to {mode:?}"));
            }
        }
        Ok(())
    };

    let no_prefix = |mode: &str| -> Result<(), String> {
        if prefix.is_some() {
            return Err(format!("--prefix does not apply to {mode:?}"));
        }
        Ok(())
    };

    match command.as_str() {
        "steam" | "manual" => {
            no_prefix(&command)?;
            if let Some(extra) = positional {
                return Err(format!(
                    "unexpected argument {extra:?} (UDC is now passed as --udc NAME)"
                ));
            }
            if command == "manual" {
                for (name, given) in [("--config", config.is_some()), ("--evdev", evdev.is_some())]
                {
                    if given {
                        return Err(format!(
                            "{name} does not apply to \"manual\" (it reads no controller; \
                             manual mode already speaks Switch buttons)"
                        ));
                    }
                }
            }
            let args = GadgetArgs {
                config,
                evdev,
                udc,
                configfs,
                skip_modprobe,
            };
            Ok(if command == "steam" {
                Command::Steam(args)
            } else {
                Command::Manual(args)
            })
        }
        "steamcheck" => {
            no_gadget_flags("steamcheck")?;
            no_prefix("steamcheck")?;
            if let Some(extra) = positional {
                return Err(format!("unexpected argument {extra:?}"));
            }
            Ok(Command::Steamcheck { config, evdev })
        }
        "hostcheck" => {
            no_gadget_flags("hostcheck")?;
            no_prefix("hostcheck")?;
            for (name, given) in [("--config", config.is_some()), ("--evdev", evdev.is_some())] {
                if given {
                    return Err(format!("{name} does not apply to \"hostcheck\""));
                }
            }
            Ok(Command::Hostcheck { device: positional })
        }
        "install" | "uninstall" => {
            no_gadget_flags(&command)?;
            if let Some(extra) = positional {
                return Err(format!("unexpected argument {extra:?}"));
            }
            for (name, given) in [
                ("--evdev", evdev.is_some()),
                // The service reads the config installed at install time.
                ("--config", command == "uninstall" && config.is_some()),
            ] {
                if given {
                    return Err(format!("{name} does not apply to {command:?}"));
                }
            }
            Ok(if command == "install" {
                Command::Install { config, prefix }
            } else {
                Command::Uninstall { prefix }
            })
        }
        "help" | "--help" | "-h" => Ok(Command::Help),
        "version" | "--version" | "-V" => Ok(Command::Version),
        other => Err(format!("unknown command {other:?}")),
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate {name}"));
    }
    *slot = Some(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(line: &str) -> Result<Command, String> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn gadget_modes_with_all_flags() {
        assert_eq!(
            parse_str("steam --config a.vdf --udc fcc00000.usb --skip-modprobe"),
            Ok(Command::Steam(GadgetArgs {
                config: Some("a.vdf".into()),
                evdev: None,
                udc: Some("fcc00000.usb".into()),
                configfs: None,
                skip_modprobe: true,
            }))
        );
        assert_eq!(
            parse_str("manual --configfs /mnt/cfg/usb_gadget"),
            Ok(Command::Manual(GadgetArgs {
                configfs: Some("/mnt/cfg/usb_gadget".into()),
                ..GadgetArgs::default()
            }))
        );
    }

    #[test]
    fn flag_equals_value_form() {
        assert_eq!(
            parse_str("steamcheck --config=configs/default.vdf --evdev /dev/input/event9"),
            Ok(Command::Steamcheck {
                config: Some("configs/default.vdf".into()),
                evdev: Some("/dev/input/event9".into()),
            })
        );
    }

    #[test]
    fn hostcheck_device_is_positional() {
        assert_eq!(
            parse_str("hostcheck /dev/hidraw3"),
            Ok(Command::Hostcheck {
                device: Some("/dev/hidraw3".into())
            })
        );
        assert_eq!(
            parse_str("hostcheck"),
            Ok(Command::Hostcheck { device: None })
        );
    }

    #[test]
    fn install_and_uninstall() {
        assert_eq!(
            parse_str("install --config configs/default.vdf --prefix /usr/local/lib/sweam"),
            Ok(Command::Install {
                config: Some("configs/default.vdf".into()),
                prefix: Some("/usr/local/lib/sweam".into()),
            })
        );
        assert_eq!(
            parse_str("install"),
            Ok(Command::Install {
                config: None,
                prefix: None
            })
        );
        assert_eq!(
            parse_str("uninstall --prefix=/usr/local/lib/sweam"),
            Ok(Command::Uninstall {
                prefix: Some("/usr/local/lib/sweam".into()),
            })
        );
    }

    #[test]
    fn help_and_version_spellings() {
        for line in ["", "help", "--help", "-h", "steam --help"] {
            assert_eq!(parse_str(line), Ok(Command::Help), "{line:?}");
        }
        for line in ["version", "--version", "-V", "steam -V"] {
            assert_eq!(parse_str(line), Ok(Command::Version), "{line:?}");
        }
    }

    #[test]
    fn errors_are_helpful() {
        for (line, needle) in [
            ("fly", "unknown command"),
            ("steam --config", "needs a value"),
            ("steam --config a --config b", "duplicate"),
            ("steam --turbo", "unknown option"),
            ("steam someudc", "--udc"),
            ("manual --config a.vdf", "does not apply"),
            ("manual --evdev /dev/input/event1", "does not apply"),
            ("steamcheck --udc x", "does not apply"),
            ("hostcheck --config a.vdf", "does not apply"),
            ("hostcheck --evdev /dev/input/event1", "does not apply"),
            ("steamcheck stray", "unexpected argument"),
            ("steam --prefix /opt/x", "does not apply"),
            ("steamcheck --prefix /opt/x", "does not apply"),
            ("install --udc x", "does not apply"),
            ("install --evdev /dev/input/event1", "does not apply"),
            ("install stray", "unexpected argument"),
            ("uninstall --config a.vdf", "does not apply"),
        ] {
            let err = parse_str(line).unwrap_err();
            assert!(err.contains(needle), "{line:?} -> {err:?}");
        }
    }
}

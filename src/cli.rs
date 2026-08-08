//! Command-line parsing (clap derive). Each subcommand carries only the
//! flags that apply to it, so inapplicable flags are rejected structurally
//! instead of via per-subcommand checks. Pure; tests run on any platform.

use clap::{Args, Parser, Subcommand};

const TRACE_LONG_ABOUT: &str = "Record the gadget-side USB conversation.

sweam's log shows what the host asked for, never what happened on the wire.
usbmon cannot help: it captures on a USB host controller, and this board is
the peripheral. This uses the UDC driver's ftrace tracepoints instead — bus
events (reset/suspend/disconnect), control requests, endpoint commands.

Recording is a ring buffer, so nothing has to be caught live. `sweam install
--trace` runs `snapshot` whenever the service stops, leaving every
disconnect's trace in the journal with nobody at the keyboard.";

const LONG_ABOUT: &str = "Steam Controller → Switch Pro Controller USB bridge.

Everything is auto-detected where possible; every detected value can be \
overridden with a flag. All flags accept both \"--flag value\" and \
\"--flag=value\".

Manual mode reads Pro Controller inputs typed on stdin — run \
\"sweam help manual\" for the stdin command reference.";

const MANUAL_LONG_ABOUT: &str = "bridge with inputs typed on stdin (testing)

Manual mode commands (typed on stdin):
  press <button…> | release <button…> | stick <l|r> <x> <y> | neutral
  gyro <x> <y> <z> (deg/s) | accel <x> <y> <z> (g)
  buttons: a b x y up down left right l r zl zr plus minus home capture
           lstick rstick;  stick x/y in -1..1";

/// Package version plus the git commit it was built from (see build.rs) —
/// deploys are a plain `scp`, so this is the only way to tell which build a
/// device is running or which one wrote a journal.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("SWEAM_BUILD"), ")");

#[derive(Debug, Parser, PartialEq)]
#[command(
    name = "sweam",
    version = VERSION,
    about = "Steam Controller → Switch Pro Controller USB bridge",
    long_about = LONG_ABOUT,
    after_help = "Docs: README.md (usage), configs/ (mappings), PLAN.md (roadmap).",
    arg_required_else_help = true,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// What `main` should do, fully validated.
#[derive(Debug, Subcommand, PartialEq)]
pub enum Command {
    /// bridge a real Steam Controller (needs root)
    Steam {
        #[command(flatten)]
        input: InputOpts,
        #[command(flatten)]
        gadget: GadgetOpts,
    },
    /// bridge with inputs typed on stdin (testing)
    #[command(long_about = MANUAL_LONG_ABOUT)]
    Manual {
        #[command(flatten)]
        gadget: GadgetOpts,
    },
    /// record the gadget-side USB conversation (dwc3 tracepoints)
    #[command(long_about = TRACE_LONG_ABOUT)]
    Trace {
        #[arg(value_enum)]
        action: TraceAction,
    },
    /// print parsed Steam Controller inputs
    Steamcheck {
        #[command(flatten)]
        input: InputOpts,
    },
    /// verify a sweam gadget from the USB host side
    Hostcheck {
        /// its hidraw node; default: detected by USB IDs among /dev/hidraw*
        #[arg(value_name = "DEVICE")]
        device: Option<String>,
    },
    /// install this binary (and the config) to /opt/sweam and enable a
    /// systemd service running "sweam steam" at boot (needs root)
    Install {
        /// Steam-style VDF controller mapping installed next to the binary
        /// and used by the service; see configs/ for commented examples
        #[arg(long, value_name = "FILE")]
        config: Option<String>,
        #[command(flatten)]
        prefix: PrefixOpt,
        /// also record the USB conversation: the service starts tracing at
        /// boot and dumps the events preceding every disconnect into the
        /// journal, so a session at the Switch needs no one at the keyboard
        #[arg(long)]
        trace: bool,
    },
    /// stop the service and remove /opt/sweam
    Uninstall {
        #[command(flatten)]
        prefix: PrefixOpt,
    },
}

/// What `sweam trace` should do to the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TraceAction {
    /// begin recording into a ring buffer
    Start,
    /// print recent events, then clear the buffer
    Snapshot,
    /// print the whole buffer, leaving it in place
    Dump,
    /// stop recording
    Stop,
}

/// Controller-input options shared by `steam` and `steamcheck`.
#[derive(Debug, Args, PartialEq, Default)]
pub struct InputOpts {
    /// Steam-style VDF controller mapping; see configs/ for commented
    /// examples. Default: built-in positional layout
    #[arg(long, value_name = "FILE")]
    pub config: Option<String>,
    /// the controller's /dev/hidrawN. Default: every dongle slot is opened
    /// and whichever delivers packets wins (a wired controller likewise)
    #[arg(long, value_name = "PATH")]
    pub hidraw_device: Option<String>,
}

/// Gadget-side options shared by `steam` and `manual`.
#[derive(Debug, Args, PartialEq, Default)]
pub struct GadgetOpts {
    /// USB device controller to bind; default: detected from /sys/class/udc
    /// (first one, with a warning if several)
    #[arg(long, value_name = "NAME")]
    pub udc: Option<String>,
    /// configfs gadget root; default: detected from /proc/mounts (usually
    /// /sys/kernel/config/usb_gadget)
    #[arg(long, value_name = "PATH")]
    pub configfs: Option<String>,
    /// don't load the libcomposite/usb_f_hid kernel modules first — use when
    /// they are built into your kernel or already loaded
    #[arg(long)]
    pub skip_modprobe: bool,
    /// current to request from the host, in mA (default 500, i.e. 0.5 A —
    /// what a real Pro Controller draws to charge). Use 0 when the board is
    /// powered independently, so the host is not asked for current it does
    /// not need to budget
    #[arg(long, value_name = "MA")]
    pub max_power: Option<u32>,
}

/// `--prefix`, shared by `install` and `uninstall`.
#[derive(Debug, Args, PartialEq, Default)]
pub struct PrefixOpt {
    /// install directory; default /opt/sweam (the systemd unit always goes
    /// to /etc/systemd/system/sweam.service)
    #[arg(long, value_name = "DIR")]
    pub prefix: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parse_str(line: &str) -> Result<Command, clap::Error> {
        Cli::try_parse_from(std::iter::once("sweam").chain(line.split_whitespace()))
            .map(|cli| cli.command)
    }

    #[test]
    fn gadget_modes_with_all_flags() {
        assert_eq!(
            parse_str("steam --config a.vdf --udc fcc00000.usb --skip-modprobe").unwrap(),
            Command::Steam {
                input: InputOpts {
                    config: Some("a.vdf".into()),
                    ..InputOpts::default()
                },
                gadget: GadgetOpts {
                    udc: Some("fcc00000.usb".into()),
                    skip_modprobe: true,
                    ..GadgetOpts::default()
                },
            }
        );
        assert_eq!(
            parse_str("manual --configfs /mnt/cfg/usb_gadget").unwrap(),
            Command::Manual {
                gadget: GadgetOpts {
                    configfs: Some("/mnt/cfg/usb_gadget".into()),
                    ..GadgetOpts::default()
                },
            }
        );
    }

    #[test]
    fn max_power_is_overridable() {
        let Command::Steam { gadget, .. } = parse_str("steam --max-power 0").unwrap() else {
            panic!("expected steam");
        };
        assert_eq!(gadget.max_power, Some(0));
        // Absent means the default (a real Pro Controller's 500 mA), which
        // the gadget layer applies — not None meaning "declare nothing".
        let Command::Steam { gadget, .. } = parse_str("steam").unwrap() else {
            panic!("expected steam");
        };
        assert_eq!(gadget.max_power, None);
    }

    #[test]
    fn flag_equals_value_form() {
        assert_eq!(
            parse_str("steamcheck --config=configs/default.vdf --hidraw-device /dev/hidraw3")
                .unwrap(),
            Command::Steamcheck {
                input: InputOpts {
                    config: Some("configs/default.vdf".into()),
                    hidraw_device: Some("/dev/hidraw3".into()),
                },
            }
        );
        assert_eq!(
            parse_str("steam --udc=fcc00000.usb --configfs=/mnt/cfg").unwrap(),
            Command::Steam {
                input: InputOpts::default(),
                gadget: GadgetOpts {
                    udc: Some("fcc00000.usb".into()),
                    configfs: Some("/mnt/cfg".into()),
                    ..GadgetOpts::default()
                },
            }
        );
    }

    #[test]
    fn hostcheck_device_is_positional() {
        assert_eq!(
            parse_str("hostcheck /dev/hidraw3").unwrap(),
            Command::Hostcheck {
                device: Some("/dev/hidraw3".into())
            }
        );
        assert_eq!(
            parse_str("hostcheck").unwrap(),
            Command::Hostcheck { device: None }
        );
    }

    #[test]
    fn install_and_uninstall() {
        assert_eq!(
            parse_str("install --config configs/default.vdf --prefix /usr/local/lib/sweam")
                .unwrap(),
            Command::Install {
                config: Some("configs/default.vdf".into()),
                prefix: PrefixOpt {
                    prefix: Some("/usr/local/lib/sweam".into()),
                },
                trace: false,
            }
        );
        assert_eq!(
            parse_str("install --trace").unwrap(),
            Command::Install {
                config: None,
                prefix: PrefixOpt::default(),
                trace: true,
            }
        );
        assert_eq!(
            parse_str("uninstall --prefix=/usr/local/lib/sweam").unwrap(),
            Command::Uninstall {
                prefix: PrefixOpt {
                    prefix: Some("/usr/local/lib/sweam".into()),
                },
            }
        );
    }

    #[test]
    fn help_and_version_spellings() {
        for line in ["--help", "-h", "help", "help manual", "steam --help"] {
            assert_eq!(
                parse_str(line).unwrap_err().kind(),
                ErrorKind::DisplayHelp,
                "{line:?}"
            );
        }
        assert_eq!(
            parse_str("").unwrap_err().kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        for line in ["--version", "-V", "steam -V"] {
            assert_eq!(
                parse_str(line).unwrap_err().kind(),
                ErrorKind::DisplayVersion,
                "{line:?}"
            );
        }
    }

    #[test]
    fn errors_are_helpful() {
        for (line, needle) in [
            ("fly", "unrecognized subcommand"),
            ("hostchek", "similar subcommands exist"), // did-you-mean
            ("steam --config", "a value is required"),
            (
                "steam --config a --config b",
                "cannot be used multiple times",
            ),
            ("steam --turbo", "unexpected argument"),
            // The boolean --hidraw is gone with the evdev source: raw is
            // the only path, so there is nothing left to switch on.
            ("steam --hidraw", "unexpected argument"),
            ("steam someudc", "unexpected argument"),
            ("manual --config a.vdf", "unexpected argument"),
            ("manual --hidraw-device /dev/hidraw1", "unexpected argument"),
            ("steamcheck --udc x", "unexpected argument"),
            ("steamcheck --skip-modprobe", "unexpected argument"),
            ("hostcheck --config a.vdf", "unexpected argument"),
            (
                "hostcheck --hidraw-device /dev/hidraw1",
                "unexpected argument",
            ),
            ("steamcheck stray", "unexpected argument"),
            ("steam --prefix /opt/x", "unexpected argument"),
            ("steamcheck --prefix /opt/x", "unexpected argument"),
            ("install --udc x", "unexpected argument"),
            (
                "install --hidraw-device /dev/hidraw1",
                "unexpected argument",
            ),
            ("install stray-positional", "unexpected argument"),
            ("uninstall --config a.vdf", "unexpected argument"),
            ("steam --skip-modprobe=false", "unexpected value"),
        ] {
            let err = parse_str(line).unwrap_err().to_string();
            assert!(err.contains(needle), "{line:?} -> {err:?}");
        }
    }
}

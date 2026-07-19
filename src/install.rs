//! `sweam install` / `sweam uninstall`: run the bridge as a systemd service.
//!
//! `install` copies the currently-running binary to /opt/sweam (plus the
//! mapping config, if given), writes a systemd unit that runs `sweam steam`
//! at boot, and enables + starts it. This lets the board work headless —
//! e.g. plugged into an actual Switch where there is no SSH — with
//! `Restart=always` retrying after failures and systemd's SIGTERM giving
//! the existing clean gadget teardown. `uninstall` reverses all of it.

#[cfg(target_os = "linux")]
pub use imp::{install, uninstall};

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{bail, Context};
    use std::path::Path;
    use std::process::Command;

    const DEFAULT_PREFIX: &str = "/opt/sweam";
    const UNIT_NAME: &str = "sweam.service";
    const UNIT_PATH: &str = "/etc/systemd/system/sweam.service";

    pub fn install(config: Option<&str>, prefix: Option<&str>) -> anyhow::Result<()> {
        ensure_root()?;
        let prefix = prefix.unwrap_or(DEFAULT_PREFIX);
        let binary_path = format!("{prefix}/sweam");
        let config_path = format!("{prefix}/config.vdf");

        std::fs::create_dir_all(prefix).with_context(|| format!("Failed to create {prefix}"))?;

        // Copy ourselves via a temp file + rename: overwriting the installed
        // binary in place would fail with ETXTBSY while the service runs.
        let exe = std::env::current_exe().context("Failed to find the running binary")?;
        let tmp = format!("{binary_path}.tmp");
        std::fs::copy(&exe, &tmp)
            .with_context(|| format!("Failed to copy {} to {tmp}", exe.display()))?;
        std::fs::rename(&tmp, &binary_path)
            .with_context(|| format!("Failed to move {tmp} to {binary_path}"))?;
        println!("Installed {binary_path} (from {})", exe.display());

        let exec_start = match config {
            Some(config) => {
                std::fs::copy(config, &config_path)
                    .with_context(|| format!("Failed to copy {config} to {config_path}"))?;
                println!("Installed {config_path} (from {config})");
                format!("{binary_path} steam --config {config_path}")
            }
            None => format!("{binary_path} steam"),
        };

        let unit = format!(
            "\
[Unit]
Description=Steam Controller to Switch Pro Controller USB bridge

[Service]
ExecStart={exec_start}
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
"
        );
        std::fs::write(UNIT_PATH, unit).with_context(|| format!("Failed to write {UNIT_PATH}"))?;
        println!("Installed {UNIT_PATH}");

        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", UNIT_NAME])?;
        // Not enable --now: that leaves an already-running old instance in
        // place on reinstall; restart covers both first install and upgrade.
        systemctl(&["restart", UNIT_NAME])?;
        println!(
            "Service enabled and started; it now runs at every boot.\n\
             Watch it with: journalctl -u {UNIT_NAME} -f"
        );
        Ok(())
    }

    pub fn uninstall(prefix: Option<&str>) -> anyhow::Result<()> {
        ensure_root()?;
        let prefix = prefix.unwrap_or(DEFAULT_PREFIX);

        // Tolerate a partial install: stop/disable only if the unit exists.
        if Path::new(UNIT_PATH).exists() {
            systemctl(&["disable", "--now", UNIT_NAME])?;
            std::fs::remove_file(UNIT_PATH)
                .with_context(|| format!("Failed to remove {UNIT_PATH}"))?;
            systemctl(&["daemon-reload"])?;
            println!("Removed {UNIT_PATH}");
        } else {
            println!("No {UNIT_PATH}; nothing to stop");
        }

        if Path::new(prefix).exists() {
            std::fs::remove_dir_all(prefix)
                .with_context(|| format!("Failed to remove {prefix}"))?;
            println!("Removed {prefix}");
        } else {
            println!("No {prefix}; nothing to remove");
        }
        Ok(())
    }

    fn ensure_root() -> anyhow::Result<()> {
        if unsafe { libc::geteuid() } != 0 {
            bail!("install/uninstall write to /opt and /etc — run as root (sudo)");
        }
        Ok(())
    }

    fn systemctl(args: &[&str]) -> anyhow::Result<()> {
        let status = Command::new("systemctl")
            .args(args)
            .status()
            .with_context(|| format!("Failed to run systemctl {args:?}"))?;
        if !status.success() {
            bail!("systemctl {args:?} failed with {status}");
        }
        Ok(())
    }
}

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
    use anyhow::{Context, bail};
    use std::path::Path;
    use std::process::Command;

    const DEFAULT_PREFIX: &str = "/opt/sweam";
    const UNIT_NAME: &str = "sweam.service";
    const UNIT_PATH: &str = "/etc/systemd/system/sweam.service";

    pub fn install(config: Option<&str>, prefix: Option<&str>) -> anyhow::Result<()> {
        ensure_root()?;
        let prefix = validate_prefix(prefix)?;
        let binary_path = format!("{prefix}/sweam");
        let config_path = format!("{prefix}/config.vdf");

        // A config typo would otherwise surface only as a headless boot-time
        // crash loop at the Switch — validate before installing anything.
        if let Some(config) = config {
            crate::steam::config::load(config).with_context(|| {
                format!("Refusing to install a config that fails to load: {config}")
            })?;
        }

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

        // Quote the paths: systemd splits ExecStart on unquoted whitespace.
        let exec_start = match config {
            Some(config) => {
                std::fs::copy(config, &config_path)
                    .with_context(|| format!("Failed to copy {config} to {config_path}"))?;
                println!("Installed {config_path} (from {config})");
                format!("\"{binary_path}\" steam --config \"{config_path}\"")
            }
            None => format!("\"{binary_path}\" steam"),
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
        let prefix = validate_prefix(prefix)?;

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

        // Remove only the files we installed, then the directory if it is
        // empty — never recursively: `--prefix /opt` must not nuke /opt.
        let mut removed_any = false;
        for file in [format!("{prefix}/sweam"), format!("{prefix}/config.vdf")] {
            if Path::new(&file).exists() {
                std::fs::remove_file(&file).with_context(|| format!("Failed to remove {file}"))?;
                println!("Removed {file}");
                removed_any = true;
            }
        }
        match std::fs::remove_dir(prefix) {
            Ok(()) => println!("Removed {prefix}"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if !removed_any {
                    println!("No {prefix}; nothing to remove");
                }
            }
            Err(err) if err.raw_os_error() == Some(libc::ENOTEMPTY) => {
                println!("Kept {prefix}: it contains files sweam did not install");
            }
            Err(err) => return Err(err).with_context(|| format!("Failed to remove {prefix}")),
        }
        Ok(())
    }

    /// Installation prefixes must be absolute (systemd requires an absolute
    /// ExecStart) and quotable into the unit file.
    fn validate_prefix(prefix: Option<&str>) -> anyhow::Result<&str> {
        let prefix = prefix.unwrap_or(DEFAULT_PREFIX);
        if !prefix.starts_with('/') || prefix.ends_with('/') {
            bail!("--prefix must be an absolute path without a trailing slash, got {prefix:?}");
        }
        if prefix.contains('"') || prefix.contains('\n') {
            bail!("--prefix must not contain quotes or newlines, got {prefix:?}");
        }
        Ok(prefix)
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

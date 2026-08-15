//! Mode switching, armed-file edition.
//!
//! /etc/greetd/config.toml is a single static file: a permanent
//! `[initial_session]` running `game-mode-dispatch` (as the games user) and a
//! `[default_session]` running the vigil greeter. Entering game mode no
//! longer rewrites any greetd config — the daemon arms a one-shot flag in
//! /run and restarts greetd; the dispatcher execs the game session while the
//! flag is fresh and exits 0 (falling through to the greeter) otherwise.
//!
//! The restart itself is still required: greetd runs `[initial_session]` only
//! on a start with no `/run/greetd.run` present, so the runfile removal and
//! the restart (both under exact-match sudoers grants) remain the trigger.

use crate::config::{Config, ARMED_FILE, ARMED_MAX_AGE_SECS};
use anyhow::Result;
use std::{fs, path::Path, process::Command, time::SystemTime};
use tracing::{debug, info, warn};

/// Whether the armed flag at `path` is present and fresh. Staleness (a crash
/// between arming and the greetd restart) must read as disarmed — a login
/// screen that spontaneously boots into Steam a day later is not a feature.
pub fn is_armed(path: &Path, max_age_secs: u64) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    match SystemTime::now().duration_since(mtime) {
        Ok(age) => age.as_secs() <= max_age_secs,
        // mtime in the future: clock weirdness, treat as fresh — the file
        // cannot exist without a recent, deliberate arm (or a reboot, which
        // clears /run entirely).
        Err(_) => true,
    }
}

fn arm() -> Result<()> {
    if let Some(dir) = Path::new(ARMED_FILE).parent() {
        fs::create_dir_all(dir)?;
    }
    // Content is informational; the dispatcher only checks existence + mtime.
    fs::write(ARMED_FILE, "armed by game-mode daemon\n")?;
    Ok(())
}

/// Remove the armed flag (best-effort — it self-expires anyway).
pub fn disarm() {
    if let Err(e) = fs::remove_file(ARMED_FILE) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!("could not remove {ARMED_FILE}: {e}");
        }
    }
}

pub fn switch_to_game_mode() -> Result<()> {
    info!("Starting game mode switch");

    let config = Config::load()?;
    arm()?;
    debug!("armed {ARMED_FILE} (max age {ARMED_MAX_AGE_SECS}s)");

    // Blank the VT and hide its cursor so the handoff gap (greeter compositor
    // teardown -> gamescope first frame) shows black instead of stale console
    // text. Best-effort: the greeter user owns the session tty.
    let config_vt = format!("/dev/tty{}", config.terminal.vt);
    if let Ok(mut tty) = fs::OpenOptions::new().write(true).open(&config_vt) {
        use std::io::Write;
        let _ = tty.write_all(b"\x1b[2J\x1b[H\x1b[?25l");
    }

    // greetd only honours [initial_session] on its first run since boot,
    // tracked by the presence of this runfile — remove it or the restart
    // below lands on the greeter instead of the game session. The exact
    // command is allowed in sudoers (see dist/sudoers-greeter-greetd).
    let status = Command::new("sudo")
        .args(["/usr/bin/rm", "-f", "/run/greetd.run"])
        .status()?;
    if !status.success() {
        disarm();
        anyhow::bail!("failed to remove greetd runfile (sudoers entry missing?)");
    }

    // Restart greetd service
    let status = Command::new("sudo")
        .args(["/usr/bin/systemctl", "restart", "greetd.service"])
        .status()?;
    debug!("systemctl command exit status: {}", status);
    if !status.success() {
        disarm();
        anyhow::bail!("failed to restart greetd.service");
    }

    info!("Successfully switched to game mode");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn missing_flag_is_disarmed() {
        assert!(!is_armed(Path::new("/nonexistent/armed"), 60));
    }

    #[test]
    fn fresh_flag_is_armed_and_stale_is_not() {
        let dir = std::env::temp_dir().join(format!("game-mode-armed-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("armed");
        fs::write(&path, "x").unwrap();
        assert!(is_armed(&path, 60));

        // Backdate the mtime instead of sleeping.
        let old = SystemTime::now() - Duration::from_secs(120);
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(old).unwrap();
        assert!(!is_armed(&path, 60));
        fs::remove_dir_all(&dir).unwrap();
    }
}

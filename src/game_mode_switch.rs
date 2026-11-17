use crate::config::{Config, ARMED_FILE, ARMED_MAX_AGE_SECS};
use anyhow::Result;
use std::{fs, path::Path, process::Command, time::SystemTime};
use tracing::{debug, info, warn};

pub fn is_armed(path: &Path, max_age_secs: u64) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    match SystemTime::now().duration_since(mtime) {
        Ok(age) => age.as_secs() <= max_age_secs,
        Err(_) => true,
    }
}

fn arm() -> Result<()> {
    if let Some(dir) = Path::new(ARMED_FILE).parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(ARMED_FILE, "armed by game-mode daemon\n")?;
    Ok(())
}

pub fn disarm() {
    if let Err(e) = fs::remove_file(ARMED_FILE) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!("could not remove {ARMED_FILE}: {e}");
        }
    }
}

pub fn switch_to_game_mode(config: &Config) -> Result<()> {
    info!("Starting game mode switch");

    arm()?;
    debug!("armed {ARMED_FILE} (max age {ARMED_MAX_AGE_SECS}s)");

    let config_vt = format!("/dev/tty{}", config.terminal.vt);
    if let Ok(mut tty) = fs::OpenOptions::new().write(true).open(&config_vt) {
        use std::io::Write;
        let _ = tty.write_all(b"\x1b[2J\x1b[H\x1b[?25l");
    }

    let status = Command::new("sudo")
        .args(["/usr/bin/rm", "-f", "/run/greetd.run"])
        .status()?;
    if !status.success() {
        disarm();
        anyhow::bail!("failed to remove greetd runfile (sudoers entry missing?)");
    }

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

        let old = SystemTime::now() - Duration::from_secs(120);
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(old).unwrap();
        assert!(!is_armed(&path, 60));
        fs::remove_dir_all(&dir).unwrap();
    }
}

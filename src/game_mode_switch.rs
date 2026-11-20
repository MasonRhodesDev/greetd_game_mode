use crate::config::{Config, ARMED_FILE, ARMED_MAX_AGE_SECS};
use anyhow::Result;
use std::{fs, path::Path, process::Command, time::SystemTime};
use tracing::{debug, error, info, warn};

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

struct Flag<'a>(&'a Path);

impl Flag<'_> {
    fn arm(&self) -> Result<()> {
        if let Some(dir) = self.0.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(self.0, "armed by game-mode daemon\n")?;
        debug!("armed {} (max age {ARMED_MAX_AGE_SECS}s)", self.0.display());
        Ok(())
    }

    fn disarm(&self) {
        if let Err(e) = fs::remove_file(self.0) {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!("could not remove {}: {e}", self.0.display());
            }
        }
    }
}

pub fn disarm() {
    Flag(Path::new(ARMED_FILE)).disarm();
}

fn sudo(args: &[&str]) -> Command {
    let mut command = Command::new("sudo");
    command.args(args);
    command
}

pub fn switch_to_game_mode(config: &Config) -> Result<()> {
    info!("Starting game mode switch");
    let vt = format!("/dev/tty{}", config.terminal.vt);
    hand_off(&Flag(Path::new(ARMED_FILE)), Path::new(&vt), &sudo)?;
    info!("Successfully switched to game mode");
    Ok(())
}

fn hand_off(flag: &Flag, vt: &Path, sudo: &dyn Fn(&[&str]) -> Command) -> Result<()> {
    flag.arm()?;
    if let Ok(mut tty) = fs::OpenOptions::new().write(true).open(vt) {
        use std::io::Write;
        let _ = tty.write_all(b"\x1b[2J\x1b[H\x1b[?25l");
    }
    restart_greetd(flag, sudo).inspect_err(|e| {
        error!("game-mode handoff failed: {e:#}");
        flag.disarm();
    })
}

fn restart_greetd(flag: &Flag, sudo: &dyn Fn(&[&str]) -> Command) -> Result<()> {
    let status = sudo(&["/usr/bin/rm", "-f", "/run/greetd.run"]).status()?;
    if !status.success() {
        flag.disarm();
        anyhow::bail!("failed to remove greetd runfile ({status}; sudoers entry missing?)");
    }

    let status = sudo(&["/usr/bin/systemctl", "restart", "greetd.service"]).status()?;
    if !status.success() {
        flag.disarm();
        anyhow::bail!("failed to restart greetd.service ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, path::PathBuf, time::Duration};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("game-mode-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_flag_is_disarmed() {
        assert!(!is_armed(Path::new("/nonexistent/armed"), 60));
    }

    #[test]
    fn fresh_flag_is_armed_and_stale_is_not() {
        let dir = scratch("armed-age");
        let path = dir.join("armed");
        fs::write(&path, "x").unwrap();
        assert!(is_armed(&path, 60));

        let old = SystemTime::now() - Duration::from_secs(120);
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(old).unwrap();
        assert!(!is_armed(&path, 60));
        fs::remove_dir_all(&dir).unwrap();
    }

    fn run_hand_off(name: &str, rm: &str, systemctl: &str) -> (Result<()>, bool, Vec<String>) {
        let dir = scratch(name);
        let armed = dir.join("run/armed");
        let calls = RefCell::new(Vec::new());
        let fake_sudo = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            Command::new(if args[0].ends_with("/systemctl") {
                systemctl
            } else {
                rm
            })
        };
        let result = hand_off(&Flag(&armed), &dir.join("tty"), &fake_sudo);
        let still_armed = armed.exists();
        fs::remove_dir_all(&dir).unwrap();
        (result, still_armed, calls.into_inner())
    }

    const RM: &str = "/usr/bin/rm -f /run/greetd.run";
    const RESTART: &str = "/usr/bin/systemctl restart greetd.service";

    #[test]
    fn successful_restart_keeps_flag_armed() {
        let (result, armed, calls) = run_hand_off("restart-ok", "true", "true");
        result.unwrap();
        assert!(armed);
        assert_eq!(calls, [RM, RESTART]);
    }

    #[test]
    fn failed_restart_disarms() {
        let (result, armed, calls) = run_hand_off("restart-fails", "true", "false");
        assert!(result.unwrap_err().to_string().contains("greetd.service"));
        assert!(!armed);
        assert_eq!(calls, [RM, RESTART]);
    }

    #[test]
    fn restart_that_cannot_spawn_disarms() {
        let (result, armed, calls) =
            run_hand_off("restart-no-spawn", "true", "/nonexistent/systemctl");
        assert!(result.is_err());
        assert!(!armed);
        assert_eq!(calls, [RM, RESTART]);
    }

    #[test]
    fn failed_runfile_removal_disarms_without_restarting() {
        let (result, armed, calls) = run_hand_off("rm-fails", "false", "true");
        assert!(result.unwrap_err().to_string().contains("runfile"));
        assert!(!armed);
        assert_eq!(calls, [RM]);
    }
}

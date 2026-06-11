use anyhow::Result;
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    process::Command,
};
use crate::config::Config;
use tracing::{debug, info};

/// Atomically point config.toml at `target`: create the symlink under a temp
/// name, then rename over the real path. rename(2) is atomic on the same
/// filesystem, so there is no window where config.toml doesn't exist (the old
/// remove_file + `ln -sf` pair could crash between the two steps and leave
/// greetd with no config).
fn set_config_symlink(target: &Path, config_path: &Path) -> Result<()> {
    let tmp = config_path.with_file_name("config.toml.new");
    debug!("Symlinking {:?} -> {:?} (via {:?})", config_path, target, tmp);
    let _ = fs::remove_file(&tmp); // stale temp from a previous crash
    symlink(target, &tmp)?;
    fs::rename(&tmp, config_path)?;
    Ok(())
}

pub fn switch_to_game_mode() -> Result<()> {
    info!("Starting game mode switch");

    let config = Config::load()?;
    let config_path = config.get_config_path();
    let game_mode_config = config.get_game_mode_config_path();

    debug!("Config path: {:?}", config_path);
    debug!("Game mode config path: {:?}", game_mode_config);
    debug!("Game mode config exists: {}", game_mode_config.exists());

    set_config_symlink(&game_mode_config, &config_path)?;

    // Restart greetd service
    let cmd = "sudo /usr/bin/systemctl restart greetd.service";
    debug!("Running command: {}", cmd);
    let status = Command::new("sudo")
        .args(["/usr/bin/systemctl", "restart", "greetd.service"])
        .status()?;
    debug!("systemctl command exit status: {}", status);

    info!("Successfully switched to game mode");
    Ok(())
}

pub fn switch_to_desktop_mode() -> Result<()> {
    info!("Starting desktop mode switch");

    let config = Config::load()?;
    let config_path = config.get_config_path();
    let default_config = config.get_default_config_path();

    debug!("Config path: {:?}", config_path);
    debug!("Default config path: {:?}", default_config);
    debug!("Default config exists: {}", default_config.exists());

    set_config_symlink(&default_config, &config_path)?;

    info!("Successfully switched to desktop mode");
    Ok(())
}

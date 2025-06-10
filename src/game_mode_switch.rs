use anyhow::Result;
use std::{
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    fs,
};
use crate::config::Config;
use tracing::{debug, info};

static SWITCH_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn switch_to_game_mode() -> Result<()> {
    info!("Starting game mode switch");
    
    // Check if switch is already in progress
    if SWITCH_IN_PROGRESS.load(Ordering::SeqCst) {
        info!("Game mode switch already in progress, ignoring request");
        return Ok(());
    }

    // Set the flag to indicate switch is in progress
    SWITCH_IN_PROGRESS.store(true, Ordering::SeqCst);
    info!("Switch in progress flag set");

    let config = Config::load()?;
    let config_path = config.get_config_path();
    let game_mode_config = config.get_game_mode_config_path();

    debug!("Config path: {:?}", config_path);
    debug!("Game mode config path: {:?}", game_mode_config);
    debug!("Config path exists: {}", config_path.exists());
    debug!("Game mode config exists: {}", game_mode_config.exists());

    // Remove existing symlink or file if it exists
    if config_path.exists() {
        debug!("Removing existing config file/symlink");
        fs::remove_file(&config_path)?;
    }

    // Create symlink to game mode config
    let cmd = format!("ln -sf {} {}", game_mode_config.to_str().unwrap(), config_path.to_str().unwrap());
    debug!("Running command: {}", cmd);
    let status = Command::new("ln")
        .args(["-sf", game_mode_config.to_str().unwrap(), config_path.to_str().unwrap()])
        .status()?;
    debug!("ln command exit status: {}", status);

    // Restart greetd service
    let cmd = format!("sudo /usr/bin/systemctl restart greetd.service");
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
    
    // create symlink to desktop mode config
    let config = Config::load()?;
    let config_path = config.get_config_path();
    let default_config = config.get_default_config_path();

    debug!("Config path: {:?}", config_path);
    debug!("Default config path: {:?}", default_config);
    debug!("Config path exists: {}", config_path.exists());
    debug!("Default config exists: {}", default_config.exists());

    // Remove existing symlink or file if it exists
    if config_path.exists() {
        debug!("Removing existing config file/symlink");
        fs::remove_file(&config_path)?;
    }

    let cmd = format!("ln -sf {} {}", default_config.to_str().unwrap(), config_path.to_str().unwrap());
    debug!("Running command: {}", cmd);
    let status = Command::new("ln")
        .args(["-sf", default_config.to_str().unwrap(), config_path.to_str().unwrap()])
        .status()?;
    debug!("ln command exit status: {}", status);

    info!("Successfully switched to desktop mode");
    Ok(())
}
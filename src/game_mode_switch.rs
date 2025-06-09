use anyhow::Result;
use std::{
    fs,
    process::Command,
    time::Duration,
    sync::atomic::{AtomicBool, Ordering},
};
use crate::config::Config;
use tracing::{debug, error, info, warn};

static SWITCH_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

fn reset_config_after_delay(config_path: std::path::PathBuf, backup_path: std::path::PathBuf, delay_secs: u64) {
    // Double fork to create a truly detached process
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // First child
        let pid2 = unsafe { libc::fork() };
        if pid2 == 0 {
            // Second child - this is our detached process
            // Close all file descriptors
            for fd in 0..1024 {
                let _ = unsafe { libc::close(fd) };
            }
            
            // Detach from controlling terminal
            let _ = unsafe { libc::setsid() };

            info!("Reset process started, waiting {} seconds before restoring config", delay_secs);
            // Wait for the switch back delay
            std::thread::sleep(Duration::from_secs(delay_secs));

            if backup_path.exists() {
                info!("Restoring original greetd configuration");
                let cmd = format!("sudo cp {} {}", backup_path.to_str().unwrap(), config_path.to_str().unwrap());
                debug!("Running command: {}", cmd);
                match Command::new("sudo")
                    .args(["cp", backup_path.to_str().unwrap(), config_path.to_str().unwrap()])
                    .status() 
                {
                    Ok(status) if status.success() => {
                        info!("Successfully restored original configuration");
                        
                        let cmd = format!("sudo rm {}", backup_path.to_str().unwrap());
                        debug!("Running command: {}", cmd);
                        match Command::new("sudo")
                            .args(["rm", backup_path.to_str().unwrap()])
                            .status() 
                        {
                            Ok(status) if status.success() => {
                                info!("Successfully removed backup file");
                                info!("Exiting game-mode process");
                                std::process::exit(0);
                            }
                            Ok(_) => {
                                warn!("Failed to remove backup file");
                                std::process::exit(1);
                            }
                            Err(e) => {
                                error!("Error removing backup file: {}", e);
                                std::process::exit(1);
                            }
                        }
                    }
                    Ok(_) => {
                        error!("Failed to restore original configuration");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        error!("Error restoring original configuration: {}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                warn!("Backup file not found at {}", backup_path.display());
                std::process::exit(1);
            }
        } else if pid2 < 0 {
            error!("Failed to create second child process");
            std::process::exit(1);
        }
        std::process::exit(0); // First child exits
    } else if pid < 0 {
        error!("Failed to create first child process");
        std::process::exit(1);
    }
    // Parent waits for first child
    let _ = unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) };
}

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
    let backup_path = config_path.with_extension("toml.bak");

    // Backup current config
    if config_path.exists() {
        let cmd = format!("sudo cp {} {}", config_path.to_str().unwrap(), backup_path.to_str().unwrap());
        debug!("Running command: {}", cmd);
        Command::new("sudo")
            .args(["cp", config_path.to_str().unwrap(), backup_path.to_str().unwrap()])
            .status()?;
        info!("Created backup of original configuration at {}", backup_path.display());
    }

    // Copy game mode config
    let cmd = format!("sudo cp {} {}", game_mode_config.to_str().unwrap(), config_path.to_str().unwrap());
    debug!("Running command: {}", cmd);
    Command::new("sudo")
        .args(["cp", game_mode_config.to_str().unwrap(), config_path.to_str().unwrap()])
        .status()?;

    // Restart greetd service
    let cmd = format!("sudo {}", config.service.restart_command);
    debug!("Running command: {}", cmd);
    Command::new("sudo")
        .args([&config.service.restart_command])
        .status()?;

    info!("Successfully switched to game mode");
    info!("Exiting game-mode process");
    std::process::exit(0);
} 
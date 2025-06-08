use anyhow::Result;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::Duration,
};

pub fn switch_to_game_mode() -> Result<()> {
    let greetd_dir = PathBuf::from("/etc/greetd");
    let config_path = greetd_dir.join("config.toml");
    let game_mode_config = greetd_dir.join("game_mode_login.toml");
    let backup_path = config_path.with_extension("toml.bak");

    // Backup current config
    if config_path.exists() {
        fs::copy(&config_path, &backup_path)?;
    }

    // Copy game mode config
    fs::copy(&game_mode_config, &config_path)?;

    // Spawn a detached process to handle the switch back
    let handle = std::thread::spawn(move || {
        // Double fork to create a truly detached process
        match unsafe { libc::fork() } {
            Ok(0) => {
                // First child
                match unsafe { libc::fork() } {
                    Ok(0) => {
                        // Second child - this is our detached process
                        // Close all file descriptors
                        for fd in 0..1024 {
                            let _ = unsafe { libc::close(fd) };
                        }
                        
                        // Detach from controlling terminal
                        let _ = unsafe { libc::setsid() };

                        std::thread::sleep(Duration::from_secs(2));
                        if backup_path.exists() {
                            let _ = fs::copy(&backup_path, &config_path);
                            let _ = fs::remove_file(&backup_path);
                        }
                        // Restart greetd service
                        let _ = Command::new("systemctl")
                            .args(["restart", "greetd"])
                            .status();
                    }
                    Ok(_) => std::process::exit(0), // First child exits
                    Err(_) => std::process::exit(1),
                }
            }
            Ok(_) => {
                // Parent waits for first child
                let _ = unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) };
            }
            Err(_) => std::process::exit(1),
        }
    });

    // Detach the thread
    handle.detach();

    Ok(())
} 
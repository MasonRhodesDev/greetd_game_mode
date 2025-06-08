mod config;
mod installer;
mod game_mode_switch;

use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Select};
use gilrs::{Button, Event, Gilrs};
use log::info;
use std::{
    env,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    time::Duration,
    process::Command,
    fs,
    path::PathBuf,
};

static RUNNING: AtomicBool = AtomicBool::new(true);

fn is_greetd_environment() -> bool {
    env::var("GREETD_SOCK").is_ok()
}

fn switch_to_game_mode() -> Result<()> {
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

fn run_game_mode() -> Result<()> {
    // Initialize logging
    env_logger::init();
    println!("Starting game mode service...");
    info!("Starting game mode service");

    // Initialize gamepad support
    let mut gilrs = Gilrs::new().map_err(|e| anyhow::anyhow!("Failed to initialize gamepad support: {}", e))?;
    
    // Print connected gamepads
    println!("\nConnected gamepads:");
    for (id, gamepad) in gilrs.gamepads() {
        println!("- {}: {}", id, gamepad.name());
        info!("Gamepad {}: {}", id, gamepad.name());
    }
    println!("\nWaiting for gamepad input...");
    println!("Press any button to see the event type. Press Ctrl+C to exit.");

    // Set up signal handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Main event loop
    while running.load(Ordering::SeqCst) {
        // Process gamepad events
        while let Some(Event { id, event, time }) = gilrs.next_event() {
            let gamepad = gilrs.gamepad(id);
            
            match event {
                gilrs::EventType::ButtonPressed(Button::Mode, _) => {
                    println!("Guide/Home button pressed on {}", gamepad.name());
                    info!("Guide/Home button pressed on {}", gamepad.name());
                    if let Err(e) = game_mode_switch::switch_to_game_mode() {
                        println!("Failed to switch to game mode: {}", e);
                        info!("Failed to switch to game mode: {}", e);
                    }
                }
                _ => {}
            }
        }

        // Small sleep to prevent high CPU usage
        std::thread::sleep(Duration::from_millis(10));
    }

    println!("\nExiting game mode service...");
    Ok(())
}

fn show_installer_menu() -> Result<()> {
    let options = vec!["Install", "Uninstall", "Test Gamepad", "Exit"];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select an option")
        .items(&options)
        .default(0)
        .interact()?;

    let mut installer = installer::Installer::new()?;

    match selection {
        0 => {
            if installer.is_installed() {
                println!("Game mode is already installed!");
                return Ok(());
            }
            installer.install()?;
        }
        1 => {
            if !installer.is_installed() {
                println!("Game mode is not installed!");
                return Ok(());
            }
            installer.uninstall()?;
        }
        2 => {
            println!("\nTesting gamepad input. Press Ctrl+C to exit.");
            run_game_mode()?;
        }
        3 => return Ok(()),
        _ => unreachable!(),
    }

    Ok(())
}

fn main() -> Result<()> {
    if is_greetd_environment() {
        run_game_mode()?;
    } else {
        show_installer_menu()?;
    }
    Ok(())
}

mod config;
mod installer;
mod game_mode_switch;

use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Select};
use gilrs::{Button, Event, Gilrs};
use tracing::{info, error, debug, warn};
use std::{
    env,
    fs,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    time::Duration,
    process::Command,
    path::PathBuf,
    io::Write,
};
use crate::config::Config;

static RUNNING: AtomicBool = AtomicBool::new(true);

fn get_lock_file_path() -> Result<PathBuf> {
    let config = Config::load()?;
    Ok(config.get_greetd_dir().join("game-mode.lock"))
}

fn write_pid_to_lock_file(pid: u32) -> Result<()> {
    let config = Config::load()?;
    let lock_file = get_lock_file_path()?;
    let mut file = fs::File::create(&lock_file)?;
    file.write_all(pid.to_string().as_bytes())?;
    
    // Ensure the lock file has the correct permissions
    if !config.is_virtual_mode() {
        let greeter_user = &config.permissions.greeter_user;
        Command::new("sudo")
            .args(["chown", &format!("{}:{}", greeter_user, greeter_user), lock_file.to_str().unwrap()])
            .status()?;
    }
    Ok(())
}

fn read_pid_from_lock_file() -> Result<Option<u32>> {
    let lock_file = get_lock_file_path()?;
    if !lock_file.exists() {
        return Ok(None);
    }
    
    let pid_str = fs::read_to_string(&lock_file)?;
    match pid_str.trim().parse::<u32>() {
        Ok(pid) => Ok(Some(pid)),
        Err(_) => Ok(None)
    }
}

fn is_process_running(pid: u32) -> bool {
    Command::new("sudo")
        .args(["ps", "-p", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn kill_existing_processes() -> Result<()> {
    // Get the current process ID
    let current_pid = std::process::id();
    info!("Current process ID: {}", current_pid);
    
    // Check if there's a lock file with a running process
    if let Some(existing_pid) = read_pid_from_lock_file()? {
        if existing_pid != current_pid && is_process_running(existing_pid) {
            info!("Found existing game-mode process with PID {}", existing_pid);
            info!("Terminating existing process to take over as singleton");
            
            let status = Command::new("sudo")
                .args(["kill", "-9", &existing_pid.to_string()])
                .status();
            
            match status {
                Ok(status) if status.success() => {
                    info!("Successfully terminated existing process {}", existing_pid);
                }
                Ok(_) => {
                    warn!("Failed to terminate process {} (process may have already terminated)", existing_pid);
                }
                Err(e) => {
                    warn!("Error terminating process {}: {}", existing_pid, e);
                }
            }
        }
    }
    
    // Write our PID to the lock file
    write_pid_to_lock_file(current_pid)?;
    info!("Registered as the active game-mode instance (PID: {})", current_pid);
    Ok(())
}

fn is_greetd_environment() -> bool {
    env::args().any(|arg| arg == "--greetd")
}

fn is_install() -> bool {
    env::args().any(|arg| arg == "--install")
}

fn setup_logging() -> Result<()> {
    let config = crate::config::Config::load()?;
    
    // Create a subscriber that always logs to stdout
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| {
            if config.game_mode.debug {
                "game_mode=debug".to_string()
            } else {
                "game_mode=info".to_string()
            }
        }))
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_names(true)
        .with_ansi(true)
        .with_level(true)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    // Try to set up file logging, but don't fail if we can't
    let log_dir = config.get_greetd_dir().join("logs");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        println!("Could not create log directory: {}", e);
    } else if !config.is_virtual_mode() {
        // Try to set permissions, but don't fail if we can't
        let greeter_user = &config.permissions.greeter_user;
        if let Err(e) = std::process::Command::new("chown")
            .args(["-R", &format!("{}:{}", greeter_user, greeter_user), log_dir.to_str().unwrap()])
            .status() 
        {
            println!("Could not set log directory permissions: {}", e);
        }
    }

    Ok(())
}

fn run_game_mode() -> Result<()> {
    // Logging is already initialized in main()
    info!("Starting game mode service");
    debug!("Running in greetd environment: {}", is_greetd_environment());

    // Initialize gamepad support
    let mut gilrs = Gilrs::new().map_err(|e| {
        error!("Failed to initialize gamepad support: {}", e);
        anyhow::anyhow!("Failed to initialize gamepad support: {}", e)
    })?;
    
    // Print connected gamepads
    info!("Connected gamepads:");
    for (id, gamepad) in gilrs.gamepads() {
        info!("- {}: {}", id, gamepad.name());
        debug!("Gamepad {} connected: {}", id, gamepad.name());
    }
    info!("Waiting for gamepad input...");

    // Set up signal handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        info!("Received shutdown signal");
        r.store(false, Ordering::SeqCst);
    })?;

    // Track if menu button has been pressed
    let menu_pressed = Arc::new(AtomicBool::new(false));
    let menu_pressed_clone = menu_pressed.clone();

    // Main event loop
    while running.load(Ordering::SeqCst) {
        // Process gamepad events
        while let Some(Event { id, event, .. }) = gilrs.next_event() {
            let gamepad = gilrs.gamepad(id);
            debug!("Received event from {}: {:?}", gamepad.name(), event);
            
            match event {
                gilrs::EventType::ButtonPressed(button, value) => {
                    info!("Button pressed on {}: {:?} (value: {})", gamepad.name(), button, value);
                    if button == Button::Mode && !menu_pressed.load(Ordering::SeqCst) {
                        info!("Guide/Home button pressed on {}", gamepad.name());
                        menu_pressed.store(true, Ordering::SeqCst);
                        if let Err(e) = game_mode_switch::switch_to_game_mode() {
                            error!("Failed to switch to game mode: {}", e);
                        }
                    }
                }
                gilrs::EventType::ButtonReleased(button, value) => {
                    info!("Button released on {}: {:?} (value: {})", gamepad.name(), button, value);
                }
                gilrs::EventType::AxisChanged(axis, value, _) => {
                    debug!("Axis changed on {}: {:?} (value: {})", gamepad.name(), axis, value);
                }
                _ => {}
            }
        }

        // Small sleep to prevent high CPU usage
        std::thread::sleep(Duration::from_millis(10));
    }

    info!("Exiting game mode service");
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
    // Initialize logging first thing
    setup_logging()?;
    info!("Game mode service starting");
    debug!("Environment check: greetd mode: {}", is_greetd_environment());
    
    // Kill any existing game-mode processes
    if let Err(e) = kill_existing_processes() {
        error!("Failed to kill existing processes: {}", e);
    }
    
    if is_install() {
        info!("Running install");
        let mut installer = installer::Installer::new()?;
        // Always uninstall first to ensure clean state
        if installer.is_installed() {
            installer.uninstall()?;
        }
        installer.install()?;
        return Ok(());
    }
    
    if is_greetd_environment() {
        info!("Running in greetd environment");
        run_game_mode()?;
    } else {
        info!("Running in installer mode");
        show_installer_menu()?;
    }
    info!("Game mode service exiting");
    Ok(())
}


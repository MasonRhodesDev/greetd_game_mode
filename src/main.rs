mod config;
mod game_mode_switch;
mod paths;

use anyhow::Result;
use gilrs::{Button, Event, Gilrs};
use tracing::{info, error, debug};
use std::{
    env,
    fs,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    time::Duration,
    process::Command,
};
use crate::config::Config;

fn setup_logging() -> Result<()> {
    let config = match crate::config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config: {}", e);
            return Err(e.into());
        }
    };
    
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

    if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("Failed to set global subscriber: {}", e);
        return Err(e.into());
    }

    // Try to set up file logging, but don't fail if we can't
    let log_dir = config.get_greetd_dir().join("logs");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("Could not create log directory: {}", e);
    } else if !config.is_virtual_mode() {
        // Try to set permissions, but don't fail if we can't
        let greeter_user = &config.permissions.greeter_user;
        if let Err(e) = std::process::Command::new("chown")
            .args(["-R", &format!("{}:{}", greeter_user, greeter_user), log_dir.to_str().unwrap()])
            .status() 
        {
            eprintln!("Could not set log directory permissions: {}", e);
        }
    }

    Ok(())
}

fn is_user_logged_in_on_tty(tty: &str) -> Result<bool> {
    let output = Command::new("who")
        .output()?;
    let who_output = String::from_utf8_lossy(&output.stdout);
    debug!("who command output: {}", who_output);
    debug!("Checking for users on TTY: {}", tty);
    
    let result = who_output.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        debug!("Checking line: {:?}", parts);
        // Only check if this line has a TTY matching our target and is not greetd
        let matches = parts.len() >= 2 && parts[1] == tty && !line.contains("greetd");
        debug!("Line matches: {}", matches);
        matches
    });
    debug!("Final result for TTY {}: {}", tty, result);
    Ok(result)
}

fn run_game_mode() -> Result<()> {
    // Logging is already initialized in main()
    info!("Starting game mode service");

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
    let _menu_pressed_clone = menu_pressed.clone();

    // Get greetd TTY
    let config = Config::load()?;
    let greetd_tty = format!("tty{}", config.terminal.vt);
    info!("Greetd running on {}", greetd_tty);

    // Main event loop
    while running.load(Ordering::SeqCst) {
        // Process gamepad events
        while let Some(Event { id, event, time }) = gilrs.next_event() {
            debug!("Gamepad event: {:?}", event);
            if is_user_logged_in_on_tty(&greetd_tty)? {
                debug!("User logged in on greetd TTY {}, ignoring gamepad events", greetd_tty);
                std::thread::sleep(Duration::from_millis(1000));
                continue;
            }
            match event {
                gilrs::EventType::ButtonPressed(Button::Mode, _) => {
                    if !menu_pressed.load(Ordering::SeqCst) {
                        menu_pressed.store(true, Ordering::SeqCst);
                        info!("Menu button pressed");
                    }
                }
                gilrs::EventType::ButtonReleased(Button::Mode, _) => {
                    menu_pressed.store(false, Ordering::SeqCst);
                    game_mode_switch::switch_to_game_mode()?;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

fn in_game_mode() -> Result<bool> {
    let config = crate::config::Config::load()?;
    let config_path = config.get_config_path();
    let game_mode_path = config.get_game_mode_config_path();

    // Check if config_path is a symlink
    if !config_path.is_symlink() {
        return Ok(false);
    }

    // Get the target of the symlink
    let target = fs::read_link(config_path)?;
    Ok(target == game_mode_path)
}

fn main() -> Result<()> {
    // Initialize logging first thing
    if let Err(e) = setup_logging() {
        eprintln!("Failed to setup logging: {}", e);
        return Err(e.into());
    }
    info!("Game mode service starting");

    // Always reset to desktop mode on startup
    if let Err(e) = game_mode_switch::switch_to_desktop_mode() {
        eprintln!("Failed to reset to desktop mode: {}", e);
        return Err(e.into());
    }

    if let Err(e) = run_game_mode() {
        eprintln!("Failed to run game mode: {}", e);
        return Err(e.into());
    }

    info!("Game mode service exiting");
    Ok(())
}


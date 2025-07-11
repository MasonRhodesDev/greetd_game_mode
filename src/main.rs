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
use serde_json::Value;

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
    let output = Command::new("loginctl")
        .arg("-j")
        .arg("list-sessions")
        .output()?;
    let sessions: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    debug!("loginctl sessions: {}", serde_json::to_string_pretty(&sessions)?);
    
    // Check if there are any non-greeter sessions on the specified TTY
    let result = sessions.iter().any(|session| {
        let user = session["user"].as_str().unwrap_or("");
        let session_tty = session["tty"].as_str().unwrap_or("");
        let is_non_greeter = user != "greeter";
        let is_on_target_tty = session_tty == tty;
        debug!("Session user: {}, tty: {}, is_non_greeter: {}, is_on_target_tty: {}", 
            user, session_tty, is_non_greeter, is_on_target_tty);
        is_non_greeter && is_on_target_tty
    });
    debug!("User logged in on TTY {}: {}", tty, result);
    Ok(result)
}

fn is_greeter_active() -> Result<bool> {
    let output = Command::new("loginctl")
        .arg("-j")
        .arg("list-sessions")
        .output()?;
    let sessions: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    debug!("loginctl sessions: {}", serde_json::to_string_pretty(&sessions)?);
    
    // Check if greeter session exists and is on the correct TTY
    let result = sessions.iter().any(|session| {
        let user = session["user"].as_str().unwrap_or("");
        let tty = session["tty"].as_str().unwrap_or("");
        let is_greeter = user == "greeter";
        let has_tty = !tty.is_empty() && tty != "-";
        debug!("Session user: {}, tty: {}, is_greeter: {}, has_tty: {}", 
            user, tty, is_greeter, has_tty);
        is_greeter && has_tty
    });
    debug!("Greeter active: {}", result);
    Ok(result)
}

fn get_active_tty() -> Result<String> {
    let output = Command::new("sudo")
        .arg("fgconsole")
        .output()?;
    
    if !output.status.success() {
        return Err(anyhow::anyhow!("fgconsole failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    
    let tty = String::from_utf8_lossy(&output.stdout).trim().to_string();
    debug!("Active TTY number: {}", tty);
    Ok(tty)
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
    let greetd_vt = config.terminal.vt.to_string();
    info!("Greetd running on {}", greetd_tty);

    // Main event loop
    while running.load(Ordering::SeqCst) {
        // Process gamepad events
        while let Some(Event { id, event, time }) = gilrs.next_event() {
            debug!("Gamepad event: {:?}", event);
            
            // Check if greetd TTY is active
            match get_active_tty() {
                Ok(active_tty) => {
                    if active_tty != greetd_vt {
                        debug!("Greetd VT {} is not active (active: {}), ignoring gamepad events", greetd_vt, active_tty);
                        std::thread::sleep(Duration::from_millis(1000));
                        continue;
                    }
                }
                Err(e) => {
                    error!("Failed to get active TTY: {}", e);
                    std::thread::sleep(Duration::from_millis(1000));
                    continue;
                }
            }

            // Check if we're in the greeter session
            if !is_greeter_active()? {
                debug!("Greeter is not active, ignoring gamepad events");
                std::thread::sleep(Duration::from_millis(1000));
                continue;
            }

            // Check if any non-greeter user is logged in
            if is_user_logged_in_on_tty(&greetd_tty)? {
                debug!("Non-greeter user logged in, ignoring gamepad events");
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


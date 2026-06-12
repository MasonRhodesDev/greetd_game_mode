mod approval;
mod config;
mod game_mode_switch;
mod paths;

use anyhow::Result;
use gilrs::{Button, Event, Gilrs};
use tracing::{info, error, debug, warn};
use std::{
    env,
    fs,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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

/// Block until greetd has started any session on the target VT, i.e. it has
/// read config.toml and acted on it. Used on startup before resetting the
/// config symlink.
fn wait_for_session_on_tty(tty: &str, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let found = Command::new("loginctl")
            .arg("-j")
            .arg("list-sessions")
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<Vec<Value>>(&o.stdout).ok())
            .map(|sessions: Vec<Value>| {
                sessions
                    .iter()
                    .any(|s| s["tty"].as_str().unwrap_or("") == tty)
            })
            .unwrap_or(false);
        if found {
            debug!("Session present on {}; greetd has consumed its config", tty);
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    warn!("No session appeared on {} within {:?}; resetting config anyway", tty, timeout);
}

/// State of a logind session ("active", "online", "closing", ...). Sessions in
/// "closing" linger after logout while background processes of the user are
/// still alive (logind KillUserProcesses=no) and must not count as logged in.
fn session_state(session_id: &str) -> String {
    Command::new("loginctl")
        .args(["show-session", session_id, "--property", "State", "--value"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn is_user_logged_in_on_tty(tty: &str) -> Result<bool> {
    let output = Command::new("loginctl")
        .arg("-j")
        .arg("list-sessions")
        .output()?;
    let sessions: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    debug!("loginctl sessions: {}", serde_json::to_string_pretty(&sessions)?);

    // Check if there are any live non-greeter login sessions on the specified
    // TTY. Service-class sessions ("manager", "background", ...) and sessions
    // stuck in "closing" after logout don't count.
    let result = sessions.iter().any(|session| {
        let user = session["user"].as_str().unwrap_or("");
        let session_tty = session["tty"].as_str().unwrap_or("");
        let class = session["class"].as_str().unwrap_or("");
        let id = session["session"].as_str().unwrap_or("");
        let is_non_greeter = user != "greeter";
        let is_on_target_tty = session_tty == tty;
        let is_login_class = class.starts_with("user");
        let candidate = is_non_greeter && is_on_target_tty && is_login_class;
        let is_live = candidate && session_state(id) != "closing";
        debug!("Session {} user: {}, tty: {}, class: {}, is_non_greeter: {}, is_on_target_tty: {}, is_live: {}",
            id, user, session_tty, class, is_non_greeter, is_on_target_tty, is_live);
        is_live
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

// Cached active-TTY lookup: called for every gamepad event, so don't hit the
// filesystem more than once a second.
static ACTIVE_TTY_CACHE: Mutex<Option<(Instant, String)>> = Mutex::new(None);

fn get_active_tty() -> Result<String> {
    let mut cache = ACTIVE_TTY_CACHE.lock().unwrap();
    if let Some((at, tty)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(1) {
            return Ok(tty.clone());
        }
    }
    // /sys/class/tty/tty0/active is world-readable and always current — no
    // sudo fgconsole subprocess (which could block the event loop) needed.
    let raw = fs::read_to_string("/sys/class/tty/tty0/active")?;
    let tty = raw.trim().trim_start_matches("tty").to_string();
    debug!("Active TTY number: {}", tty);
    *cache = Some((Instant::now(), tty.clone()));
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
                    // Gate entry on a phone passkey approval (fail-closed).
                    if approval::require_approval() {
                        game_mode_switch::switch_to_game_mode()?;
                    } else {
                        info!("game-mode entry not approved; staying at greeter");
                    }
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

    // Always reset to desktop mode on startup — but not before greetd has
    // consumed config.toml. This service restarts together with greetd
    // (BindsTo), so resetting immediately races greetd's config read and
    // turns an approved game-mode entry back into a plain greeter.
    let config = Config::load()?;
    wait_for_session_on_tty(&format!("tty{}", config.terminal.vt), Duration::from_secs(30));
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


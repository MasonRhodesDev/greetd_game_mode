//! Greeter-state checks shared by every game-mode entry trigger (the Guide
//! button in the daemon, the Wake-on-LAN listener): is greetd's VT in front,
//! is the greeter session up, and is nobody else logged in on that VT.

use anyhow::Result;
use serde_json::Value;
use std::{
    fs,
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};
use tracing::debug;

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

pub fn is_user_logged_in_on_tty(tty: &str) -> Result<bool> {
    let output = Command::new("loginctl")
        .arg("-j")
        .arg("list-sessions")
        .output()?;
    let sessions: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    debug!(
        "loginctl sessions: {}",
        serde_json::to_string_pretty(&sessions)?
    );

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

pub fn is_greeter_active() -> Result<bool> {
    let output = Command::new("loginctl")
        .arg("-j")
        .arg("list-sessions")
        .output()?;
    let sessions: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    debug!(
        "loginctl sessions: {}",
        serde_json::to_string_pretty(&sessions)?
    );

    // Check if greeter session exists and is on the correct TTY
    let result = sessions.iter().any(|session| {
        let user = session["user"].as_str().unwrap_or("");
        let tty = session["tty"].as_str().unwrap_or("");
        let is_greeter = user == "greeter";
        let has_tty = !tty.is_empty() && tty != "-";
        debug!(
            "Session user: {}, tty: {}, is_greeter: {}, has_tty: {}",
            user, tty, is_greeter, has_tty
        );
        is_greeter && has_tty
    });
    debug!("Greeter active: {}", result);
    Ok(result)
}

// Cached active-TTY lookup: called for every gamepad event, so don't hit the
// filesystem more than once a second.
static ACTIVE_TTY_CACHE: Mutex<Option<(Instant, String)>> = Mutex::new(None);

pub fn get_active_tty() -> Result<String> {
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

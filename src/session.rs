use crate::config::{Config, GREETER_USER};
use anyhow::Result;
use serde_json::Value;
use std::{fs, process::Command};
use tracing::debug;

fn session_state(session_id: &str) -> String {
    Command::new("loginctl")
        .args(["show-session", session_id, "--property", "State", "--value"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn sessions() -> Result<Vec<Value>> {
    let output = Command::new("loginctl")
        .args(["-j", "list-sessions"])
        .output()?;
    let sessions: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    debug!(
        "loginctl sessions: {}",
        serde_json::to_string_pretty(&sessions)?
    );
    Ok(sessions)
}

fn field<'a>(session: &'a Value, key: &str) -> &'a str {
    session[key].as_str().unwrap_or("")
}

fn is_greeter_active(sessions: &[Value]) -> bool {
    sessions.iter().any(|session| {
        let tty = field(session, "tty");
        field(session, "user") == GREETER_USER && !tty.is_empty() && tty != "-"
    })
}

fn is_user_logged_in_on_tty(sessions: &[Value], tty: &str) -> bool {
    sessions.iter().any(|session| {
        field(session, "user") != GREETER_USER
            && field(session, "tty") == tty
            && field(session, "class").starts_with("user")
            && session_state(field(session, "session")) != "closing"
    })
}

fn active_vt() -> Result<String> {
    let raw = fs::read_to_string("/sys/class/tty/tty0/active")?;
    Ok(raw.trim().trim_start_matches("tty").to_string())
}

pub fn at_idle_greeter(config: &Config) -> Result<bool> {
    let vt = config.terminal.vt.to_string();
    let active = active_vt()?;
    if active != vt {
        debug!("greetd VT {vt} is not active (active: {active})");
        return Ok(false);
    }
    let sessions = sessions()?;
    if !is_greeter_active(&sessions) {
        debug!("greeter is not active");
        return Ok(false);
    }
    if is_user_logged_in_on_tty(&sessions, &format!("tty{vt}")) {
        debug!("non-greeter user logged in on tty{vt}");
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn greeter_needs_a_tty() {
        let on_tty = vec![json!({"user": GREETER_USER, "tty": "tty1"})];
        let manager = vec![json!({"user": GREETER_USER, "tty": "-"})];
        assert!(is_greeter_active(&on_tty));
        assert!(!is_greeter_active(&manager));
        assert!(!is_greeter_active(&[
            json!({"user": "alice", "tty": "tty1"})
        ]));
    }

    #[test]
    fn only_user_class_sessions_on_the_tty_count() {
        let sessions = vec![
            json!({"user": GREETER_USER, "tty": "tty1", "class": "greeter"}),
            json!({"user": "alice", "tty": "", "class": "manager"}),
            json!({"user": "alice", "tty": "tty2", "class": "user"}),
        ];
        assert!(!is_user_logged_in_on_tty(&sessions, "tty1"));
    }
}

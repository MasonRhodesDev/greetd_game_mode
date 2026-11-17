use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::Value;
use tracing::{info, warn};

const ENV_FILE: &str = "/etc/game-mode/approval.env";
const DEFAULT_SOCKET: &str = "/run/access-gate/ctrl.sock";

struct Cfg {
    socket: String,
    timeout_secs: u64,
    disabled: bool,
}

fn load_cfg() -> Cfg {
    let mut file_vars: HashMap<String, String> = HashMap::new();
    if let Ok(text) = fs::read_to_string(ENV_FILE) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                file_vars.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    let get = |key: &str| {
        std::env::var(key)
            .ok()
            .or_else(|| file_vars.get(key).cloned())
    };
    Cfg {
        socket: get("AG_CTRL_SOCKET").unwrap_or_else(|| DEFAULT_SOCKET.into()),
        timeout_secs: get("AG_TIMEOUT").and_then(|v| v.parse().ok()).unwrap_or(90),
        disabled: get("AG_DISABLED")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false),
    }
}

fn notify(msg: &str) {
    info!("greeter: {msg}");
    if let Ok(config) = crate::config::Config::load() {
        let _ = std::fs::write(&config.greeter.banner_file, msg);
    }
}

fn read_json_line(reader: &mut BufReader<UnixStream>) -> Option<Value> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

pub struct Prompt<'a> {
    pub title: &'a str,
    pub path: &'a str,
    pub retry_hint: &'a str,
}

pub const GUIDE_PROMPT: Prompt<'static> = Prompt {
    title: "Enter game mode?",
    path: "switch this PC into Steam game mode",
    retry_hint: "press the Guide button to retry",
};

pub fn require_approval() -> bool {
    require_approval_for(&GUIDE_PROMPT)
}

pub fn require_approval_for(prompt: &Prompt) -> bool {
    let cfg = load_cfg();

    if cfg.disabled {
        warn!("approval gate DISABLED (AG_DISABLED) — entering game mode without phone approval");
        return true;
    }

    info!("requesting phone approval to enter game mode...");
    let Ok(mut stream) = UnixStream::connect(&cfg.socket) else {
        warn!(
            "verifier socket {} unreachable; refusing game-mode entry",
            cfg.socket
        );
        notify("Game mode: approval service unreachable");
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(cfg.timeout_secs + 10)));

    let request = serde_json::json!({
        "exe": "game-mode",
        "path": prompt.path,
        "group": "login",
        "title": prompt.title,
        "timeout_secs": cfg.timeout_secs,
    });
    if stream
        .write_all(format!("{request}\n").as_bytes())
        .and_then(|()| stream.flush())
        .is_err()
    {
        warn!("failed to send approval request; refusing game-mode entry");
        notify("Game mode: approval service unreachable");
        return false;
    }

    let mut reader = BufReader::new(stream);

    let Some(ack) = read_json_line(&mut reader) else {
        warn!("no ack from verifier; refusing game-mode entry");
        notify("Game mode: approval service unreachable");
        return false;
    };
    info!(
        "approval request {} created; awaiting the phone",
        ack["id"].as_str().unwrap_or("?")
    );
    notify("Approval sent to your phone — confirm with fingerprint");

    let Some(decision) = read_json_line(&mut reader) else {
        warn!("no decision from verifier; refusing game-mode entry");
        notify("Game mode: approval service unreachable");
        return false;
    };
    match decision["status"].as_str().unwrap_or("") {
        "approved" => {
            info!("game-mode entry approved");
            notify("Approved — entering game mode");
            true
        }
        "denied" => {
            warn!("game-mode entry denied");
            notify("Game mode entry denied");
            false
        }
        "timeout" | "expired" | "unknown" => {
            warn!("approval timed out");
            notify(&format!("Approval timed out — {}", prompt.retry_hint));
            false
        }
        other => {
            warn!("unexpected approval status {other:?}");
            notify("Game mode: unexpected approval response");
            false
        }
    }
}

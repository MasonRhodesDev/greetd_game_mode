//! Phone passkey approval gate for game-mode entry.
//!
//! Talks directly to the access-gate verifier's localhost control plane:
//! create a request (the verifier Web-Pushes the phone), show progress
//! banners on the greeter via hyprctl, and poll for the decision.
//! Fail-closed: every error path keeps us at the greeter.

use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{info, warn};

const ENV_FILE: &str = "/etc/game-mode/approval.env";

struct Cfg {
    ctrl: String,
    timeout_secs: u64,
}

/// AG_* settings: process environment wins (systemd EnvironmentFile), with
/// the env file as fallback so a manual run behaves identically.
fn load_cfg() -> Option<Cfg> {
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
    let get = |key: &str| std::env::var(key).ok().or_else(|| file_vars.get(key).cloned());

    let Some(ctrl) = get("AG_VERIFIER_CTRL") else {
        warn!("AG_VERIFIER_CTRL not set (environment or {ENV_FILE})");
        return None;
    };
    let timeout_secs = get("AG_TIMEOUT").and_then(|v| v.parse().ok()).unwrap_or(90);
    Some(Cfg { ctrl, timeout_secs })
}

/// Best-effort on-screen banner on the greeter's Hyprland instance.
/// Icons: 0 warning, 1 info, 3 error, 5 ok.
fn notify(icon: u8, ms: u64, msg: &str) {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    let Ok(entries) = fs::read_dir(format!("{runtime_dir}/hypr")) else {
        return;
    };
    // newest instance dir = the running compositor
    let newest = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    let Some(sig) = newest.map(|e| e.file_name()) else {
        return;
    };
    let _ = Command::new("hyprctl")
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HYPRLAND_INSTANCE_SIGNATURE", &sig)
        .args(["notify", &icon.to_string(), &ms.to_string(), "0", msg])
        .output();
}

fn poll_status(agent: &ureq::Agent, ctrl: &str, id: &str) -> Option<String> {
    let resp = agent.get(&format!("{ctrl}/result/{id}")).call().ok()?;
    let v: Value = resp.into_json().ok()?;
    v["status"].as_str().map(str::to_string)
}

/// Block on a phone passkey approval before entering game mode. Returns true
/// only on an approved decision; deny/timeout/verifier-down all return false
/// (fail-closed: stay at the greeter).
pub fn require_approval() -> bool {
    info!("requesting phone approval to enter game mode...");
    let Some(cfg) = load_cfg() else {
        notify(3, 8000, "Game mode: approval service not configured");
        return false;
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();

    let id = agent
        .post(&format!("{}/request", cfg.ctrl))
        .send_json(serde_json::json!({
            "exe": "game-mode",
            "path": "switch this PC into Steam game mode",
            "group": "login",
            "title": "Enter game mode?",
        }))
        .ok()
        .and_then(|r| r.into_json::<Value>().ok())
        .and_then(|v| v["id"].as_str().map(str::to_string))
        .filter(|id| !id.is_empty());
    let Some(id) = id else {
        warn!("verifier unreachable; refusing game-mode entry");
        notify(3, 8000, "Game mode: approval service unreachable");
        return false;
    };
    info!("approval request {id} created; awaiting the phone");
    notify(
        1,
        cfg.timeout_secs * 1000,
        "Approval sent to your phone — confirm with fingerprint",
    );

    let deadline = Instant::now() + Duration::from_secs(cfg.timeout_secs);
    while Instant::now() < deadline {
        let Some(status) = poll_status(&agent, &cfg.ctrl, &id) else {
            warn!("verifier unreachable while polling; refusing game-mode entry");
            notify(3, 8000, "Game mode: approval service unreachable");
            return false;
        };
        match status.as_str() {
            "approved" => {
                info!("game-mode entry approved");
                notify(5, 3000, "Approved — entering game mode");
                return true;
            }
            "denied" => {
                warn!("game-mode entry denied");
                notify(3, 6000, "Game mode entry denied");
                return false;
            }
            "expired" | "unknown" => {
                warn!("approval request expired");
                notify(0, 6000, "Approval expired — press the Guide button to retry");
                return false;
            }
            _ => thread::sleep(Duration::from_secs(1)),
        }
    }
    warn!("approval timed out");
    notify(0, 6000, "Approval timed out — press the Guide button to retry");
    false
}

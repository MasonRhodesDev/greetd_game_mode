//! Phone passkey approval gate for game-mode entry.
//!
//! Talks to the access-gate verifier over its unix control socket with
//! blocking request/response semantics: write one request line, the verifier
//! answers `{"id":..}` immediately (the phone has been pushed) and
//! `{"status":..}` once the phone decides — no polling, no TCP. Progress is
//! shown on the greeter via hyprctl banners. Fail-closed: every error path
//! keeps us at the greeter.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use tracing::{info, warn};

const ENV_FILE: &str = "/etc/game-mode/approval.env";
const DEFAULT_SOCKET: &str = "/run/access-gate/ctrl.sock";

struct Cfg {
    socket: String,
    timeout_secs: u64,
}

/// AG_* settings: process environment wins (systemd EnvironmentFile), with
/// the env file as fallback so a manual run behaves identically.
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
    let get = |key: &str| std::env::var(key).ok().or_else(|| file_vars.get(key).cloned());
    Cfg {
        socket: get("AG_CTRL_SOCKET").unwrap_or_else(|| DEFAULT_SOCKET.into()),
        timeout_secs: get("AG_TIMEOUT").and_then(|v| v.parse().ok()).unwrap_or(90),
    }
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

fn read_json_line(reader: &mut BufReader<UnixStream>) -> Option<Value> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

/// Block on a phone passkey approval before entering game mode. Returns true
/// only on an approved decision; deny/timeout/verifier-down all return false
/// (fail-closed: stay at the greeter).
pub fn require_approval() -> bool {
    info!("requesting phone approval to enter game mode...");
    let cfg = load_cfg();

    let Ok(mut stream) = UnixStream::connect(&cfg.socket) else {
        warn!("verifier socket {} unreachable; refusing game-mode entry", cfg.socket);
        notify(3, 8000, "Game mode: approval service unreachable");
        return false;
    };
    // The verifier answers the final status itself after at most timeout_secs;
    // pad the read timeout so we always get its answer rather than racing it.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(cfg.timeout_secs + 10)));

    let request = serde_json::json!({
        "exe": "game-mode",
        "path": "switch this PC into Steam game mode",
        "group": "login",
        "title": "Enter game mode?",
        "timeout_secs": cfg.timeout_secs,
    });
    if stream
        .write_all(format!("{request}\n").as_bytes())
        .and_then(|()| stream.flush())
        .is_err()
    {
        warn!("failed to send approval request; refusing game-mode entry");
        notify(3, 8000, "Game mode: approval service unreachable");
        return false;
    }

    let mut reader = BufReader::new(stream);

    let Some(ack) = read_json_line(&mut reader) else {
        warn!("no ack from verifier; refusing game-mode entry");
        notify(3, 8000, "Game mode: approval service unreachable");
        return false;
    };
    info!(
        "approval request {} created; awaiting the phone",
        ack["id"].as_str().unwrap_or("?")
    );
    notify(
        1,
        cfg.timeout_secs * 1000,
        "Approval sent to your phone — confirm with fingerprint",
    );

    let Some(decision) = read_json_line(&mut reader) else {
        warn!("no decision from verifier; refusing game-mode entry");
        notify(3, 8000, "Game mode: approval service unreachable");
        return false;
    };
    match decision["status"].as_str().unwrap_or("") {
        "approved" => {
            info!("game-mode entry approved");
            notify(5, 3000, "Approved — entering game mode");
            true
        }
        "denied" => {
            warn!("game-mode entry denied");
            notify(3, 6000, "Game mode entry denied");
            false
        }
        "timeout" | "expired" | "unknown" => {
            warn!("approval timed out");
            notify(0, 6000, "Approval timed out — press the Guide button to retry");
            false
        }
        other => {
            warn!("unexpected approval status {other:?}");
            notify(3, 6000, "Game mode: unexpected approval response");
            false
        }
    }
}

use std::process::Command;
use tracing::{info, warn};

const HELPER: &str = "/usr/local/bin/game-mode-approve";

/// Block on a phone passkey approval before entering game mode. Runs the
/// approval helper (request -> ntfy push -> poll); its stdout/stderr inherit to
/// the journal. Returns true only on an approved decision; deny/timeout/offline
/// /verifier-down all return false (fail-closed: stay at the greeter).
pub fn require_approval() -> bool {
    info!("requesting phone approval to enter game mode...");
    match Command::new(HELPER).status() {
        Ok(s) if s.success() => {
            info!("game-mode entry approved");
            true
        }
        Ok(s) => {
            warn!("game-mode entry not approved (helper exit {:?})", s.code());
            false
        }
        Err(e) => {
            warn!("approval helper failed to run ({e}); refusing game-mode entry");
            false
        }
    }
}

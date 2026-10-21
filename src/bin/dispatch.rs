//! game-mode-dispatch: greetd's permanent `[initial_session]` command.
//!
//! greetd runs this (as the games user) once per start with no
//! `/run/greetd.run` present — every boot, and every restart the game-mode
//! daemon performs for an approved entry. Behaviour:
//!
//!   - armed flag fresh (the daemon just wrote it): consume the arm and exec
//!     the configured game-session argv. This *is* the game session; when it
//!     exits, greetd falls through to `[default_session]` — the greeter.
//!   - otherwise (normal boot, stale flag): exit 0 immediately, and greetd
//!     falls through to the greeter.
//!
//! Keeping this decision in a tiny exec-or-exit binary is what lets
//! /etc/greetd/config.toml be one static file forever (issue vigil#29): mode
//! logic lives here and in the daemon, never in greetd's config.

use game_mode::config::{Config, ARMED_FILE, ARMED_MAX_AGE_SECS};
use game_mode::game_mode_switch::is_armed;
use game_mode::output;
use std::os::unix::process::CommandExt;
use std::path::Path;

/// greetd's runfile (its default `[general] runfile`). greetd runs
/// `[initial_session]` only while this is absent.
const GREETD_RUNFILE: &str = "/run/greetd.run";

/// `game-mode-dispatch --pre-greetd`, run as root from greetd.service's
/// `ExecStartPre`. On a plain boot nothing is armed, so the initial session
/// would only exec-and-exit; but greetd then holds the greeter back for at
/// least a second after any session shorter than that (hard-coded in its
/// `check_children`), and pays a full PAM/logind session open and close for
/// the games user. Pre-creating the runfile makes greetd start the greeter
/// directly. When armed (the daemon's restart for an approved entry) the
/// runfile is left absent so the initial session runs as before.
fn pre_greetd() {
    if is_armed(Path::new(ARMED_FILE), ARMED_MAX_AGE_SECS) {
        return;
    }
    if let Err(e) = std::fs::File::create(GREETD_RUNFILE) {
        // Harmless: greetd falls back to running this dispatcher normally.
        eprintln!("game-mode-dispatch: cannot create {GREETD_RUNFILE}: {e}");
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--pre-greetd") {
        pre_greetd();
        return;
    }
    if !is_armed(Path::new(ARMED_FILE), ARMED_MAX_AGE_SECS) {
        // Normal boot: nothing to do, greetd falls through to the greeter.
        return;
    }
    // Consume the arm (best-effort — it self-expires anyway; /run/game-mode
    // is group-writable for exactly this).
    let _ = std::fs::remove_file(ARMED_FILE);

    let command = match Config::load() {
        Ok(config) => config.session.command,
        Err(e) => {
            // A typo'd config must not brick the login path: land on the
            // greeter and say why on greetd's log.
            eprintln!("game-mode-dispatch: config error, not launching: {e}");
            return;
        }
    };
    // Point gamescope at the best display unless the config already does.
    let command = match output::preferred_outputs() {
        Some(outputs) => output::with_preferred_outputs(&command, &outputs),
        None => command,
    };
    let Some((program, args)) = command.split_first() else {
        eprintln!("game-mode-dispatch: empty [session] command, not launching");
        return;
    };
    eprintln!("game-mode-dispatch: armed; exec {command:?}");
    // exec only returns on failure; falling out of main lands on the greeter.
    let err = std::process::Command::new(program).args(args).exec();
    eprintln!("game-mode-dispatch: exec {program}: {err}");
}

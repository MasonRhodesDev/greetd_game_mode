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

fn main() {
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

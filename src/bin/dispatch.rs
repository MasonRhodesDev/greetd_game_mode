use game_mode::config::{Config, ARMED_FILE, ARMED_MAX_AGE_SECS};
use game_mode::game_mode_switch::is_armed;
use game_mode::output;
use std::os::unix::process::CommandExt;
use std::path::Path;

const GREETD_RUNFILE: &str = "/run/greetd.run";

fn pre_greetd() {
    if is_armed(Path::new(ARMED_FILE), ARMED_MAX_AGE_SECS) {
        return;
    }
    if let Err(e) = std::fs::File::create(GREETD_RUNFILE) {
        eprintln!("game-mode-dispatch: cannot create {GREETD_RUNFILE}: {e}");
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--pre-greetd") {
        pre_greetd();
        return;
    }
    if !is_armed(Path::new(ARMED_FILE), ARMED_MAX_AGE_SECS) {
        return;
    }
    let _ = std::fs::remove_file(ARMED_FILE);

    let command = match Config::load() {
        Ok(config) => config.session.command,
        Err(e) => {
            eprintln!("game-mode-dispatch: config error, not launching: {e}");
            return;
        }
    };
    let command = match output::preferred_outputs() {
        Some(outputs) => output::with_preferred_outputs(&command, &outputs),
        None => command,
    };
    let Some((program, args)) = command.split_first() else {
        eprintln!("game-mode-dispatch: empty [session] command, not launching");
        return;
    };
    eprintln!("game-mode-dispatch: armed; exec {command:?}");
    let err = std::process::Command::new(program).args(args).exec();
    eprintln!("game-mode-dispatch: exec {program}: {err}");
}

mod setup;

use anyhow::Result;
use game_mode::config::Config;
use game_mode::{approval, config, game_mode_switch, session};
use gilrs::{Button, Event, Gilrs};
use std::{
    env, fs,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, error, info};

/// Log to stdout; game-mode.service appends that to the log file, whose
/// directory `game-mode setup` creates and hands to the greeter.
fn setup_logging() -> Result<()> {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "game_mode=info".to_string()))
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
    Ok(())
}

/// The approved-entry countdown: announce through the greeter's banner
/// channel and watch the gamepad for a cancel. Returns whether to proceed.
///
/// The banner is vigil's `[greeter] banner_file` surface — best-effort; a
/// greeter without it configured just shows no text, and the countdown still
/// runs. Cancel is the East button (B on an Xbox pad): keyboard Esc would
/// need the greeter's cooperation, and game mode deliberately has no vigil
/// coupling beyond this write-only banner file.
fn entry_countdown(gilrs: &mut Gilrs, config: &Config) -> bool {
    const SECONDS: u64 = 5;
    let banner = &config.greeter.banner_file;
    let write_banner = |text: &str| {
        if let Err(e) = fs::write(banner, text) {
            debug!("banner write ({banner}): {e}");
        }
    };
    let start = Instant::now();
    let mut proceed = true;
    'outer: for remaining in (1..=SECONDS).rev() {
        write_banner(&format!(
            "Entering game mode in {remaining}s — press B to cancel"
        ));
        while start.elapsed() < Duration::from_secs(SECONDS - remaining + 1) {
            while let Some(Event { event, .. }) = gilrs.next_event() {
                if let gilrs::EventType::ButtonPressed(Button::East, _) = event {
                    proceed = false;
                    break 'outer;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    write_banner(if proceed { "Entering game mode…" } else { "" });
    proceed
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

    let config = Config::load()?;
    info!("Greetd running on tty{}", config.terminal.vt);

    // Main event loop
    while running.load(Ordering::SeqCst) {
        while let Some(Event { event, .. }) = gilrs.next_event() {
            debug!("Gamepad event: {:?}", event);
            if !matches!(event, gilrs::EventType::ButtonReleased(Button::Mode, _)) {
                continue;
            }
            // Checked per Guide press, not per event: stick movement alone
            // produces a stream of events.
            match session::at_idle_greeter(&config) {
                Ok(true) => {}
                Ok(false) => {
                    debug!("Guide released but not at an idle greeter; ignoring");
                    continue;
                }
                Err(e) => {
                    error!("Failed to check greeter state: {e:#}");
                    continue;
                }
            }
            info!("Guide button released at the greeter");
            // Gate entry on a phone passkey approval (fail-closed),
            // then give the couch a short window to bail out.
            if approval::require_approval() {
                if entry_countdown(&mut gilrs, &config) {
                    game_mode_switch::switch_to_game_mode(&config)?;
                } else {
                    info!("game-mode entry cancelled during countdown");
                }
            } else {
                info!("game-mode entry not approved; staying at greeter");
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

fn main() -> Result<()> {
    // Host provisioning subcommand (`sudo game-mode setup`): plain
    // stdout/stderr, no tracing/log-dir setup — it may run on a box where
    // /etc/greetd doesn't have its final permissions yet.
    if env::args().nth(1).as_deref() == Some("setup") {
        return setup::run();
    }

    // Initialize logging first thing
    if let Err(e) = setup_logging() {
        eprintln!("Failed to setup logging: {}", e);
        return Err(e);
    }

    // Exercise the approval gate without a gamepad (run as the greeter user).
    if env::args().any(|a| a == "--test-approval") {
        let ok = approval::require_approval();
        println!(
            "approval result: {}",
            if ok { "APPROVED" } else { "NOT APPROVED" }
        );
        std::process::exit(if ok { 0 } else { 1 });
    }

    info!("Game mode service starting");

    // Housekeeping: a *stale* armed flag (crash between arming and the
    // greetd restart) is already inert — the dispatcher checks freshness —
    // but remove it so its presence can't confuse anyone. A FRESH flag means
    // we are restarting together with greetd (BindsTo) right after an
    // approved entry and the dispatcher may not have consumed it yet: leave
    // it alone.
    {
        use game_mode_switch::is_armed;
        use std::path::Path;
        let armed = Path::new(config::ARMED_FILE);
        if armed.exists() && !is_armed(armed, config::ARMED_MAX_AGE_SECS) {
            debug!("removing stale armed flag");
            game_mode_switch::disarm();
        }
    }

    if let Err(e) = run_game_mode() {
        eprintln!("Failed to run game mode: {}", e);
        return Err(e);
    }

    info!("Game mode service exiting");
    Ok(())
}

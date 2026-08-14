use crate::paths::PathManager;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

// Built-in defaults. Anything host-specific can be overridden at runtime via
// /etc/game-mode/config.toml (written by `game-mode setup`); the daemon and
// helpers fall back to these when the file (or a key) is absent.
pub const CONFIG_TOML: &str = "/etc/game-mode/config.toml";
pub const GREETD_DIR: &str = "/etc/greetd";
pub const GREETER_USER: &str = "greeter";
pub const VT_NUMBER: u32 = 1;
pub const GAMES_USER: &str = "games";
pub const GAMES_GROUP: &str = "games";
pub const GAMES_DIR: &str = "/games";

/// The one-shot arming flag: the daemon writes it just before restarting
/// greetd, and `game-mode-dispatch` (greetd's permanent `[initial_session]`)
/// execs the game session only while it is fresh. /run is tmpfs, so a reboot
/// always disarms.
pub const ARMED_FILE: &str = "/run/game-mode/armed";
/// An armed flag older than this is stale (a crash between arming and the
/// greetd restart) and must not launch a game session.
pub const ARMED_MAX_AGE_SECS: u64 = 60;

/// Where vigil polls for its status banner (`[greeter] banner_file` in
/// vigil.toml) — the entry countdown is announced through it. Best-effort:
/// a greeter without the banner channel just shows no countdown text.
pub const BANNER_FILE: &str = "/run/vigil/banner";

/// Default game-session argv, matching what `[initial_session]` used to run
/// directly. `game-mode setup` writes the resolved value into
/// /etc/game-mode/config.toml where it can be edited.
pub fn default_game_command() -> Vec<String> {
    [
        "/usr/bin/gamescope",
        "-e",
        "--hdr-enabled",
        "--hdr-itm-enabled",
        "--hide-cursor-delay",
        "3000",
        "--",
        "/usr/bin/game-mode-wrapper",
    ]
    .map(str::to_string)
    .to_vec()
}

// Default log filter is info; set RUST_LOG (e.g. in game-mode.service) for
// debug logging at runtime instead of flipping this at build time.
pub const DEBUG_MODE: bool = false;

/// On-disk shape of /etc/game-mode/config.toml. Every key is optional so a
/// partial (or absent) file falls back to the built-in defaults; unknown keys
/// are ignored so old binaries tolerate newer configs.
#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    #[serde(default)]
    terminal: FileTerminal,
    #[serde(default)]
    session: FileSession,
    #[serde(default)]
    greeter: FileGreeter,
}

#[derive(Debug, Deserialize, Default)]
struct FileTerminal {
    vt: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
struct FileSession {
    user: Option<String>,
    group: Option<String>,
    dir: Option<String>,
    command: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct FileGreeter {
    banner_file: Option<String>,
}

#[derive(Debug)]
pub struct Config {
    pub paths: Paths,
    pub game_mode: GameMode,
    pub permissions: Permissions,
    pub terminal: Terminal,
    pub session: Session,
    pub greeter: Greeter,
    path_manager: PathManager,
}

// Concrete locations live in PathManager (get_greetd_dir() etc.); this only
// carries the virtual-root override used by tests.
#[derive(Debug)]
pub struct Paths {
    pub virtual_root: String,
}

#[derive(Debug)]
pub struct GameMode {
    pub debug: bool,
}

// Joystick-only device access is granted by the package's udev rule and the
// systemd unit's dedicated SupplementaryGroups= entry, not tracked here.
#[derive(Debug)]
pub struct Permissions {
    pub greeter_user: String,
}

#[derive(Debug)]
pub struct Terminal {
    pub vt: u32,
}

/// The identity the game session autologs in as, the shared game library
/// directory (bound read-write into the bwrap home mask), and the argv
/// `game-mode-dispatch` execs when armed.
#[derive(Debug)]
pub struct Session {
    pub user: String,
    pub group: String,
    pub dir: String,
    pub command: Vec<String>,
}

/// Greeter integration surfaces (all best-effort).
#[derive(Debug)]
pub struct Greeter {
    pub banner_file: String,
}

impl Config {
    /// Load configuration: built-in defaults overridden by
    /// /etc/game-mode/config.toml when present. A missing file is fine
    /// (pure defaults); a file that exists but fails to parse is an error —
    /// silently ignoring a typo'd config would misconfigure the session.
    pub fn load() -> Result<Self> {
        Self::load_from(CONFIG_TOML)
    }

    pub fn load_from(config_toml: &str) -> Result<Self> {
        let file: FileConfig = match std::fs::read_to_string(config_toml) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("failed to parse {config_toml}"))?
            }
            Err(_) => FileConfig::default(),
        };

        let config = Config {
            paths: Paths {
                virtual_root: String::new(),
            },
            game_mode: GameMode { debug: DEBUG_MODE },
            permissions: Permissions {
                greeter_user: GREETER_USER.to_string(),
            },
            terminal: Terminal {
                vt: file.terminal.vt.unwrap_or(VT_NUMBER),
            },
            session: Session {
                user: file.session.user.unwrap_or_else(|| GAMES_USER.to_string()),
                group: file
                    .session
                    .group
                    .unwrap_or_else(|| GAMES_GROUP.to_string()),
                dir: file.session.dir.unwrap_or_else(|| GAMES_DIR.to_string()),
                command: file.session.command.unwrap_or_else(default_game_command),
            },
            greeter: Greeter {
                banner_file: file
                    .greeter
                    .banner_file
                    .unwrap_or_else(|| BANNER_FILE.to_string()),
            },
            // Real root, not "": PathManager joins root + greetd_dir, and an
            // empty root yields a *relative* "etc/greetd" that resolves under
            // the daemon's working directory instead of /etc/greetd.
            path_manager: PathManager::new("/", GREETD_DIR),
        };

        Ok(config)
    }

    pub fn is_virtual_mode(&self) -> bool {
        !self.paths.virtual_root.is_empty()
    }

    pub fn get_greetd_dir(&self) -> PathBuf {
        self.path_manager.get_greetd_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn defaults_when_file_missing() {
        let config = Config::load_from("/nonexistent/game-mode-config.toml").unwrap();
        assert_eq!(config.terminal.vt, VT_NUMBER);
        assert_eq!(config.session.user, GAMES_USER);
        assert_eq!(config.session.group, GAMES_GROUP);
        assert_eq!(config.session.dir, GAMES_DIR);
    }

    #[test]
    fn file_overrides_defaults() {
        let dir = std::env::temp_dir().join(format!("game-mode-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "[terminal]\nvt = 3\n\n[session]\nuser = \"couch\"\ngroup = \"couch\"\ndir = \"/srv/games\"\n"
        )
        .unwrap();

        let config = Config::load_from(path.to_str().unwrap()).unwrap();
        assert_eq!(config.terminal.vt, 3);
        assert_eq!(config.session.user, "couch");
        assert_eq!(config.session.group, "couch");
        assert_eq!(config.session.dir, "/srv/games");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let dir = std::env::temp_dir().join(format!("game-mode-cfg-part-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[session]\nuser = \"couch\"\n").unwrap();

        let config = Config::load_from(path.to_str().unwrap()).unwrap();
        assert_eq!(config.terminal.vt, VT_NUMBER);
        assert_eq!(config.session.user, "couch");
        assert_eq!(config.session.group, GAMES_GROUP);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn garbage_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("game-mode-cfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "not [ valid toml").unwrap();
        assert!(Config::load_from(path.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

use crate::paths::PathManager;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

// Built-in defaults. Anything host-specific can be overridden at runtime via
// /etc/game-mode/config.toml (written by `game-mode setup`); the daemon and
// helpers fall back to these when the file (or a key) is absent.
pub const CONFIG_TOML: &str = "/etc/game-mode/config.toml";
pub const GREETD_DIR: &str = "/etc/greetd";
pub const CONFIG_FILE: &str = "config.toml";
pub const GAME_MODE_CONFIG: &str = "game_mode_login.toml";
pub const GREETER_USER: &str = "greeter";
pub const VT_NUMBER: u32 = 1;
pub const GAMES_USER: &str = "games";
pub const GAMES_GROUP: &str = "games";
pub const GAMES_DIR: &str = "/games";

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
}

#[derive(Debug)]
pub struct Config {
    pub paths: Paths,
    pub game_mode: GameMode,
    pub permissions: Permissions,
    pub terminal: Terminal,
    pub session: Session,
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

// Device access groups (input/video) are granted by the systemd unit's
// SupplementaryGroups=, not tracked here.
#[derive(Debug)]
pub struct Permissions {
    pub greeter_user: String,
}

#[derive(Debug)]
pub struct Terminal {
    pub vt: u32,
}

/// The identity the game session autologs in as, and the shared game library
/// directory (bound read-write into the bwrap home mask).
#[derive(Debug)]
pub struct Session {
    pub user: String,
    pub group: String,
    pub dir: String,
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
            },
            // Real root, not "": PathManager joins root + greetd_dir, and an
            // empty root yields a *relative* "etc/greetd" that resolves under
            // the daemon's working directory instead of /etc/greetd.
            path_manager: PathManager::new("/", GREETD_DIR, CONFIG_FILE, GAME_MODE_CONFIG),
        };

        Ok(config)
    }

    pub fn is_virtual_mode(&self) -> bool {
        !self.paths.virtual_root.is_empty()
    }

    pub fn get_greetd_dir(&self) -> PathBuf {
        self.path_manager.get_greetd_dir()
    }

    pub fn get_config_path(&self) -> PathBuf {
        self.path_manager.get_config_path()
    }

    pub fn get_default_config_path(&self) -> PathBuf {
        self.path_manager.get_default_config_path()
    }

    pub fn get_game_mode_config_path(&self) -> PathBuf {
        self.path_manager.get_game_mode_config_path()
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

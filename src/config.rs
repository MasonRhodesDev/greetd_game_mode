use anyhow::{Context, Result};
use serde::Deserialize;

pub const CONFIG_TOML: &str = "/etc/game-mode/config.toml";
pub const GREETD_DIR: &str = "/etc/greetd";
pub const GREETER_USER: &str = "greeter";
pub const VT_NUMBER: u32 = 1;
pub const GAMES_USER: &str = "games";
pub const GAMES_GROUP: &str = "games";
pub const GAMES_DIR: &str = "/games";

pub const ARMED_FILE: &str = "/run/game-mode/armed";
pub const ARMED_MAX_AGE_SECS: u64 = 60;

pub const BANNER_FILE: &str = "/run/vigil/banner";

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
    pub terminal: Terminal,
    pub session: Session,
    pub greeter: Greeter,
}

#[derive(Debug)]
pub struct Terminal {
    pub vt: u32,
}

#[derive(Debug)]
pub struct Session {
    pub user: String,
    pub group: String,
    pub dir: String,
    pub command: Vec<String>,
}

#[derive(Debug)]
pub struct Greeter {
    pub banner_file: String,
}

impl Config {
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
        };

        Ok(config)
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

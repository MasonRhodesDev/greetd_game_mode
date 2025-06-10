use std::path::{Path, PathBuf};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct PathManager {
    root: PathBuf,
    greetd_dir: PathBuf,
    config_file: String,
    game_mode_config: String,
}

impl PathManager {
    pub fn new(root: impl AsRef<Path>, greetd_dir: impl AsRef<Path>, config_file: &str, game_mode_config: &str) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            greetd_dir: greetd_dir.as_ref().to_path_buf(),
            config_file: config_file.to_string(),
            game_mode_config: game_mode_config.to_string(),
        }
    }

    pub fn get_greetd_dir(&self) -> PathBuf {
        self.root.join(&self.greetd_dir)
    }

    pub fn get_config_path(&self) -> PathBuf {
        self.get_greetd_dir().join(&self.config_file)
    }

    pub fn get_default_config_path(&self) -> PathBuf {
        self.get_greetd_dir().join("config_default.toml")
    }

    pub fn get_game_mode_config_path(&self) -> PathBuf {
        self.get_greetd_dir().join(&self.game_mode_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_manager() {
        let manager = PathManager::new(
            "/",
            "/etc/greetd",
            "config.toml",
            "game_mode_login.toml"
        );

        assert_eq!(manager.get_greetd_dir(), PathBuf::from("/etc/greetd"));
        assert_eq!(manager.get_config_path(), PathBuf::from("/etc/greetd/config.toml"));
        assert_eq!(manager.get_default_config_path(), PathBuf::from("/etc/greetd/config_default.toml"));
        assert_eq!(manager.get_game_mode_config_path(), PathBuf::from("/etc/greetd/game_mode_login.toml"));
    }

    #[test]
    fn test_path_manager_with_virtual_root() {
        let manager = PathManager::new(
            "/tmp/test",
            "/etc/greetd",
            "config.toml",
            "game_mode_login.toml"
        );

        assert_eq!(manager.get_greetd_dir(), PathBuf::from("/tmp/test/etc/greetd"));
        assert_eq!(manager.get_config_path(), PathBuf::from("/tmp/test/etc/greetd/config.toml"));
        assert_eq!(manager.get_default_config_path(), PathBuf::from("/tmp/test/etc/greetd/config_default.toml"));
        assert_eq!(manager.get_game_mode_config_path(), PathBuf::from("/tmp/test/etc/greetd/game_mode_login.toml"));
    }
} 
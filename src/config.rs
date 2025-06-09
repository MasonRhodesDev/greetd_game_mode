use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use anyhow::Result;

#[derive(Serialize, Deserialize, Default)]
pub struct InstallationState {
    pub installed: bool,
    pub modified_files: Vec<PathBuf>,
    pub backup_files: Vec<PathBuf>,
    pub greeter_user_configured: bool,
}

impl InstallationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_modified_file(&mut self, path: PathBuf) {
        if !self.modified_files.contains(&path) {
            self.modified_files.push(path);
        }
    }

    pub fn add_backup_file(&mut self, path: PathBuf) {
        if !self.backup_files.contains(&path) {
            self.backup_files.push(path);
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub paths: Paths,
    pub service: Service,
    pub game_mode: GameMode,
    pub permissions: Permissions,
}

#[derive(Debug, Deserialize)]
pub struct Paths {
    pub virtual_root: String,
    pub greetd_dir: String,
    pub config_file: String,
    pub game_mode_config: String,
}

#[derive(Debug, Deserialize)]
pub struct Service {
    pub name: String,
    pub restart_command: String,
}

#[derive(Debug, Deserialize)]
pub struct GameMode {
    pub switch_back_delay: u64,
    pub debug: bool,
}

#[derive(Debug, Deserialize)]
pub struct Permissions {
    pub greeter_user: String,
    pub required_groups: Vec<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_str = include_str!("../config.toml");
        let config: Config = toml::from_str(config_str)?;
        Ok(config)
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            // Get the directory containing the config.toml
            let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            config_dir.join(path)
        }
    }

    pub fn get_greetd_dir(&self) -> PathBuf {
        if self.is_virtual_mode() {
            // In virtual mode, we just change the root but keep the same path structure
            PathBuf::from(&self.paths.virtual_root).join(self.paths.greetd_dir.strip_prefix("/").unwrap_or(&self.paths.greetd_dir))
        } else {
            PathBuf::from(&self.paths.greetd_dir)
        }
    }

    pub fn get_config_path(&self) -> PathBuf {
        self.get_greetd_dir().join(&self.paths.config_file)
    }

    pub fn get_game_mode_config_path(&self) -> PathBuf {
        self.get_greetd_dir().join(&self.paths.game_mode_config)
    }

    pub fn is_virtual_mode(&self) -> bool {
        !self.paths.virtual_root.is_empty()
    }
} 
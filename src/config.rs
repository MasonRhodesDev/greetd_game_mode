use std::path::PathBuf;
use anyhow::Result;
use crate::paths::PathManager;

// Build-time constants
pub const GREETD_DIR: &str = "/etc/greetd";
pub const CONFIG_FILE: &str = "config.toml";
pub const GAME_MODE_CONFIG: &str = "game_mode_login.toml";
pub const GREETER_USER: &str = "greeter";
pub const REQUIRED_GROUPS: &[&str] = &["input", "video"];
pub const VT_NUMBER: u32 = 1;

// Service configuration
pub const SERVICE_NAME: &str = "greetd";
pub const RESTART_COMMAND: &str = "systemctl restart greetd";
pub const SERVICE_DEPENDENCY: &str = "greetd.service";

// Game mode configuration
pub const DEBUG_MODE: bool = true;

#[derive(Default)]
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

#[derive(Debug)]
pub struct Config {
    pub paths: Paths,
    pub service: Service,
    pub game_mode: GameMode,
    pub permissions: Permissions,
    pub terminal: Terminal,
    path_manager: PathManager,
}

#[derive(Debug)]
pub struct Paths {
    pub virtual_root: String,
    pub greetd_dir: String,
    pub config_file: String,
    pub game_mode_config: String,
}

#[derive(Debug)]
pub struct Service {
    pub name: String,
    pub restart_command: String,
    pub dependency: String,
}

#[derive(Debug)]
pub struct GameMode {
    pub debug: bool,
}

#[derive(Debug)]
pub struct Permissions {
    pub greeter_user: String,
    pub required_groups: Vec<String>,
}

#[derive(Debug)]
pub struct Terminal {
    pub vt: u32,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config = Config {
            paths: Paths {
                virtual_root: String::new(),
                greetd_dir: GREETD_DIR.to_string(),
                config_file: CONFIG_FILE.to_string(),
                game_mode_config: GAME_MODE_CONFIG.to_string(),
            },
            service: Service {
                name: SERVICE_NAME.to_string(),
                restart_command: RESTART_COMMAND.to_string(),
                dependency: SERVICE_DEPENDENCY.to_string(),
            },
            game_mode: GameMode {
                debug: DEBUG_MODE,
            },
            permissions: Permissions {
                greeter_user: GREETER_USER.to_string(),
                required_groups: REQUIRED_GROUPS.iter().map(|&s| s.to_string()).collect(),
            },
            terminal: Terminal {
                vt: VT_NUMBER,
            },
            path_manager: PathManager::new(
                "",
                GREETD_DIR,
                CONFIG_FILE,
                GAME_MODE_CONFIG
            ),
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

    pub fn get_service_file_path(&self) -> PathBuf {
        self.path_manager.get_service_file_path()
    }

    pub fn get_logs_dir(&self) -> PathBuf {
        self.path_manager.get_logs_dir()
    }

    pub fn get_sudoers_path(&self) -> PathBuf {
        self.path_manager.get_sudoers_path()
    }

    pub fn get_systemd_service_path(&self) -> PathBuf {
        self.path_manager.get_systemd_service_path()
    }

    pub fn get_binary_path(&self) -> PathBuf {
        self.path_manager.get_binary_path()
    }
} 
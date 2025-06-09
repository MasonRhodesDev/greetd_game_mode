use anyhow::{Context, Result};
use indicatif::ProgressBar;
use log::{debug, error};
use std::{
    fs,
    path::PathBuf,
    process::Command,
};

use crate::config::{Config, InstallationState};

pub struct Installer {
    state: InstallationState,
    state_file: PathBuf,
    config: Config,
}

impl Installer {
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        debug!("Loaded config with virtual_root: {:?}", config.paths.virtual_root);
        
        let state_file = if config.is_virtual_mode() {
            let greetd_dir = config.get_greetd_dir();
            debug!("Creating virtual greetd directory: {:?}", greetd_dir);
            fs::create_dir_all(&greetd_dir)?;
            greetd_dir.join("game-mode-state.json")
        } else {
            PathBuf::from("/etc/greetd/game-mode-state.json")
        };
        debug!("Using state file: {:?}", state_file);

        let state = if state_file.exists() {
            debug!("Loading existing state file");
            serde_json::from_str(&fs::read_to_string(&state_file)?)?
        } else {
            debug!("Creating new state");
            InstallationState::new()
        };

        Ok(Self { state, state_file, config })
    }

    pub fn save_state(&self) -> Result<()> {
        debug!("Saving state to: {:?}", self.state_file);
        let json = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_file, json)?;
        Ok(())
    }

    fn setup_virtual_permissions(&self, greetd_dir: &PathBuf) -> Result<()> {
        debug!("Setting up virtual permissions in: {:?}", greetd_dir);
        
        // Create a mock sudoers file in the virtual root
        let sudoers_dir = greetd_dir.parent().unwrap().join("sudoers.d");
        debug!("Creating virtual sudoers directory: {:?}", sudoers_dir);
        fs::create_dir_all(&sudoers_dir)?;
        let sudoers_content = format!(
            "{} ALL=(ALL) NOPASSWD: /usr/bin/{}\n",
            self.config.permissions.greeter_user,
            self.config.service.restart_command
        );
        fs::write(sudoers_dir.join("greeter-greetd"), sudoers_content)?;

        // Create mock group files
        let group_dir = greetd_dir.parent().unwrap().join("group");
        debug!("Creating virtual group file: {:?}", group_dir);
        fs::create_dir_all(group_dir.parent().unwrap())?;
        let mut group_content = String::new();
        for group in &self.config.permissions.required_groups {
            group_content.push_str(&format!("{}:x:1000:{}\n", group, self.config.permissions.greeter_user));
        }
        fs::write(group_dir, group_content)?;

        Ok(())
    }

    pub fn install(&mut self) -> Result<()> {
        // Check if running as root
        if unsafe { libc::geteuid() } != 0 {
            return Err(anyhow::anyhow!("Installer must be run as root"));
        }

        let pb = ProgressBar::new(7);
        pb.set_message("Installing game mode...");
        debug!("Starting installation");

        // Configure greeter user
        if !self.state.greeter_user_configured {
            if self.config.is_virtual_mode() {
                debug!("Setting up virtual permissions");
                self.setup_virtual_permissions(&self.config.get_greetd_dir())?;
            } else {
                // Create user without home directory
                Command::new("useradd")
                    .args(["-M", "-r", &self.config.permissions.greeter_user])
                    .status()
                    .context("Failed to create greeter user")?;

                // Add to required groups
                for group in &self.config.permissions.required_groups {
                    Command::new("usermod")
                        .args(["-a", "-G", group, &self.config.permissions.greeter_user])
                        .status()
                        .context(format!("Failed to add user to group {}", group))?;
                }

                // Set proper permissions for greetd config directory
                Command::new("chown")
                    .args(["-R", &format!("{}:{}", 
                        self.config.permissions.greeter_user,
                        self.config.permissions.greeter_user
                    ), &self.config.paths.greetd_dir])
                    .status()
                    .context("Failed to set greetd directory permissions")?;
            }
            self.state.greeter_user_configured = true;
        }

        // Always set up sudoers file during installation
        if !self.config.is_virtual_mode() {
            // Add sudo permissions for specific commands
            let sudoers_content = format!(
                "{} ALL=(ALL) NOPASSWD: /usr/bin/{}\n\
                 {} ALL=(ALL) NOPASSWD: /usr/bin/cp /etc/greetd/config.toml /etc/greetd/config.toml.bak\n\
                 {} ALL=(ALL) NOPASSWD: /usr/bin/cp /etc/greetd/game_mode_login.toml /etc/greetd/config.toml\n\
                 {} ALL=(ALL) NOPASSWD: /usr/bin/cp /etc/greetd/config.toml.bak /etc/greetd/config.toml\n\
                 {} ALL=(ALL) NOPASSWD: /usr/bin/rm /etc/greetd/config.toml.bak\n\
                 {} ALL=(ALL) NOPASSWD: /etc/greetd/start_greeter.sh\n\
                 {} ALL=(ALL) NOPASSWD: /usr/local/bin/game-mode\n\
                 {} ALL=(ALL) NOPASSWD: /usr/bin/pgrep -f game-mode\n\
                 {} ALL=(ALL) NOPASSWD: /usr/bin/kill -9 [0-9]*\n",
                self.config.permissions.greeter_user,
                self.config.service.restart_command,
                self.config.permissions.greeter_user,
                self.config.permissions.greeter_user,
                self.config.permissions.greeter_user,
                self.config.permissions.greeter_user,
                self.config.permissions.greeter_user,
                self.config.permissions.greeter_user,
                self.config.permissions.greeter_user,
                self.config.permissions.greeter_user
            );
            let sudoers_path = "/etc/sudoers.d/greeter-greetd";
            debug!("Creating sudoers file at: {}", sudoers_path);
            debug!("Sudoers content:\n{}", sudoers_content);

            // Create sudoers file directly as root
            fs::write(sudoers_path, sudoers_content)
                .context("Failed to create sudoers file")?;

            // Set proper permissions for sudoers file
            debug!("Setting sudoers file permissions");
            Command::new("chmod")
                .args(["440", sudoers_path])
                .status()
                .context("Failed to set sudoers file permissions")?;

            // Verify sudoers file exists and has correct permissions
            let metadata = fs::metadata(sudoers_path)
                .context("Failed to get sudoers file metadata")?;
            let permissions = metadata.permissions();
            debug!("Sudoers file permissions: {:?}", permissions);
            
            // Check if file is readable by root and group, but not others
            if !permissions.readonly() {
                return Err(anyhow::anyhow!("Sudoers file must be read-only"));
            }

            // Verify sudoers file syntax
            debug!("Verifying sudoers file");
            let output = Command::new("visudo")
                .args(["-c", "-f", sudoers_path])
                .output()
                .context("Failed to verify sudoers file")?;
            debug!("visudo check output: {}", String::from_utf8_lossy(&output.stdout));
            if !output.status.success() {
                error!("visudo check failed: {}", String::from_utf8_lossy(&output.stderr));
                return Err(anyhow::anyhow!("Sudoers file validation failed"));
            }
        }
        pb.inc(1);

        // Create greetd directory if it doesn't exist
        let greetd_dir = self.config.get_greetd_dir();
        debug!("Creating greetd directory: {:?}", greetd_dir);
        fs::create_dir_all(&greetd_dir)
            .with_context(|| format!("Failed to create greetd directory at {:?}", greetd_dir))?;
        pb.inc(1);

        // Create logs directory
        let logs_dir = greetd_dir.join("logs");
        debug!("Creating logs directory: {:?}", logs_dir);
        fs::create_dir_all(&logs_dir)
            .with_context(|| format!("Failed to create logs directory at {:?}", logs_dir))?;
        pb.inc(1);

        // Backup existing greetd config
        let greetd_config = self.config.get_config_path();
        if greetd_config.exists() {
            let backup_path = greetd_config.with_extension("toml.bak");
            debug!("Backing up existing config to: {:?}", backup_path);
            fs::copy(&greetd_config, &backup_path)
                .with_context(|| format!("Failed to backup config from {:?} to {:?}", greetd_config, backup_path))?;
            self.state.add_backup_file(backup_path);
        }
        pb.inc(1);

        // Copy our config files
        let greetd_src = PathBuf::from("greetd");
        debug!("Copying all files from {:?}", greetd_src);
        
        if !greetd_src.exists() {
            return Err(anyhow::anyhow!("Source directory {:?} does not exist", greetd_src));
        }
        
        // Copy all files from greetd directory
        for entry in fs::read_dir(&greetd_src)
            .with_context(|| format!("Failed to read directory {:?}", greetd_src))? 
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let dest = greetd_dir.join(path.file_name().unwrap());
                debug!("Copying file from {:?} to {:?}", path, dest);
                fs::copy(&path, &dest)
                    .with_context(|| format!("Failed to copy file from {:?} to {:?}", path, dest))?;
                self.state.add_modified_file(dest);
            }
        }
        pb.inc(1);

        // Copy our binary
        let binary_path = if self.config.is_virtual_mode() {
            // In virtual mode, we want /usr/local/bin/game-mode under the virtual root
            let path = PathBuf::from(&self.config.paths.virtual_root)
                .join("usr")
                .join("local")
                .join("bin")
                .join("game-mode");
            debug!("Creating virtual binary path: {:?}", path);
            fs::create_dir_all(path.parent().unwrap())
                .with_context(|| format!("Failed to create binary directory at {:?}", path.parent().unwrap()))?;
            path
        } else {
            // In real mode, we want /usr/local/bin/game-mode
            PathBuf::from("/usr/local/bin/game-mode")
        };
        debug!("Copying binary to: {:?}", binary_path);
        let binary_src = PathBuf::from("target/release/game_mode");
        if !binary_src.exists() {
            return Err(anyhow::anyhow!("Binary not found at {:?}", binary_src));
        }
        fs::copy(&binary_src, &binary_path)
            .with_context(|| format!("Failed to copy binary from {:?} to {:?}", binary_src, binary_path))?;
        self.state.add_modified_file(binary_path);
        pb.inc(1);

        self.state.installed = true;
        self.save_state()?;
        pb.finish_with_message("Installation complete!");

        Ok(())
    }

    pub fn uninstall(&mut self) -> Result<()> {
        let pb = ProgressBar::new(3);
        pb.set_message("Uninstalling game mode...");

        // Remove modified files
        for path in &self.state.modified_files {
            let path = if self.config.is_virtual_mode() {
                // In virtual mode, we need to resolve paths relative to virtual root
                PathBuf::from(&self.config.paths.virtual_root).join(path.strip_prefix("/").unwrap_or(path))
            } else {
                path.clone()
            };
            if path.exists() {
                fs::remove_file(&path)?;
            }
        }
        pb.inc(1);

        // Restore backups
        for backup in &self.state.backup_files {
            let backup = if self.config.is_virtual_mode() {
                PathBuf::from(&self.config.paths.virtual_root).join(backup.strip_prefix("/").unwrap_or(backup))
            } else {
                backup.clone()
            };
            if backup.exists() {
                let original = backup.with_extension("toml");
                fs::copy(&backup, &original)?;
                fs::remove_file(&backup)?;
            }
        }
        pb.inc(1);

        // Remove state file
        if self.state_file.exists() {
            fs::remove_file(&self.state_file)?;
        }
        pb.inc(1);

        pb.finish_with_message("Uninstallation complete!");
        Ok(())
    }

    pub fn is_installed(&self) -> bool {
        self.state.installed
    }
} 
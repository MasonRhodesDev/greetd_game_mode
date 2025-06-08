use anyhow::{Context, Result};
use indicatif::ProgressBar;
use std::{
    fs,
    path::PathBuf,
    process::Command,
};

use crate::config::InstallationState;

pub struct Installer {
    state: InstallationState,
    state_file: PathBuf,
}

impl Installer {
    pub fn new() -> Result<Self> {
        let state_file = PathBuf::from("/etc/greetd/game-mode-state.json");
        let state = if state_file.exists() {
            serde_json::from_str(&fs::read_to_string(&state_file)?)?
        } else {
            InstallationState::new()
        };

        Ok(Self { state, state_file })
    }

    pub fn save_state(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_file, json)?;
        Ok(())
    }

    pub fn install(&mut self) -> Result<()> {
        let pb = ProgressBar::new(8);
        pb.set_message("Installing game mode...");

        // Configure greeter user
        if !self.state.greeter_user_configured {
            // Add to input group for gamepad access
            Command::new("usermod")
                .args(["-a", "-G", "input", "greeter"])
                .status()
                .context("Failed to configure greeter user")?;

            // Add sudo permissions for greetd service management
            let sudoers_content = "greeter ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart greetd\n";
            fs::write("/etc/sudoers.d/greeter-greetd", sudoers_content)
                .context("Failed to create sudoers file")?;

            // Set proper permissions for greetd config directory
            Command::new("chown")
                .args(["-R", "greeter:greeter", "/etc/greetd"])
                .status()
                .context("Failed to set greetd directory permissions")?;

            self.state.greeter_user_configured = true;
        }
        pb.inc(1);

        // Create greetd directory if it doesn't exist
        let greetd_dir = PathBuf::from("/etc/greetd");
        if !greetd_dir.exists() {
            fs::create_dir_all(&greetd_dir)?;
        }
        pb.inc(1);

        // Create logs directory
        let logs_dir = greetd_dir.join("logs");
        if !logs_dir.exists() {
            fs::create_dir_all(&logs_dir)?;
        }
        pb.inc(1);

        // Backup existing greetd config
        let greetd_config = greetd_dir.join("config.toml");
        if greetd_config.exists() {
            let backup_path = greetd_config.with_extension("toml.bak");
            fs::copy(&greetd_config, &backup_path)?;
            self.state.add_backup_file(backup_path);
        }
        pb.inc(1);

        // Copy our config files
        let config_files = [
            "config.toml",
            "config_autologin.toml",
            "gamepad_handler.sh",
            "hypr.conf",
            "regreet.toml",
        ];

        for file in config_files {
            let src = PathBuf::from("greetd").join(file);
            let dest = greetd_dir.join(file);
            fs::copy(&src, &dest)?;
            self.state.add_modified_file(dest);
        }
        pb.inc(1);

        // Make gamepad handler executable
        Command::new("chmod")
            .args(["+x", "/etc/greetd/gamepad_handler.sh"])
            .status()
            .context("Failed to make gamepad handler executable")?;
        pb.inc(1);

        // Copy our binary
        let binary_path = PathBuf::from("/usr/local/bin/game-mode");
        fs::copy("target/release/game-mode", &binary_path)?;
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
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        pb.inc(1);

        // Restore backups
        for backup in &self.state.backup_files {
            if backup.exists() {
                let original = backup.with_extension("toml");
                fs::copy(backup, original)?;
                fs::remove_file(backup)?;
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
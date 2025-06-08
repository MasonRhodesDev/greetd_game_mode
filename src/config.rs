use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
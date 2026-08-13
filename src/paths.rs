use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Default)]
pub struct PathManager {
    root: PathBuf,
    greetd_dir: PathBuf,
}

impl PathManager {
    pub fn new(root: impl AsRef<Path>, greetd_dir: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            greetd_dir: greetd_dir.as_ref().to_path_buf(),
        }
    }

    pub fn get_greetd_dir(&self) -> PathBuf {
        // Path::join with an absolute path *replaces* the base, so strip the
        // leading "/" or the virtual root would be silently discarded.
        let rel = self
            .greetd_dir
            .strip_prefix("/")
            .unwrap_or(&self.greetd_dir);
        self.root.join(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_manager() {
        let manager = PathManager::new("/", "/etc/greetd");
        assert_eq!(manager.get_greetd_dir(), PathBuf::from("/etc/greetd"));
    }

    #[test]
    fn test_path_manager_with_virtual_root() {
        let manager = PathManager::new("/tmp/test", "/etc/greetd");
        assert_eq!(
            manager.get_greetd_dir(),
            PathBuf::from("/tmp/test/etc/greetd")
        );
    }
}

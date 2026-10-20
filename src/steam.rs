//! Steam install helpers shared by the session-side binaries
//! (game-mode-steam-config, game-mode-steam-shortcut, game-mode-watchdog).

use std::fs;
use std::path::{Path, PathBuf};

/// Lets a CEF GPU context loss respawn steamwebhelper with GPU instead of
/// falling back to a black or software-rendered UI.
pub const CEF_FLAG: &str = "--disable-gpu-process-crash-limit";
const WRAP_EXEC: &str = "exec ./steamwebhelper \"$@\"";

/// Steam's data root under `home`.
pub fn steam_root(home: &Path) -> PathBuf {
    home.join(".local/share/Steam")
}

/// The script Steam launches steamwebhelper through.
pub fn webhelper_wrap(home: &Path) -> PathBuf {
    steam_root(home).join("ubuntu12_64/steamwebhelper_sniper_wrap.sh")
}

/// Whether any process's comm is exactly `name`.
pub fn process_running(name: &str) -> bool {
    fs::read_dir("/proc")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .any(|e| fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == name))
        })
        .unwrap_or(false)
}

/// Steam rewrites its config files from memory on exit, so they are only
/// safe to edit while it is not running.
pub fn steam_running() -> bool {
    process_running("steam")
}

/// Append [`CEF_FLAG`] to the `exec ./steamwebhelper "$@"` line. No-op when
/// already present. Returns the new text or None if the exec line is missing.
pub fn patch_webhelper_wrap(text: &str) -> Option<String> {
    if text.contains(CEF_FLAG) {
        return Some(text.to_string());
    }
    if !text.contains(WRAP_EXEC) {
        return None;
    }
    Some(text.replace(WRAP_EXEC, &format!("{WRAP_EXEC} {CEF_FLAG}")))
}

/// [`patch_webhelper_wrap`] applied to the file at `path`. Returns whether
/// the file changed.
pub fn patch_webhelper_wrap_file(path: &Path) -> Result<bool, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let new = patch_webhelper_wrap(&text)
        .ok_or_else(|| "exec ./steamwebhelper line not found".to_string())?;
    if new == text {
        return Ok(false);
    }
    fs::write(path, new).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRAP: &str = "#!/bin/bash\nexport LD_LIBRARY_PATH=.${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}\necho \"<6>exec ./steamwebhelper $*\"\necho \"<remaining-lines-assume-level=7>\"\nexec ./steamwebhelper \"$@\"\n";

    #[test]
    fn appends_cef_flag_once() {
        let once = patch_webhelper_wrap(WRAP).unwrap();
        assert!(once.contains("exec ./steamwebhelper \"$@\" --disable-gpu-process-crash-limit"));
        let twice = patch_webhelper_wrap(&once).unwrap();
        assert_eq!(once, twice, "second apply is a no-op");
    }

    #[test]
    fn wrap_without_exec_line_is_an_error() {
        assert!(patch_webhelper_wrap("#!/bin/bash\nnothing\n").is_none());
    }
}

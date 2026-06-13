//! game-mode-steam-config: state-aware Steam idle-power configuration +
//! steamwebhelper crash-limit flag, applied at game-mode session start
//! (before Steam launches).
//!
//! Why: with `-steamos3` Steam runs console-style idle management and will
//! call logind Suspend after `IdleSuspendACSeconds`. If suspend is unavailable
//! (target masked / sleep-blocked — e.g. while this box tethers another
//! server), the refusal wedges Big Picture black. So:
//!   suspend available  → IdleSuspendACSeconds=3600  (normal console behavior)
//!   suspend unavailable→ IdleSuspendACSeconds=0     (never attempt → no wedge)
//! Dim stays at 900 either way (harmless, wakes on input).
//!
//! Also appends `--disable-gpu-process-crash-limit` to
//! steamwebhelper_sniper_wrap.sh (Steam reverts it on updates; the in-session
//! watchdog re-asserts every 120s) so a CEF GPU context-loss respawns with GPU
//! instead of black/software (the desktop daemon's proven fix).
//!
//! Usage: game-mode-steam-config apply
//! Exit: 0 applied/no-op, 2 = Steam is running (unsafe to edit), 1 = error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const DIM_SECONDS: &str = "900";
const SUSPEND_SECONDS_NORMAL: &str = "3600";
const CEF_FLAG: &str = "--disable-gpu-process-crash-limit";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("apply") => apply(),
        _ => {
            eprintln!("usage: game-mode-steam-config apply");
            ExitCode::from(1)
        }
    }
}

fn apply() -> ExitCode {
    if steam_running() {
        eprintln!("steam is running — config.vdf is unsafe to edit; skipping");
        return ExitCode::from(2);
    }
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => {
            eprintln!("HOME not set");
            return ExitCode::from(1);
        }
    };

    let suspend = if suspend_available() { SUSPEND_SECONDS_NORMAL } else { "0" };
    println!(
        "suspend {}: IdleSuspendACSeconds={suspend}, IdleBacklightDimACSeconds={DIM_SECONDS}",
        if suspend == "0" { "UNAVAILABLE (masked/inhibited)" } else { "available" }
    );

    let mut failures = 0;
    for rel in [
        ".local/share/Steam/config/config.vdf",
        // per-user mirror (the exploration found the same keys here)
        ".local/share/Steam/userdata/80409974/config/localconfig.vdf",
    ] {
        let path = home.join(rel);
        if !path.exists() {
            continue;
        }
        match patch_idle_keys_file(&path, DIM_SECONDS, suspend) {
            Ok(true) => println!("patched {}", path.display()),
            Ok(false) => println!("up-to-date {}", path.display()),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                failures += 1;
            }
        }
    }

    let wrap = home.join(".local/share/Steam/ubuntu12_64/steamwebhelper_sniper_wrap.sh");
    match patch_webhelper_wrap_file(&wrap) {
        Ok(true) => println!("patched {} (+{CEF_FLAG})", wrap.display()),
        Ok(false) => println!("up-to-date {}", wrap.display()),
        Err(e) => {
            eprintln!("{}: {e}", wrap.display());
            failures += 1;
        }
    }

    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Steam-running guard (same approach as game-mode-steam-shortcut).
fn steam_running() -> bool {
    fs::read_dir("/proc")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .any(|e| fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "steam"))
        })
        .unwrap_or(false)
}

/// Suspend is available iff suspend.target isn't masked and no active
/// sleep-block inhibitor exists.
fn suspend_available() -> bool {
    let masked = Command::new("systemctl")
        .args(["is-enabled", "suspend.target"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "masked")
        .unwrap_or(true);
    if masked {
        return false;
    }
    // `systemd-inhibit --list --mode=block` lines with WHAT containing "sleep"
    let blocked = Command::new("systemd-inhibit")
        .args(["--list", "--mode=block"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.to_lowercase().contains("sleep"))
        })
        .unwrap_or(false);
    !blocked
}

// ---------------------------------------------------------------------------
// Pure patchers (unit-tested below)
// ---------------------------------------------------------------------------

/// Set the two idle keys inside the `"System" { ... }` block of a text-KeyValues
/// VDF. Replaces existing values; inserts missing keys at the top of the block.
/// Returns the new text, or `None` if no `"System"` block exists.
pub fn patch_idle_keys(text: &str, dim: &str, suspend: &str) -> Option<String> {
    let sys_pos = text.find("\"System\"")?;
    let open = text[sys_pos..].find('{')? + sys_pos;
    // find the matching close brace of the System block
    let mut depth = 0usize;
    let mut close = None;
    for (i, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let mut block = text[open + 1..close].to_string();

    for (key, value) in [
        ("IdleBacklightDimACSeconds", dim),
        ("IdleSuspendACSeconds", suspend),
        // battery variants: keep consistent on a desktop (no battery, harmless)
        ("IdleBacklightDimBatterySeconds", dim),
        ("IdleSuspendBatterySeconds", suspend),
    ] {
        block = set_kv(&block, key, value);
    }

    let mut out = String::with_capacity(text.len() + 64);
    out.push_str(&text[..open + 1]);
    out.push_str(&block);
    out.push_str(&text[close..]);
    Some(out)
}

/// Replace `"key"  "..."` in a KeyValues block, or insert it after the leading
/// newline if absent.
fn set_kv(block: &str, key: &str, value: &str) -> String {
    let needle = format!("\"{key}\"");
    if let Some(kpos) = block.find(&needle) {
        let after_key = kpos + needle.len();
        // value = next quoted string
        if let Some(vstart_rel) = block[after_key..].find('"') {
            let vstart = after_key + vstart_rel + 1;
            if let Some(vlen) = block[vstart..].find('"') {
                let mut out = String::with_capacity(block.len());
                out.push_str(&block[..vstart]);
                out.push_str(value);
                out.push_str(&block[vstart + vlen..]);
                return out;
            }
        }
        block.to_string()
    } else {
        // insert right after the first newline in the block
        let insert_at = block.find('\n').map(|i| i + 1).unwrap_or(0);
        let indent = "\t\t\t\t\t";
        let mut out = String::with_capacity(block.len() + 64);
        out.push_str(&block[..insert_at]);
        out.push_str(&format!("{indent}\"{key}\"\t\t\"{value}\"\n"));
        out.push_str(&block[insert_at..]);
        out
    }
}

/// Append the CEF flag to the `exec ./steamwebhelper "$@"` line. No-op when
/// already present. Returns the new text or None if the exec line is missing.
pub fn patch_webhelper_wrap(text: &str) -> Option<String> {
    if text.contains(CEF_FLAG) {
        return Some(text.to_string()); // already patched (no-op)
    }
    let marker = "exec ./steamwebhelper \"$@\"";
    if !text.contains(marker) {
        return None;
    }
    Some(text.replace(marker, &format!("exec ./steamwebhelper \"$@\" {CEF_FLAG}")))
}

// ---------------------------------------------------------------------------
// File wrappers (backup + write)
// ---------------------------------------------------------------------------

fn patch_idle_keys_file(path: &Path, dim: &str, suspend: &str) -> Result<bool, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let new =
        patch_idle_keys(&text, dim, suspend).ok_or_else(|| "no \"System\" block found".to_string())?;
    if new == text {
        return Ok(false);
    }
    fs::copy(path, path.with_extension("vdf.bak-game-mode-idle")).map_err(|e| e.to_string())?;
    fs::write(path, new).map_err(|e| e.to_string())?;
    Ok(true)
}

fn patch_webhelper_wrap_file(path: &Path) -> Result<bool, String> {
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

    const VDF: &str = r#""InstallConfigStore"
{
	"Software"
	{
		"Valve"
		{
		}
	}
	"System"
	{
		"IdleBacklightDimBatterySeconds"		"300"
		"IdleBacklightDimACSeconds"		"900"
		"IdleSuspendBatterySeconds"		"900"
		"IdleSuspendACSeconds"		"3600"
		"WifiPowerManagementEnabled"		"1"
		"DisplayBrightness"		"0.75"
	}
}
"#;

    #[test]
    fn replaces_existing_idle_keys() {
        let out = patch_idle_keys(VDF, "900", "0").unwrap();
        assert!(out.contains("\"IdleSuspendACSeconds\"\t\t\"0\""), "{out}");
        assert!(out.contains("\"IdleSuspendBatterySeconds\"\t\t\"0\""));
        assert!(out.contains("\"IdleBacklightDimACSeconds\"\t\t\"900\""));
        // untouched neighbours
        assert!(out.contains("\"WifiPowerManagementEnabled\"\t\t\"1\""));
        assert!(out.contains("\"DisplayBrightness\"\t\t\"0.75\""));
    }

    #[test]
    fn restores_normal_suspend_when_available() {
        let zeroed = patch_idle_keys(VDF, "900", "0").unwrap();
        let restored = patch_idle_keys(&zeroed, "900", "3600").unwrap();
        assert!(restored.contains("\"IdleSuspendACSeconds\"\t\t\"3600\""));
    }

    #[test]
    fn idempotent_when_values_match() {
        let once = patch_idle_keys(VDF, "900", "0").unwrap();
        let twice = patch_idle_keys(&once, "900", "0").unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn inserts_missing_keys() {
        let minimal = "\"Root\"\n{\n\t\"System\"\n\t{\n\t\t\"DisplayBrightness\"\t\t\"0.5\"\n\t}\n}\n";
        let out = patch_idle_keys(minimal, "900", "0").unwrap();
        assert!(out.contains("\"IdleSuspendACSeconds\"\t\t\"0\""));
        assert!(out.contains("\"IdleBacklightDimACSeconds\"\t\t\"900\""));
        assert!(out.contains("\"DisplayBrightness\"\t\t\"0.5\""));
    }

    #[test]
    fn no_system_block_is_an_error() {
        assert!(patch_idle_keys("\"Root\"\n{\n}\n", "900", "0").is_none());
    }

    #[test]
    fn does_not_touch_other_blocks_keys() {
        // A same-named key OUTSIDE System must not be modified.
        let vdf = "\"Root\"\n{\n\t\"Other\"\n\t{\n\t\t\"IdleSuspendACSeconds\"\t\t\"1234\"\n\t}\n\t\"System\"\n\t{\n\t\t\"IdleSuspendACSeconds\"\t\t\"3600\"\n\t}\n}\n";
        let out = patch_idle_keys(vdf, "900", "0").unwrap();
        assert!(out.contains("\"Other\"\n\t{\n\t\t\"IdleSuspendACSeconds\"\t\t\"1234\""), "{out}");
    }

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

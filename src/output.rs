//! Which display the game session should use. gamescope picks an output on
//! its own; `-O` gives it a preference list instead. The rule: 16:9 outputs
//! first (Big Picture's native shape, and what TVs are), then the highest
//! resolution, so the list still falls back cleanly when a display is off.

use std::fs;
use std::path::Path;

const SYSFS_DRM: &str = "/sys/class/drm";

/// A connected output: its connector name as gamescope spells it (`DP-1`)
/// and its largest mode.
#[derive(Debug, PartialEq)]
struct Output {
    connector: String,
    width: u32,
    height: u32,
}

impl Output {
    fn is_16x9(&self) -> bool {
        self.width * 9 == self.height * 16
    }
}

fn parse_mode(line: &str) -> Option<(u32, u32)> {
    let (w, h) = line.trim().split_once('x')?;
    // Interlaced modes are listed as e.g. "1920x1080i".
    let h = h.trim_end_matches(|c: char| !c.is_ascii_digit());
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Connected outputs under a DRM sysfs dir (`card1-DP-1/status`, `/modes`).
fn connected_outputs(drm: &Path) -> Vec<Output> {
    let Ok(entries) = fs::read_dir(drm) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // card1-DP-1 -> DP-1; skips bare cards and render nodes.
            let (card, connector) = name.split_once('-')?;
            if !card.starts_with("card") {
                return None;
            }
            let status = fs::read_to_string(e.path().join("status")).ok()?;
            if status.trim() != "connected" {
                return None;
            }
            let (width, height) = fs::read_to_string(e.path().join("modes"))
                .ok()?
                .lines()
                .filter_map(parse_mode)
                .max_by_key(|(w, h)| w * h)?;
            Some(Output {
                connector: connector.to_string(),
                width,
                height,
            })
        })
        .collect()
}

fn preference(mut outputs: Vec<Output>) -> Vec<String> {
    outputs.sort_by_key(|o| (!o.is_16x9(), std::cmp::Reverse(o.width * o.height)));
    outputs.into_iter().map(|o| o.connector).collect()
}

/// Connected outputs in preference order, comma-joined for gamescope's
/// `-O`. None when no connected output is visible.
pub fn preferred_outputs() -> Option<String> {
    let order = preference(connected_outputs(Path::new(SYSFS_DRM)));
    (!order.is_empty()).then(|| order.join(","))
}

/// Insert `-O <outputs>` into a gamescope argv unless it already names its
/// outputs. Other programs are left alone.
pub fn with_preferred_outputs(command: &[String], outputs: &str) -> Vec<String> {
    let is_gamescope = command
        .first()
        .is_some_and(|p| Path::new(p).file_name().is_some_and(|n| n == "gamescope"));
    let has_output = command
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "-O" || a == "--prefer-output" || a.starts_with("--prefer-output="));
    if !is_gamescope || has_output {
        return command.to_vec();
    }
    let mut out = vec![command[0].clone(), "-O".to_string(), outputs.to_string()];
    out.extend_from_slice(&command[1..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(connector: &str, width: u32, height: u32) -> Output {
        Output {
            connector: connector.to_string(),
            width,
            height,
        }
    }

    #[test]
    fn prefers_largest_16x9_then_the_rest() {
        let order = preference(vec![
            output("HDMI-A-1", 1920, 1080),
            output("DP-2", 3440, 1440),
            output("DP-1", 3840, 2160),
        ]);
        assert_eq!(order, ["DP-1", "HDMI-A-1", "DP-2"]);
    }

    #[test]
    fn reads_connected_outputs_from_sysfs() {
        let drm = std::env::temp_dir().join(format!("game-mode-drm-{}", std::process::id()));
        for (dir, status, modes) in [
            ("card1-DP-1", "connected", "3840x2160\n1920x1080\n"),
            ("card1-DP-2", "connected", "1920x1080\n3440x1440\n"),
            ("card1-HDMI-A-1", "disconnected", ""),
            ("card1-HDMI-A-2", "connected", "1920x1080i\n"),
        ] {
            fs::create_dir_all(drm.join(dir)).unwrap();
            fs::write(drm.join(dir).join("status"), format!("{status}\n")).unwrap();
            fs::write(drm.join(dir).join("modes"), modes).unwrap();
        }
        fs::create_dir_all(drm.join("card1")).unwrap();
        fs::create_dir_all(drm.join("renderD128")).unwrap();
        assert_eq!(
            preference(connected_outputs(&drm)),
            ["DP-1", "HDMI-A-2", "DP-2"]
        );
        fs::remove_dir_all(&drm).unwrap();
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn inserts_outputs_into_gamescope_only() {
        let cmd = argv(&[
            "/usr/bin/gamescope",
            "-e",
            "--",
            "/usr/bin/game-mode-wrapper",
        ]);
        assert_eq!(
            with_preferred_outputs(&cmd, "DP-1,DP-2"),
            argv(&[
                "/usr/bin/gamescope",
                "-O",
                "DP-1,DP-2",
                "-e",
                "--",
                "/usr/bin/game-mode-wrapper"
            ])
        );
        let other = argv(&["/usr/bin/steam", "-O"]);
        assert_eq!(with_preferred_outputs(&other, "DP-1"), other);
    }

    #[test]
    fn keeps_an_explicit_output_choice() {
        for cmd in [
            argv(&["gamescope", "-O", "HDMI-A-1", "--", "x"]),
            argv(&["gamescope", "--prefer-output", "HDMI-A-1", "--", "x"]),
            argv(&["gamescope", "--prefer-output=HDMI-A-1", "--", "x"]),
        ] {
            assert_eq!(with_preferred_outputs(&cmd, "DP-1"), cmd);
        }
        // An -O after "--" belongs to the child, not gamescope.
        let child = argv(&["gamescope", "--", "x", "-O"]);
        assert_eq!(
            with_preferred_outputs(&child, "DP-1"),
            argv(&["gamescope", "-O", "DP-1", "--", "x", "-O"])
        );
    }
}

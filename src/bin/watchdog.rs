//! game-mode-watchdog: in-session guardian against black-screen Big Picture.
//!
//! Spawned by game-mode-wrapper.sh --inner (as the game user, inside the bwrap
//! mask). Empirically-calibrated recoveries (2026-06-12 incident):
//!
//! 1. CEF GPU crash-burst ("GPU process exited unexpectedly" in cef_log.txt,
//!    >=3 within 45s, debounced 300s) → restart steamwebhelper (Steam respawns
//!    its UI; games untouched), then re-sync Steam Input: bounce the
//!    controller's Bluetooth and force input routing to the BP UI (appid 769).
//!    The re-sync ladder exists because a webhelper restart visibly desyncs
//!    Steam Input focus routing.
//! 2. Suspend-wedge ("Closing timeline on system suspend" in console-linux.txt
//!    while no actual suspend happened — e.g. suspend.target masked): in-place
//!    recovery is IMPOSSIBLE (Steam's input pump parks in pre-suspend state and
//!    never resumes; proven live). The correct response is a clean session end
//!    (`steam -shutdown` → greetd falls to the greeter). Prevention is Layer 1
//!    (game-mode-steam-config zeroes IdleSuspendACSeconds while suspend is
//!    unavailable), so this path firing means prevention was bypassed.
//! 3. Every 120s: re-assert the webhelper --disable-gpu-process-crash-limit
//!    flag (Steam updates rewrite the wrap script mid-session).
//!
//! Logs to /tmp/game-mode-watchdog.log.

use std::collections::VecDeque;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

const CEF_MARKER: &str = "GPU process exited unexpectedly";
const WEDGE_MARKER: &str = "Closing timeline on system suspend";
const CRASH_WINDOW: Duration = Duration::from_secs(45);
const CRASH_LIMIT: usize = 3;
const RESTART_DEBOUNCE: Duration = Duration::from_secs(300);
const FLAG_REASSERT_EVERY: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(500);
/// >=2 webhelper restarts in this window with crashes continuing → escalate.
const ESCALATE_WINDOW: Duration = Duration::from_secs(600);

fn main() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into()));
    let logs = home.join(".local/share/Steam/logs");
    let mut wd = Watchdog::new(logs, home);
    wd.log("watchdog started");
    wd.run();
}

// ---------------------------------------------------------------------------
// Pure, unit-tested decision logic
// ---------------------------------------------------------------------------

/// Sliding-window crash counter with restart debounce (port of the desktop
/// daemon's cef_log_watch thresholds).
pub struct CrashWindow {
    crashes: VecDeque<Instant>,
    last_restart: Option<Instant>,
}

impl CrashWindow {
    pub fn new() -> Self {
        CrashWindow { crashes: VecDeque::new(), last_restart: None }
    }

    /// Record a crash at `now`; returns true if a restart should fire.
    pub fn on_crash(&mut self, now: Instant) -> bool {
        self.crashes.push_back(now);
        while let Some(&front) = self.crashes.front() {
            if now.duration_since(front) > CRASH_WINDOW {
                self.crashes.pop_front();
            } else {
                break;
            }
        }
        if self.crashes.len() < CRASH_LIMIT {
            return false;
        }
        if let Some(last) = self.last_restart {
            if now.duration_since(last) < RESTART_DEBOUNCE {
                return false;
            }
        }
        self.crashes.clear();
        self.last_restart = Some(now);
        true
    }
}

/// Escalation tracker: N recoveries inside the window.
pub struct Escalation {
    restarts: VecDeque<Instant>,
}

impl Escalation {
    pub fn new() -> Self {
        Escalation { restarts: VecDeque::new() }
    }
    /// Record a webhelper recovery; true → escalate to session end.
    pub fn on_recovery(&mut self, now: Instant) -> bool {
        self.restarts.push_back(now);
        while let Some(&front) = self.restarts.front() {
            if now.duration_since(front) > ESCALATE_WINDOW {
                self.restarts.pop_front();
            } else {
                break;
            }
        }
        self.restarts.len() >= 2
    }
}

/// A rotation-aware log tail. Returns complete new lines since the last call.
pub struct LogTail {
    path: PathBuf,
    inode: u64,
    offset: u64,
    carry: String,
}

impl LogTail {
    /// Opens at end-of-file (history is not replayed).
    pub fn new(path: PathBuf) -> Self {
        let (inode, offset) = match fs::metadata(&path) {
            Ok(m) => (m.ino(), m.len()),
            Err(_) => (0, 0),
        };
        LogTail { path, inode, offset, carry: String::new() }
    }

    pub fn read_new_lines(&mut self) -> Vec<String> {
        let Ok(meta) = fs::metadata(&self.path) else { return Vec::new() };
        // rotation/truncation: new inode or shrunk file → start from 0
        if meta.ino() != self.inode || meta.len() < self.offset {
            self.inode = meta.ino();
            self.offset = 0;
            self.carry.clear();
        }
        if meta.len() == self.offset {
            return Vec::new();
        }
        let Ok(mut f) = fs::File::open(&self.path) else { return Vec::new() };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = String::new();
        use std::io::Read;
        let Ok(n) = f.take(1 << 20).read_to_string(&mut buf) else {
            // non-utf8 chunk: skip it wholesale
            self.offset = meta.len();
            return Vec::new();
        };
        self.offset += n as u64;
        let combined = format!("{}{}", self.carry, buf);
        let mut lines: Vec<String> = combined.split('\n').map(str::to_string).collect();
        self.carry = lines.pop().unwrap_or_default(); // tail partial line
        lines.into_iter().filter(|l| !l.is_empty()).collect()
    }
}

/// Did a *real* suspend happen? `/sys/power/suspend_stats/success` increments
/// only on completed suspends; with suspend masked it never moves.
pub fn suspend_success_count() -> u64 {
    fs::read_to_string("/sys/power/suspend_stats/success")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------------

struct Watchdog {
    cef: LogTail,
    console: LogTail,
    crash: CrashWindow,
    escalate: Escalation,
    home: PathBuf,
    log_file: PathBuf,
    last_flag_check: Instant,
}

impl Watchdog {
    fn new(steam_logs: PathBuf, home: PathBuf) -> Self {
        Watchdog {
            cef: LogTail::new(steam_logs.join("cef_log.txt")),
            console: LogTail::new(steam_logs.join("console-linux.txt")),
            crash: CrashWindow::new(),
            escalate: Escalation::new(),
            home,
            log_file: PathBuf::from("/tmp/game-mode-watchdog.log"),
            last_flag_check: Instant::now(),
        }
    }

    fn log(&self, msg: &str) {
        let ts = humantime(SystemTime::now());
        let line = format!("[{ts}] {msg}\n");
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&self.log_file) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn run(&mut self) {
        loop {
            let now = Instant::now();

            // 1. CEF crash bursts
            for line in self.cef.read_new_lines() {
                if line.contains(CEF_MARKER) {
                    self.log(&format!("cef crash: {}", &line[..line.len().min(160)]));
                    if self.crash.on_crash(now) {
                        if self.escalate.on_recovery(now) && !game_running() {
                            self.log("repeated crash bursts with no game running — ending session");
                            self.end_session();
                        } else {
                            self.recover_webhelper();
                        }
                    }
                }
            }

            // 2. Suspend wedge
            for line in self.console.read_new_lines() {
                if line.contains(WEDGE_MARKER) {
                    self.log("steam entered suspend flow; verifying a real suspend follows…");
                    let before = suspend_success_count();
                    std::thread::sleep(Duration::from_secs(25));
                    if suspend_success_count() > before {
                        self.log("real suspend+resume detected — no action (normal sleep)");
                    } else {
                        self.log(
                            "SUSPEND WEDGE: suspend never completed (masked/inhibited?). \
                             In-place recovery is impossible — ending session cleanly.",
                        );
                        self.end_session();
                    }
                }
            }

            // 3. Webhelper flag re-assertion
            if now.duration_since(self.last_flag_check) >= FLAG_REASSERT_EVERY {
                self.last_flag_check = now;
                self.reassert_webhelper_flag();
            }

            std::thread::sleep(POLL);
        }
    }

    /// Tier 1 recovery: webhelper restart + controller BT bounce + input
    /// re-route (the empirically-derived ladder).
    fn recover_webhelper(&self) {
        self.log("RECOVERY: restarting steamwebhelper (games keep running)");
        let _ = Command::new("pkill").args(["-x", "steamwebhelper"]).status();
        std::thread::sleep(Duration::from_secs(6));

        // Re-sync Steam Input: bounce the controller's BT connection…
        if let Some(mac) = connected_xbox_mac() {
            self.log(&format!("re-sync: bouncing controller bluetooth {mac}"));
            let _ = Command::new("bluetoothctl").args(["disconnect", &mac]).status();
            std::thread::sleep(Duration::from_secs(2));
            let _ = Command::new("bluetoothctl").args(["connect", &mac]).status();
            std::thread::sleep(Duration::from_secs(3));
        }
        // …and force input routing back to the BP UI.
        self.log("re-sync: forcing Steam Input routing to BP (appid 769)");
        let _ = Command::new("steam").arg("steam://forceinputappid/769").spawn();
        self.log("recovery complete — BP should repaint with working input");
    }

    /// Tier 2: clean session end (greetd falls through to the greeter).
    fn end_session(&self) {
        let _ = Command::new("steam").arg("-shutdown").status();
        // If Steam ignores it (wedged hard), the session root dies with us:
        std::thread::sleep(Duration::from_secs(30));
        if game_running() || steam_running() {
            self.log("steam -shutdown ignored; sending SIGTERM to steam");
            let _ = Command::new("pkill").args(["-x", "steam"]).status();
        }
    }

    fn reassert_webhelper_flag(&self) {
        let wrap = self.home.join(".local/share/Steam/ubuntu12_64/steamwebhelper_sniper_wrap.sh");
        let Ok(text) = fs::read_to_string(&wrap) else { return };
        if text.contains("--disable-gpu-process-crash-limit") {
            return;
        }
        let marker = "exec ./steamwebhelper \"$@\"";
        if !text.contains(marker) {
            return;
        }
        let new = text.replace(marker, "exec ./steamwebhelper \"$@\" --disable-gpu-process-crash-limit");
        if fs::write(&wrap, new).is_ok() {
            self.log("re-applied --disable-gpu-process-crash-limit (Steam had reverted it)");
        }
    }
}

fn steam_running() -> bool {
    proc_comm_exists("steam")
}

/// A game is running iff a Steam reaper child exists.
fn game_running() -> bool {
    fs::read_dir("/proc")
        .map(|d| {
            d.filter_map(|e| e.ok()).any(|e| {
                fs::read_to_string(e.path().join("cmdline"))
                    .is_ok_and(|c| c.contains("reaper") && c.contains("SteamLaunch"))
            })
        })
        .unwrap_or(false)
}

fn proc_comm_exists(name: &str) -> bool {
    fs::read_dir("/proc")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .any(|e| fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == name))
        })
        .unwrap_or(false)
}

/// MAC of the connected Xbox controller, if any.
fn connected_xbox_mac() -> Option<String> {
    let out = Command::new("bluetoothctl").args(["devices", "Connected"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.to_lowercase().contains("xbox"))
        .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
}

fn humantime(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn crash_window_requires_three_in_window() {
        let mut w = CrashWindow::new();
        let base = t0();
        assert!(!w.on_crash(base));
        assert!(!w.on_crash(base + Duration::from_secs(10)));
        assert!(w.on_crash(base + Duration::from_secs(20)), "3rd within 45s fires");
    }

    #[test]
    fn crash_window_expires_old_crashes() {
        let mut w = CrashWindow::new();
        let base = t0();
        assert!(!w.on_crash(base));
        assert!(!w.on_crash(base + Duration::from_secs(10)));
        // 3rd crash arrives after the first slid out of the 45s window
        assert!(!w.on_crash(base + Duration::from_secs(50)), "only 2 in window");
    }

    #[test]
    fn restart_debounce_blocks_rapid_refire() {
        let mut w = CrashWindow::new();
        let base = t0();
        w.on_crash(base);
        w.on_crash(base + Duration::from_secs(1));
        assert!(w.on_crash(base + Duration::from_secs(2)), "first restart");
        // a fresh burst right after must be debounced
        w.on_crash(base + Duration::from_secs(60));
        w.on_crash(base + Duration::from_secs(61));
        assert!(!w.on_crash(base + Duration::from_secs(62)), "debounced (<300s)");
        // …but fires again past the debounce
        w.on_crash(base + Duration::from_secs(400));
        w.on_crash(base + Duration::from_secs(401));
        assert!(w.on_crash(base + Duration::from_secs(402)), "after debounce");
    }

    #[test]
    fn escalation_after_two_recoveries_in_window() {
        let mut e = Escalation::new();
        let base = t0();
        assert!(!e.on_recovery(base));
        assert!(e.on_recovery(base + Duration::from_secs(300)), "2nd within 10min");
        // far-apart recoveries don't escalate
        let mut e = Escalation::new();
        assert!(!e.on_recovery(base));
        assert!(!e.on_recovery(base + Duration::from_secs(700)), "outside window");
    }

    #[test]
    fn log_tail_reads_only_new_lines_and_handles_rotation() {
        let dir = std::env::temp_dir().join(format!("gmwd-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        fs::write(&path, "old line\n").unwrap();

        let mut tail = LogTail::new(path.clone());
        assert!(tail.read_new_lines().is_empty(), "starts at EOF — no history replay");

        // append two lines + one partial
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"crash A\ncrash B\npart").unwrap();
        drop(f);
        assert_eq!(tail.read_new_lines(), vec!["crash A".to_string(), "crash B".to_string()]);

        // complete the partial line
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"ial C\n").unwrap();
        drop(f);
        assert_eq!(tail.read_new_lines(), vec!["partial C".to_string()]);

        // rotation: replace with a fresh shorter file → read from 0
        fs::remove_file(&path).unwrap();
        fs::write(&path, "fresh\n").unwrap();
        assert_eq!(tail.read_new_lines(), vec!["fresh".to_string()]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn markers_match_real_log_lines() {
        let cef = "[3843278:0612/110501:ERROR:gpu_process_host.cc(1002)] GPU process exited unexpectedly: exit_code=8704";
        assert!(cef.contains(CEF_MARKER));
        let wedge = "[2026-06-12 11:05:15] Game Recording - Closing timeline on system suspend [gameid=13795919238117982208]";
        assert!(wedge.contains(WEDGE_MARKER));
    }
}

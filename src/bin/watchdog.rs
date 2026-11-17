use game_mode::steam::{self, steam_running, CEF_FLAG};
use std::collections::VecDeque;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

const CEF_MARKER: &str = "GPU process exited unexpectedly";
const WEDGE_MARKER: &str = "Closing timeline on system suspend";
const CRASH_WINDOW: Duration = Duration::from_secs(45);
const CRASH_LIMIT: usize = 3;
const RESTART_DEBOUNCE: Duration = Duration::from_secs(300);
const FLAG_REASSERT_EVERY: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(500);
const ESCALATE_WINDOW: Duration = Duration::from_secs(600);

fn main() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into()));
    let logs = steam::steam_root(&home).join("logs");
    let mut wd = Watchdog::new(logs, home);
    wd.log("watchdog started");
    wd.run();
}

pub struct CrashWindow {
    crashes: VecDeque<Instant>,
    last_restart: Option<Instant>,
}

impl Default for CrashWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl CrashWindow {
    pub fn new() -> Self {
        CrashWindow {
            crashes: VecDeque::new(),
            last_restart: None,
        }
    }

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

pub struct Escalation {
    restarts: VecDeque<Instant>,
}

impl Default for Escalation {
    fn default() -> Self {
        Self::new()
    }
}

impl Escalation {
    pub fn new() -> Self {
        Escalation {
            restarts: VecDeque::new(),
        }
    }
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

pub struct LogTail {
    path: PathBuf,
    inode: u64,
    offset: u64,
    carry: String,
}

impl LogTail {
    pub fn new(path: PathBuf) -> Self {
        let (inode, offset) = match fs::metadata(&path) {
            Ok(m) => (m.ino(), m.len()),
            Err(_) => (0, 0),
        };
        LogTail {
            path,
            inode,
            offset,
            carry: String::new(),
        }
    }

    pub fn read_new_lines(&mut self) -> Vec<String> {
        let Ok(meta) = fs::metadata(&self.path) else {
            return Vec::new();
        };
        if meta.ino() != self.inode || meta.len() < self.offset {
            self.inode = meta.ino();
            self.offset = 0;
            self.carry.clear();
        }
        if meta.len() == self.offset {
            return Vec::new();
        }
        let Ok(mut f) = fs::File::open(&self.path) else {
            return Vec::new();
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = String::new();
        use std::io::Read;
        let Ok(n) = f.take(1 << 20).read_to_string(&mut buf) else {
            self.offset = meta.len();
            return Vec::new();
        };
        self.offset += n as u64;
        let combined = format!("{}{}", self.carry, buf);
        let mut lines: Vec<String> = combined.split('\n').map(str::to_string).collect();
        self.carry = lines.pop().unwrap_or_default();
        lines.into_iter().filter(|l| !l.is_empty()).collect()
    }
}

pub fn suspend_success_count() -> u64 {
    fs::read_to_string("/sys/power/suspend_stats/success")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

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
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn run(&mut self) {
        loop {
            let now = Instant::now();

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

            if now.duration_since(self.last_flag_check) >= FLAG_REASSERT_EVERY {
                self.last_flag_check = now;
                self.reassert_webhelper_flag();
            }

            std::thread::sleep(POLL);
        }
    }

    fn recover_webhelper(&self) {
        self.log("RECOVERY: restarting steamwebhelper (games keep running)");
        let _ = Command::new("pkill")
            .args(["-x", "steamwebhelper"])
            .status();
        std::thread::sleep(Duration::from_secs(6));

        for mac in connected_gamepads() {
            self.log(&format!("re-sync: bouncing controller bluetooth {mac}"));
            let _ = Command::new("bluetoothctl")
                .args(["disconnect", &mac])
                .status();
            std::thread::sleep(Duration::from_secs(2));
            let _ = Command::new("bluetoothctl")
                .args(["connect", &mac])
                .status();
            std::thread::sleep(Duration::from_secs(3));
        }
        self.log("re-sync: forcing Steam Input routing to BP (appid 769)");
        let _ = Command::new("steam")
            .arg("steam://forceinputappid/769")
            .spawn();
        self.log("recovery complete — BP should repaint with working input");
    }

    fn end_session(&self) {
        let _ = Command::new("steam").arg("-shutdown").status();
        std::thread::sleep(Duration::from_secs(30));
        if game_running() || steam_running() {
            self.log("steam -shutdown ignored; sending SIGTERM to steam");
            let _ = Command::new("pkill").args(["-x", "steam"]).status();
        }
    }

    fn reassert_webhelper_flag(&self) {
        if let Ok(true) = steam::patch_webhelper_wrap_file(&steam::webhelper_wrap(&self.home)) {
            self.log(&format!("re-applied {CEF_FLAG} (Steam had reverted it)"));
        }
    }
}

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

fn connected_gamepads() -> Vec<String> {
    let bluetoothctl = |args: &[&str]| {
        Command::new("bluetoothctl")
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    bluetoothctl(&["devices", "Connected"])
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .filter(|mac| bluetoothctl(&["info", mac]).contains("Icon: input-gaming"))
        .collect()
}

fn humantime(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
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
        assert!(
            w.on_crash(base + Duration::from_secs(20)),
            "3rd within 45s fires"
        );
    }

    #[test]
    fn crash_window_expires_old_crashes() {
        let mut w = CrashWindow::new();
        let base = t0();
        assert!(!w.on_crash(base));
        assert!(!w.on_crash(base + Duration::from_secs(10)));
        assert!(
            !w.on_crash(base + Duration::from_secs(50)),
            "only 2 in window"
        );
    }

    #[test]
    fn restart_debounce_blocks_rapid_refire() {
        let mut w = CrashWindow::new();
        let base = t0();
        w.on_crash(base);
        w.on_crash(base + Duration::from_secs(1));
        assert!(w.on_crash(base + Duration::from_secs(2)), "first restart");
        w.on_crash(base + Duration::from_secs(60));
        w.on_crash(base + Duration::from_secs(61));
        assert!(
            !w.on_crash(base + Duration::from_secs(62)),
            "debounced (<300s)"
        );
        w.on_crash(base + Duration::from_secs(400));
        w.on_crash(base + Duration::from_secs(401));
        assert!(
            w.on_crash(base + Duration::from_secs(402)),
            "after debounce"
        );
    }

    #[test]
    fn escalation_after_two_recoveries_in_window() {
        let mut e = Escalation::new();
        let base = t0();
        assert!(!e.on_recovery(base));
        assert!(
            e.on_recovery(base + Duration::from_secs(300)),
            "2nd within 10min"
        );
        let mut e = Escalation::new();
        assert!(!e.on_recovery(base));
        assert!(
            !e.on_recovery(base + Duration::from_secs(700)),
            "outside window"
        );
    }

    #[test]
    fn log_tail_reads_only_new_lines_and_handles_rotation() {
        let dir = std::env::temp_dir().join(format!("gmwd-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        fs::write(&path, "old line\n").unwrap();

        let mut tail = LogTail::new(path.clone());
        assert!(
            tail.read_new_lines().is_empty(),
            "starts at EOF — no history replay"
        );

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"crash A\ncrash B\npart").unwrap();
        drop(f);
        assert_eq!(
            tail.read_new_lines(),
            vec!["crash A".to_string(), "crash B".to_string()]
        );

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"ial C\n").unwrap();
        drop(f);
        assert_eq!(tail.read_new_lines(), vec!["partial C".to_string()]);

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

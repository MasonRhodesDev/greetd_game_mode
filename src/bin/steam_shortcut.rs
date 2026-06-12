//! Idempotently add a non-Steam shortcut to every Steam user's
//! userdata/<id>/config/shortcuts.vdf (binary VDF). Steam must be closed —
//! it rewrites the file from memory on exit.
//!
//! Usage: game-mode-steam-shortcut --name Discord --exe /usr/local/bin/game-mode-discord
//!
//! Exit: 0 = added or already present (per existing userdata), 1 = bad args
//! or no userdata found, 2 = parse/write failure.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const TYPE_MAP: u8 = 0x00;
const TYPE_STRING: u8 = 0x01;
const TYPE_INT: u8 = 0x02;
const END_MAP: u8 = 0x08;

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn put_string(out: &mut Vec<u8>, key: &str, value: &str) {
    out.push(TYPE_STRING);
    out.extend_from_slice(key.as_bytes());
    out.push(0);
    out.extend_from_slice(value.as_bytes());
    out.push(0);
}

fn put_int(out: &mut Vec<u8>, key: &str, value: u32) {
    out.push(TYPE_INT);
    out.extend_from_slice(key.as_bytes());
    out.push(0);
    out.extend_from_slice(&value.to_le_bytes());
}

/// Steam's classic non-Steam appid: crc32(Exe + AppName) with the top bit set.
/// Grid art (incl. the square `<appid>_icon.png`) keys off this value.
fn shortcut_appid(exe_quoted: &str, name: &str) -> u32 {
    crc32(format!("{exe_quoted}{name}").as_bytes()) | 0x8000_0000
}

/// Serialize one shortcut entry map keyed by its index.
fn entry_bytes(index: usize, name: &str, exe_quoted: &str, start_dir_quoted: &str) -> Vec<u8> {
    let appid = shortcut_appid(exe_quoted, name);

    let mut e = Vec::new();
    e.push(TYPE_MAP);
    e.extend_from_slice(index.to_string().as_bytes());
    e.push(0);
    put_int(&mut e, "appid", appid);
    put_string(&mut e, "appname", name);
    put_string(&mut e, "Exe", exe_quoted);
    put_string(&mut e, "StartDir", start_dir_quoted);
    put_string(&mut e, "icon", "");
    put_string(&mut e, "ShortcutPath", "");
    put_string(&mut e, "LaunchOptions", "");
    put_int(&mut e, "IsHidden", 0);
    put_int(&mut e, "AllowDesktopConfig", 1); // Steam Input desktop layout
    put_int(&mut e, "AllowOverlay", 1);
    put_int(&mut e, "OpenVR", 0);
    put_int(&mut e, "Devkit", 0);
    put_string(&mut e, "DevkitGameID", "");
    put_int(&mut e, "DevkitOverrideAppID", 0);
    put_int(&mut e, "LastPlayTime", 0);
    put_string(&mut e, "FlatpakAppID", "");
    // empty tags map
    e.push(TYPE_MAP);
    e.extend_from_slice(b"tags");
    e.push(0);
    e.push(END_MAP);
    e.push(END_MAP); // close entry
    e
}

/// Byte span (start..end) of a string VALUE inside the buffer (excl. NUL).
type Span = (usize, usize);

#[derive(Default)]
struct EntryInfo {
    name: String,
    appid: u32,
    exe_span: Option<Span>,
    dir_span: Option<Span>,
}

struct Scanner<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.buf.get(self.pos).ok_or("unexpected EOF")?;
        self.pos += 1;
        Ok(b)
    }
    fn cstring(&mut self) -> Result<(String, Span), String> {
        let start = self.pos;
        while *self.buf.get(self.pos).ok_or("unexpected EOF in string")? != 0 {
            self.pos += 1;
        }
        let s = String::from_utf8_lossy(&self.buf[start..self.pos]).into_owned();
        let span = (start, self.pos);
        self.pos += 1; // NUL
        Ok((s, span))
    }
    /// Consume a map body (after its key), recording appname/Exe/StartDir.
    fn map_body(&mut self, info: &mut EntryInfo) -> Result<(), String> {
        loop {
            match self.byte()? {
                END_MAP => return Ok(()),
                TYPE_MAP => {
                    let _ = self.cstring()?;
                    self.map_body(info)?;
                }
                TYPE_STRING => {
                    let (key, _) = self.cstring()?;
                    let (val, span) = self.cstring()?;
                    if key.eq_ignore_ascii_case("appname") {
                        info.name = val;
                    } else if key.eq_ignore_ascii_case("exe") {
                        info.exe_span = Some(span);
                    } else if key.eq_ignore_ascii_case("startdir") {
                        info.dir_span = Some(span);
                    }
                }
                TYPE_INT => {
                    let (key, _) = self.cstring()?;
                    if self.pos + 4 > self.buf.len() {
                        return Err("unexpected EOF in int".into());
                    }
                    if key.eq_ignore_ascii_case("appid") {
                        let bytes = &self.buf[self.pos..self.pos + 4];
                        info.appid = u32::from_le_bytes(bytes.try_into().unwrap());
                    }
                    self.pos += 4;
                }
                t => return Err(format!("unknown vdf type byte 0x{t:02x} at {}", self.pos - 1)),
            }
        }
    }
}

/// Parse shortcuts.vdf into per-entry info.
fn parse(buf: &[u8]) -> Result<Vec<EntryInfo>, String> {
    let mut s = Scanner { buf, pos: 0 };
    if s.byte()? != TYPE_MAP {
        return Err("file does not start with a map".into());
    }
    let (root, _) = s.cstring()?;
    if !root.eq_ignore_ascii_case("shortcuts") {
        return Err(format!("unexpected root key {root:?}"));
    }
    let mut entries = Vec::new();
    loop {
        match s.byte()? {
            END_MAP => break, // end of "shortcuts"
            TYPE_MAP => {
                let _ = s.cstring()?;
                let mut info = EntryInfo::default();
                s.map_body(&mut info)?;
                entries.push(info);
            }
            t => return Err(format!("unexpected type 0x{t:02x} in shortcuts map")),
        }
    }
    Ok(entries)
}

/// Replace byte spans (value bytes, excl. NUL) with new strings. Spans must
/// be disjoint; applied back-to-front so offsets stay valid.
fn splice(buf: &[u8], mut edits: Vec<(Span, String)>) -> Vec<u8> {
    edits.sort_by_key(|((start, _), _)| std::cmp::Reverse(*start));
    let mut out = buf.to_vec();
    for ((start, end), new) in edits {
        out.splice(start..end, new.into_bytes());
    }
    out
}

fn add_to_file(path: &Path, name: &str, exe: &str, start_dir: &str) -> Result<(&'static str, u32), String> {
    let exe_quoted = format!("\"{exe}\"");
    let dir_quoted = format!("\"{start_dir}\"");
    let new_appid = shortcut_appid(&exe_quoted, name);

    let buf = match fs::read(path) {
        Ok(b) if !b.is_empty() => b,
        _ => {
            // No shortcuts yet: create the whole file.
            let mut out = Vec::new();
            out.push(TYPE_MAP);
            out.extend_from_slice(b"shortcuts");
            out.push(0);
            out.extend_from_slice(&entry_bytes(0, name, &exe_quoted, &dir_quoted));
            out.push(END_MAP);
            out.push(END_MAP);
            fs::write(path, out).map_err(|e| e.to_string())?;
            return Ok(("created", new_appid));
        }
    };

    let entries = parse(&buf)?;

    if let Some(existing) = entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)) {
        // Same name: make sure it points at our exe. The appid field is left
        // untouched so grid art and controller configs stay attached.
        let current_exe = existing
            .exe_span
            .map(|(s, e)| String::from_utf8_lossy(&buf[s..e]).into_owned())
            .unwrap_or_default();
        if current_exe == exe_quoted {
            return Ok(("unchanged", existing.appid));
        }
        let mut edits = Vec::new();
        if let Some(span) = existing.exe_span {
            edits.push((span, exe_quoted.clone()));
        }
        if let Some(span) = existing.dir_span {
            edits.push((span, dir_quoted.clone()));
        }
        if edits.is_empty() {
            return Err("existing entry has no Exe field to update".into());
        }
        fs::copy(path, path.with_extension("vdf.bak-game-mode")).map_err(|e| e.to_string())?;
        fs::write(path, splice(&buf, edits)).map_err(|e| e.to_string())?;
        return Ok(("updated", existing.appid));
    }

    if buf.len() < 2 || buf[buf.len() - 2] != END_MAP || buf[buf.len() - 1] != END_MAP {
        return Err("file does not end with the expected map terminators".into());
    }
    fs::copy(path, path.with_extension("vdf.bak-game-mode")).map_err(|e| e.to_string())?;
    let mut out = buf[..buf.len() - 2].to_vec();
    out.extend_from_slice(&entry_bytes(entries.len(), name, &exe_quoted, &dir_quoted));
    out.push(END_MAP);
    out.push(END_MAP);
    fs::write(path, out).map_err(|e| e.to_string())?;
    Ok(("added", new_appid))
}

/// Copy `src` to `<grid>/<appid>_icon.png` (the square library icon), unless
/// an identical file is already there. Returns Ok(true) if written.
fn write_icon_art(config_dir: &Path, appid: u32, src: &Path) -> Result<bool, String> {
    let grid = config_dir.join("grid");
    fs::create_dir_all(&grid).map_err(|e| e.to_string())?;
    let dst = grid.join(format!("{appid}_icon.png"));
    let data = fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    if fs::read(&dst).map(|cur| cur == data).unwrap_or(false) {
        return Ok(false);
    }
    fs::write(&dst, &data).map_err(|e| e.to_string())?;
    Ok(true)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut name = None;
    let mut exe = None;
    let mut icon = None;
    let mut i = 1;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--name" => name = Some(args[i + 1].clone()),
            "--exe" => exe = Some(args[i + 1].clone()),
            "--icon" => icon = Some(args[i + 1].clone()),
            _ => {}
        }
        i += 2;
    }
    let (Some(name), Some(exe)) = (name, exe) else {
        eprintln!("usage: game-mode-steam-shortcut --name <AppName> --exe </path/to/exe> [--icon <png>]");
        std::process::exit(1);
    };
    let icon = icon.map(PathBuf::from);
    let start_dir = Path::new(&exe)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());

    if fs::read_dir("/proc")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .any(|e| fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "steam"))
        })
        .unwrap_or(false)
    {
        eprintln!("Steam is running — it rewrites shortcuts.vdf on exit. Close it and re-run.");
        std::process::exit(2);
    }

    let home = env::var("HOME").unwrap_or_default();
    let userdata = PathBuf::from(&home).join(".local/share/Steam/userdata");
    let Ok(users) = fs::read_dir(&userdata) else {
        eprintln!("no Steam userdata at {} (log into Steam once first)", userdata.display());
        std::process::exit(1);
    };

    let mut touched = false;
    for entry in users.filter_map(|e| e.ok()) {
        let id = entry.file_name().to_string_lossy().into_owned();
        if !id.chars().all(|c| c.is_ascii_digit()) || id == "0" {
            continue;
        }
        let config_dir = entry.path().join("config");
        if !config_dir.is_dir() {
            continue;
        }
        let vdf = config_dir.join("shortcuts.vdf");
        match add_to_file(&vdf, &name, &exe, &start_dir) {
            Ok((outcome, appid)) => {
                println!("{name:?} shortcut {outcome} for Steam user {id}");
                touched = true;
                // Square library icon (<appid>_icon.png): Steam stores a
                // misnamed .ico for some shortcuts, which gamepadui can't
                // render (blank grey square). A real PNG fixes it.
                if let Some(ref src) = icon {
                    match write_icon_art(&config_dir, appid, src) {
                        Ok(true) => println!("  wrote square icon {appid}_icon.png"),
                        Ok(false) => {}
                        Err(e) => eprintln!("  icon art skipped: {e}"),
                    }
                }
            }
            Err(e) => {
                eprintln!("user {id}: {e}");
                std::process::exit(2);
            }
        }
    }
    if !touched {
        eprintln!("no Steam users found under {}", userdata.display());
        std::process::exit(1);
    }
}

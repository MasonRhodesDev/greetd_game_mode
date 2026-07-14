//! `game-mode setup` — one-time host provisioning.
//!
//! The package installs files; this subcommand does everything that is
//! host-specific (the job the old install.sh mixed into file installation):
//!
//!   - prompts for the game-session user/group/library dir and writes the
//!     runtime config /etc/game-mode/config.toml
//!   - creates the games user/group and the shared game library directory
//!   - applies the packaged sysusers/tmpfiles snippets (access-gate user,
//!     /var/lib/access-gate)
//!   - sets up /etc/greetd group ownership + logs dir
//!   - renders the shipped greetd templates from /usr/share/game-mode/greetd
//!     into /etc/greetd (greetd itself needs concrete values on disk) and
//!     swaps the config.toml symlink to the greeter config
//!   - renders and installs the sudoers grant for the greeter user
//!   - checks tailscale and writes /etc/game-mode/approval.env (the WebAuthn
//!     verifier's RP ID / origin come from the tailnet FQDN)
//!   - enables the systemd units
//!
//! Idempotent: re-run it after upgrades or to reconfigure (existing answers
//! in config.toml become the new defaults).

use anyhow::{bail, Context, Result};
use dialoguer::{Confirm, Input};
use serde_json::Value;
use std::fs;
use std::io::IsTerminal;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::config::{self, Config};
use crate::game_mode_switch;

const SHARE_GREETD: &str = "/usr/share/game-mode/greetd";
const SUDOERS_TEMPLATE: &str = "/usr/share/game-mode/sudoers/greeter-greetd";
const SUDOERS_PATH: &str = "/etc/sudoers.d/greeter-greetd";
const ETC_DIR: &str = "/etc/game-mode";
const APPROVAL_ENV: &str = "/etc/game-mode/approval.env";

/// Files copied verbatim from /usr/share/game-mode/greetd to /etc/greetd.
const STATIC_FILES: &[&str] = &["regreet.toml", "bg.png", "environments"];
/// Files rendered ({{vt}}, {{games_user}}) into /etc/greetd.
const TEMPLATE_FILES: &[&str] = &["config_default.toml", "game_mode_login.toml"];

pub fn run() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("game-mode setup must run as root: sudo game-mode setup");
    }
    let interactive = std::io::stdin().is_terminal();

    // Existing config (or defaults) seeds the prompts, so re-runs are quiet
    // confirmations rather than re-interrogations.
    let cfg = Config::load()?;
    let mut user = cfg.session.user.clone();
    let mut group = cfg.session.group.clone();
    let mut dir = cfg.session.dir.clone();
    let vt = cfg.terminal.vt;

    if interactive {
        println!("Configure the game session (Enter accepts the default):");
        user = Input::new()
            .with_prompt("  autologin user")
            .default(user)
            .interact_text()?;
        group = Input::new()
            .with_prompt("  group")
            .default(group)
            .interact_text()?;
        dir = Input::new()
            .with_prompt("  game library dir")
            .default(dir)
            .interact_text()?;
    } else {
        println!("Non-interactive: using user={user} group={group} dir={dir}");
    }

    write_runtime_config(vt, &user, &group, &dir)?;
    ensure_games_identity(&user, &group)?;
    setup_games_dir(&dir, &group)?;
    apply_sysusers_tmpfiles();
    setup_greetd_dir(&cfg)?;
    deploy_greetd_files(&cfg, vt, &user)?;
    verify_greeter_binaries()?;
    install_sudoers(&cfg.permissions.greeter_user)?;
    // Land on the greeter config (atomic symlink swap, same code path the
    // daemon uses to reset after a game session).
    game_mode_switch::switch_to_desktop_mode()?;
    tailscale_approval_env()?;
    enable_services(interactive)?;
    print_next_steps();
    Ok(())
}

fn run_checked(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

fn write_runtime_config(vt: u32, user: &str, group: &str, dir: &str) -> Result<()> {
    fs::create_dir_all(ETC_DIR)?;
    let text = format!(
        "# game-mode runtime configuration — written by `game-mode setup`.\n\
         # Read by the game-mode daemon and the game-mode-wrapper session script.\n\
         # Re-run `sudo game-mode setup` after editing (the greetd session\n\
         # configs under /etc/greetd are rendered from these values).\n\
         \n\
         [terminal]\n\
         vt = {vt}\n\
         \n\
         [session]\n\
         user = \"{user}\"\n\
         group = \"{group}\"\n\
         dir = \"{dir}\"\n"
    );
    fs::write(config::CONFIG_TOML, text)
        .with_context(|| format!("failed to write {}", config::CONFIG_TOML))?;
    println!("Wrote {}", config::CONFIG_TOML);
    Ok(())
}

fn exists_group(name: &str) -> bool {
    Command::new("getent")
        .args(["group", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn exists_user(name: &str) -> bool {
    Command::new("getent")
        .args(["passwd", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ensure_games_identity(user: &str, group: &str) -> Result<()> {
    if !exists_group(group) {
        println!("Creating group {group}...");
        run_checked("groupadd", &[group])?;
    }
    if !exists_user(user) {
        println!("Creating user {user}...");
        run_checked("useradd", &["-m", "-g", group, "-s", "/bin/bash", user])?;
    }
    Ok(())
}

fn setup_games_dir(dir: &str, group: &str) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {dir}"))?;
    run_checked("chgrp", &["-R", group, dir])?;
    // setgid + group-writable so everything created inside stays shared.
    run_checked("chmod", &["-R", "g+rwxs", dir])?;
    Ok(())
}

/// Apply the packaged sysusers/tmpfiles snippets (access-gate system user,
/// /var/lib/access-gate). Normally the package manager already ran these;
/// re-applying is idempotent and covers manual installs.
fn apply_sysusers_tmpfiles() {
    let _ = Command::new("systemd-sysusers").status();
    let _ = Command::new("systemd-tmpfiles").arg("--create").status();
}

fn setup_greetd_dir(cfg: &Config) -> Result<()> {
    let greetd_dir = cfg.get_greetd_dir();
    let greeter = &cfg.permissions.greeter_user;
    fs::create_dir_all(greetd_dir.join("logs"))?;
    run_checked("chgrp", &["-R", greeter, greetd_dir.to_str().unwrap()])?;
    run_checked("chmod", &["g+rwxs", greetd_dir.to_str().unwrap()])?;
    Ok(())
}

/// Copy the static greeter payload and render the templated greetd configs
/// into /etc/greetd. greetd (and regreet/Hyprland) read these files directly,
/// so they must exist with concrete values — everything else reads
/// /etc/game-mode/config.toml at runtime instead.
fn deploy_greetd_files(cfg: &Config, vt: u32, games_user: &str) -> Result<()> {
    let greetd_dir = cfg.get_greetd_dir();
    for name in STATIC_FILES {
        let src = Path::new(SHARE_GREETD).join(name);
        let dst = greetd_dir.join(name);
        fs::copy(&src, &dst)
            .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
    }
    for name in TEMPLATE_FILES {
        let src = Path::new(SHARE_GREETD).join(name);
        let text = fs::read_to_string(&src)
            .with_context(|| format!("failed to read template {}", src.display()))?
            .replace("{{vt}}", &vt.to_string())
            .replace("{{games_user}}", games_user);
        let dst = greetd_dir.join(name);
        fs::write(&dst, text).with_context(|| format!("failed to write {}", dst.display()))?;
    }
    println!("Deployed greetd configs to {}", greetd_dir.display());
    Ok(())
}

/// The greeter is cage + regreet only — a compositor upgrade must never be
/// able to break the login path, so there is no Hyprland greeter (and no
/// greeter config verification) anymore. Sanity-check the binaries exist.
fn verify_greeter_binaries() -> Result<()> {
    for bin in ["/usr/bin/cage", "/usr/bin/regreet"] {
        if !Path::new(bin).exists() {
            bail!("{bin} not found — the greeter cannot start without it");
        }
    }
    Ok(())
}

/// Render the sudoers grant (greetd restart, fgconsole, greetd runfile
/// removal) from the shipped template and install it via a visudo -c gate.
fn install_sudoers(greeter_user: &str) -> Result<()> {
    let text = fs::read_to_string(SUDOERS_TEMPLATE)
        .with_context(|| format!("failed to read {SUDOERS_TEMPLATE}"))?
        .replace("{{greeter_user}}", greeter_user);
    let tmp = format!("{SUDOERS_PATH}.tmp");
    fs::write(&tmp, text)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o440))?;
    if let Err(e) = run_checked("visudo", &["-c", "-f", &tmp]) {
        let _ = fs::remove_file(&tmp);
        return Err(e.context("rendered sudoers file failed visudo -c"));
    }
    fs::rename(&tmp, SUDOERS_PATH)?;
    println!("Installed {SUDOERS_PATH}");
    Ok(())
}

/// The verifier's WebAuthn RP ID / origin come from the tailnet FQDN, so
/// tailscale must be up and logged in for the approval gate to work. Missing
/// tailscale degrades to a warning: everything else still provisions, and the
/// daemon fails closed (no approvals possible) until this is completed.
fn tailscale_approval_env() -> Result<()> {
    let out = match Command::new("tailscale")
        .args(["status", "--json"])
        .output()
    {
        Ok(out) => out,
        Err(_) => {
            println!("WARN: tailscale not installed — the passkey approval gate needs it.");
            println!("      pacman -S tailscale && sudo systemctl enable --now tailscaled");
            println!("      && sudo tailscale up, then re-run: sudo game-mode setup");
            return Ok(());
        }
    };
    if !out.status.success() {
        println!("WARN: tailscale is not running or not logged in.");
        println!("      sudo systemctl enable --now tailscaled && sudo tailscale up");
        println!("      then re-run: sudo game-mode setup");
        return Ok(());
    }
    let status: Value =
        serde_json::from_slice(&out.stdout).context("failed to parse `tailscale status --json`")?;
    let Some(fqdn) = status["Self"]["DNSName"]
        .as_str()
        .map(|s| s.trim_end_matches('.'))
    else {
        println!("WARN: could not determine the tailnet FQDN; skipping approval.env");
        return Ok(());
    };

    if Path::new(APPROVAL_ENV).exists() {
        println!("{APPROVAL_ENV} already exists; leaving it untouched");
    } else {
        fs::create_dir_all(ETC_DIR)?;
        let text = format!(
            "AG_RP_ID={fqdn}\n\
             AG_ORIGIN=https://{fqdn}\n\
             AG_DATA_DIR=/var/lib/access-gate\n\
             AG_CTRL_SOCKET=/run/access-gate/ctrl.sock\n\
             AG_APPROVE_BASE=https://{fqdn}/approve\n\
             AG_TIMEOUT=90\n\
             AG_VAPID_SUB=access-gate@{fqdn}\n"
        );
        fs::write(APPROVAL_ENV, text)?;
        fs::set_permissions(APPROVAL_ENV, fs::Permissions::from_mode(0o644))?;
        println!("Wrote {APPROVAL_ENV} (RP ID {fqdn})");
    }

    // WebAuthn needs a real TLS origin; serve the verifier over the tailnet.
    let served = Command::new("tailscale")
        .args(["serve", "--bg", "--https=443", "http://127.0.0.1:8730"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !served {
        println!("WARN: run manually: sudo tailscale serve --bg --https=443 http://127.0.0.1:8730");
    }
    Ok(())
}

fn enable_services(interactive: bool) -> Result<()> {
    run_checked("systemctl", &["daemon-reload"])?;
    run_checked("systemctl", &["enable", "game-mode.service"])?;
    run_checked("systemctl", &["enable", "access-gate-verifier.service"])?;
    if Path::new(APPROVAL_ENV).exists() {
        run_checked("systemctl", &["restart", "access-gate-verifier.service"])?;
    } else {
        println!("access-gate-verifier not started (no {APPROVAL_ENV} yet)");
    }

    // Restarting greetd respawns the greeter on its VT (active desktop
    // sessions keep running). Only do it when the operator says so.
    let restart = interactive
        && Confirm::new()
            .with_prompt("Restart greetd now to apply the new greeter config?")
            .default(false)
            .interact()?;
    if restart {
        run_checked("systemctl", &["restart", "greetd.service"])?;
    } else {
        println!("Skipping greetd restart; apply later with: sudo systemctl restart greetd");
    }
    Ok(())
}

fn print_next_steps() {
    println!();
    println!("Setup complete. Remaining one-time steps:");
    println!("  1. Phone passkey enrollment (only while no key is enrolled):");
    println!("       sudo -u access-gate touch /var/lib/access-gate/enroll-open");
    println!("     then open https://<tailnet-fqdn>/enroll on the phone");
    println!("     (and https://<tailnet-fqdn>/setup for push notifications).");
    println!("  2. \"Discord\" non-Steam shortcut (with Steam closed, as the games user):");
    println!("       game-mode-steam-shortcut --name Discord --exe /usr/bin/game-mode-discord");
    println!("  3. Test the approval gate without a gamepad:");
    println!("       sudo -u greeter game-mode --test-approval");
}

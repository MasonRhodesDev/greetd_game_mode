//! game-mode-wol: Wake-on-LAN as a second game-mode entry trigger beside the
//! Guide button (issue #5).
//!
//! A machine sitting at greetd is already powered on, so NIC-level WOL has
//! nothing to do and no Steam is running to answer Remote Play discovery.
//! The Steam Link app, finding a saved host absent from discovery, sends a
//! standard magic packet to UDP 9 (unicast to each saved address and
//! broadcast) when its connect screen opens, then keeps retrying for up to
//! five minutes and finishes the connect by itself once real Steam answers.
//! So this listener only has to notice the packet and run the existing
//! entry path: same greeter-state checks as the daemon, same fail-closed
//! phone approval, same switch. No Steam protocol, no account state.
//!
//! Desktop Steam clients and Steam Decks do not send WOL; only Steam Link
//! does, and only for a host that has advertised its MAC to it before.
//!
//! A magic packet is unauthenticated and anyone on the LAN can send one, so
//! the phone approval is the actual gate here; the packet only starts it.

use anyhow::{Context, Result};
use game_mode::approval::{self, Prompt};
use game_mode::config::Config;
use game_mode::game_mode_switch;
use game_mode::session::at_idle_greeter;
use std::{
    env, fs,
    net::{IpAddr, UdpSocket},
    path::Path,
    time::{Duration, Instant},
};
use tracing::{debug, error, info, warn};

/// The port Steam Link sends its magic packet to.
const DEFAULT_PORT: u16 = 9;

/// Steam Link sends one packet per saved address plus a broadcast, and the
/// approval blocks for up to its timeout; matches arriving this soon after
/// an attempt ends are the same wake, not a new one.
const COOLDOWN: Duration = Duration::from_secs(30);

const SYSFS_NET: &str = "/sys/class/net";

type Mac = [u8; 6];

/// The target MAC of a magic packet: six `0xFF` bytes, then one MAC repeated
/// sixteen times. Trailing bytes (a SecureOn password) are ignored.
fn magic_packet_mac(buf: &[u8]) -> Option<Mac> {
    if buf.len() < 102 || buf[..6] != [0xFF; 6] {
        return None;
    }
    let mac: Mac = buf[6..12].try_into().ok()?;
    if mac == [0xFF; 6] {
        return None;
    }
    buf[6..102]
        .as_chunks::<6>()
        .0
        .iter()
        .all(|c| *c == mac)
        .then_some(mac)
}

fn parse_mac(text: &str) -> Option<Mac> {
    let mut mac = [0u8; 6];
    let mut parts = text.trim().split(':');
    for byte in mac.iter_mut() {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}

/// MACs of this machine's interfaces, read fresh on every packet so hotplugged
/// or renamed NICs need no restart and nothing host-specific is configured.
fn local_macs(net_dir: &Path) -> Vec<Mac> {
    let Ok(entries) = fs::read_dir(net_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name() != "lo")
        .filter_map(|e| fs::read_to_string(e.path().join("address")).ok())
        .filter_map(|text| parse_mac(&text))
        .filter(|mac| *mac != [0; 6])
        .collect()
}

fn fmt_mac(mac: &Mac) -> String {
    mac.map(|b| format!("{b:02x}")).join(":")
}

fn handle_wake(sender: IpAddr) -> Result<()> {
    let config = Config::load()?;
    if !at_idle_greeter(&config)? {
        info!("wake from {sender} ignored: not at an idle greeter");
        return Ok(());
    }
    let path = format!("a Wake-on-LAN packet from {sender} asked to start Steam game mode");
    let prompt = Prompt {
        title: "Wake into game mode?",
        path: &path,
        retry_hint: "reopen the host in Steam Link to retry",
    };
    if approval::require_approval_for(&prompt) {
        game_mode_switch::switch_to_game_mode(&config)?;
    } else {
        info!("wake from {sender} not approved; staying at greeter");
    }
    Ok(())
}

fn port_from_args() -> Result<u16> {
    let mut args = env::args().skip(1);
    let mut port = DEFAULT_PORT;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = args
                    .next()
                    .context("--port needs a value")?
                    .parse()
                    .context("--port must be a UDP port number")?;
            }
            other => anyhow::bail!("unknown argument {other:?} (usage: game-mode-wol [--port N])"),
        }
    }
    Ok(port)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "game_mode=info".to_string()))
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .init();

    let port = port_from_args()?;
    let socket = UdpSocket::bind(("0.0.0.0", port)).with_context(|| {
        format!("failed to bind UDP {port} (needs CAP_NET_BIND_SERVICE below 1024)")
    })?;
    info!("listening for Wake-on-LAN on UDP {port}");

    let mut last_attempt: Option<Instant> = None;
    let mut buf = [0u8; 1500];
    loop {
        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e) => {
                warn!("recv failed: {e}");
                continue;
            }
        };
        let Some(mac) = magic_packet_mac(&buf[..len]) else {
            debug!("ignoring {len} byte non-magic packet from {from}");
            continue;
        };
        if !local_macs(Path::new(SYSFS_NET)).contains(&mac) {
            debug!(
                "ignoring magic packet for {} from {from}: not ours",
                fmt_mac(&mac)
            );
            continue;
        }
        if last_attempt.is_some_and(|t| t.elapsed() < COOLDOWN) {
            debug!("ignoring magic packet from {from}: within cooldown");
            continue;
        }
        info!("magic packet for {} from {from}", fmt_mac(&mac));
        if let Err(e) = handle_wake(from.ip()) {
            error!("wake from {from} failed: {e:#}");
        }
        last_attempt = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: Mac = [0x02, 0x00, 0x00, 0xaa, 0xbb, 0xcc];

    fn packet(mac: &Mac) -> Vec<u8> {
        let mut p = vec![0xFF; 6];
        for _ in 0..16 {
            p.extend_from_slice(mac);
        }
        p
    }

    #[test]
    fn parses_magic_packet() {
        assert_eq!(magic_packet_mac(&packet(&MAC)), Some(MAC));
    }

    #[test]
    fn ignores_trailing_password() {
        let mut p = packet(&MAC);
        p.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(magic_packet_mac(&p), Some(MAC));
    }

    #[test]
    fn rejects_malformed_packets() {
        assert_eq!(magic_packet_mac(&packet(&MAC)[..101]), None);
        let mut bad_sync = packet(&MAC);
        bad_sync[0] = 0;
        assert_eq!(magic_packet_mac(&bad_sync), None);
        let mut bad_rep = packet(&MAC);
        bad_rep[101] ^= 1;
        assert_eq!(magic_packet_mac(&bad_rep), None);
        assert_eq!(magic_packet_mac(&[0xFF; 102]), None);
    }

    #[test]
    fn parses_sysfs_macs() {
        assert_eq!(parse_mac("02:00:00:aa:bb:cc\n"), Some(MAC));
        assert_eq!(parse_mac("02:00:00:aa:bb"), None);
        assert_eq!(parse_mac("02:00:00:aa:bb:cc:dd"), None);
        assert_eq!(parse_mac("zz:00:00:aa:bb:cc"), None);
    }

    #[test]
    fn reads_local_macs_skipping_lo_and_zero() {
        let dir = env::temp_dir().join(format!("game-mode-wol-net-{}", std::process::id()));
        for (iface, addr) in [
            ("lo", "00:00:00:00:00:00"),
            ("eth0", "02:00:00:aa:bb:cc"),
            ("tun0", "00:00:00:00:00:00"),
        ] {
            fs::create_dir_all(dir.join(iface)).unwrap();
            fs::write(dir.join(iface).join("address"), format!("{addr}\n")).unwrap();
        }
        assert_eq!(local_macs(&dir), vec![MAC]);
        fs::remove_dir_all(&dir).unwrap();
    }
}

//! A phone: internet sharing in Termux, and nothing of the worker. No root,
//! no app of ours: the release's Android binary run by Termux, kept awake
//! by `termux-wake-lock` and started at boot by Termux:Boot (docs/INTERNET.md,
//! "Android"). Docker, WSL and systemd are not there; the commands that need
//! them say so (`ANDROID_NO_CONTAINERS`).
//!
//! What the phone can say without root is which network carries its traffic
//! now: the source address the kernel picks for the default route, and the
//! interface that has it (`wlan0` is Wi-Fi, `rmnet_data0` or `ccmni0` is
//! mobile data). By default a phone shares only on Wi-Fi (or a cable);
//! `internet on --mobile-data` lends mobile data too. A network it cannot
//! name counts as mobile data: the cost falls on the person otherwise.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};

/// The worker's commands, on a phone.
pub const NO_CONTAINERS: &str = "ANDROID_NO_CONTAINERS";
/// A hold of internet sharing: the phone is on mobile data, or a network it
/// cannot name, and only Wi-Fi was allowed.
pub const NOT_ON_WIFI: &str = "INTERNET_NOT_ON_WIFI";

/// This build runs on Android.
pub const PHONE: bool = cfg!(target_os = "android");

const TERMUX_PREFIX: &str = "/data/data/com.termux/files/usr";
const TERMUX_HOME: &str = "/data/data/com.termux/files/home";
const BOOT_SCRIPT: &str = "shutup-and-run-internet";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map_or_else(|| PathBuf::from(TERMUX_HOME), PathBuf::from)
}

/// Where internet sharing keeps its files on a phone: Termux's home, which
/// needs no root and outlives a reinstall of Termux's packages.
pub fn data_dir() -> PathBuf {
    home().join(".shutup-and-run")
}

// ---------------------------------------------------------------------------
// Which network

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Wifi,
    Ethernet,
    Mobile,
    /// An interface it cannot name: a VPN (`tun0`), or a maker's own name.
    Unknown,
    /// No route out at all.
    Offline,
}

/// What `internet-status.json` says of the network: its kind and interface,
/// never its address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
}

impl Seen {
    pub fn text(&self) -> String {
        let kind = match self.kind {
            Kind::Wifi => "Wi-Fi",
            Kind::Ethernet => "cable",
            Kind::Mobile => "mobile data",
            Kind::Unknown => "a network it cannot name",
            Kind::Offline => "offline",
        };
        match &self.interface {
            Some(name) => format!("{kind} ({name})"),
            None => kind.to_string(),
        }
    }
}

/// The network the phone's traffic leaves through now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub seen: Seen,
    /// Only to tell a change: a new address means the connections made on
    /// the old one are dead.
    pub address: Option<IpAddr>,
}

impl Network {
    /// May a new stream start on it: Wi-Fi and a cable always, mobile data
    /// and an unnamed network only with `--mobile-data`. Offline holds
    /// nothing: there is nothing to dial, and the status says why.
    pub fn allowed(&self, mobile_data: bool) -> bool {
        match self.seen.kind {
            Kind::Wifi | Kind::Ethernet | Kind::Offline => true,
            Kind::Mobile | Kind::Unknown => mobile_data,
        }
    }

    /// The hold's detail when it is not allowed.
    pub fn why_held(&self) -> String {
        format!(
            "on {}, and this phone shares only on Wi-Fi",
            self.seen.text()
        )
    }
}

/// Wi-Fi, mobile data or a cable, by the interface's name. The makers'
/// modems are `rmnet` (Qualcomm), `ccmni` (MediaTek), `seth` (Unisoc) and a
/// few more; `v4-` is the IPv4 side of an IPv6-only mobile network.
pub fn classify(interface: &str) -> Kind {
    let name = interface.strip_prefix("v4-").unwrap_or(interface);
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if starts(&[
        "rmnet",
        "r_rmnet",
        "rev_rmnet",
        "ccmni",
        "seth",
        "pdp",
        "ppp",
        "wwan",
        "clat",
        "umts",
        "radio",
    ]) {
        Kind::Mobile
    } else if starts(&["wlan", "wifi", "wl"]) {
        Kind::Wifi
    } else if starts(&["eth", "en"]) {
        Kind::Ethernet
    } else {
        Kind::Unknown
    }
}

/// Reads it now: a UDP socket "connected" to an address on the internet
/// sends nothing, but the kernel picks the route, so its local address is
/// the one of the network in use. Then the interface that has that address.
pub fn current() -> Network {
    let Some(address) = local_address() else {
        return Network {
            seen: Seen {
                kind: Kind::Offline,
                interface: None,
            },
            address: None,
        };
    };
    let interface = interface_of(address);
    Network {
        seen: Seen {
            kind: interface.as_deref().map_or(Kind::Unknown, classify),
            interface,
        },
        address: Some(address),
    }
}

fn local_address() -> Option<IpAddr> {
    // Documentation addresses: routed by the default route, never reached.
    for (bind, to) in [("0.0.0.0:0", "192.0.2.1:9"), ("[::]:0", "[2001:db8::1]:9")] {
        let Ok(socket) = UdpSocket::bind(bind) else {
            continue;
        };
        if socket.connect(to).is_err() {
            continue;
        }
        if let Ok(local) = socket.local_addr() {
            if !local.ip().is_unspecified() {
                return Some(local.ip());
            }
        }
    }
    None
}

#[cfg(unix)]
fn interface_of(address: IpAddr) -> Option<String> {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list it allocated, or
    // fails and leaves it alone; every node is read before freeifaddrs, and
    // an address is read as the family its sa_family says.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut node = list;
        while !node.is_null() {
            let entry = &*node;
            node = entry.ifa_next;
            let addr = entry.ifa_addr;
            if addr.is_null() || entry.ifa_name.is_null() {
                continue;
            }
            let ip = match i32::from((*addr).sa_family) {
                libc::AF_INET => {
                    let v4 = &*(addr as *const libc::sockaddr_in);
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let v6 = &*(addr as *const libc::sockaddr_in6);
                    IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.s6_addr))
                }
                _ => continue,
            };
            if ip == address {
                found = Some(
                    std::ffi::CStr::from_ptr(entry.ifa_name)
                        .to_string_lossy()
                        .into_owned(),
                );
                break;
            }
        }
        libc::freeifaddrs(list);
        found
    }
}

#[cfg(not(unix))]
fn interface_of(_address: IpAddr) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// Termux: the process, the boot and the wake lock

/// `~/.termux/boot/shutup-and-run-internet`: Termux:Boot runs every file of
/// that folder once the phone has started.
pub fn boot_script_path() -> PathBuf {
    home().join(".termux").join("boot").join(BOOT_SCRIPT)
}

pub fn boot_script(exe: &Path) -> String {
    let prefix = std::env::var("PREFIX")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| TERMUX_PREFIX.to_string());
    format!(
        "#!{prefix}/bin/sh\n\
         # Written by `shutup-and-run internet on`; `internet off` removes it.\n\
         # Termux:Boot runs it once the phone has started.\n\
         termux-wake-lock\n\
         # Again a minute after it ends with an error, as the logon task and\n\
         # the systemd unit do. It ends well when turned off, or when one\n\
         # runs already.\n\
         until '{}' internet serve --background; do sleep 60; done\n",
        exe.display()
    )
}

/// `internet on` on a phone: the boot script, the wake lock, and the
/// process now unless one runs already (`running`). One line each.
pub fn start(running: bool) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let script = boot_script_path();
    if let Some(parent) = script.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&script, boot_script(&exe)).map_err(|e| format!("{}: {e}", script.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", script.display()))?;
    }
    let mut lines = vec![if running {
        "already running: it reads the new settings within 5 seconds".to_string()
    } else {
        spawn(&script)?;
        "started in the background, again a minute after an error".to_string()
    }];
    lines.push(match run("termux-wake-lock") {
        true => "wake lock held (termux-wake-lock): it goes on with the screen off".to_string(),
        false => "termux-wake-lock did not run: with the screen off Android may stop it (docs/INTERNET.md, Android)".to_string(),
    });
    lines.push(format!(
        "Termux:Boot starts it at boot ({})",
        script.display()
    ));
    Ok(lines.join("\n"))
}

/// `internet off` on a phone: no boot script, no wake lock. The process
/// sees the file and stops by itself.
pub fn stop() -> String {
    let script = boot_script_path();
    let removed = std::fs::remove_file(&script).is_ok();
    run("termux-wake-unlock");
    let mut text = String::new();
    if removed {
        text.push_str(&format!("removed {}\n", script.display()));
    }
    text.push_str("wake lock released; a running `internet serve` stops within 5 seconds");
    text
}

fn run(program: &str) -> bool {
    std::process::Command::new(program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The boot script, which runs `internet serve` until it ends well, in a
/// process group of its own: closing the Termux session that ran `on`
/// does not end it.
fn spawn(script: &Path) -> Result<(), String> {
    let mut command = std::process::Command::new(script);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map(drop)
        .map_err(|e| format!("{}: {e}", script.display()))
}

/// Asks the process `pid` (an `internet serve` of another version) to end.
pub fn terminate(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill only sends a signal; a pid that is gone is an error it
    // returns, and nothing is read back.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interface_names_the_network() {
        for (name, kind) in [
            ("wlan0", Kind::Wifi),
            ("wlp2s0", Kind::Wifi),
            ("rmnet_data0", Kind::Mobile),
            ("rmnet0", Kind::Mobile),
            ("v4-rmnet_data2", Kind::Mobile),
            ("ccmni1", Kind::Mobile),
            ("seth_lte0", Kind::Mobile),
            ("eth0", Kind::Ethernet),
            ("enp3s0", Kind::Ethernet),
            ("tun0", Kind::Unknown),
            ("lo", Kind::Unknown),
        ] {
            assert_eq!(classify(name), kind, "{name}");
        }
    }

    #[test]
    fn only_wifi_unless_mobile_data_was_allowed() {
        let on = |kind, name: &str| Network {
            seen: Seen {
                kind,
                interface: Some(name.to_string()),
            },
            address: None,
        };
        assert!(on(Kind::Wifi, "wlan0").allowed(false));
        assert!(on(Kind::Ethernet, "eth0").allowed(false));
        assert!(!on(Kind::Mobile, "rmnet_data0").allowed(false));
        assert!(on(Kind::Mobile, "rmnet_data0").allowed(true));
        assert!(
            !on(Kind::Unknown, "tun0").allowed(false),
            "a network it cannot name may be mobile data"
        );
        let why = on(Kind::Mobile, "rmnet_data0").why_held();
        assert!(why.contains("mobile data (rmnet_data0)"), "{why}");
    }

    #[test]
    fn this_machines_network_has_an_interface_when_it_has_an_address() {
        let network = current();
        // Interfaces are read on Unix only: Windows never asks.
        if cfg!(unix) && network.address.is_some() {
            assert!(network.seen.interface.is_some(), "{network:?}");
        } else if network.address.is_none() {
            assert_eq!(network.seen.kind, Kind::Offline);
        }
        // The status never carries the address.
        let json = serde_json::to_string(&network.seen).unwrap();
        assert!(!json.contains("address"), "{json}");
    }

    #[test]
    fn the_boot_script_holds_the_wake_lock_and_runs_the_process() {
        let script = boot_script(Path::new(
            "/data/data/com.termux/files/usr/bin/shutup-and-run",
        ));
        assert!(script.starts_with("#!/"));
        assert!(script.contains("\ntermux-wake-lock\n"));
        assert!(script.ends_with(
            "until '/data/data/com.termux/files/usr/bin/shutup-and-run' internet serve --background; do sleep 60; done\n"
        ));
    }
}

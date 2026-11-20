//! `shutup-and-run fit`: what this PC can lend, before anything is
//! installed. Containers need WSL2 on Windows and a PC above the small
//! machine line; the internet connection needs neither. Reads only what the
//! doctor and the safety levels already read.

use crate::cli::{check, emit, soft, Check, Out};
use serde_json::json;
use std::process::ExitCode;

/// What `fit` reads of the machine.
#[derive(Clone, Debug)]
pub struct Machine {
    pub windows: bool,
    /// `wsl --status` worked, or what it said. Only read on Windows.
    pub wsl: Result<(), String>,
    pub memory_bytes: Option<u64>,
    pub cpus: u64,
}

impl Machine {
    pub fn read() -> Machine {
        let windows = cfg!(windows);
        Machine {
            windows,
            wsl: if windows {
                crate::wsl::wsl_ready()
            } else {
                Ok(())
            },
            memory_bytes: crate::wsl::host_memory_bytes(),
            cpus: crate::wsl::host_cpus(),
        }
    }
}

/// The two lines, and `both` or `internet`.
pub fn answer(machine: &Machine) -> (Vec<Check>, &'static str) {
    let size = format!(
        "{} of memory, {} processors",
        machine
            .memory_bytes
            .map_or("unknown".to_string(), crate::resources::human),
        machine.cpus
    );
    let small = machine
        .memory_bytes
        .is_some_and(|m| m < crate::safety::SMALL_MEMORY)
        || machine.cpus <= crate::safety::SMALL_CPUS;
    let containers = match &machine.wsl {
        Err(why) if machine.windows => check(
            "containers",
            false,
            format!("no WSL2 here yet ({why}): it needs an administrator and a restart"),
        )
        .coded("WSL_UNAVAILABLE"),
        _ if small => soft(
            "containers",
            false,
            format!("{size}: jobs would need the owner's authorization"),
        )
        .coded(crate::safety::SMALL_MACHINE),
        _ => check("containers", true, format!("can run them: {size}")),
    };
    let lend = if containers.ok { "both" } else { "internet" };
    let internet = check(
        "internet",
        true,
        "can lend it: no WSL, no Docker, no administrator; it dials out to the operator's relay",
    );
    (vec![containers, internet], lend)
}

pub fn cli(args: &[String]) -> ExitCode {
    let flags = match crate::wsl::parse_flags(args, &[], &["--json"]) {
        Ok(flags) => flags,
        Err(e) => {
            eprintln!("{}\nshutup-and-run fit [--json]", e.0);
            return ExitCode::from(2);
        }
    };
    let machine = Machine::read();
    let (checks, lend) = answer(&machine);
    let (report, _) = crate::cli::report(&checks);
    let next = if lend == "both" {
        "next: the worker (`wsl install` on Windows, install.sh on Linux) and, if the operator wants it, `internet on`"
    } else {
        "next: `internet on --relay HOST:PORT`; containers once the line above is solved"
    };
    let value = json!({
        "lend": lend,
        "containers": checks[0].ok,
        "internet": true,
        "os": if machine.windows { "windows" } else { "linux" },
        "memory_bytes": machine.memory_bytes,
        "cpus": machine.cpus,
        "checks": crate::cli::checks_json(&checks),
    });
    emit(
        Out::ok(
            value,
            format!("lend: {lend}\n{}\n{next}", report.trim_end()),
        ),
        flags.has("--json"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn machine(windows: bool, wsl: bool, gib: u64, cpus: u64) -> Machine {
        Machine {
            windows,
            wsl: if wsl {
                Ok(())
            } else {
                Err("WSL is not installed".to_string())
            },
            memory_bytes: Some(gib * GIB),
            cpus,
        }
    }

    #[test]
    fn a_pc_with_wsl2_and_room_lends_both() {
        let (checks, lend) = answer(&machine(true, true, 16, 8));
        assert_eq!(lend, "both");
        assert!(checks.iter().all(|c| c.ok));
    }

    #[test]
    fn without_wsl2_only_the_internet_and_the_reason() {
        let (checks, lend) = answer(&machine(true, false, 16, 8));
        assert_eq!(lend, "internet");
        assert_eq!(checks[0].code(), "WSL_UNAVAILABLE");
        assert!(checks[0].detail.contains("administrator"));
        assert!(checks[1].ok);
    }

    #[test]
    fn a_small_pc_lends_its_internet() {
        let (checks, lend) = answer(&machine(true, true, 6, 8));
        assert_eq!(lend, "internet");
        assert_eq!(checks[0].code(), "SAFETY_SMALL_MACHINE");
        let (_, lend) = answer(&machine(false, false, 32, 2));
        assert_eq!(lend, "internet", "two processors is small");
    }

    #[test]
    fn linux_does_not_ask_for_wsl() {
        let (_, lend) = answer(&machine(false, false, 16, 8));
        assert_eq!(lend, "both");
    }
}

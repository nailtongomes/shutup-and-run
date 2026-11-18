//! Memory, for the person at the PC and whoever looks after the machine:
//!
//! - which programs hold the most of it, from the Windows supervisor, sent
//!   only while the PC's memory is tight ([`top_programs`]);
//! - how much to free before the next job can start ([`need`]);
//! - the hours of the week it ran short in the last seven days, and the
//!   `lend_hours` that would keep out of them ([`short_hours`]);
//! - how long new jobs waited for it, today and this week;
//! - giving back to Windows what WSL2 keeps as cache after a job
//!   ([`give_back`]).
//!
//! The program list stays on the PC: the local page and the local doctor
//! show it. It is not in `status.json`, a log line, the poll body, a
//! callback or `doctor --report`. Nothing here closes a program or changes
//! a setting: it says what is there and what would help.

use crate::lending::{LendHours, Local};
use crate::resources::human;
use crate::safety::{Admission, Readings, Report};
use serde::{Deserialize, Serialize};

pub const MIB: u64 = 1024 * 1024;

/// A program and its processes, added up: `chrome`, 38 processes, 3.1 GiB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    pub name: String,
    pub processes: u32,
    /// Memory only it holds, as far as Windows says: for each process the
    /// smaller of its working set and its private bytes.
    pub bytes: u64,
}

/// The doctor's line with the programs: shown on the PC, never in a report.
pub const PROGRAMS_CODE: &str = "MEMORY_HELD_BY_PROGRAMS";

/// At most this many programs, each holding at least `PROGRAM_MIN_BYTES`.
pub const TOP: usize = 5;
pub const PROGRAM_MIN_BYTES: u64 = 50 * MIB;

/// Windows itself, its security, and this worker: never a program to tell
/// the person to close. Names without `.exe`, in lower case.
const NOT_TO_CLOSE: &[&str] = &[
    "system",
    "registry",
    "memory compression",
    "secure system",
    "idle",
    "smss",
    "csrss",
    "wininit",
    "winlogon",
    "services",
    "lsass",
    "lsaiso",
    "svchost",
    "dwm",
    "explorer",
    "fontdrvhost",
    "sihost",
    "ctfmon",
    "taskhostw",
    "runtimebroker",
    "searchindexer",
    "searchhost",
    "startmenuexperiencehost",
    "shellexperiencehost",
    "textinputhost",
    "spoolsv",
    "audiodg",
    "conhost",
    "msmpeng",
    "nissrv",
    "mpdefendercoreservice",
    "securityhealthservice",
    "smartscreen",
    "vmmem",
    "vmmemwsl",
    "vmcompute",
    "vmwp",
    "wslservice",
    "wsl",
    "wslhost",
    "wslrelay",
    "shutup-and-run",
];

/// The programs holding the most memory, from `(executable, bytes)` per
/// process. System processes and this worker are left out.
pub fn top_programs(processes: impl IntoIterator<Item = (String, u64)>) -> Vec<Program> {
    let mut programs: Vec<Program> = Vec::new();
    for (exe, bytes) in processes {
        let name = exe
            .strip_suffix(".exe")
            .or_else(|| exe.strip_suffix(".EXE"))
            .unwrap_or(&exe)
            .to_string();
        let key = name.to_lowercase();
        if name.is_empty() || NOT_TO_CLOSE.contains(&key.as_str()) {
            continue;
        }
        match programs.iter_mut().find(|p| p.name.to_lowercase() == key) {
            Some(program) => {
                program.processes += 1;
                program.bytes += bytes;
            }
            None => programs.push(Program {
                name,
                processes: 1,
                bytes,
            }),
        }
    }
    programs.retain(|p| p.bytes >= PROGRAM_MIN_BYTES);
    programs.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    programs.truncate(TOP);
    programs
}

/// `chrome 3.1 GiB (38 processes) · Teams 1.2 GiB`.
pub fn programs_text(programs: &[Program]) -> String {
    programs_in(programs, "processes")
}

/// The same, with the word for processes in the page's language.
pub fn programs_in(programs: &[Program], processes: &str) -> String {
    programs
        .iter()
        .map(|p| {
            if p.processes > 1 {
                format!(
                    "{} {} ({} {processes})",
                    p.name,
                    human(p.bytes),
                    p.processes
                )
            } else {
                format!("{} {}", p.name, human(p.bytes))
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The PC's memory is tight enough that the supervisor sends the program
/// list: under the warning line of `src/safety.rs` (20%), or under the
/// level at which a block there clears (1.5 GiB).
pub fn tight(total: u64, available: u64) -> bool {
    available
        < (total / 100 * crate::safety::MEMORY_WARN_PERCENT)
            .max(crate::safety::OWNER_MEMORY_FLOOR / 2 * 3)
}

/// How much memory to free before a new job can start, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Need {
    /// `pc` (Windows), `wsl2` (the VM the jobs run in), or `machine`.
    pub place: &'static str,
    pub to_free_bytes: u64,
}

impl Need {
    pub fn text(&self) -> String {
        let place = match self.place {
            "pc" => "this PC",
            "wsl2" => "WSL2",
            _ => "this machine",
        };
        format!("free about {} on {place}", human(self.to_free_bytes))
    }
}

/// The largest gap between what is available and the line a new job needs
/// it above: a block clears at its recovery line (15%, at least 1.5 GiB on
/// the PC), a warning the owner has not agreed to clears at 20%, and the
/// free memory a job needs in the VM is `shortage`. None when memory is not
/// what keeps a job from starting.
pub fn need(
    report: &Report,
    readings: &Readings,
    shortage: Option<&crate::resources::Shortage>,
) -> Option<Need> {
    use crate::safety::{
        MEMORY_CRITICAL, MEMORY_LOW, MEMORY_RECOVER_PERCENT, MEMORY_WARN_PERCENT,
        OWNER_MEMORY_FLOOR, VM_MEMORY_FLOOR,
    };
    let percent = |total: u64, p: u64| total / 100 * p;
    let mut gaps: Vec<Need> = Vec::new();
    let mut gap = |place, available: Option<u64>, line: u64| {
        if let Some(available) = available.filter(|a| *a < line) {
            gaps.push(Need {
                place,
                to_free_bytes: line - available,
            });
        }
    };
    if report.applies {
        let critical = report.reasons.iter().any(|r| r.code == MEMORY_CRITICAL);
        let low = report.admission == Admission::AuthorizationRequired
            && report.needs_authorization.iter().any(|c| c == MEMORY_LOW);
        let (owner_place, owner_total, owner_available) = if readings.windows {
            let host = readings.host.unwrap_or_default();
            ("pc", host.memory_total, host.memory_available)
        } else {
            (
                "machine",
                readings.inner.memory_total,
                readings.inner.memory_available,
            )
        };
        if let Some(total) = owner_total {
            if critical {
                gap(
                    owner_place,
                    owner_available,
                    percent(total, MEMORY_RECOVER_PERCENT).max(OWNER_MEMORY_FLOOR / 2 * 3),
                );
            } else if low {
                gap(
                    owner_place,
                    owner_available,
                    percent(total, MEMORY_WARN_PERCENT),
                );
            }
        }
        if critical && readings.windows {
            if let Some(total) = readings.inner.memory_total {
                gap(
                    "wsl2",
                    readings.inner.memory_available,
                    percent(total, MEMORY_RECOVER_PERCENT).max(VM_MEMORY_FLOOR / 2 * 3),
                );
            }
        }
    }
    if let Some(short) = shortage.filter(|s| s.resource == "memory") {
        gaps.push(Need {
            place: if readings.windows { "wsl2" } else { "machine" },
            to_free_bytes: short.needed_bytes.saturating_sub(short.free_bytes),
        });
    }
    gaps.into_iter().max_by_key(|n| n.to_free_bytes)
}

/// One hour of the week and the lowest memory available in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hour {
    /// Monday 00:00 to 01:00 is 0, Sunday 23:00 to 24:00 is 167.
    pub slot: u32,
    pub lowest: u64,
    pub total: u64,
}

pub const WEEK_SECS: i64 = 7 * 86_400;

/// The hour of the week now, on the clock `lend_hours` is read on.
pub fn slot_of(now_unix: i64, offset_secs: i64) -> u32 {
    let local = Local::at(now_unix, offset_secs);
    local.weekday as u32 * 24 + local.minute / 60
}

/// The hours memory ran short (under 20% available), as `lend_hours`
/// windows, and the `lend_hours` that would keep new jobs out of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortHours {
    /// `mon-fri 09-18`.
    pub when: String,
    /// `mon-fri 00-09; mon-fri 18-24; sat,sun`. Inside the `lend_hours` in
    /// force, when there is one. None when no hour is left.
    pub suggested_lend_hours: Option<String>,
}

/// None when no hour with a reading ran short, or when none of those is an
/// hour this machine lends itself in now.
pub fn short_hours(hours: &[Hour], lend: Option<&LendHours>) -> Option<ShortHours> {
    let mut short = [false; 168];
    for hour in hours.iter().filter(|h| h.slot < 168) {
        short[hour.slot as usize] =
            hour.lowest < hour.total / 100 * crate::safety::MEMORY_WARN_PERCENT;
    }
    let lends = |slot: usize| {
        lend.is_none_or(|l| {
            l.allows(Local {
                weekday: slot / 24,
                minute: (slot % 24) as u32 * 60,
            })
        })
    };
    let mut in_lending = [false; 168];
    let mut keep = [false; 168];
    for slot in 0..168 {
        in_lending[slot] = short[slot] && lends(slot);
        keep[slot] = !short[slot] && lends(slot);
    }
    if !in_lending.contains(&true) {
        return None;
    }
    Some(ShortHours {
        when: grid_text(&in_lending),
        suggested_lend_hours: keep.contains(&true).then(|| grid_text(&keep)),
    })
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The hours that are true, written as `lend_hours` reads them. Days with
/// the same hours share a window.
pub fn grid_text(grid: &[bool; 168]) -> String {
    let ranges = |day: usize| {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for hour in 0..24 {
            if grid[day * 24 + hour] {
                match out.last_mut() {
                    Some(last) if last.1 == hour => last.1 = hour + 1,
                    _ => out.push((hour, hour + 1)),
                }
            }
        }
        out
    };
    // Hours of a day as (start, end) pairs, and the days that have them.
    type Ranges = Vec<(usize, usize)>;
    let mut groups: Vec<(Ranges, Vec<usize>)> = Vec::new();
    for day in 0..7 {
        let day_ranges = ranges(day);
        if day_ranges.is_empty() {
            continue;
        }
        match groups.iter_mut().find(|(r, _)| *r == day_ranges) {
            Some((_, days)) => days.push(day),
            None => groups.push((day_ranges, vec![day])),
        }
    }
    let mut windows = Vec::new();
    for (day_ranges, days) in groups {
        let days = days_text(&days);
        for (start, end) in day_ranges {
            windows.push(if (start, end) == (0, 24) {
                days.clone()
            } else {
                format!("{days} {start:02}-{end:02}")
            });
        }
    }
    windows.join("; ")
}

/// `mon-fri`, `sat,sun`, `mon-wed,fri`.
fn days_text(days: &[usize]) -> String {
    let mut parts = Vec::new();
    let mut at = 0;
    while at < days.len() {
        let mut end = at;
        while end + 1 < days.len() && days[end + 1] == days[end] + 1 {
            end += 1;
        }
        if end - at >= 2 {
            parts.push(format!("{}-{}", DAYS[days[at]], DAYS[days[end]]));
        } else {
            for day in &days[at..=end] {
                parts.push(DAYS[*day].to_string());
            }
        }
        at = end + 1;
    }
    parts.join(",")
}

/// `2 h 40 min`, `12 min`, `0 min`.
pub fn duration_text(secs: u64) -> String {
    let minutes = secs / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// What the page and `status.json` say about memory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct View {
    /// Null when memory does not keep a job from starting.
    pub need: Option<Need>,
    /// Since when new jobs wait for memory, in seconds since 1970.
    pub waiting_since: Option<i64>,
    pub waited_today_secs: u64,
    pub waited_week_secs: u64,
    pub short_hours: Option<ShortHours>,
}

/// Asks the kernel of the WSL2 VM to drop its file cache and compact what
/// is left, so the VM hands that memory back to Windows. Run once when the
/// last job of a Windows PC ends: the cache a job's image and files filled
/// is otherwise held by `vmmemWSL` until WSL2 restarts. Errors are
/// returned, never fatal.
#[cfg(target_os = "linux")]
pub fn give_back() -> std::io::Result<()> {
    std::fs::write("/proc/sys/vm/drop_caches", "1\n")?;
    // Not every kernel has it; the drop above is the part that counts.
    let _ = std::fs::write("/proc/sys/vm/compact_memory", "1\n");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn give_back() -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Feed;
    use crate::resources::GIB;
    use crate::safety::{Finding, Level};

    #[test]
    fn the_programs_holding_the_most_are_added_up_and_windows_itself_is_left_out() {
        let mut list = vec![("svchost.exe".to_string(), 900 * MIB)];
        for _ in 0..38 {
            list.push(("chrome.exe".to_string(), 80 * MIB));
        }
        list.extend([
            ("Teams.exe".to_string(), 700 * MIB),
            ("ms-teams.exe".to_string(), 500 * MIB),
            ("MsMpEng.exe".to_string(), 300 * MIB),
            ("vmmemWSL".to_string(), 4 * GIB),
            ("notepad.exe".to_string(), 10 * MIB),
            ("EXCEL.EXE".to_string(), 400 * MIB),
            ("shutup-and-run.exe".to_string(), 60 * MIB),
            ("a.exe".to_string(), 60 * MIB),
            ("b.exe".to_string(), 55 * MIB),
        ]);
        let top = top_programs(list);
        let names: Vec<&str> = top.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["chrome", "Teams", "ms-teams", "EXCEL", "a"]);
        assert_eq!(top[0].processes, 38);
        assert_eq!(top[0].bytes, 38 * 80 * MIB);
        assert_eq!(
            programs_text(&top[..2]),
            "chrome 3.0 GiB (38 processes) · Teams 700 MiB"
        );
        assert!(top_programs(Vec::new()).is_empty());
    }

    #[test]
    fn tight_is_under_a_fifth_or_under_one_and_a_half_gib() {
        assert!(tight(16 * GIB, 3 * GIB));
        assert!(!tight(16 * GIB, 4 * GIB));
        assert!(tight(4 * GIB, 1400 * MIB));
        assert!(!tight(4 * GIB, 2 * GIB));
    }

    fn windows(total: u64, available: u64) -> Readings {
        Readings {
            windows: true,
            host: Some(Feed {
                memory_total: Some(total),
                memory_available: Some(available),
                ..Feed::default()
            }),
            ..Readings::healthy()
        }
    }

    fn report(codes: &[&str], admission: Admission, unagreed: &[&str]) -> Report {
        Report {
            applies: true,
            admission,
            reasons: codes
                .iter()
                .map(|code| Finding {
                    code: code.to_string(),
                    level: Level::Warning,
                    detail: String::new(),
                })
                .collect(),
            needs_authorization: unagreed.iter().map(|c| c.to_string()).collect(),
            ..Report::default()
        }
    }

    #[test]
    fn the_memory_to_free_is_the_gap_to_the_line_that_lets_a_job_start() {
        use crate::safety::{MEMORY_CRITICAL, MEMORY_LOW};
        // Blocked at 1 GiB of 16: it clears at 15%, 2.4 GiB.
        let blocked = report(&[MEMORY_CRITICAL], Admission::Blocked, &[]);
        let need = need(&blocked, &windows(16 * GIB, GIB), None).unwrap();
        assert_eq!(need.place, "pc");
        assert_eq!(need.to_free_bytes, 16 * GIB / 100 * 15 - GIB);
        assert_eq!(need.text(), "free about 1.4 GiB on this PC");
        // A warning not agreed to: up to 20%.
        let low = report(
            &[MEMORY_LOW],
            Admission::AuthorizationRequired,
            &[MEMORY_LOW],
        );
        let need_low = super::need(&low, &windows(16 * GIB, 3 * GIB), None).unwrap();
        assert_eq!(need_low.to_free_bytes, 16 * GIB / 100 * 20 - 3 * GIB);
        // Agreed to: memory keeps nothing out.
        let agreed = report(&[MEMORY_LOW], Admission::Open, &[]);
        assert_eq!(
            super::need(&agreed, &windows(16 * GIB, 3 * GIB), None),
            None
        );
        // The VM short of what a job needs.
        let short = crate::resources::Shortage {
            resource: "memory",
            free_bytes: 700 * MIB,
            needed_bytes: 2 * GIB,
        };
        let vm = super::need(
            &Report::default(),
            &windows(16 * GIB, 8 * GIB),
            Some(&short),
        );
        assert_eq!(
            vm,
            Some(Need {
                place: "wsl2",
                to_free_bytes: 2 * GIB - 700 * MIB
            })
        );
    }

    #[test]
    fn short_hours_become_lend_hours_windows_inside_the_ones_in_force() {
        let hour = |slot: u32, lowest: u64| Hour {
            slot,
            lowest,
            total: 16 * GIB,
        };
        let mut hours = Vec::new();
        for day in 0..7u32 {
            for h in 0..24u32 {
                let office = day < 5 && (9..18).contains(&h);
                hours.push(hour(day * 24 + h, if office { GIB } else { 8 * GIB }));
            }
        }
        let found = short_hours(&hours, None).unwrap();
        assert_eq!(found.when, "mon-fri 09-18");
        let suggested = found.suggested_lend_hours.unwrap();
        assert_eq!(suggested, "mon-fri 00-09; mon-fri 18-24; sat,sun");
        assert!(LendHours::parse(&suggested).is_ok());

        // Lending only at night already: nothing to say.
        let night = LendHours::parse("19-07").unwrap();
        assert_eq!(short_hours(&hours, Some(&night)), None);
        // Lending on weekdays from 8 to 20: what is left of it.
        let day = LendHours::parse("mon-fri 08-20").unwrap();
        let found = short_hours(&hours, Some(&day)).unwrap();
        assert_eq!(found.when, "mon-fri 09-18");
        assert_eq!(
            found.suggested_lend_hours.as_deref(),
            Some("mon-fri 08-09; mon-fri 18-20")
        );
        // No reading short: nothing.
        assert_eq!(short_hours(&[hour(3, 8 * GIB)], None), None);
    }

    #[test]
    fn days_and_hours_are_written_as_lend_hours_reads_them() {
        let mut grid = [false; 168];
        for day in [0usize, 1, 2, 4] {
            for h in 22..24 {
                grid[day * 24 + h] = true;
            }
        }
        assert_eq!(grid_text(&grid), "mon-wed,fri 22-24");
        assert!(LendHours::parse(&grid_text(&grid)).is_ok());
        assert_eq!(slot_of(1_795_010_700, 0), 2 * 24 + 14);
        assert_eq!(slot_of(1_795_010_700, -3 * 3600), 2 * 24 + 11);
        assert_eq!(duration_text(9_600), "2 h 40 min");
        assert_eq!(duration_text(720), "12 min");
        assert_eq!(duration_text(7_200), "2 h");
    }
}

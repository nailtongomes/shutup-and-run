//! What the Windows side knows and the service inside WSL2 cannot read: how
//! long the person has not touched the PC, the PC's own memory, processors
//! and disk, its battery, and whether the person is presenting. The
//! supervisor (`wsl serve`) writes one line to the service's stdin every
//! `FEED_EVERY`. Nothing is asked for, no file, no call back to Windows.
//!
//! A feed that stopped coming is forgotten after `FEED_STALE`: admission then
//! goes on as if `min_idle_secs` were off, so a supervisor that hangs cannot
//! park the worker for good.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const FEED_EVERY: Duration = Duration::from_secs(10);
pub const FEED_STALE: Duration = Duration::from_secs(45);
/// Marks a feed line, so anything else on stdin is ignored.
pub const FEED_MARK: &str = "host-feed ";

/// Every field is optional: a reading Windows did not give is left out, and
/// is never guessed here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    /// Seconds since the last keyboard or mouse input in the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
    /// Physical memory of the PC, and how much of it is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_available: Option<u64>,
    /// Logical processors of the PC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u64>,
    /// How busy those processors were since the line before this one, 0 to
    /// 100. Missing on the first line: there is nothing to compare with yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_busy_percent: Option<u64>,
    /// Free bytes on the Windows drive that holds the WSL2 disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_available: Option<u64>,
    /// The PC runs on its battery (true) or on the mains (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_battery: Option<bool>,
    /// The battery's charge, 0 to 100. Missing on a PC without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_percent: Option<u64>,
    /// The person presents, or a program is in full screen (Windows'
    /// "do not disturb" states of `SHQueryUserNotificationState`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presenting: Option<bool>,
    /// How far the Windows clock is ahead of UTC, in seconds, daylight
    /// saving included: `lend_hours` follows the clock the person sees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset_secs: Option<i64>,
}

static LAST: Mutex<Option<(Instant, Feed)>> = Mutex::new(None);
/// The programs that hold the most memory, sent with a feed line only while
/// the PC's memory is tight. Kept apart from `Feed`, which is a few numbers
/// copied everywhere; never written to a file or a log.
static PROGRAMS: Mutex<Option<(Instant, Vec<crate::memory::Program>)>> = Mutex::new(None);

#[derive(Serialize, Deserialize)]
struct Line {
    #[serde(flatten)]
    feed: Feed,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    programs: Vec<crate::memory::Program>,
}

pub fn line(feed: &Feed) -> String {
    line_with(feed, &[])
}

pub fn line_with(feed: &Feed, programs: &[crate::memory::Program]) -> String {
    let line = Line {
        feed: *feed,
        programs: programs.to_vec(),
    };
    format!(
        "{FEED_MARK}{}\n",
        serde_json::to_string(&line).unwrap_or_else(|_| "{}".into())
    )
}

pub fn parse(line: &str) -> Option<Feed> {
    parse_with(line).map(|(feed, _)| feed)
}

pub fn parse_with(line: &str) -> Option<(Feed, Vec<crate::memory::Program>)> {
    let line: Line =
        serde_json::from_str(line.trim().strip_prefix(FEED_MARK.trim_end())?.trim()).ok()?;
    Some((line.feed, line.programs))
}

pub fn note(feed: Feed) {
    *LAST.lock().expect("host feed") = Some((Instant::now(), feed));
}

pub fn note_programs(programs: Vec<crate::memory::Program>) {
    *PROGRAMS.lock().expect("host programs") = Some((Instant::now(), programs));
}

/// The programs of the last fresh line. Empty when the PC's memory was not
/// tight then, or on Linux.
pub fn programs() -> Vec<crate::memory::Program> {
    match &*PROGRAMS.lock().expect("host programs") {
        Some((at, programs)) if fresh(at.elapsed()) => programs.clone(),
        _ => Vec::new(),
    }
}

/// The last feed, when it is fresh.
pub fn current() -> Option<Feed> {
    let (at, feed) = (*LAST.lock().expect("host feed"))?;
    fresh(at.elapsed()).then_some(feed)
}

fn fresh(age: Duration) -> bool {
    age < FEED_STALE
}

/// Reads the feed from stdin until it closes. On Linux nothing writes it.
pub fn spawn_reader() {
    std::thread::spawn(|| {
        for text in std::io::stdin().lines() {
            let Ok(text) = text else { break };
            if let Some((feed, programs)) = parse_with(&text) {
                note(feed);
                note_programs(programs);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feed_line_round_trips_and_anything_else_is_ignored() {
        let feed = Feed {
            idle_secs: Some(42),
            memory_total: Some(8 << 30),
            memory_available: None,
            cpus: Some(8),
            cpu_busy_percent: Some(12),
            disk_available: None,
            on_battery: Some(true),
            battery_percent: Some(72),
            presenting: None,
            utc_offset_secs: Some(-10_800),
        };
        let text = line(&feed);
        assert!(text.starts_with("host-feed {") && text.ends_with("}\n"));
        assert!(!text.contains("memory_available"));
        assert_eq!(parse(&text), Some(feed));
        assert_eq!(parse("host-feed {}"), Some(Feed::default()));
        let chrome = crate::memory::Program {
            name: "chrome".into(),
            processes: 38,
            bytes: 3 << 30,
        };
        let with = line_with(&feed, std::slice::from_ref(&chrome));
        assert_eq!(parse_with(&with), Some((feed, vec![chrome])));
        assert_eq!(parse(&with), Some(feed));
        assert!(!text.contains("programs"));
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("host-feed not json"), None);
        // A key from a newer supervisor does not break an older service.
        assert_eq!(
            parse(r#"host-feed {"idle_secs":1,"later":true}"#)
                .unwrap()
                .idle_secs,
            Some(1)
        );
    }

    #[test]
    fn an_old_feed_is_forgotten() {
        assert!(fresh(Duration::from_secs(44)));
        assert!(!fresh(FEED_STALE));
    }
}

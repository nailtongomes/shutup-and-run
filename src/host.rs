//! What the Windows side knows and the service inside WSL2 cannot read: how
//! long the person has not touched the PC, and the PC's own memory. The
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
}

static LAST: Mutex<Option<(Instant, Feed)>> = Mutex::new(None);

pub fn line(feed: &Feed) -> String {
    format!(
        "{FEED_MARK}{}\n",
        serde_json::to_string(feed).unwrap_or_else(|_| "{}".into())
    )
}

pub fn parse(line: &str) -> Option<Feed> {
    serde_json::from_str(line.trim().strip_prefix(FEED_MARK.trim_end())?.trim()).ok()
}

pub fn note(feed: Feed) {
    *LAST.lock().expect("host feed") = Some((Instant::now(), feed));
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
            if let Some(feed) = parse(&text) {
                note(feed);
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
        };
        let text = line(&feed);
        assert!(text.starts_with("host-feed {") && text.ends_with("}\n"));
        assert!(!text.contains("memory_available"));
        assert_eq!(parse(&text), Some(feed));
        assert_eq!(parse("host-feed {}"), Some(Feed::default()));
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

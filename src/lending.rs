//! When this machine lends itself at all, apart from whether it has room
//! (`src/resources.rs`) and whether that is safe for its owner
//! (`src/safety.rs`):
//!
//! - inside `lend_hours`, when it is set: on Windows the clock the person
//!   sees (the supervisor sends its offset from UTC), else the machine's;
//! - on Windows, not on a battery below [`BATTERY_MIN_PERCENT`]. On a battery
//!   above it a job starts, and the page says the PC is on its battery;
//! - on Windows, not while the person at the PC presents or has a program in
//!   full screen (a game, a video, a slide show, a call in full screen).
//!
//! Each is a wait for a new job, read when a job asks to start and before a
//! poll. A running job is never touched by any of them. A Windows reading
//! that is missing or stale does not hold, like `min_idle_secs`: a
//! supervisor that hangs must not park the worker for good.

use crate::host::Feed;

/// On its battery and below this, a Windows PC starts no new job. A
/// constant, like the safety thresholds: it protects the person at the PC.
pub const BATTERY_MIN_PERCENT: u64 = 60;

pub const OUTSIDE_LEND_HOURS: &str = "OUTSIDE_LEND_HOURS";
pub const BATTERY_LOW: &str = "BATTERY_LOW";
pub const USER_PRESENTING: &str = "USER_PRESENTING";
/// Not a wait: the PC is on its battery and above the minimum.
pub const ON_BATTERY: &str = "ON_BATTERY";

/// Why a new job waits. It can start later, with nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hold {
    pub code: String,
    pub detail: String,
}

impl std::fmt::Display for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

/// The first reason there is to wait, or None.
pub fn hold(hours: Option<&LendHours>, feed: Option<Feed>, now_unix: i64) -> Option<Hold> {
    let hold = |code: &str, detail: String| {
        Some(Hold {
            code: code.to_string(),
            detail,
        })
    };
    if let Some(hours) = hours {
        let offset = offset_secs(feed, now_unix);
        let now = Local::at(now_unix, offset);
        if !hours.allows(now) {
            return hold(
                OUTSIDE_LEND_HOURS,
                format!(
                    "outside lend_hours {} (now {now}, {})",
                    hours.text,
                    zone(offset)
                ),
            );
        }
    }
    let feed = feed?;
    if feed.on_battery == Some(true) {
        match feed.battery_percent {
            Some(percent) if percent >= BATTERY_MIN_PERCENT => {}
            Some(percent) => {
                return hold(
                    BATTERY_LOW,
                    format!(
                        "the PC is on its battery at {percent}%, {BATTERY_MIN_PERCENT}% needed"
                    ),
                )
            }
            None => {
                return hold(
                    BATTERY_LOW,
                    "the PC is on its battery and Windows does not say how full".to_string(),
                )
            }
        }
    }
    if feed.presenting == Some(true) {
        return hold(
            USER_PRESENTING,
            "the person at the PC is presenting or has a program in full screen".to_string(),
        );
    }
    None
}

/// The battery's charge while the PC runs on it and jobs still start.
pub fn on_battery(feed: Option<Feed>) -> Option<u64> {
    battery(feed).filter(|percent| *percent >= BATTERY_MIN_PERCENT)
}

/// The battery's charge while the PC runs on it, above or below the minimum.
pub fn battery(feed: Option<Feed>) -> Option<u64> {
    let feed = feed?;
    (feed.on_battery == Some(true))
        .then_some(feed.battery_percent)
        .flatten()
}

/// A time of the week, on the machine's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    /// Monday is 0.
    pub weekday: usize,
    /// Minutes since midnight.
    pub minute: u32,
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

impl Local {
    /// `offset_secs` from [`offset_secs`].
    pub fn at(now_unix: i64, offset_secs: i64) -> Self {
        Self::of(now_unix + offset_secs)
    }

    /// From seconds since 1970 already moved to local time. 1970-01-01 was
    /// a Thursday.
    pub fn of(local_secs: i64) -> Self {
        Self {
            weekday: (local_secs.div_euclid(86_400) + 3).rem_euclid(7) as usize,
            minute: (local_secs.rem_euclid(86_400) / 60) as u32,
        }
    }
}

impl std::fmt::Display for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:02}:{:02}",
            DAYS[self.weekday],
            self.minute / 60,
            self.minute % 60
        )
    }
}

/// How far the clock `lend_hours` is read on is ahead of UTC: the Windows
/// clock when the supervisor sent it, else this machine's own.
pub fn offset_secs(feed: Option<Feed>, now_unix: i64) -> i64 {
    feed.and_then(|feed| feed.utc_offset_secs)
        .unwrap_or_else(|| utc_offset_secs(now_unix))
}

/// The time of the week now on that clock, and its zone: `wed 18:30,
/// UTC-03:00`. Shown beside `lend_hours`, so a wrong clock is easy to see.
pub fn clock(feed: Option<Feed>, now_unix: i64) -> String {
    let offset = offset_secs(feed, now_unix);
    format!("{}, {}", Local::at(now_unix, offset), zone(offset))
}

/// `15:30` on that clock, for the end of a pause.
pub fn hour_at(feed: Option<Feed>, at_unix: i64) -> String {
    let local = Local::at(at_unix, offset_secs(feed, at_unix));
    format!("{:02}:{:02}", local.minute / 60, local.minute % 60)
}

/// Seconds from now to the next midnight on that clock: a pause "for the
/// rest of the day". At least a minute.
pub fn secs_to_midnight(feed: Option<Feed>, now_unix: i64) -> u64 {
    secs_to_minute(0, offset_secs(feed, now_unix), now_unix)
}

/// Seconds from now to the next `minute` of the day (0 to 1439) on a clock
/// `offset` seconds ahead of UTC: `pause --until 08:00`. At least a minute;
/// the minute now is tomorrow's.
pub fn secs_to_minute(minute: u32, offset: i64, now_unix: i64) -> u64 {
    let local = (now_unix + offset).rem_euclid(86_400);
    let mut secs = i64::from(minute) * 60 - local;
    if secs <= 0 {
        secs += 86_400;
    }
    secs.max(60) as u64
}

/// `UTC-03:00`.
pub fn zone(offset_secs: i64) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let minutes = offset_secs.unsigned_abs() / 60;
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// What the C library says the local time is ahead of UTC: `TZ`, else
/// `/etc/localtime`, which WSL sets from Windows. 0 where it cannot say.
#[cfg(unix)]
// `tm_gmtoff` is a C long: i64 here, i32 on a 32-bit target.
#[allow(clippy::unnecessary_cast)]
fn utc_offset_secs(now_unix: i64) -> i64 {
    let time = now_unix as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r is the reentrant form; it fills the one tm given
    // and returns null on failure, and the tm is read only when it did not.
    unsafe {
        if libc::localtime_r(&time, tm.as_mut_ptr()).is_null() {
            return 0;
        }
        tm.assume_init().tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
fn utc_offset_secs(_now_unix: i64) -> i64 {
    0
}

/// `lend_hours`: when the machine lends itself. One or more windows split
/// by `;`, each `[DAYS] HH[:MM]-HH[:MM]`, or only `DAYS` for the whole day.
/// DAYS is a list like `mon-fri` or `sat,sun`; without it, every day. A
/// window whose end comes before its start goes past midnight and belongs
/// to the day it starts: `fri 19-07` runs into Saturday morning.
///
/// `19-07`, `mon-fri 19-07; sat,sun`, `mon-fri 12:00-13:30`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LendHours {
    pub text: String,
    windows: Vec<Window>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Window {
    days: [bool; 7],
    start: u32,
    end: u32,
}

impl LendHours {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut windows = Vec::new();
        for part in text.split(';').map(str::trim) {
            if part.is_empty() {
                continue;
            }
            windows.push(window(part).map_err(|why| {
                format!(
                    "lend_hours {text:?}: {why}. Write windows like \"19-07\", \"mon-fri 19-07\" or \"mon-fri 19-07; sat,sun\""
                )
            })?);
        }
        if windows.is_empty() {
            return Err("lend_hours is empty: leave the key out to lend at any time".to_string());
        }
        Ok(Self {
            text: text.trim().to_string(),
            windows,
        })
    }

    pub fn allows(&self, now: Local) -> bool {
        let yesterday = (now.weekday + 6) % 7;
        self.windows.iter().any(|w| {
            if w.start < w.end {
                w.days[now.weekday] && (w.start..w.end).contains(&now.minute)
            } else {
                (w.days[now.weekday] && now.minute >= w.start)
                    || (w.days[yesterday] && now.minute < w.end)
            }
        })
    }
}

fn window(part: &str) -> Result<Window, String> {
    let mut words = part.split_whitespace();
    let (first, second) = (words.next().unwrap_or(""), words.next());
    if words.next().is_some() {
        return Err(format!("{part:?} has more than days and hours"));
    }
    let (days, hours) = match second {
        Some(hours) => (days(first)?, Some(hours)),
        None if first.starts_with(|c: char| c.is_ascii_digit()) => ([true; 7], Some(first)),
        None => (days(first)?, None),
    };
    let (start, end) = match hours {
        None => (0, 24 * 60),
        Some(hours) => {
            let (start, end) = hours
                .split_once('-')
                .ok_or_else(|| format!("{hours:?} is not a range like 19-07"))?;
            (minute(start)?, minute(end)?)
        }
    };
    if start == end || start >= 24 * 60 {
        return Err(format!("{part:?} is an empty window"));
    }
    Ok(Window { days, start, end })
}

fn days(text: &str) -> Result<[bool; 7], String> {
    let day = |name: &str| {
        DAYS.iter()
            .position(|d| d.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("{name:?} is not a day: use mon, tue, wed, thu, fri, sat, sun"))
    };
    let mut days = [false; 7];
    for item in text.split(',') {
        match item.split_once('-') {
            Some((from, to)) => {
                let (from, to) = (day(from)?, day(to)?);
                let mut at = from;
                loop {
                    days[at] = true;
                    if at == to {
                        break;
                    }
                    at = (at + 1) % 7;
                }
            }
            None => days[day(item)?] = true,
        }
    }
    Ok(days)
}

/// `7`, `07`, `19:30`, `24`.
fn minute(text: &str) -> Result<u32, String> {
    let bad = || format!("{text:?} is not an hour like 19 or 19:30");
    let (hours, minutes) = text.split_once(':').unwrap_or((text, "0"));
    if hours.len() > 2 || minutes.len() > 2 {
        return Err(bad());
    }
    let hours: u32 = hours.parse().map_err(|_| bad())?;
    let minutes: u32 = minutes.parse().map_err(|_| bad())?;
    if hours > 24 || minutes > 59 || (hours == 24 && minutes > 0) {
        return Err(bad());
    }
    Ok(hours * 60 + minutes)
}

/// One line a person understands, per code.
pub fn label(code: &str, portuguese: bool) -> &'static str {
    let (en, pt) = match code {
        OUTSIDE_LEND_HOURS => (
            "Outside the hours this machine lends itself (lend_hours)",
            "Fora do horário em que esta máquina é emprestada (lend_hours)",
        ),
        BATTERY_LOW => (
            "The PC is on its battery, below 60%",
            "O PC está na bateria, abaixo de 60%",
        ),
        USER_PRESENTING => (
            "The person at the PC is presenting or has a program in full screen",
            "Quem usa o PC está apresentando ou com um programa em tela cheia",
        ),
        _ => ("Waiting", "Esperando"),
    };
    if portuguese {
        pt
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(weekday: usize, hour: u32, minute: u32) -> Local {
        Local {
            weekday,
            minute: hour * 60 + minute,
        }
    }

    #[test]
    fn a_night_window_runs_into_the_next_morning_of_its_own_days() {
        let hours = LendHours::parse("mon-fri 19-07").unwrap();
        assert!(hours.allows(at(0, 19, 0)));
        assert!(hours.allows(at(1, 6, 59)), "Monday night");
        assert!(!hours.allows(at(1, 7, 0)));
        assert!(!hours.allows(at(2, 12, 0)));
        assert!(hours.allows(at(5, 6, 0)), "Friday night into Saturday");
        assert!(!hours.allows(at(5, 19, 0)), "Saturday evening");
        assert!(!hours.allows(at(0, 6, 0)), "Sunday night is not a window");
    }

    #[test]
    fn several_windows_days_alone_and_minutes() {
        let hours = LendHours::parse("mon-fri 19-07; sat,sun").unwrap();
        assert!(hours.allows(at(6, 12, 0)));
        assert!(hours.allows(at(5, 3, 0)), "Saturday is a whole day");
        assert!(
            !hours.allows(at(0, 3, 0)),
            "Sunday night is not a weekday night"
        );
        assert!(!hours.allows(at(0, 8, 0)));
        let lunch = LendHours::parse("mon-fri 12:00-13:30").unwrap();
        assert!(lunch.allows(at(2, 13, 29)) && !lunch.allows(at(2, 13, 30)));
        let every = LendHours::parse("19-07").unwrap();
        assert!(every.allows(at(6, 23, 0)) && every.allows(at(0, 6, 0)));
        assert!(!every.allows(at(3, 12, 0)));
        let wrap = LendHours::parse("fri-mon 00-24").unwrap();
        assert!(wrap.allows(at(0, 1, 0)) && wrap.allows(at(6, 1, 0)) && !wrap.allows(at(2, 1, 0)));
        assert_eq!(LendHours::parse(" 19-07 ").unwrap().text, "19-07");
    }

    #[test]
    fn a_window_that_cannot_be_read_is_refused() {
        for bad in [
            "",
            ";",
            "19",
            "7-7",
            "25-07",
            "19:60-07",
            "funday 19-07",
            "mon-fri 19-07 extra",
            "24-07",
            "007-08",
        ] {
            assert!(LendHours::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_clock_gives_the_day_and_the_minute() {
        // 2026-11-18 is a Wednesday; 14:05 UTC.
        let wednesday = 1_795_010_700;
        assert_eq!(Local::of(wednesday), at(2, 14, 5));
        assert_eq!(Local::of(0), at(3, 0, 0));
        assert_eq!(Local::of(-60), at(2, 23, 59));
        assert_eq!(at(2, 14, 5).to_string(), "wed 14:05");
    }

    #[test]
    fn lend_hours_follow_the_windows_clock_when_the_supervisor_sends_it() {
        // Wednesday 21:30 UTC is 18:30 in São Paulo: outside "19-07" there.
        let now = 1_795_010_700 + 7 * 3600 + 25 * 60;
        let hours = LendHours::parse("19-07").unwrap();
        let sao_paulo = Feed {
            utc_offset_secs: Some(-3 * 3600),
            ..Feed::default()
        };
        let held = hold(Some(&hours), Some(sao_paulo), now).unwrap();
        assert_eq!(held.code, OUTSIDE_LEND_HOURS);
        assert_eq!(
            held.detail,
            "outside lend_hours 19-07 (now wed 18:30, UTC-03:00)"
        );
        let utc = Feed {
            utc_offset_secs: Some(0),
            ..Feed::default()
        };
        assert_eq!(hold(Some(&hours), Some(utc), now), None);
        assert_eq!(offset_secs(Some(sao_paulo), now), -10_800);
        assert_eq!(clock(Some(sao_paulo), now), "wed 18:30, UTC-03:00");
        assert_eq!(hour_at(Some(sao_paulo), now + 3600), "19:30");
        // 18:30 in São Paulo: five and a half hours to its midnight.
        assert_eq!(secs_to_midnight(Some(sao_paulo), now), 5 * 3600 + 1800);
        assert_eq!(zone(19_800), "UTC+05:30");
        assert_eq!(zone(0), "UTC+00:00");
    }

    #[test]
    fn a_battery_below_sixty_percent_or_a_presentation_holds_and_a_missing_reading_does_not() {
        let feed = |on_battery, percent, presenting| Feed {
            on_battery,
            battery_percent: percent,
            presenting,
            ..Feed::default()
        };
        let code = |f: Option<Feed>| hold(None, f, 0).map(|h| h.code);
        assert_eq!(
            code(Some(feed(Some(true), Some(59), None))).as_deref(),
            Some(BATTERY_LOW)
        );
        assert_eq!(
            code(Some(feed(Some(true), None, None))).as_deref(),
            Some(BATTERY_LOW)
        );
        assert_eq!(code(Some(feed(Some(true), Some(60), None))), None);
        assert_eq!(on_battery(Some(feed(Some(true), Some(72), None))), Some(72));
        assert_eq!(on_battery(Some(feed(Some(true), Some(40), None))), None);
        assert_eq!(code(Some(feed(Some(false), Some(10), None))), None);
        assert_eq!(on_battery(Some(feed(Some(false), Some(90), None))), None);
        assert_eq!(
            code(Some(feed(None, None, Some(true)))).as_deref(),
            Some(USER_PRESENTING)
        );
        assert_eq!(code(Some(Feed::default())), None);
        assert_eq!(code(None), None);
        // The hours come first.
        let never = LendHours::parse("mon 00:00-00:01").unwrap();
        let held = hold(
            Some(&never),
            Some(feed(Some(true), Some(10), None)),
            1_795_010_700,
        );
        assert_eq!(held.unwrap().code, OUTSIDE_LEND_HOURS);
        for code in [OUTSIDE_LEND_HOURS, BATTERY_LOW, USER_PRESENTING] {
            assert_ne!(label(code, false), "Waiting");
        }
    }
}

//! Download queues and their schedules.
//!
//! A queue is an ordered list of downloads that run when the queue is started,
//! by hand or by its schedule, at most `max_concurrent` at a time. Downloads
//! wait in it as `waiting` until their turn. The daemon owns the queues and
//! saves them with the history; this module holds the types and the calendar
//! arithmetic, which needs no clock of its own so it can be tested.
//!
//! Times are the computer's local time, as minutes since midnight, and days
//! are counted from 1970-01-01.

use serde::{Deserialize, Serialize};

/// The queue that always exists; "Download later" uses it by default.
pub const MAIN_QUEUE: u64 = 1;
pub const MAIN_QUEUE_NAME: &str = "Main queue";
/// Longest a countdown before shutting down or sleeping runs, in seconds.
pub const COUNTDOWN_SECS: u32 = 60;

/// What happens when a queue's run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhenDone {
    #[default]
    Nothing,
    /// Close UIDM's window and daemon.
    Exit,
    Sleep,
    Hibernate,
    ShutDown,
}

impl WhenDone {
    pub const ALL: [WhenDone; 5] = [
        WhenDone::Nothing,
        WhenDone::Exit,
        WhenDone::Sleep,
        WhenDone::Hibernate,
        WhenDone::ShutDown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WhenDone::Nothing => "Do nothing",
            WhenDone::Exit => "Exit UIDM",
            WhenDone::Sleep => "Sleep",
            WhenDone::Hibernate => "Hibernate",
            WhenDone::ShutDown => "Shut down",
        }
    }

    /// Actions that turn the computer off or put it to sleep, which get a
    /// countdown the user can cancel.
    pub fn needs_countdown(self) -> bool {
        matches!(self, WhenDone::Sleep | WhenDone::Hibernate | WhenDone::ShutDown)
    }
}

/// When a queue starts and stops by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Start time, `HH:MM`.
    pub start: String,
    /// Stop time, `HH:MM`. A stop earlier than the start means the next day.
    /// Without one the queue runs until it is done.
    #[serde(default)]
    pub stop: Option<String>,
    /// Days to run on, 0 = Monday through 6 = Sunday. Ignored with a `date`.
    #[serde(default)]
    pub days: Vec<u8>,
    /// Run once, on this date (`YYYY-MM-DD`), instead of on `days`.
    #[serde(default)]
    pub date: Option<String>,
    /// Start again this many minutes after the last start, until the run's
    /// stop time (or midnight without one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_minutes: Option<u32>,
}

/// Longest gap "start again every" allows, in minutes: 99 hours 59 minutes,
/// as IDM's fields hold.
pub const REPEAT_MAX_MINUTES: u32 = 99 * 60 + 59;

/// A queue as saved and as sent to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    pub id: u64,
    pub name: String,
    /// How many of its downloads run at once.
    pub max_concurrent: usize,
    /// Whether it is started.
    #[serde(default)]
    pub running: bool,
    /// Its downloads, in the order they run.
    #[serde(default)]
    pub order: Vec<u64>,
    #[serde(default)]
    pub schedule: Option<Schedule>,
    #[serde(default)]
    pub when_done: WhenDone,
    /// Replaces `when_done` for the current run only.
    #[serde(default)]
    pub when_done_once: Option<WhenDone>,
    /// Times a failed download is started again before the queue counts it
    /// as finished.
    #[serde(default)]
    pub retries: u32,
    /// The day of the scheduled run last started, so it starts only once.
    #[serde(default)]
    pub last_scheduled_day: Option<i64>,
    /// When the schedule last started it, in minutes since 1970 local time,
    /// for starting it again every so often.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scheduled_start: Option<i64>,
    /// Whether the current run was started by the schedule (and so is stopped
    /// by it).
    #[serde(default)]
    pub started_by_schedule: bool,
    /// A program or file to open each time a run of it finishes, before
    /// `when_done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_when_done: Option<String>,
    /// Start it whenever UIDM starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_on_launch: bool,
    /// Shutting down when done closes programs that would hold it up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_close: bool,
}

pub const QUEUE_CONCURRENCY_RANGE: (usize, usize) = (1, 10);
pub const QUEUE_RETRIES_MAX: u32 = 10;

impl Queue {
    pub fn new(id: u64, name: &str) -> Self {
        Queue {
            id,
            name: name.to_string(),
            max_concurrent: 1,
            running: false,
            order: Vec::new(),
            schedule: None,
            when_done: WhenDone::Nothing,
            when_done_once: None,
            retries: 0,
            last_scheduled_day: None,
            last_scheduled_start: None,
            started_by_schedule: false,
            open_when_done: None,
            start_on_launch: false,
            force_close: false,
        }
    }

    pub fn main() -> Self {
        Queue::new(MAIN_QUEUE, MAIN_QUEUE_NAME)
    }

    /// Check the parts a user edits, with a message fit to show them.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("A queue needs a name.".to_string());
        }
        let (lo, hi) = QUEUE_CONCURRENCY_RANGE;
        if !(lo..=hi).contains(&self.max_concurrent) {
            return Err(format!("A queue runs between {lo} and {hi} downloads at once."));
        }
        if self.retries > QUEUE_RETRIES_MAX {
            return Err(format!("A queue retries a download at most {QUEUE_RETRIES_MAX} times."));
        }
        if let Some(s) = &self.schedule {
            s.validate()?;
        }
        if let Some(f) = self.open_when_done.as_deref().filter(|f| !std::path::Path::new(f.trim()).is_absolute()) {
            return Err(format!("The file to open must be a full path: {f}"));
        }
        Ok(())
    }
}

/// A local date and time, to the minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Moment {
    /// Days since 1970-01-01.
    pub day: i64,
    /// Minutes since midnight.
    pub minute: u32,
}

impl Moment {
    /// Minutes since 1970-01-01 00:00.
    pub fn total(self) -> i64 {
        self.day * 1440 + self.minute as i64
    }

    pub fn from_total(total: i64) -> Self {
        Moment {
            day: total.div_euclid(1440),
            minute: total.rem_euclid(1440) as u32,
        }
    }

    /// `Mon 2026-10-26 23:00`.
    pub fn display(self) -> String {
        let (y, m, d) = civil_from_days(self.day);
        let name = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][weekday(self.day) as usize];
        format!("{name} {y:04}-{m:02}-{d:02} {:02}:{:02}", self.minute / 60, self.minute % 60)
    }
}

/// Days since 1970-01-01 for a calendar date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The calendar date of a day number.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// 0 = Monday through 6 = Sunday. 1970-01-01 was a Thursday.
pub fn weekday(day: i64) -> u8 {
    (day + 3).rem_euclid(7) as u8
}

/// `HH:MM` to minutes since midnight.
pub fn parse_time(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    if !(1..=2).contains(&h.len()) || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(time: *mut SystemTime);
}

/// The computer's local date and time now.
pub fn local_now() -> Moment {
    let mut t = SystemTime::default();
    // SAFETY: fills the struct it is given.
    unsafe { GetLocalTime(&mut t) };
    Moment {
        day: days_from_civil(t.year as i64, t.month as u32, t.day as u32),
        minute: t.hour as u32 * 60 + t.minute as u32,
    }
}

/// `YYYY-MM-DD` to a day number.
pub fn parse_date(s: &str) -> Option<i64> {
    let mut parts = s.trim().splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || d == 0 {
        return None;
    }
    let day = days_from_civil(y, m, d);
    // Reject the 31st of a 30-day month and the like.
    (civil_from_days(day) == (y, m, d)).then_some(day)
}

impl Schedule {
    pub fn validate(&self) -> Result<(), String> {
        let start = parse_time(&self.start)
            .ok_or_else(|| format!("\"{}\" is not a start time. Use HH:MM, such as 23:00.", self.start))?;
        if let Some(stop) = &self.stop {
            let stop_min =
                parse_time(stop).ok_or_else(|| format!("\"{stop}\" is not a stop time. Use HH:MM, such as 07:00."))?;
            if stop_min == start {
                return Err("The stop time must differ from the start time.".to_string());
            }
        }
        if let Some(every) = self.repeat_minutes {
            if !(1..=REPEAT_MAX_MINUTES).contains(&every) {
                return Err("Start again every 1 minute to 99 hours 59 minutes.".to_string());
            }
        }
        match &self.date {
            Some(date) => {
                parse_date(date).ok_or_else(|| format!("\"{date}\" is not a date. Use YYYY-MM-DD."))?;
            }
            None => {
                if self.days.is_empty() {
                    return Err("Pick at least one day, or a date, for the schedule.".to_string());
                }
                if self.days.iter().any(|d| *d > 6) {
                    return Err("Days of the week run from 0 (Monday) to 6 (Sunday).".to_string());
                }
            }
        }
        Ok(())
    }

    fn start_minute(&self) -> Option<u32> {
        parse_time(&self.start)
    }

    /// Whether a run starts on `day`.
    fn runs_on(&self, day: i64) -> bool {
        match &self.date {
            Some(date) => parse_date(date) == Some(day),
            None => self.days.contains(&weekday(day)),
        }
    }

    /// When the run that starts on `day` ends: at the stop time (the next day
    /// if it is earlier than the start), or at the end of the day without one.
    pub fn end_of_run(&self, day: i64) -> Option<Moment> {
        let start = self.start_minute()?;
        let end = match self.stop.as_deref().and_then(parse_time) {
            Some(stop) if stop > start => day * 1440 + stop as i64,
            Some(stop) => (day + 1) * 1440 + stop as i64,
            None => (day + 1) * 1440,
        };
        Some(Moment::from_total(end))
    }

    /// The day whose run is open at `now`, if any: its start has passed and
    /// its end has not.
    pub fn open_run(&self, now: Moment) -> Option<i64> {
        let start = self.start_minute()?;
        // A run that crosses midnight can still be open from yesterday.
        [now.day, now.day - 1].into_iter().find(|&day| {
            self.runs_on(day)
                && now.total() >= day * 1440 + start as i64
                && self.end_of_run(day).is_some_and(|end| now < end)
        })
    }

    /// The day of the run to start at `now`, if one is due. `last_day` and
    /// `last_start` are the day and time (minutes since 1970) the schedule
    /// last started it. Each run starts once at its start time and, with
    /// `repeat_minutes`, again that long after each start while it is open.
    pub fn due(&self, now: Moment, last_day: Option<i64>, last_start: Option<i64>) -> Option<i64> {
        let day = self.open_run(now)?;
        let Some(every) = self.repeat_minutes else {
            return (last_day != Some(day)).then_some(day);
        };
        let opened = day * 1440 + self.start_minute()? as i64;
        let next = match last_start {
            Some(last) if last >= opened => last + every as i64,
            _ => opened,
        };
        (now.total() >= next).then_some(day)
    }

    /// When it next starts by itself after `now`, counting a repeat after
    /// `last_start` (minutes since 1970).
    pub fn next_run(&self, now: Moment, last_start: Option<i64>) -> Option<Moment> {
        if let (Some(every), Some(day)) = (self.repeat_minutes, self.open_run(now)) {
            let opened = day * 1440 + self.start_minute()? as i64;
            let again = last_start.filter(|last| *last >= opened).map(|last| Moment::from_total(last + every as i64));
            if let Some(again) = again.filter(|m| self.end_of_run(day).is_some_and(|end| *m < end)) {
                return Some(again);
            }
        }
        self.next_start(now)
    }

    /// The next start after `now`, within the coming week (or on the date).
    pub fn next_start(&self, now: Moment) -> Option<Moment> {
        let start = self.start_minute()? as i64;
        (now.day..=now.day + 7)
            .map(|day| Moment::from_total(day * 1440 + start))
            .find(|m| *m > now && self.runs_on(m.day))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(date: &str, time: &str) -> Moment {
        Moment {
            day: parse_date(date).unwrap(),
            minute: parse_time(time).unwrap(),
        }
    }

    fn nightly(days: &[u8]) -> Schedule {
        Schedule {
            start: "23:00".into(),
            stop: Some("07:00".into()),
            days: days.to_vec(),
            date: None,
            repeat_minutes: None,
        }
    }

    #[test]
    fn calendar_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        for day in [-800_000, -1, 0, 59, 10_957, 20_752, 2_000_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
        // 2026-10-26 is a Monday; 2000-02-29 existed, 2100-02-29 does not.
        assert_eq!(weekday(parse_date("2026-10-26").unwrap()), 0);
        assert_eq!(weekday(parse_date("2026-11-01").unwrap()), 6);
        assert!(parse_date("2000-02-29").is_some());
        assert!(parse_date("2100-02-29").is_none());
        assert!(parse_date("2026-04-31").is_none());
        assert!(parse_date("2026-13-01").is_none());
        assert!(parse_date("tomorrow").is_none());
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("00:00"), Some(0));
        assert_eq!(parse_time(" 23:59 "), Some(1439));
        assert_eq!(parse_time("7:05"), Some(425));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("12:60"), None);
        assert_eq!(parse_time("noon"), None);
        assert_eq!(parse_time("7:5"), None);
        assert_eq!(parse_time("007:05"), None);
        assert_eq!(at("2026-10-26", "23:00").display(), "Mon 2026-10-26 23:00");
    }

    #[test]
    fn overnight_run_is_open_across_midnight() {
        // Monday only, 23:00 to 07:00.
        let s = nightly(&[0]);
        let monday = parse_date("2026-10-26").unwrap();
        assert_eq!(s.open_run(at("2026-10-26", "22:59")), None);
        assert_eq!(s.open_run(at("2026-10-26", "23:00")), Some(monday));
        assert_eq!(s.open_run(at("2026-10-27", "03:00")), Some(monday));
        assert_eq!(s.open_run(at("2026-10-27", "07:00")), None);
        // Tuesday night is not scheduled.
        assert_eq!(s.open_run(at("2026-10-27", "23:30")), None);
        assert_eq!(s.end_of_run(monday), Some(at("2026-10-27", "07:00")));
    }

    #[test]
    fn same_day_run_and_no_stop_time() {
        let mut s = Schedule {
            start: "09:00".into(),
            stop: Some("17:30".into()),
            days: vec![0, 1, 2, 3, 4],
            date: None,
            repeat_minutes: None,
        };
        assert!(s.open_run(at("2026-10-28", "12:00")).is_some());
        assert!(s.open_run(at("2026-10-28", "17:30")).is_none());
        // Saturday.
        assert!(s.open_run(at("2026-10-31", "12:00")).is_none());
        // Without a stop time the run is open for the rest of the day.
        s.stop = None;
        assert!(s.open_run(at("2026-10-28", "23:59")).is_some());
        assert!(s.open_run(at("2026-10-29", "08:59")).is_none());
    }

    #[test]
    fn next_start() {
        let s = nightly(&[0, 4]); // Monday and Friday
        assert_eq!(s.next_start(at("2026-10-26", "10:00")), Some(at("2026-10-26", "23:00")));
        assert_eq!(s.next_start(at("2026-10-26", "23:00")), Some(at("2026-10-30", "23:00")));
        assert_eq!(s.next_start(at("2026-10-31", "00:00")), Some(at("2026-11-02", "23:00")));

        let once = Schedule {
            start: "02:00".into(),
            stop: None,
            days: vec![],
            date: Some("2026-12-24".into()),
            repeat_minutes: None,
        };
        assert_eq!(once.next_start(at("2026-12-20", "10:00")), Some(at("2026-12-24", "02:00")));
        assert_eq!(once.next_start(at("2026-12-24", "02:00")), None);
        assert!(once.open_run(at("2026-12-24", "10:00")).is_some());
        assert!(once.open_run(at("2026-12-25", "01:00")).is_none());
    }

    #[test]
    fn a_run_starts_once_without_a_repeat() {
        let s = nightly(&[0]);
        let monday = parse_date("2026-10-26").unwrap();
        assert_eq!(s.due(at("2026-10-26", "23:00"), None, None), Some(monday));
        assert_eq!(s.due(at("2026-10-27", "03:00"), Some(monday), None), None);
        assert_eq!(s.due(at("2026-10-26", "22:00"), None, None), None, "not open yet");
    }

    #[test]
    fn a_repeat_counts_from_the_last_start() {
        // Weekdays 09:00 to 17:00, again every 2 hours 30 minutes.
        let s = Schedule {
            start: "09:00".into(),
            stop: Some("17:00".into()),
            days: vec![0, 1, 2, 3, 4],
            date: None,
            repeat_minutes: Some(150),
        };
        let wednesday = parse_date("2026-10-28").unwrap();
        let started = |time| Some(at("2026-10-28", time).total());
        assert_eq!(s.due(at("2026-10-28", "09:00"), None, None), Some(wednesday));
        assert_eq!(s.due(at("2026-10-28", "11:29"), Some(wednesday), started("09:00")), None);
        assert_eq!(s.due(at("2026-10-28", "11:30"), Some(wednesday), started("09:00")), Some(wednesday));
        // A start that came late, say because the queue still ran, moves
        // the next one along.
        assert_eq!(s.due(at("2026-10-28", "13:00"), Some(wednesday), started("11:45")), None);
        assert_eq!(s.due(at("2026-10-28", "14:15"), Some(wednesday), started("11:45")), Some(wednesday));
        assert_eq!(s.due(at("2026-10-28", "17:00"), Some(wednesday), started("14:15")), None, "the run closed");
        // The next day starts at its start time, whatever ran before.
        let thursday = parse_date("2026-10-29").unwrap();
        assert_eq!(s.due(at("2026-10-29", "09:00"), Some(wednesday), started("16:45")), Some(thursday));

        assert_eq!(s.next_run(at("2026-10-28", "10:00"), started("09:00")), Some(at("2026-10-28", "11:30")));
        // A repeat past the stop time gives way to the next day's start.
        assert_eq!(s.next_run(at("2026-10-28", "16:00"), started("15:00")), Some(at("2026-10-29", "09:00")));
        assert_eq!(s.next_run(at("2026-10-28", "08:00"), None), Some(at("2026-10-28", "09:00")));
    }

    #[test]
    fn local_clock_is_sane() {
        let now = local_now();
        assert!(now.minute < 1440);
        // After 2024-01-01 and before 2200.
        assert!(now.day > days_from_civil(2024, 1, 1) && now.day < days_from_civil(2200, 1, 1));
    }

    #[test]
    fn validation() {
        assert!(nightly(&[0]).validate().is_ok());
        assert!(nightly(&[]).validate().is_err());
        assert!(nightly(&[7]).validate().is_err());
        let bad = |f: fn(&mut Schedule)| {
            let mut s = nightly(&[0]);
            f(&mut s);
            s.validate().is_err()
        };
        assert!(bad(|s| s.start = "25:00".into()));
        assert!(bad(|s| s.stop = Some("23:00".into())));
        assert!(bad(|s| s.stop = Some("x".into())));
        assert!(bad(|s| s.date = Some("2026-02-30".into())));
        assert!(bad(|s| s.repeat_minutes = Some(0)));
        assert!(bad(|s| s.repeat_minutes = Some(REPEAT_MAX_MINUTES + 1)));
        let mut every = nightly(&[0]);
        every.repeat_minutes = Some(90);
        assert!(every.validate().is_ok());

        let mut q = Queue::new(2, "Night");
        assert!(q.validate().is_ok());
        q.max_concurrent = 0;
        assert!(q.validate().is_err());
        q.max_concurrent = 3;
        q.name = " ".into();
        assert!(q.validate().is_err());
        q.name = "Night".into();
        q.retries = 11;
        assert!(q.validate().is_err());
    }
}

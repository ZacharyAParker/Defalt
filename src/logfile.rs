//! logs/console.log: what the console noticed, and what the station said.
//!
//! A bug report is only as good as what was written down before anyone knew
//! there was a bug, so this keeps a rolling file of the console's own
//! diagnostics and every line the station child prints. Five old files of
//! five megabytes each, like the station's own logs/station.log, and every
//! line starts with the same ISO stamp so the two can be merged by time.
//!
//! Nothing here may fail loudly. A log that cannot be written is a log that
//! is not written; the console carries on.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const BACKUPS: usize = 5;

static LOG: OnceLock<Mutex<Rolling>> = OnceLock::new();

/// Start logging into `<root>/logs/console.log`. Once; later calls do nothing.
pub fn init(root: &Path) {
    let rolling = Rolling::new(root.join("logs").join("console.log"), MAX_BYTES, BACKUPS);
    if LOG.set(Mutex::new(rolling)).is_ok() {
        write("console", &format!(
            "console starting, v{}, pid {}", env!("CARGO_PKG_VERSION"), std::process::id()));
    }
}

/// One line (or several), tagged with where it came from.
pub fn write(tag: &str, text: &str) {
    if let Some(log) = LOG.get() {
        if let Ok(mut log) = log.lock() {
            log.write(tag, text);
        }
    }
}

/// A line the station child printed.
pub fn station_line(line: &str) {
    write("station", line);
}

/// `eprintln!`, and into the log as well.
macro_rules! log {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        eprintln!("{line}");
        $crate::logfile::write("console", &line);
    }};
}
pub(crate) use log;

/// The same scheme as Python's rotating handler: console.log.1 is the newest
/// old file, console.log.5 the oldest kept.
pub struct Rolling {
    path: PathBuf,
    max_bytes: u64,
    backups: usize,
    file: Option<File>,
    size: u64,
}

impl Rolling {
    pub fn new(path: PathBuf, max_bytes: u64, backups: usize) -> Self {
        Rolling { path, max_bytes, backups, file: None, size: 0 }
    }

    fn open(&mut self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    fn numbered(&self, index: usize) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{index}"));
        self.path.with_file_name(name)
    }

    fn rotate(&mut self) {
        self.file = None;
        for index in (1..self.backups).rev() {
            let older = self.numbered(index);
            if older.exists() {
                let _ = std::fs::rename(&older, self.numbered(index + 1));
            }
        }
        if self.backups > 0 {
            let _ = std::fs::rename(&self.path, self.numbered(1));
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub fn write(&mut self, tag: &str, text: &str) {
        self.write_at(tag, text, now_ms());
    }

    pub fn write_at(&mut self, tag: &str, text: &str, at_ms: i64) {
        let prefix = format!("{} [{tag}] ", stamp(at_ms, offset()));
        let mut data = String::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            data.push_str(&prefix);
            data.push_str(line.trim_end());
            data.push('\n');
        }
        if data.is_empty() {
            return;
        }
        if self.file.is_none() && self.open().is_err() {
            return;
        }
        if self.size > 0 && self.size + data.len() as u64 > self.max_bytes {
            self.rotate();
            if self.open().is_err() {
                return;
            }
        }
        match self.file.as_mut().map(|file| file.write_all(data.as_bytes())) {
            Some(Ok(())) => self.size += data.len() as u64,
            _ => self.file = None,
        }
    }
}

/* ── Time, without a date crate ──────────────────────────────────────── */

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Seconds east of UTC, asked of the platform each time so a log that runs
/// across a clock change stays right.
pub fn offset() -> i64 {
    crate::local_offset()
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = month as i64;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The wall-clock pieces of a moment, at an offset.
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

pub fn civil_at(at_ms: i64, offset_secs: i64) -> Civil {
    let local = at_ms + offset_secs * 1000;
    let days = local.div_euclid(86_400_000);
    let rest = local.rem_euclid(86_400_000);
    let (year, month, day) = civil(days);
    Civil {
        year, month, day,
        hour: (rest / 3_600_000) as u32,
        minute: (rest / 60_000 % 60) as u32,
        second: (rest / 1000 % 60) as u32,
        millis: (rest % 1000) as u32,
    }
}

pub fn zone(offset_secs: i64) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let minutes = offset_secs.abs() / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `2026-09-22T14:03:05.123-05:00`, the shape Python's
/// `datetime.isoformat(timespec="milliseconds")` writes.
pub fn stamp(at_ms: i64, offset_secs: i64) -> String {
    let c = civil_at(at_ms, offset_secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.millis, zone(offset_secs))
}

/// Milliseconds since the epoch from an ISO stamp. No offset means local.
pub fn parse_stamp(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = text.get(from..from + len)?;
        part.bytes().all(|c| c.is_ascii_digit()).then(|| part.parse().ok())?
    };
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ')
        || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)? as u32, digits(8, 2)? as u32);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut at = 19;
    let mut millis = 0;
    if b.get(at) == Some(&b'.') {
        let start = at + 1;
        let mut end = start;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            return None;
        }
        let fraction = &text[start..end.min(start + 3)];
        millis = format!("{fraction:0<3}").parse::<i64>().ok()?;
        at = end;
    }
    let offset = match b.get(at) {
        None => crate::local_offset(),
        Some(b'Z') if at + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) => {
            let rest = &text[at + 1..];
            let (hours, minutes) = match rest.len() {
                5 if rest.as_bytes()[2] == b':' => (digits(at + 1, 2)?, digits(at + 4, 2)?),
                4 => (digits(at + 1, 2)?, digits(at + 3, 2)?),
                _ => return None,
            };
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'-' { -seconds } else { seconds }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset) * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("defalt-logfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stamps_round_trip_at_any_offset() {
        let at = 1_790_000_000_123;
        for offset in [0, -5 * 3600, 5 * 3600 + 1800, -(9 * 3600 + 30 * 60)] {
            let text = stamp(at, offset);
            assert_eq!(parse_stamp(&text), Some(at), "{text}");
        }
        assert_eq!(stamp(0, 0), "1970-01-01T00:00:00.000+00:00");
        assert_eq!(stamp(951_782_400_000, 0), "2000-02-29T00:00:00.000+00:00");
        assert_eq!(parse_stamp("2026-09-22T14:03:05Z"), Some(1_790_085_785_000));
        assert_eq!(parse_stamp("2026-09-22T14:03:05.5+0000"), Some(1_790_085_785_500));
        assert_eq!(parse_stamp("2026-09-22T14:03:05.123456-05:00"), Some(1_790_103_785_123));
        for bad in ["", "not a time", "2026-13-01T00:00:00Z", "2026-09-22T14:03:05.Z", "2026-09-22T14:03:05+5"] {
            assert_eq!(parse_stamp(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_log_rotates_and_keeps_a_fixed_number_of_files() {
        let dir = scratch("rotate");
        let mut log = Rolling::new(dir.join("console.log"), 200, 2);
        for i in 0..40 {
            log.write("console", &format!("line {i} {}", "x".repeat(20)));
        }
        drop(log);
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["console.log", "console.log.1", "console.log.2"]);
        for name in &names {
            let text = std::fs::read_to_string(dir.join(name)).unwrap();
            assert!(text.len() <= 200);
            for line in text.lines() {
                let (at, rest) = line.split_once(' ').unwrap();
                assert!(parse_stamp(at).is_some(), "{line}");
                assert!(rest.starts_with("[console] line "));
            }
        }
        assert!(std::fs::read_to_string(dir.join("console.log")).unwrap().contains("line 39"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_lines_each_get_a_stamp_and_blank_ones_are_dropped() {
        let dir = scratch("lines");
        let mut log = Rolling::new(dir.join("console.log"), MAX_BYTES, BACKUPS);
        log.write_at("station", "Traceback:\n\n  File x\nValueError\n", 1_790_000_000_000);
        let text = std::fs::read_to_string(dir.join("console.log")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.contains(" [station] ")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

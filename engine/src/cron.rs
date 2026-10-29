//! Cron schedules, as CronJob uses them: five fields (minute hour day-of-month month
//! day-of-week), `*`, lists, ranges, steps, month and weekday names, and the `@hourly` family.
//! Times are UTC, in epoch seconds.

/// A parsed schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    any_day: bool,
    any_weekday: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// One field as a bit set over `low..=high`, and whether it was a bare `*`.
fn field(text: &str, low: u32, high: u32, names: &[&str], first_name: u32) -> Result<(u64, bool), String> {
    let number = |s: &str| -> Result<u32, String> {
        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
            return Ok(i as u32 + first_name);
        }
        s.parse::<u32>()
            .map_err(|_| format!("failed to parse int from {s}: strconv.Atoi: parsing \"{s}\": invalid syntax"))
    };
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step.parse().map_err(|_| {
                    format!("failed to parse int from {step}: strconv.Atoi: parsing \"{step}\": invalid syntax")
                })?;
                if step == 0 {
                    return Err("step of range should be a positive number".into());
                }
                (range, step)
            }
            None => (part, 1),
        };
        let (start, end) = if range == "*" || range == "?" {
            (low, high)
        } else if let Some((a, b)) = range.split_once('-') {
            (number(a)?, number(b)?)
        } else {
            let n = number(range)?;
            // `5/10` means from 5 to the end, in steps of 10.
            (n, if part.contains('/') { high } else { n })
        };
        if start < low || end > high || start > end {
            return Err(format!(
                "beginning of range ({start}) beyond end of range ({end}) or outside {low}-{high}: {part}"
            ));
        }
        let mut value = start;
        while value <= end {
            bits |= 1 << value;
            value += step;
        }
    }
    Ok((bits, text == "*" || text == "?"))
}

pub fn parse(text: &str) -> Result<Schedule, String> {
    let expanded = match text.trim() {
        "@yearly" | "@annually" => "0 0 1 1 *",
        "@monthly" => "0 0 1 * *",
        "@weekly" => "0 0 * * 0",
        "@daily" | "@midnight" => "0 0 * * *",
        "@hourly" => "0 * * * *",
        other => other,
    };
    let fields: Vec<&str> = expanded.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!("expected exactly 5 fields, found {}: {text}", fields.len()));
    }
    let (minutes, _) = field(fields[0], 0, 59, &[], 0)?;
    let (hours, _) = field(fields[1], 0, 23, &[], 0)?;
    let (days, any_day) = field(fields[2], 1, 31, &[], 1)?;
    let (months, _) = field(fields[3], 1, 12, &MONTHS, 1)?;
    let (mut weekdays, any_weekday) = field(fields[4], 0, 7, &WEEKDAYS, 0)?;
    if weekdays & (1 << 7) != 0 {
        weekdays = (weekdays & !(1 << 7)) | 1;
    }
    Ok(Schedule {
        minutes,
        hours: hours as u32,
        days: days as u32,
        months: months as u16,
        weekdays: weekdays as u8,
        any_day,
        any_weekday,
    })
}

/// Days since 1970-01-01 → (year, month, day).
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

impl Schedule {
    fn day_matches(&self, days: i64) -> bool {
        let (_, month, day) = civil(days);
        if self.months & (1 << month) == 0 {
            return false;
        }
        let weekday = (days + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
        let by_day = self.days & (1 << day) != 0;
        let by_weekday = self.weekdays & (1 << weekday) != 0;
        // Like cron: when both are restricted, either one is enough.
        match (self.any_day, self.any_weekday) {
            (false, false) => by_day || by_weekday,
            (false, true) => by_day,
            (true, false) => by_weekday,
            (true, true) => true,
        }
    }

    /// The first scheduled minute strictly after `after`.
    pub fn next_after(&self, after: i64) -> Option<i64> {
        let first_minute = after.div_euclid(60) + 1;
        let first_day = first_minute.div_euclid(1440);
        for day in first_day..first_day + 5 * 366 {
            if !self.day_matches(day) {
                continue;
            }
            let from = if day == first_day {
                first_minute.rem_euclid(1440)
            } else {
                0
            };
            for minute_of_day in from..1440 {
                let (hour, minute) = (minute_of_day / 60, minute_of_day % 60);
                if self.hours & (1 << hour) != 0 && self.minutes & (1 << minute) != 0 {
                    return Some((day * 1440 + minute_of_day) * 60);
                }
            }
        }
        None
    }

    /// The last scheduled minute at or before `until` and strictly after `after`, if any.
    pub fn last_between(&self, after: i64, until: i64) -> Option<i64> {
        let mut found = None;
        let mut at = after;
        while let Some(next) = self.next_after(at) {
            if next > until {
                break;
            }
            found = Some(next);
            at = next;
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-27 09:00:00 UTC
    const T: i64 = 1_793_091_600;

    #[test]
    fn every_five_minutes_fires_on_the_five() {
        let s = parse("*/5 * * * *").unwrap();
        assert_eq!(s.next_after(T), Some(T + 300));
        assert_eq!(s.next_after(T + 1), Some(T + 300));
        assert_eq!(s.next_after(T + 299), Some(T + 300));
        assert_eq!(s.next_after(T + 300), Some(T + 600));
    }

    #[test]
    fn midnight_utc_is_the_next_day() {
        let s = parse("0 0 * * *").unwrap();
        assert_eq!(s.next_after(T), Some(T + 15 * 3600));
        assert_eq!(parse("@daily"), Ok(s));
    }

    #[test]
    fn lists_ranges_names_and_weekdays() {
        let s = parse("30 8-10 * * mon-fri").unwrap();
        // 2026-10-27 is a Tuesday: 08:30 has passed at 09:00, so 09:30 is next.
        assert_eq!(s.next_after(T), Some(T + 1800));
        let sunday = parse("0 12 * * 0").unwrap();
        assert_eq!(sunday, parse("0 12 * * 7").unwrap());
        // The next Sunday after Tue 27 Oct is 1 Nov.
        assert_eq!(sunday.next_after(T), Some(T + 4 * 86_400 + 27 * 3600));
        assert!(parse("0 0 1 jan,jul *").is_ok());
    }

    #[test]
    fn day_of_month_or_weekday_when_both_are_given() {
        let s = parse("0 0 1 * 1").unwrap();
        // Wed 28 Oct is neither; the first of November comes before the Monday after it.
        assert_eq!(s.next_after(T + 86_400), Some(T + 4 * 86_400 + 15 * 3600));
        assert_eq!(
            s.next_after(T + 4 * 86_400 + 15 * 3600),
            Some(T + 5 * 86_400 + 15 * 3600)
        );
    }

    #[test]
    fn last_between_picks_the_most_recent_miss() {
        let s = parse("*/5 * * * *").unwrap();
        assert_eq!(s.last_between(T, T + 1000), Some(T + 900));
        assert_eq!(s.last_between(T, T + 299), None);
    }

    #[test]
    fn bad_schedules_say_why() {
        assert_eq!(parse("* * *"), Err("expected exactly 5 fields, found 3: * * *".into()));
        assert!(parse("61 * * * *").unwrap_err().contains("outside 0-59"));
        assert!(parse("*/0 * * * *").is_err());
        assert!(parse("a * * * *").is_err());
    }
}

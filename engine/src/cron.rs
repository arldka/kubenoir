//! Cron schedules, as CronJob uses them: upstream parses them with `robfig/cron/v3`'s
//! `ParseStandard` (five fields: minute hour day-of-month month day-of-week, `*`/`?`, lists,
//! ranges, steps, month and weekday names, the `@hourly` family and `@every <duration>`), and
//! this is a port of that parser, error texts included. Times are UTC, in epoch seconds.

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
    /// `@every <duration>`: a fixed delay in seconds instead of the fields.
    every: Option<i64>,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A field's bounds and names (robfig's `bounds`).
struct Bounds {
    min: u32,
    max: u32,
    names: &'static [&'static str],
    first_name: u32,
}

const MINUTE: Bounds = Bounds {
    min: 0,
    max: 59,
    names: &[],
    first_name: 0,
};
const HOUR: Bounds = Bounds {
    min: 0,
    max: 23,
    names: &[],
    first_name: 0,
};
const DOM: Bounds = Bounds {
    min: 1,
    max: 31,
    names: &[],
    first_name: 0,
};
const MONTH: Bounds = Bounds {
    min: 1,
    max: 12,
    names: &MONTHS,
    first_name: 1,
};
const DOW: Bounds = Bounds {
    min: 0,
    max: 6,
    names: &WEEKDAYS,
    first_name: 0,
};

/// Go's `strconv.Atoi` error text.
fn atoi(s: &str) -> Result<i64, String> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(format!("strconv.Atoi: parsing {s:?}: invalid syntax"));
    }
    s.parse::<i64>()
        .map_err(|_| format!("strconv.Atoi: parsing {s:?}: value out of range"))
}

/// `mustParseInt`.
fn must_parse_int(expr: &str) -> Result<u32, String> {
    let n = atoi(expr).map_err(|e| format!("failed to parse int from {expr}: {e}"))?;
    if n < 0 {
        return Err(format!("negative number ({n}) not allowed: {expr}"));
    }
    Ok(n.min(u32::MAX as i64) as u32)
}

/// `parseIntOrName`.
fn int_or_name(expr: &str, b: &Bounds) -> Result<u32, String> {
    if let Some(i) = b.names.iter().position(|n| n.eq_ignore_ascii_case(expr)) {
        return Ok(i as u32 + b.first_name);
    }
    must_parse_int(expr)
}

/// `getRange`: the bits of one range, and whether it was a bare `*`/`?` (robfig's star bit).
fn range(expr: &str, b: &Bounds) -> Result<(u64, bool), String> {
    let range_and_step: Vec<&str> = expr.split('/').collect();
    let low_and_high: Vec<&str> = range_and_step[0].split('-').collect();
    let single = low_and_high.len() == 1;
    let (start, mut end, mut star);
    if low_and_high[0] == "*" || low_and_high[0] == "?" {
        (start, end, star) = (b.min, b.max, true);
    } else {
        start = int_or_name(low_and_high[0], b)?;
        end = match low_and_high.len() {
            1 => start,
            2 => int_or_name(low_and_high[1], b)?,
            _ => return Err(format!("too many hyphens: {expr}")),
        };
        star = false;
    }
    let step = match range_and_step.len() {
        1 => 1,
        2 => {
            let step = must_parse_int(range_and_step[1])?;
            if single {
                end = b.max;
            }
            if step > 1 {
                star = false;
            }
            step
        }
        _ => return Err(format!("too many slashes: {expr}")),
    };
    if start < b.min {
        return Err(format!(
            "beginning of range ({start}) below minimum ({}): {expr}",
            b.min
        ));
    }
    if end > b.max {
        return Err(format!("end of range ({end}) above maximum ({}): {expr}", b.max));
    }
    if start > end {
        return Err(format!(
            "beginning of range ({start}) beyond end of range ({end}): {expr}"
        ));
    }
    if step == 0 {
        return Err(format!("step of range should be a positive number: {expr}"));
    }
    let mut bits = 0u64;
    let mut value = start;
    while value <= end {
        bits |= 1 << value;
        value += step;
    }
    Ok((bits, star))
}

/// `getField`: a comma-separated list of ranges (empty items skipped).
fn field(text: &str, b: &Bounds) -> Result<(u64, bool), String> {
    let mut bits = 0u64;
    let mut star = false;
    for expr in text.split(',').filter(|e| !e.is_empty()) {
        let (more, s) = range(expr, b)?;
        bits |= more;
        star |= s;
    }
    Ok((bits, star))
}

/// Go's `time.ParseDuration`, in whole seconds (fractions truncated), with its error texts.
fn parse_duration(s: &str) -> Result<f64, String> {
    let invalid = || format!("time: invalid duration {s:?}");
    let mut rest = s;
    let negative = rest.starts_with('-');
    rest = rest.strip_prefix(['+', '-']).unwrap_or(rest);
    if rest == "0" {
        return Ok(0.0);
    }
    if rest.is_empty() {
        return Err(invalid());
    }
    let mut total = 0.0;
    while !rest.is_empty() {
        let number_len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let number = &rest[..number_len];
        if number.is_empty() || number == "." {
            return Err(invalid());
        }
        let value: f64 = number.parse().map_err(|_| invalid())?;
        rest = &rest[number_len..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        if unit.is_empty() {
            return Err(format!("time: missing unit in duration {s:?}"));
        }
        let scale = match unit {
            "ns" => 1e-9,
            "us" | "µs" | "μs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return Err(format!("time: unknown unit {unit:?} in duration {s:?}")),
        };
        total += value * scale;
        rest = &rest[unit_len..];
    }
    Ok(if negative { -total } else { total })
}

/// `cron.ParseStandard`.
pub fn parse(text: &str) -> Result<Schedule, String> {
    if text.is_empty() {
        return Err("empty spec string".into());
    }
    let mut spec = text;
    if spec.starts_with("TZ=") || spec.starts_with("CRON_TZ=") {
        let eq = spec.find('=').unwrap_or(0);
        let Some(space) = spec.find(' ') else {
            return Err(format!(
                "provided bad location {}: unknown time zone {}",
                &spec[eq + 1..],
                &spec[eq + 1..]
            ));
        };
        let location = &spec[eq + 1..space];
        if !crate::tznames::is_known(location) {
            return Err(format!(
                "provided bad location {location}: unknown time zone {location}"
            ));
        }
        spec = spec[space..].trim();
    }
    if spec.starts_with('@') {
        let expanded = match spec {
            "@yearly" | "@annually" => "0 0 1 1 *",
            "@monthly" => "0 0 1 * *",
            "@weekly" => "0 0 * * 0",
            "@daily" | "@midnight" => "0 0 * * *",
            "@hourly" => "0 * * * *",
            s if s.starts_with("@every ") => {
                let duration = &s["@every ".len()..];
                let secs = parse_duration(duration).map_err(|e| format!("failed to parse duration {s}: {e}"))?;
                return Ok(Schedule {
                    minutes: 0,
                    hours: 0,
                    days: 0,
                    months: 0,
                    weekdays: 0,
                    any_day: true,
                    any_weekday: true,
                    every: Some((secs as i64).max(1)),
                });
            }
            other => return Err(format!("unrecognized descriptor: {other}")),
        };
        return parse(expanded);
    }
    let fields: Vec<&str> = spec.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "expected exactly 5 fields, found {}: [{}]",
            fields.len(),
            fields.join(" ")
        ));
    }
    let (minutes, _) = field(fields[0], &MINUTE)?;
    let (hours, _) = field(fields[1], &HOUR)?;
    let (days, any_day) = field(fields[2], &DOM)?;
    let (months, _) = field(fields[3], &MONTH)?;
    let (weekdays, any_weekday) = field(fields[4], &DOW)?;
    Ok(Schedule {
        minutes,
        hours: hours as u32,
        days: days as u32,
        months: months as u16,
        weekdays: weekdays as u8,
        any_day,
        any_weekday,
        every: None,
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
        // robfig: when either field is a `*`, both must match; otherwise either one is enough.
        if self.any_day || self.any_weekday {
            by_day && by_weekday
        } else {
            by_day || by_weekday
        }
    }

    /// The first scheduled minute strictly after `after`.
    pub fn next_after(&self, after: i64) -> Option<i64> {
        if let Some(delay) = self.every {
            return Some(after + delay);
        }
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
        // robfig's day-of-week runs 0-6: 7 is not Sunday.
        assert_eq!(parse("0 12 * * 7"), Err("end of range (7) above maximum (6): 7".into()));
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
        // Texts from robfig/cron v3's ParseStandard.
        assert_eq!(
            parse("* * *"),
            Err("expected exactly 5 fields, found 3: [* * *]".into())
        );
        assert_eq!(
            parse("61 * * * *"),
            Err("end of range (61) above maximum (59): 61".into())
        );
        assert_eq!(
            parse("*/0 * * * *"),
            Err("step of range should be a positive number: */0".into())
        );
        assert_eq!(
            parse("a * * * *"),
            Err("failed to parse int from a: strconv.Atoi: parsing \"a\": invalid syntax".into())
        );
        assert_eq!(parse("@every 90s").unwrap().next_after(T), Some(T + 90));
        assert_eq!(parse("@sometimes"), Err("unrecognized descriptor: @sometimes".into()));
    }
}

//! Times as kubectl prints them: the age of an object and RFC 3339 timestamps.

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// An RFC 3339 time (`2024-05-01T10:00:00Z`, optional fraction and offset) as seconds since
/// 1970.
pub fn parse(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse().ok() };
    if bytes.len() < 20 {
        return None;
    }
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let mut at = 19;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
    }
    let offset = match bytes.get(at)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let minutes = number(at + 1, at + 3)? * 60 + number(at + 4, at + 6)?;
            if *sign == b'+' { minutes } else { -minutes }
        }
        _ => return None,
    };
    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - offset * 60,
    )
}

/// `2024-05-01T10:00:00Z` of milliseconds since 1970.
pub fn format(millis: f64) -> String {
    let seconds = (millis / 1000.0).floor() as i64;
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// An age in seconds the way kubectl prints it: `45s`, `3m20s`, `5h`, `12d`.
pub fn human(seconds: i64) -> String {
    if seconds < 0 {
        return "<invalid>".into();
    }
    if seconds < 120 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 10 {
        return match seconds % 60 {
            0 => format!("{minutes}m"),
            rest => format!("{minutes}m{rest}s"),
        };
    }
    if minutes < 180 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 8 {
        return match minutes % 60 {
            0 => format!("{hours}h"),
            rest => format!("{hours}h{rest}m"),
        };
    }
    if hours < 48 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if hours < 24 * 8 {
        return match hours % 24 {
            0 => format!("{days}d"),
            rest => format!("{days}d{rest}h"),
        };
    }
    if hours < 24 * 365 * 2 {
        return format!("{days}d");
    }
    let years = days / 365;
    if hours < 24 * 365 * 8 {
        return match days % 365 {
            0 => format!("{years}y"),
            rest => format!("{years}y{rest}d"),
        };
    }
    format!("{years}y")
}

/// The age of the RFC 3339 `time` at `now` (milliseconds since 1970); `<unknown>` when it does
/// not parse.
pub fn age(now: f64, time: &str) -> String {
    parse(time).map_or_else(
        || "<unknown>".into(),
        |then| human((now / 1000.0).floor() as i64 - then),
    )
}

//! Dates and times as browsers write them for `en-US`, from milliseconds since 1970 and
//! the minutes the local time is ahead of UTC (`Init` settings `timezoneOffset`).

/// `(year, month, day, hour, minute, second)` of `ms` since 1970, `offset` minutes ahead of
/// UTC.
pub fn civil(ms: f64, offset: f64) -> (i64, u32, u32, u32, u32, u32) {
    let seconds = ((ms / 1000.0) + offset * 60.0).floor() as i64;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rest = rest as u32;
    (year, month, day, rest / 3600, rest / 60 % 60, rest % 60)
}

/// The local time of `ms` as browsers write it for `en-US`, like `3:04:05 PM` (with a
/// narrow no-break space).
pub fn time(ms: f64, offset: f64) -> String {
    let (_, _, _, hour, minute, second) = civil(ms, offset);
    let half = if hour < 12 { "AM" } else { "PM" };
    let hour = match hour % 12 {
        0 => 12,
        hour => hour,
    };
    format!("{hour}:{minute:02}:{second:02}\u{202f}{half}")
}

/// The local date and time of `ms`, like `11/5/2025, 3:04:05 PM`.
pub fn date(ms: f64, offset: f64) -> String {
    let (year, month, day, ..) = civil(ms, offset);
    format!("{month}/{day}/{year}, {}", time(ms, offset))
}

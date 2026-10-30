use chrono::{
    DateTime, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, Offset, SecondsFormat,
    TimeZone, Utc,
};
use serde_json::{Value, json};

pub(super) fn text(raw: &str, date_only: bool) -> Value {
    text_in_zone(raw, date_only, &Local)
}

pub(super) fn binary(parts: (u16, u8, u8, u8, u8, u8, u32)) -> Value {
    binary_in_zone(parts, &Local)
}

fn text_in_zone<T: TimeZone>(raw: &str, date_only: bool, zone: &T) -> Value {
    let parsed = (|| {
        let date = raw.get(..10)?;
        let mut parts = date.split('-');
        let year = parts.next()?.parse::<i32>().ok()?;
        let month = parts.next()?.parse::<u32>().ok()?;
        let day = parts.next()?.parse::<u32>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        let local = if date_only {
            if raw.len() != 10 {
                return None;
            }
            // mysql2's DATE parser uses the numeric Date constructor, including
            // its year 0..99 remapping and month/day overflow normalization.
            normalized(year, month, day, 0, 0, 0, 0, true)?
        } else {
            if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
                return None;
            }
            let time = raw.get(11..19)?;
            let mut parts = time.split(':');
            let hour = parts.next()?.parse().ok()?;
            let minute = parts.next()?.parse().ok()?;
            let second = parts.next()?.parse().ok()?;
            if parts.next().is_some() {
                return None;
            }
            let fraction = if raw.len() > 19 {
                if raw.as_bytes().get(19) != Some(&b'.') {
                    return None;
                }
                raw.get(20..)?
            } else {
                ""
            };
            if !fraction.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let millis = format!("{fraction:0<3}").get(..3)?.parse().ok()?;
            normalized(year, month, day, hour, minute, second, millis, false)?
        };
        to_utc(local, zone)
    })();
    json_date(parsed)
}

fn binary_in_zone<T: TimeZone>(parts: (u16, u8, u8, u8, u8, u8, u32), zone: &T) -> Value {
    let (year, month, day, hour, minute, second, micros) = parts;
    if parts == (0, 0, 0, 0, 0, 0, 0) {
        return Value::Null;
    }
    let local = normalized(
        year.into(),
        month.into(),
        day.into(),
        hour.into(),
        minute.into(),
        second.into(),
        micros / 1000,
        true,
    );
    json_date(local.and_then(|date| to_utc(date, zone)))
}

#[allow(clippy::too_many_arguments)]
fn normalized(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
    legacy_year: bool,
) -> Option<NaiveDateTime> {
    let year = if legacy_year && (0..100).contains(&year) {
        year + 1900
    } else {
        year
    };
    let month = i64::from(month) - 1;
    let year = i64::from(year) + month.div_euclid(12);
    let date =
        NaiveDate::from_ymd_opt(year.try_into().ok()?, (month.rem_euclid(12) + 1) as u32, 1)?;
    date.and_hms_opt(0, 0, 0)?
        .checked_add_signed(Duration::days(i64::from(day) - 1))?
        .checked_add_signed(Duration::hours(hour.into()))?
        .checked_add_signed(Duration::minutes(minute.into()))?
        .checked_add_signed(Duration::seconds(second.into()))?
        .checked_add_signed(Duration::milliseconds(millis.into()))
}

fn to_utc<T: TimeZone>(local: NaiveDateTime, zone: &T) -> Option<DateTime<Utc>> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(date) => Some(date.with_timezone(&Utc)),
        // ECMAScript chooses the earlier instant when clocks fall back.
        LocalResult::Ambiguous(first, second) => {
            Some(first.with_timezone(&Utc).min(second.with_timezone(&Utc)))
        }
        LocalResult::None => {
            // A nonexistent wall time advances by the transition's gap.
            // Applying the last pre-transition offset has precisely that effect,
            // including half-hour DST and whole-day dateline transitions.
            for minutes in 1..=48 * 60 {
                let before = local.checked_sub_signed(Duration::minutes(minutes))?;
                let offset = match zone.from_local_datetime(&before) {
                    LocalResult::Single(date) => date.offset().fix(),
                    LocalResult::Ambiguous(first, second) => first.min(second).offset().fix(),
                    LocalResult::None => continue,
                };
                return local
                    .checked_sub_signed(Duration::seconds(offset.local_minus_utc().into()))
                    .map(|date| date.and_utc());
            }
            None
        }
    }
}

fn json_date(date: Option<DateTime<Utc>>) -> Value {
    date.map(|date| json!(date.to_rfc3339_opts(SecondsFormat::Millis, true)))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::{America::New_York, Asia::Kathmandu, Australia::Lord_Howe, Pacific::Apia};

    #[test]
    fn wall_clock_dates_use_historical_timezone_offsets_and_truncate_microseconds() {
        assert_eq!(
            text_in_zone("2026-01-15 12:34:56.123456", false, &New_York),
            "2026-01-15T17:34:56.123Z"
        );
        assert_eq!(
            text_in_zone("2026-07-15 12:34:56.9", false, &New_York),
            "2026-07-15T16:34:56.900Z"
        );
        assert_eq!(
            text_in_zone("2026-01-01", true, &Kathmandu),
            "2025-12-31T18:15:00.000Z"
        );
        assert_eq!(
            binary_in_zone((2026, 7, 15, 12, 34, 56, 123456), &New_York),
            "2026-07-15T16:34:56.123Z"
        );
    }

    #[test]
    fn dst_overlap_uses_earlier_instant_and_gap_advances_like_javascript_date() {
        assert_eq!(
            text_in_zone("2026-11-01 01:30:00", false, &New_York),
            "2026-11-01T05:30:00.000Z"
        );
        assert_eq!(
            text_in_zone("2026-03-08 02:30:00", false, &New_York),
            "2026-03-08T07:30:00.000Z"
        );
        assert_eq!(
            text_in_zone("2026-10-04 02:15:00", false, &Lord_Howe),
            "2026-10-03T15:45:00.000Z"
        );
        assert_eq!(
            text_in_zone("2011-12-30 12:00:00", false, &Apia),
            "2011-12-30T22:00:00.000Z"
        );
    }

    #[test]
    fn invalid_dates_and_numeric_constructor_normalization_match_mysql2() {
        assert_eq!(
            text_in_zone("0000-00-00 00:00:00", false, &Utc),
            Value::Null
        );
        assert_eq!(binary_in_zone((0, 0, 0, 0, 0, 0, 0), &Utc), Value::Null);
        assert_eq!(
            text_in_zone("2026-00-15 00:00:00", false, &Utc),
            Value::Null
        );
        assert_eq!(
            text_in_zone("2026-02-31 00:00:00", false, &Utc),
            "2026-03-03T00:00:00.000Z"
        );
        assert_eq!(
            text_in_zone("0000-00-00", true, &Utc),
            "1899-11-30T00:00:00.000Z"
        );
        assert_eq!(
            text_in_zone("0099-01-02", true, &Utc),
            "1999-01-02T00:00:00.000Z"
        );
        assert_eq!(
            text_in_zone("0099-01-02 12:00:00", false, &Utc),
            "0099-01-02T12:00:00.000Z"
        );
    }
}

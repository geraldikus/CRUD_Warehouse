use chrono::{DateTime, Datelike, TimeZone, Utc};

/// How close an expiry is, from the client's clock.
///
/// The server publishes `expiring` / `expired` once per step. This color is
/// local and updates every frame, so a row can turn red before the next
/// MQTT message arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpiryStatus {
    None,
    Ok,
    /// Inside the next 24 hours, same horizon as the server's "day" notice.
    Soon,
    Expired,
}

pub(crate) fn expiry_status(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> ExpiryStatus {
    match expires_at {
        None => ExpiryStatus::None,
        Some(at) if at <= now => ExpiryStatus::Expired,
        Some(at) if at <= now + chrono::Duration::hours(24) => ExpiryStatus::Soon,
        Some(_) => ExpiryStatus::Ok,
    }
}

/// The date picker stores a calendar day (`jiff`). The API stores a UTC
/// timestamp (`chrono`). Saving a day means the end of that day in UTC,
/// so the item is not already expired at 00:00.
pub(crate) fn utc_end_of_civil_day(date: jiff::civil::Date) -> DateTime<Utc> {
    let naive = chrono::NaiveDate::from_ymd_opt(
        i32::from(date.year()),
        date.month() as u32,
        date.day() as u32,
    )
    .expect("civil date is in chrono's range")
    .and_hms_opt(23, 59, 59)
    .expect("23:59:59 is a valid time");
    Utc.from_utc_datetime(&naive)
}

pub(crate) fn civil_date_of(expires_at: DateTime<Utc>) -> jiff::civil::Date {
    let date = expires_at.date_naive();
    jiff::civil::Date::new(date.year() as i16, date.month() as i8, date.day() as i8)
        .expect("chrono date is a valid civil date")
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Timelike};

    use super::*;

    fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
    }

    #[test]
    fn missing_date_has_no_status() {
        assert_eq!(expiry_status(None, at(2026, 11, 6, 12)), ExpiryStatus::None);
    }

    #[test]
    fn past_date_is_expired() {
        let now = at(2026, 11, 6, 12);
        assert_eq!(
            expiry_status(Some(at(2026, 11, 5, 12)), now),
            ExpiryStatus::Expired
        );
    }

    #[test]
    fn inside_a_day_is_soon() {
        let now = at(2026, 11, 6, 12);
        assert_eq!(
            expiry_status(Some(at(2026, 11, 7, 10)), now),
            ExpiryStatus::Soon
        );
    }

    #[test]
    fn later_than_a_day_is_ok() {
        let now = at(2026, 11, 6, 12);
        assert_eq!(
            expiry_status(Some(at(2026, 11, 9, 12)), now),
            ExpiryStatus::Ok
        );
    }

    #[test]
    fn civil_day_round_trips_through_utc_end() {
        let date = jiff::civil::Date::new(2026, 11, 6).unwrap();
        let stamp = utc_end_of_civil_day(date);
        assert_eq!(
            stamp,
            at(2026, 11, 6, 23)
                .with_minute(59)
                .unwrap()
                .with_second(59)
                .unwrap()
        );
        assert_eq!(civil_date_of(stamp), date);
    }
}

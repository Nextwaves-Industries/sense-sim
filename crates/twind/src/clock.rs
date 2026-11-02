//! Wall-clock time as the store and the log write it. Always UTC, so journal file names
//! don't depend on the host's time zone or daylight saving.

use std::time::{SystemTime, UNIX_EPOCH};

const MS_PER_DAY: u64 = 86_400_000;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// `YYYYMM`, which names an event journal file.
pub fn month_key(ts_ms: u64) -> String {
    let (year, month, _) = civil_date(ts_ms / MS_PER_DAY);
    format!("{year:04}{month:02}")
}

pub fn timestamp(ts_ms: u64) -> String {
    let (year, month, day) = civil_date(ts_ms / MS_PER_DAY);
    let seconds = ts_ms % MS_PER_DAY / 1000;
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

/// Days since 1970-01-01 to a Gregorian date, counted in 400-year eras from March 1, so the
/// leap day falls at the end of a year.
fn civil_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_boundaries_land_in_the_right_file() {
        assert_eq!(month_key(0), "197001");
        // 2024-02-29 12:00 UTC, a leap day.
        assert_eq!(month_key(1_709_208_000_000), "202402");
        // The last and first milliseconds around a new year.
        assert_eq!(month_key(1_735_689_599_999), "202412");
        assert_eq!(month_key(1_735_689_600_000), "202501");
    }

    #[test]
    fn timestamps_read_as_utc() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00Z");
        assert_eq!(timestamp(1_735_689_599_999), "2024-12-31 23:59:59Z");
    }
}

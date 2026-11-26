//! Korean dates and times for the made-up world. Nexon states Korean time everywhere, and
//! `mfp-kms` counts it in minutes since 1970 ([`Minutes`]).

use std::time::{SystemTime, UNIX_EPOCH};

use mfp_kms::notice::Minutes;

/// The weekdays as Nexon abbreviates them in a notice, beginning with the weekday of the
/// first day of 1970.
const WEEKDAYS: [&str; 7] = ["목", "금", "토", "일", "월", "화", "수"];

/// A day in Korea.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub weekday: &'static str,
}

/// The day a moment lies in. (Howard Hinnant's `civil_from_days`.)
pub fn day_of(at: Minutes) -> Day {
    let since_1970 = at.div_euclid(1440);
    let days = since_1970 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Day { year, month, day, weekday: WEEKDAYS[since_1970.rem_euclid(7) as usize] }
}

/// The day of a moment as a notice writes it: `11/26(목)`.
pub fn notice_day(at: Minutes) -> String {
    let Day { month, day, weekday, .. } = day_of(at);
    format!("{month}/{day}({weekday})")
}

/// A moment as the inspection service writes it: `2026-11-26T04:00:00+09:00`.
pub fn service_stamp(at: Minutes) -> String {
    let Day { year, month, day, .. } = day_of(at);
    let of_day = at.rem_euclid(1440);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:00+09:00", of_day / 60, of_day % 60)
}

/// Now in Korea, to the second, for a line in a log: `2026-11-26 13:05:09`.
pub fn log_stamp() -> String {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64) + 9 * 3600;
    let Day { year, month, day, .. } = day_of(seconds.div_euclid(60));
    let of_day = seconds.rem_euclid(86_400);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", of_day / 3600, of_day / 60 % 60, of_day % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_told_as_the_notices_write_them() {
        // 2026-11-26 04:00 in Korea, a Thursday: 20,783 days after the first day of 1970.
        let at = 20_783 * 1440 + 4 * 60;
        assert_eq!(day_of(at), Day { year: 2026, month: 11, day: 26, weekday: "목" });
        assert_eq!(notice_day(at), "11/26(목)");
        assert_eq!(service_stamp(at), "2026-11-26T04:00:00+09:00");
        // The last minute of a leap day, and the first of the day after.
        let leap = 19_782 * 1440 + 1439;
        assert_eq!(notice_day(leap), "2/29(목)");
        assert_eq!(notice_day(leap + 1), "3/1(금)");
        assert_eq!(day_of(0), Day { year: 1970, month: 1, day: 1, weekday: "목" });
    }
}

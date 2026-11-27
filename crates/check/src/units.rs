//! Figures as the report writes them. Sizes go by 1024, as Windows shows them.

const MB: f64 = (1u64 << 20) as f64;
const GB: f64 = (1u64 << 30) as f64;

/// A whole number with a comma after every three digits.
pub fn count(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The figure of a size in gigabytes, with one place after the point and without the unit:
/// `211.3`, `3,722.7`.
pub fn gigabytes(bytes: u64) -> String {
    let tenths = (bytes as f64 / GB * 10.0).round() as u64;
    format!("{}.{}", count(tenths / 10), tenths % 10)
}

/// A size in gigabytes with one place after the point: `211.3 GB`.
pub fn gb(bytes: u64) -> String {
    format!("{} GB", gigabytes(bytes))
}

/// A size in megabytes below a gigabyte and in gigabytes from there: `512 MB`, `1.1 GB`.
pub fn size(bytes: u64) -> String {
    if bytes < 1 << 30 { format!("{:.0} MB", bytes as f64 / MB) } else { gb(bytes) }
}

/// The figure of a speed in megabytes a second, without the unit: `82`, `1,900`, and one
/// place after the point below ten.
pub fn megabytes_a_second(bytes_a_second: f64) -> String {
    let megabytes = bytes_a_second / MB;
    if megabytes < 10.0 { format!("{megabytes:.1}") } else { count(megabytes.round() as u64) }
}

/// A speed from bytes a second: `82 MB/s`, `1,900 MB/s`, and one place after the point
/// below ten.
pub fn speed(bytes_a_second: f64) -> String {
    format!("{} MB/s", megabytes_a_second(bytes_a_second))
}

/// A time in milliseconds: `12 ms`, and one place after the point below ten.
pub fn milliseconds(milliseconds: f64) -> String {
    if milliseconds < 10.0 {
        format!("{milliseconds:.1} ms")
    } else {
        format!("{} ms", count(milliseconds.round() as u64))
    }
}

/// A time as minutes and seconds: `7분 12초`, `42초`, and tenths below ten seconds.
pub fn took(seconds: f64) -> String {
    let whole = seconds.round() as u64;
    match whole {
        0..10 => format!("{seconds:.1}초"),
        10..60 => format!("{whole}초"),
        _ => format!("{}분 {}초", whole / 60, whole % 60),
    }
}

/// The speed of a line from bits a second: `1 Gbps`, `2.5 Gbps`, `100 Mbps`.
pub fn line_speed(bits_a_second: u64) -> String {
    let megabits = bits_a_second as f64 / 1e6;
    if megabits >= 1000.0 {
        let gigabits = megabits / 1000.0;
        if (gigabits - gigabits.round()).abs() < 0.05 {
            format!("{gigabits:.0} Gbps")
        } else {
            format!("{gigabits:.1} Gbps")
        }
    } else {
        format!("{megabits:.0} Mbps")
    }
}

/// A moment as Korea writes it, from seconds since 1970: `2026.11.26 15:41`. Korea keeps
/// one time all year, nine hours ahead of the world's.
pub fn korean_time(unix_seconds: u64) -> String {
    let (date, minutes) = korean_parts(unix_seconds);
    format!("{:04}.{:02}.{:02} {:02}:{:02}", date.0, date.1, date.2, minutes / 60, minutes % 60)
}

/// The same moment for the name of a file: `20261126-1541`.
pub fn korean_time_for_a_name(unix_seconds: u64) -> String {
    let (date, minutes) = korean_parts(unix_seconds);
    format!("{:04}{:02}{:02}-{:02}{:02}", date.0, date.1, date.2, minutes / 60, minutes % 60)
}

/// The date in Korea as year, month and day, and the minutes since midnight there.
fn korean_parts(unix_seconds: u64) -> ((i64, i64, i64), u64) {
    let local = unix_seconds + 9 * 3600;
    let (days, rest) = ((local / 86_400) as i64, local % 86_400);
    // Days since 1970 to a date, after Howard Hinnant's `civil_from_days`.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    ((year, month, day), rest / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_numbers_get_a_comma_every_three_digits() {
        assert_eq!(
            [0, 7, 999, 1000, 1239, 68_100, 1_234_567].map(count),
            ["0", "7", "999", "1,000", "1,239", "68,100", "1,234,567"]
        );
    }

    #[test]
    fn sizes_and_speeds_go_by_1024() {
        assert_eq!(gb(211 << 30), "211.0 GB");
        assert_eq!(gb((49 << 30) + (922 << 20)), "49.9 GB");
        // A thousand and more get their comma, and the figure can stand without its unit.
        assert_eq!(gb((3722 << 30) + (717 << 20)), "3,722.7 GB");
        assert_eq!((gigabytes(80 << 30), gigabytes((7451 << 30) + (820 << 20))), ("80.0".into(), "7,451.8".into()));
        assert_eq!(gigabytes((9 << 30) + (1023 << 20)), "10.0");
        assert_eq!(size(512 << 20), "512 MB");
        assert_eq!(size(1 << 30), "1.0 GB");
        assert_eq!(speed(82.0 * MB), "82 MB/s");
        assert_eq!(speed(1900.4 * MB), "1,900 MB/s");
        assert_eq!(speed(9.84 * MB), "9.8 MB/s");
        assert_eq!((megabytes_a_second(1799.2 * MB), megabytes_a_second(4.26 * MB)), ("1,799".into(), "4.3".into()));
    }

    #[test]
    fn short_times_are_milliseconds() {
        assert_eq!([0.34, 4.66, 12.4, 1234.0].map(milliseconds), ["0.3 ms", "4.7 ms", "12 ms", "1,234 ms"]);
    }

    #[test]
    fn times_are_minutes_and_seconds() {
        assert_eq!(took(0.94), "0.9초");
        assert_eq!(took(42.2), "42초");
        assert_eq!(took(432.0), "7분 12초");
        assert_eq!(took(59.6), "1분 0초");
    }

    #[test]
    fn line_speeds_are_named_as_cards_name_them() {
        assert_eq!(line_speed(1_000_000_000), "1 Gbps");
        assert_eq!(line_speed(2_500_000_000), "2.5 Gbps");
        assert_eq!(line_speed(100_000_000), "100 Mbps");
        assert_eq!(line_speed(866_700_000), "867 Mbps");
    }

    #[test]
    fn moments_are_written_in_korean_time() {
        // 2026-11-26 06:41:00 in the world's time is 15:41 in Korea.
        assert_eq!(korean_time(1_795_675_260), "2026.11.26 15:41");
        assert_eq!(korean_time_for_a_name(1_795_675_260), "20261126-1541");
        // The day turns over nine hours early, also at the end of a year and of February.
        assert_eq!(korean_time(1_798_729_200), "2027.01.01 00:00");
        assert_eq!(korean_time(1_709_218_800 - 9 * 3600), "2024.02.29 15:00");
        assert_eq!(korean_time(0), "1970.01.01 09:00");
    }
}

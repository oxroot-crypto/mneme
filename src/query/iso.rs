//! ISO 8601 与 Unix 毫秒互转(零依赖;设计 06 §1 的 `ts"..."` 字面量)。
//!
//! 支持 `YYYY-MM-DD[Thh:mm[:ss[.fff]]][Z|±hh:mm|±hhmm]`;缺省时区按 UTC。
//! 秒域 0–59(不支持闰秒 60,直接拒绝);小数秒最多读 9 位、截断到毫秒。
//! 仅用于过滤 DSL 的时间戳字面量,不追求 ISO 8601 全部边缘语法。

/// 一天的毫秒数。
const DAY_MS: i64 = 86_400_000;
/// 一天的秒数。
const DAY_SECS: i64 = 86_400;
/// 一小时的毫秒数。
const HOUR_MS: i64 = 3_600_000;
/// 一小时的秒数。
const HOUR_SECS: i64 = 3_600;
/// 一分钟的秒数。
const MINUTE_SECS: i64 = 60;
/// 一分钟的毫秒数。
const MINUTE_MS: i64 = 60_000;
/// 一秒的毫秒数。
const SECOND_MS: i64 = 1_000;

/// [`format_iso8601_ms`] 可被 [`parse_iso8601_ms`] 原样读回的最小 Unix 毫秒
/// (`0000-01-01T00:00:00.000Z`);解析器只接受 4 位年份,超出即无法往返。
pub(crate) const MIN_ROUNDTRIP_MS: i64 = -62_167_219_200_000;

/// [`format_iso8601_ms`] 可被 [`parse_iso8601_ms`] 原样读回的最大 Unix 毫秒
/// (`9999-12-31T23:59:59.999Z`)。
pub(crate) const MAX_ROUNDTRIP_MS: i64 = 253_402_300_799_999;

/// 吃掉恰好 `count` 位数字并返回其值。
fn take_fixed_digits(bytes: &[u8], count: usize) -> Option<(i64, &[u8])> {
    if bytes.len() < count || !bytes[..count].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0_i64;
    for &byte in &bytes[..count] {
        value = value * 10 + i64::from(byte - b'0');
    }
    Some((value, &bytes[count..]))
}

/// 吃掉 1..=`max` 位数字并返回其值(遇到非数字即停)。
fn take_digits(bytes: &[u8], max: usize) -> Option<(i64, &[u8])> {
    let mut cursor = 0;
    while cursor < bytes.len() && cursor < max && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == 0 {
        return None;
    }
    let mut value = 0_i64;
    for &byte in &bytes[..cursor] {
        value = value * 10 + i64::from(byte - b'0');
    }
    Some((value, &bytes[cursor..]))
}

/// 若首字节等于 `expected` 则吃掉并返回剩余。
fn strip(bytes: &[u8], expected: u8) -> Option<&[u8]> {
    bytes
        .strip_prefix(&[expected])
        .or_else(|| bytes.strip_prefix(&[expected.to_ascii_lowercase()]))
}

/// 解析可选的小数秒,返回毫秒(最多读 9 位;截断到 3 位,不足按十分位/百分位补零)。
///
/// 只读取**已确认是数字**的前 `cursor` 个字节;不足 3 位时绝不能越过 `cursor`
/// 去读时区等后续字符(`.5Z` 必须得 500ms,而不是把 `Z` 当数字)。
fn parse_fraction(bytes: &[u8]) -> Option<(i64, &[u8])> {
    let mut cursor = 0;
    while cursor < bytes.len() && cursor < 9 && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == 0 {
        return None;
    }
    let digits = &bytes[..cursor.min(3)];
    let mut milli = 0_i64;
    for &byte in digits {
        milli = milli * 10 + i64::from(byte - b'0');
    }
    for _ in digits.len()..3 {
        milli *= 10;
    }
    Some((milli, &bytes[cursor..]))
}

/// 解析时区后缀,返回相对 UTC 的偏移毫秒。
fn parse_offset(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return Some(0);
    }
    if let Some(rest) = strip(bytes, b'Z') {
        return rest.is_empty().then_some(0);
    }
    let (sign, rest) = match bytes.first()? {
        b'+' => (1_i64, &bytes[1..]),
        b'-' => (-1_i64, &bytes[1..]),
        _ => return None,
    };
    let (hour, rest) = take_fixed_digits(rest, 2)?;
    let rest = match strip(rest, b':') {
        Some(rest) => rest,
        None => rest,
    };
    let (minute, rest) = take_fixed_digits(rest, 2)?;
    if !rest.is_empty() || hour > 23 || minute > 59 {
        return None;
    }
    Some(sign * (hour * HOUR_MS + minute * MINUTE_MS))
}

/// 解析日期时间部分(年-月-日之后),返回 `(时, 分, 秒, 毫秒, 时区偏移)`。
fn parse_time_of_day(bytes: &[u8]) -> Option<(i64, i64, i64, i64, i64)> {
    let (mut hour, mut minute, mut second, mut milli) = (0, 0, 0, 0);
    let rest = match strip(bytes, b'T') {
        Some(rest) => {
            let (h, rest) = take_fixed_digits(rest, 2)?;
            let rest = strip(rest, b':')?;
            let (m, rest) = take_fixed_digits(rest, 2)?;
            let (s, rest) = match strip(rest, b':') {
                Some(rest) => take_fixed_digits(rest, 2)?,
                None => (0, rest),
            };
            let rest = match strip(rest, b'.') {
                Some(rest) => {
                    let (ms, rest) = parse_fraction(rest)?;
                    milli = ms;
                    rest
                }
                None => rest,
            };
            hour = h;
            minute = m;
            second = s;
            rest
        }
        None => bytes,
    };
    if hour > 23 || minute > 59 || second > 59 {
        // 闰秒(60)不支持:线性秒数会把它静默平移,不如直接拒绝。
        return None;
    }
    let offset = parse_offset(rest)?;
    Some((hour, minute, second, milli, offset))
}

/// 把 ISO 8601 字符串解析为 Unix 毫秒;格式非法返回 `None`。
///
/// # Arguments
/// * `input` - ISO 8601 字符串,如 `2024-06-01T00:00:00Z`。
///
/// # Returns
/// 合法时返回 Unix 毫秒(1970 前为负);非法返回 `None`。
pub(crate) fn parse_iso8601_ms(input: &str) -> Option<i64> {
    let bytes = input.as_bytes();
    let (year, rest) = take_fixed_digits(bytes, 4)?;
    let rest = strip(rest, b'-')?;
    let (month, rest) = take_digits(rest, 2)?;
    let rest = strip(rest, b'-')?;
    let (day, rest) = take_digits(rest, 2)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (hour, minute, second, milli, offset) = parse_time_of_day(rest)?;
    let days = days_from_civil(year, month as u32, day as u32);
    // 往返校验拒绝 2 月 30 日等越界日期(避免静默归一化)。
    if civil_from_days(days) != (year, month as u32, day as u32) {
        return None;
    }
    let seconds = days * DAY_SECS + hour * HOUR_SECS + minute * MINUTE_SECS + second;
    Some(seconds * SECOND_MS + milli - offset)
}

/// 把 Unix 毫秒格式化为 UTC 的 `YYYY-MM-DDTHH:MM:SS.mmmZ`。
///
/// # Arguments
/// * `ms` - Unix 毫秒(1970 前为负,按 Euclidean 取整)。
///
/// # Returns
/// 固定宽度、可被 [`parse_iso8601_ms`] 原样读回的字符串。
pub(crate) fn format_iso8601_ms(ms: i64) -> String {
    let days = ms.div_euclid(DAY_MS);
    let rem = ms.rem_euclid(DAY_MS);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second, milli) = (
        rem / HOUR_MS,
        rem / MINUTE_MS % MINUTE_SECS,
        rem / SECOND_MS % MINUTE_SECS,
        rem % SECOND_MS,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// 从自 1970-01-01 起的天数换算为公历年月日(Howard Hinnant 算法)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 从公历年月日换算为自 1970-01-01 起的天数(Howard Hinnant 算法)。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_design_example() {
        assert_eq!(
            parse_iso8601_ms("2024-06-01T00:00:00Z"),
            Some(1_717_200_000_000)
        );
    }

    #[test]
    fn parses_with_millis_and_offset() {
        let utc = parse_iso8601_ms("2024-06-01T12:30:45.123Z").expect("utc");
        let offset = parse_iso8601_ms("2024-06-01T20:30:45.123+08:00").expect("offset");
        assert_eq!(utc, offset);
        assert_eq!(utc % 1_000, 123);
    }

    #[test]
    fn date_only_defaults_to_midnight_utc() {
        assert_eq!(
            parse_iso8601_ms("2024-06-01"),
            parse_iso8601_ms("2024-06-01T00:00:00Z")
        );
    }

    #[test]
    fn rejects_calendar_and_format_violations() {
        assert_eq!(parse_iso8601_ms("2023-02-29T00:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("2024-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("2024-06-01T24:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("2024-06-01T23:59:60Z"), None);
        assert_eq!(parse_iso8601_ms("not-a-date"), None);
        assert_eq!(parse_iso8601_ms("2024-06-01T00:00:00+25:00"), None);
        // 时分秒各须恰好 2 位。
        assert_eq!(parse_iso8601_ms("2024-06-01T1:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("2024-06-01T00:0:00Z"), None);
    }

    /// FC-QUERY-POST-007(小数秒不足 3 位按位补零;绝不读入后续时区字符)
    #[test]
    fn fractional_seconds_pad_to_millis() {
        assert_eq!(
            parse_iso8601_ms("2024-06-01T00:00:00.5Z"),
            Some(1_717_200_000_500)
        );
        assert_eq!(
            parse_iso8601_ms("2024-06-01T00:00:00.12Z"),
            Some(1_717_200_000_120)
        );
        // 带负时区偏移的 1 位小数秒:修复前 debug 构建在此触发减法溢出 panic。
        assert_eq!(
            parse_iso8601_ms("2024-06-01T00:00:00.1-05:00"),
            parse_iso8601_ms("2024-06-01T05:00:00.100Z")
        );
        // 超过 3 位截断;无时区后缀(EOF)同样补零。
        assert_eq!(
            parse_iso8601_ms("2024-06-01T00:00:00.123456Z"),
            Some(1_717_200_000_123)
        );
        assert_eq!(
            parse_iso8601_ms("2024-06-01T00:00:00.5"),
            Some(1_717_200_000_500)
        );
    }

    /// FC-QUERY-POST-007(小写 `t`/`z`、紧凑 `±hhmm`;时区时分恰好 2 位)
    #[test]
    fn accepts_lowercase_and_compact_offset() {
        assert_eq!(
            parse_iso8601_ms("2024-06-01t12:00:00z"),
            parse_iso8601_ms("2024-06-01T12:00:00Z")
        );
        assert_eq!(
            parse_iso8601_ms("2024-06-01T20:00:00+0800"),
            parse_iso8601_ms("2024-06-01T12:00:00Z")
        );
        assert_eq!(parse_iso8601_ms("2024-06-01T00:00:00+5:00"), None);
        assert_eq!(parse_iso8601_ms("2024-06-01T00:00:00+05:0"), None);
    }

    /// FC-QUERY-POST-007(全范围采样往返,不只端点)
    #[test]
    fn roundtrip_covers_range_samples() {
        let span = MAX_ROUNDTRIP_MS - MIN_ROUNDTRIP_MS;
        for index in 0..=100 {
            let ms = MIN_ROUNDTRIP_MS + span * index / 100;
            let text = format_iso8601_ms(ms);
            assert_eq!(parse_iso8601_ms(&text), Some(ms), "{text}");
        }
    }

    #[test]
    fn pre_epoch_and_roundtrip() {
        let ms = parse_iso8601_ms("1969-12-31T23:59:59.999Z").expect("pre epoch");
        assert_eq!(ms, -1);
        assert_eq!(format_iso8601_ms(ms), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn format_parses_back_for_sample_range() {
        for ms in [
            -2_208_988_800_000,
            0,
            1_717_200_000_000,
            1_717_200_000_123,
            4_102_444_800_000,
        ] {
            let text = format_iso8601_ms(ms);
            assert_eq!(parse_iso8601_ms(&text), Some(ms), "{text}");
        }
    }

    #[test]
    fn leap_day_is_accepted() {
        assert!(parse_iso8601_ms("2024-02-29T00:00:00Z").is_some());
    }

    #[test]
    fn roundtrip_bounds_cover_exact_range() {
        for (ms, text) in [
            (MIN_ROUNDTRIP_MS, "0000-01-01T00:00:00.000Z".to_string()),
            (MAX_ROUNDTRIP_MS, "9999-12-31T23:59:59.999Z".to_string()),
        ] {
            assert_eq!(format_iso8601_ms(ms), text);
            assert_eq!(parse_iso8601_ms(&text), Some(ms));
        }
    }
}

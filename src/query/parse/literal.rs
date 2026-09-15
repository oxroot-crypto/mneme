//! 过滤 DSL 的字面量解析(值 / 数字 / 字符串 / 时间)。
//!
//! 从 [`super`] 拆出的取值解析:所有字面量在产出时即校验可表示性
//! (数字必须有限、相对时间必须落在 ISO 可往返范围内),绝不产出
//! 打印后读不回的 `Val`。

use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::pred::Val;
use crate::query::iso;

use super::parser::Parser;

/// 相对时间量的最大毫秒数(约 2^53):超出后 `f64` 不再精确,拒绝以免静默失真。
const MAX_DELTA_MS: f64 = 9.0e15;

/// 秒的毫秒数。
const SECOND_MS: f64 = 1_000.0;
/// 分的毫秒数。
const MINUTE_MS: f64 = 60_000.0;
/// 时的毫秒数。
const HOUR_MS: f64 = 3_600_000.0;
/// 日的毫秒数。
const DAY_MS: f64 = 86_400_000.0;
/// 周的毫秒数。
const WEEK_MS: f64 = 604_800_000.0;

impl<'a> Parser<'a> {
    /// 只接受字符串字面量的取值位置(`startswith` / `endswith` / `~`)。
    pub(super) fn parse_string_value(&mut self) -> Result<Arc<str>> {
        match self.parse_value()? {
            Val::Str(text) => Ok(text),
            _ => Err(self.error("此位置只接受字符串字面量")),
        }
    }

    /// `value = string | number | "true" | "false" | timestamp | duration_expr`。
    pub(super) fn parse_value(&mut self) -> Result<Val> {
        self.skip_ws();
        let Some(first) = self.rest().chars().next() else {
            return Err(self.error("期望取值"));
        };
        if first == '"' {
            let text = self.parse_quoted()?;
            return Ok(Val::Str(Arc::from(text)));
        }
        if first == 't' && self.eat_keyword("true") {
            return Ok(Val::Bool(true));
        }
        if first == 'f' && self.eat_keyword("false") {
            return Ok(Val::Bool(false));
        }
        if self.at_timestamp() {
            return self.parse_timestamp();
        }
        if self.at_keyword("now") {
            return self.parse_now();
        }
        let (text, is_int) = self.parse_number_text()?;
        if is_int && let Ok(int) = text.parse::<i64>() {
            return Ok(Val::Int(int));
        }
        // `parse_number_text` 已保证可解析且有限。
        let value = text.parse::<f64>().map_err(|_| self.error("非法数字"))?;
        Ok(Val::Num(value))
    }

    /// 当前位置是否匹配关键字(后随字符不是标识符续字符)。
    pub(super) fn at_keyword(&self, keyword: &str) -> bool {
        let Some(after) = self.rest().strip_prefix(keyword) else {
            return false;
        };
        !after.starts_with(|c: char| c.is_alphanumeric() || c == '_')
    }

    /// `value` 位置是否是以 `ts"` 开头的时间戳字面量。
    fn at_timestamp(&self) -> bool {
        let rest = self.rest();
        let Some(after) = rest.strip_prefix("ts") else {
            return false;
        };
        after.trim_start().starts_with('"')
    }

    /// `ts"2024-06-01T00:00:00Z"` → `Val::Ts(Unix 毫秒)`。
    fn parse_timestamp(&mut self) -> Result<Val> {
        self.pos += "ts".len();
        let text = self.parse_quoted()?;
        let ms =
            iso::parse_iso8601_ms(&text).ok_or_else(|| self.error("ts 字面量不是合法 ISO 8601"))?;
        // 时区偏移可把 4 位年份边界推出可往返范围;越界即拒绝,保证 `Display` 读得回。
        if !(iso::MIN_ROUNDTRIP_MS..=iso::MAX_ROUNDTRIP_MS).contains(&ms) {
            return Err(self.error("ts 字面量超出可往返范围"));
        }
        Ok(Val::Ts(ms))
    }

    /// `now ( - | + ) duration`,查询时刻求值为绝对毫秒。
    ///
    /// 结果必须落在 [`iso`] 可往返格式化的范围内,否则返回 `FilterParse`
    /// (避免 `Display` 打印出解析器读不回的时间)。
    fn parse_now(&mut self) -> Result<Val> {
        self.pos += "now".len();
        self.skip_ws();
        let sign = if self.eat_symbol("-") {
            -1.0
        } else if self.eat_symbol("+") {
            1.0
        } else {
            return Err(self.error("now 后须为 '-' 或 '+' 与时间量(如 now - 7d)"));
        };
        self.skip_ws();
        let (text, _) = self.parse_number_text()?;
        let amount = text.parse::<f64>().map_err(|_| self.error("非法数字"))?;
        let unit = self
            .rest()
            .chars()
            .next()
            .ok_or_else(|| self.error("时间量缺单位(s/m/h/d/w)"))?;
        let unit_ms = match unit {
            's' => SECOND_MS,
            'm' => MINUTE_MS,
            'h' => HOUR_MS,
            'd' => DAY_MS,
            'w' => WEEK_MS,
            _ => return Err(self.error("时间量单位须为 s/m/h/d/w")),
        };
        self.pos += unit.len_utf8();
        let delta = amount * unit_ms;
        if !delta.is_finite() || delta > MAX_DELTA_MS {
            return Err(self.error("时间量超出可表示范围"));
        }
        // `sign * delta` 的绝对值不超过 `MAX_DELTA_MS`,不会饱和转换。
        let offset = (sign * delta) as i64;
        let ts = self
            .now_ms
            .checked_add(offset)
            .ok_or_else(|| self.error("时间量超出可表示范围"))?;
        if !(iso::MIN_ROUNDTRIP_MS..=iso::MAX_ROUNDTRIP_MS).contains(&ts) {
            return Err(self.error("时间量超出 ISO 8601 可表示范围"));
        }
        Ok(Val::Ts(ts))
    }

    /// 读取数字字面量,返回 `(原文, 是否无小数/指数)`。
    fn parse_number_text(&mut self) -> Result<(&'a str, bool)> {
        let rest = self.rest();
        let bytes = rest.as_bytes();
        let mut end = 0;
        if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
            end += 1;
        }
        let digits_start = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let mut is_int = true;
        if end < bytes.len() && bytes[end] == b'.' {
            is_int = false;
            end += 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
        }
        if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
            is_int = false;
            end += 1;
            if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
                end += 1;
            }
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
        }
        if end == digits_start {
            return Err(self.error("期望数字"));
        }
        let text = &rest[..end];
        let value = text.parse::<f64>().map_err(|_| self.error("非法数字"))?;
        if !value.is_finite() {
            return Err(self.error("数字超出可表示范围"));
        }
        self.pos += end;
        Ok((text, is_int))
    }

    /// 双引号字符串(支持 `\"` `\\` `\n` `\t` `\r`)。
    fn parse_quoted(&mut self) -> Result<String> {
        self.skip_ws();
        if !self.eat_symbol("\"") {
            return Err(self.error("期望字符串字面量"));
        }
        let mut out = String::new();
        loop {
            let Some(c) = self.rest().chars().next() else {
                return Err(self.error("字符串未闭合"));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\\' => {
                    self.pos += 1;
                    let Some(escaped) = self.rest().chars().next() else {
                        return Err(self.error("转义序列未完成"));
                    };
                    let decoded = match escaped {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        _ => return Err(self.error("不支持的转义序列")),
                    };
                    out.push(decoded);
                    self.pos += escaped.len_utf8();
                }
                _ => {
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }
}

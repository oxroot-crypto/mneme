//! 过滤 DSL 解析器(设计 06 §1)。
//!
//! 手写递归下降,优先级 `not > and > or`;接受 `&&` / `||` / `!` 别名,
//! 以及 `always` / `never` 真值常量(供 `Display` 往返闭合)。对任意输入返回
//! 结构化 [`MnemeError::FilterParse`](crate::MnemeError::FilterParse) 且带位置,
//! 绝不 panic(I7)。
//!
//! 值字面量(数字/字符串/时间)的解析见 [`literal`](self::literal)。

use crate::core::error::{MnemeError, Result};
use crate::core::options::{Clock, SystemClock};
use crate::memory::pred::{CmpOp, Expr, Val};

mod literal;

/// 表达式最大嵌套深度(防畸形输入导致递归栈溢出)。
const MAX_DEPTH: usize = 128;

impl Expr {
    /// 解析过滤 DSL 字符串。
    ///
    /// 相对时间字面量(`now - 7d`)以调用时刻求值为绝对毫秒;需要确定性测试时
    /// 请使用解析后的 [`Expr`] 或直接构造组合器。
    ///
    /// # Arguments
    /// * `input` - DSL 原文,如 `kind == "preference" && importance > 0.5`。
    ///
    /// # Returns
    /// 解析成功的过滤表达式。
    ///
    /// # Errors
    /// 语法错误返回 [`MnemeError::FilterParse`],消息携带出错字节位置。
    ///
    /// # Examples
    /// ```
    /// use mneme::Expr;
    ///
    /// let expr = Expr::from_str(r#"kind == "preference" && importance > 0.5"#).unwrap();
    /// assert!(Expr::from_str("kind ==").is_err());
    /// # let _ = expr;
    /// ```
    // 固有方法而非 `FromStr` 实现:宿主 `Expr::from_str(..)` 无需 import trait。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(input: &str) -> Result<Expr> {
        let now_ms = SystemClock.now_unix_ms();
        parse_at(input, now_ms)
    }
}

/// 以给定"当前时刻"解析 DSL(测试可注入固定时间,保证相对时间可重现)。
pub(crate) fn parse_at(input: &str, now_ms: i64) -> Result<Expr> {
    let mut parser = Parser::new(input, now_ms);
    let expr = parser.parse_or()?;
    parser.skip_ws();
    if !parser.rest().is_empty() {
        return Err(parser.error("表达式末尾有未解析内容"));
    }
    Ok(expr)
}

/// 递归下降解析器(按字节推进;非 ASCII 只出现在标识符与字符串内)。
struct Parser<'a> {
    input: &'a str,
    pos: usize,
    now_ms: i64,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, now_ms: i64) -> Self {
        Self {
            input,
            pos: 0,
            now_ms,
            depth: 0,
        }
    }

    /// 未消费的剩余输入。
    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    /// 跳过 Unicode 空白。
    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// 构造带字节位置的解析错误。
    fn error(&self, message: impl std::fmt::Display) -> MnemeError {
        MnemeError::FilterParse(format!("第 {} 字节: {message}", self.pos))
    }

    /// 吃掉恰好匹配的符号(`==`、`(`、`~` 等 ASCII 片段)。
    fn eat_symbol(&mut self, symbol: &str) -> bool {
        if self.rest().starts_with(symbol) {
            self.pos += symbol.len();
            true
        } else {
            false
        }
    }

    /// 吃掉关键字;要求后随字符不是标识符续字符(`a` 不匹配 `ab`)。
    fn eat_keyword(&mut self, keyword: &str) -> bool {
        let rest = self.rest();
        let Some(after) = rest.strip_prefix(keyword) else {
            return false;
        };
        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return false;
        }
        self.pos += keyword.len();
        true
    }

    /// 关键字后(允许空白)是否紧跟 `(`,用于 `exists(...)` / `is_null(...)`。
    fn at_call(&self, keyword: &str) -> bool {
        let rest = self.rest();
        let Some(after) = rest.strip_prefix(keyword) else {
            return false;
        };
        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return false;
        }
        after.trim_start().starts_with('(')
    }

    /// 吃掉真值常量关键字(`always` / `never`),仅当后随边界是表达式分隔符。
    fn eat_literal_keyword(&mut self, keyword: &str) -> bool {
        let rest = self.rest();
        let Some(after) = rest.strip_prefix(keyword) else {
            return false;
        };
        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return false;
        }
        let trimmed = after.trim_start();
        // `and`/`or` 也要看后续字符:`always andy == 1` 中的 `andy` 不是边界。
        let keyword_boundary = |word: &str| {
            trimmed
                .strip_prefix(word)
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        };
        let boundary = trimmed.is_empty()
            || trimmed.starts_with(')')
            || trimmed.starts_with(',')
            || keyword_boundary("and")
            || keyword_boundary("or")
            || trimmed.starts_with("&&")
            || trimmed.starts_with("||");
        if boundary {
            self.pos += keyword.len();
        }
        boundary
    }

    /// 吃掉 `!` 别名(不吃 `!=` 的首字符)。
    fn eat_not_alias(&mut self) -> bool {
        if self.rest().starts_with('!') && !self.rest().starts_with("!=") {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `or = and { ("or" | "||") and }`。
    fn parse_or(&mut self) -> Result<Expr> {
        let first = self.parse_and()?;
        let mut rest = Vec::new();
        loop {
            let save = self.pos;
            self.skip_ws();
            if self.eat_keyword("or") || self.eat_symbol("||") {
                rest.push(self.parse_and()?);
            } else {
                self.pos = save;
                break;
            }
        }
        if rest.is_empty() {
            return Ok(first);
        }
        let mut parts = Vec::with_capacity(rest.len() + 1);
        parts.push(first);
        parts.extend(rest);
        Ok(Expr::Or(parts.into_boxed_slice()))
    }

    /// `and = not { ("and" | "&&") not }`。
    fn parse_and(&mut self) -> Result<Expr> {
        let first = self.parse_not()?;
        let mut rest = Vec::new();
        loop {
            let save = self.pos;
            self.skip_ws();
            if self.eat_keyword("and") || self.eat_symbol("&&") {
                rest.push(self.parse_not()?);
            } else {
                self.pos = save;
                break;
            }
        }
        if rest.is_empty() {
            return Ok(first);
        }
        let mut parts = Vec::with_capacity(rest.len() + 1);
        parts.push(first);
        parts.extend(rest);
        Ok(Expr::And(parts.into_boxed_slice()))
    }

    /// `not = [ "not" | "!" ] primary`。
    fn parse_not(&mut self) -> Result<Expr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("表达式嵌套过深"));
        }
        self.skip_ws();
        let result = if self.eat_keyword("not") || self.eat_not_alias() {
            self.parse_not().map(|inner| Expr::Not(Box::new(inner)))
        } else {
            self.parse_primary()
        };
        self.depth -= 1;
        result
    }

    /// `primary = "(" expr ")" | "exists" "(" path ")" | "is_null" "(" path ")" | cmp`。
    fn parse_primary(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.eat_symbol("(") {
            let expr = self.parse_or()?;
            self.skip_ws();
            if !self.eat_symbol(")") {
                return Err(self.error("期望 ')'"));
            }
            return Ok(expr);
        }
        if self.at_call("exists") {
            self.pos += "exists".len();
            return self.parse_unary_path_call("exists", Expr::Exists as fn(String) -> Expr);
        }
        if self.at_call("is_null") {
            self.pos += "is_null".len();
            return self.parse_unary_path_call("is_null", Expr::IsNull as fn(String) -> Expr);
        }
        if self.eat_literal_keyword("always") {
            return Ok(Expr::Always);
        }
        if self.eat_literal_keyword("never") {
            return Ok(Expr::Never);
        }
        let field = self.parse_path()?;
        self.parse_comparison(field)
    }

    /// 解析 `exists(path)` / `is_null(path)` 的公共形态。
    fn parse_unary_path_call(&mut self, name: &str, build: fn(String) -> Expr) -> Result<Expr> {
        self.skip_ws();
        if !self.eat_symbol("(") {
            return Err(self.error(format!("{name} 后须为 '(...)'")));
        }
        self.skip_ws();
        let field = self.parse_path()?;
        self.skip_ws();
        if !self.eat_symbol(")") {
            return Err(self.error("期望 ')'"));
        }
        Ok(build(field))
    }

    /// `path = ident { "." ident }`。
    fn parse_path(&mut self) -> Result<String> {
        let start = self.pos;
        self.parse_ident_segment()?;
        loop {
            let save = self.pos;
            if !self.eat_symbol(".") {
                self.pos = save;
                break;
            }
            if self.parse_ident_segment().is_err() {
                self.pos = save;
                break;
            }
        }
        Ok(self.input[start..self.pos].to_string())
    }

    /// 单个标识符段:首字符为字母或 `_`,后续可含数字、`_` 与 `-`。
    fn parse_ident_segment(&mut self) -> Result<()> {
        let Some(first) = self.rest().chars().next() else {
            return Err(self.error("期望字段名"));
        };
        if !(first.is_alphabetic() || first == '_') {
            return Err(self.error("字段名须以字母或 '_' 开头"));
        }
        self.pos += first.len_utf8();
        while let Some(c) = self.rest().chars().next() {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        Ok(())
    }

    /// 路径之后的运算符与取值(比较 / `in` / `contains` / 字符串算子 / `~`)。
    fn parse_comparison(&mut self, field: String) -> Result<Expr> {
        self.skip_ws();
        let op = if self.eat_symbol("==") {
            Some(CmpOp::Eq)
        } else if self.eat_symbol("!=") {
            Some(CmpOp::Ne)
        } else if self.eat_symbol(">=") {
            Some(CmpOp::Ge)
        } else if self.eat_symbol("<=") {
            Some(CmpOp::Le)
        } else if self.eat_symbol(">") {
            Some(CmpOp::Gt)
        } else if self.eat_symbol("<") {
            Some(CmpOp::Lt)
        } else {
            None
        };
        if let Some(op) = op {
            let val = self.parse_value()?;
            return Ok(Expr::Cmp { op, field, val });
        }
        if self.eat_symbol("~") {
            let pattern = self.parse_string_value()?;
            return Ok(Expr::Glob(field, pattern));
        }
        if self.eat_keyword("in") {
            return self.parse_in(field);
        }
        if self.eat_keyword("contains") {
            let val = self.parse_value()?;
            return Ok(Expr::Contains(field, val));
        }
        if self.eat_keyword("startswith") {
            let text = self.parse_string_value()?;
            return Ok(Expr::StartsWith(field, text));
        }
        if self.eat_keyword("endswith") {
            let text = self.parse_string_value()?;
            return Ok(Expr::EndsWith(field, text));
        }
        Err(self.error("期望比较运算符(== != > >= < <= in contains startswith endswith ~)"))
    }

    /// `in ( value { "," value } )`;取值去重。
    fn parse_in(&mut self, field: String) -> Result<Expr> {
        self.skip_ws();
        if !self.eat_symbol("(") {
            return Err(self.error("in 后须为 '(...)'"));
        }
        let mut vals: Vec<Val> = Vec::new();
        loop {
            let val = self.parse_value()?;
            // 线性查重:`Val` 含 `f64`,无 `Hash`/`Ord`;`in` 列表规模受解析
            // 预算 $O(L)$ 限制(现阶段用例千级以内),$O(n^2)$ 可接受。
            if !vals.contains(&val) {
                vals.push(val);
            }
            self.skip_ws();
            if !self.eat_symbol(",") {
                break;
            }
        }
        if !self.eat_symbol(")") {
            return Err(self.error("期望 ')'"));
        }
        Ok(Expr::In(field, vals.into_boxed_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-QUERY-ERR-001
    #[test]
    fn parses_precedence_and_aliases() {
        let expr = Expr::from_str(r#"a == 1 || b == 2 && !(c == 3)"#).expect("parse");
        let Expr::Or(parts) = expr else {
            panic!("顶层应是 Or");
        };
        assert!(matches!(parts[1], Expr::And(_)));
    }

    /// FC-QUERY-ERR-001
    #[test]
    fn parses_every_operator_family() {
        for text in [
            r#"n != 1"#,
            r#"n >= 1"#,
            r#"n <= 1"#,
            r#"n > 1"#,
            r#"n < 1"#,
            r#"s in ("a", "b")"#,
            r#"tags contains "x""#,
            r#"s startswith "he""#,
            r#"s endswith "lo""#,
            r#"s ~ "a*c?""#,
            r#"exists(kind)"#,
            r#"is_null(kind)"#,
            r#"a.b.c == true"#,
            r#"always"#,
            r#"never"#,
        ] {
            Expr::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        }
    }

    /// FC-QUERY-ERR-001(相对时间以固定 now 求值)
    #[test]
    fn resolves_relative_time_with_injected_now() {
        let expr = parse_at("created_at > now - 7d", 1_000_000_000).expect("parse");
        let Expr::Cmp {
            val: Val::Ts(ms), ..
        } = expr
        else {
            panic!("应是 Ts 比较");
        };
        assert_eq!(ms, 1_000_000_000 - 7 * 86_400_000);
    }

    /// FC-QUERY-ERR-001(ts 字面量)
    #[test]
    fn resolves_timestamp_literal() {
        let expr = Expr::from_str(r#"created_at >= ts"2024-06-01T00:00:00Z""#).expect("parse");
        assert!(matches!(
            expr,
            Expr::Cmp {
                val: Val::Ts(_),
                ..
            }
        ));
    }

    /// FC-QUERY-ERR-001(任意非法输入返回结构化错误且带位置)
    #[test]
    fn malformed_inputs_report_position() {
        for text in [
            "kind ==",
            "kind = 1",
            "in (1)",
            "exists",
            "exists(",
            r#"s startswith 3"#,
            r#"s contains"#,
            "a and",
            "(a == 1",
            r#"x == "unterminated"#,
            "now 7d",
            "now - 7x",
            "in",
        ] {
            let error = Expr::from_str(text)
                .err()
                .unwrap_or_else(|| panic!("{text} 应解析失败"));
            let MnemeError::FilterParse(message) = error else {
                panic!("{text} 应是 FilterParse");
            };
            assert!(
                message.contains("第 ") && message.contains(" 字节"),
                "{text}: 错误消息缺位置: {message}"
            );
        }
    }

    /// FC-QUERY-ERR-001(数字/时间量超出可表示范围时结构化拒绝,绝不溢出 panic)
    #[test]
    fn out_of_range_numbers_and_durations_are_rejected() {
        for text in [
            "x == 1e999",
            "x == -1e999",
            "x == inf",
            "created_at > now + 9999999999999999999d",
            "created_at > now - 9999999999999999999d",
            "created_at > now + 1e300w",
            "created_at > now + 3000000d",
            "created_at > now - 3000000d",
            r#"created_at > ts"9999-12-31T23:59:59.999-23:59""#,
            r#"created_at > ts"0000-01-01T00:00:00.000+23:59""#,
        ] {
            assert!(
                matches!(Expr::from_str(text), Err(MnemeError::FilterParse(_))),
                "{text} 应被结构化拒绝"
            );
        }
    }

    /// FC-QUERY-ERR-001(过深嵌套不 panic)
    #[test]
    fn deeply_nested_input_is_rejected_without_panic() {
        let depth = MAX_DEPTH + 10;
        let text = format!("{}a == 1{}", "(".repeat(depth), ")".repeat(depth));
        assert!(matches!(
            Expr::from_str(&text),
            Err(MnemeError::FilterParse(_))
        ));
    }

    #[test]
    fn in_deduplicates_values() {
        let expr = Expr::from_str(r#"k in (1, 1, 2)"#).expect("parse");
        let Expr::In(_, vals) = expr else {
            panic!("应是 In");
        };
        assert_eq!(vals.len(), 2);
    }
}

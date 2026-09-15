//! 解析入口与字节级游标:公开入口 `Expr::from_str` / `parse_at`,以及 `Parser`
//! 的游标状态与词法工具(空白、符号、关键字、带位置的错误构造)。

use crate::core::error::{MnemeError, Result};
use crate::core::options::{Clock, SystemClock};
use crate::memory::pred::Expr;

/// 表达式最大嵌套深度(防畸形输入导致递归栈溢出)。
pub(super) const MAX_DEPTH: usize = 128;

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
pub(super) struct Parser<'a> {
    pub(super) input: &'a str,
    pub(super) pos: usize,
    pub(super) now_ms: i64,
    pub(super) depth: usize,
}

impl<'a> Parser<'a> {
    pub(super) fn new(input: &'a str, now_ms: i64) -> Self {
        Self {
            input,
            pos: 0,
            now_ms,
            depth: 0,
        }
    }

    /// 未消费的剩余输入。
    pub(super) fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    /// 跳过 Unicode 空白。
    pub(super) fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// 构造带字节位置的解析错误。
    pub(super) fn error(&self, message: impl std::fmt::Display) -> MnemeError {
        MnemeError::FilterParse(format!("第 {} 字节: {message}", self.pos))
    }

    /// 吃掉恰好匹配的符号(`==`、`(`、`~` 等 ASCII 片段)。
    pub(super) fn eat_symbol(&mut self, symbol: &str) -> bool {
        if self.rest().starts_with(symbol) {
            self.pos += symbol.len();
            true
        } else {
            false
        }
    }

    /// 吃掉关键字;要求后随字符不是标识符续字符(`a` 不匹配 `ab`)。
    pub(super) fn eat_keyword(&mut self, keyword: &str) -> bool {
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
    pub(super) fn at_call(&self, keyword: &str) -> bool {
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
    pub(super) fn eat_literal_keyword(&mut self, keyword: &str) -> bool {
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
    pub(super) fn eat_not_alias(&mut self) -> bool {
        if self.rest().starts_with('!') && !self.rest().starts_with("!=") {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

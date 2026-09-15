//! 递归下降语法规则:优先级 `not > and > or`,覆盖括号、`exists` / `is_null`
//! 调用、真值常量与字段比较。

use crate::core::error::Result;
use crate::memory::pred::{CmpOp, Expr, Val};

use super::parser::{MAX_DEPTH, Parser};

impl<'a> Parser<'a> {
    /// `or = and { ("or" | "||") and }`。
    pub(super) fn parse_or(&mut self) -> Result<Expr> {
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
    pub(super) fn parse_and(&mut self) -> Result<Expr> {
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
    pub(super) fn parse_not(&mut self) -> Result<Expr> {
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
    pub(super) fn parse_primary(&mut self) -> Result<Expr> {
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
    pub(super) fn parse_unary_path_call(
        &mut self,
        name: &str,
        build: fn(String) -> Expr,
    ) -> Result<Expr> {
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
    pub(super) fn parse_path(&mut self) -> Result<String> {
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
    pub(super) fn parse_ident_segment(&mut self) -> Result<()> {
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
    pub(super) fn parse_comparison(&mut self, field: String) -> Result<Expr> {
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
    pub(super) fn parse_in(&mut self, field: String) -> Result<Expr> {
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

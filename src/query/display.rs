//! 过滤 AST 的文本打印(设计 06 §1 的 `Display` 入口)。
//!
//! 打印结果可被 [`Expr::from_str`](crate::Expr::from_str) 原样读回(等价):
//! 按优先级决定是否补括号,`Ts` 打印为 `ts"..."` 绝对时间,字符串做转义。
//!
//! “等价”以解析产物为准:解析器会对 `In` 列表去重,故程序直接构造的重复项
//! (如 `In("x", [1, 1])`)打印后读回会规约为去重形式(FC-QUERY-POST-002)。

use std::fmt::{self, Write};

use crate::memory::pred::{CmpOp, Expr, Val};

use super::iso;

/// 表达式的打印优先级:`Or` 最低、叶子最高。
fn precedence(expr: &Expr) -> u8 {
    match expr {
        Expr::Or(_) => 1,
        Expr::And(_) => 2,
        Expr::Not(_) => 3,
        _ => 4,
    }
}

/// 打印一个子表达式;子优先级低于父级时补括号。
fn write_expr(f: &mut fmt::Formatter<'_>, expr: &Expr, parent: u8) -> fmt::Result {
    let parens = precedence(expr) < parent;
    if parens {
        f.write_char('(')?;
    }
    write_inner(f, expr)?;
    if parens {
        f.write_char(')')?;
    }
    Ok(())
}

/// 打印 `parts` 列表,元素间以 `separator` 连接(括号由各元素自行处理)。
fn write_joined(
    f: &mut fmt::Formatter<'_>,
    parts: &[Expr],
    separator: &str,
    parent: u8,
) -> fmt::Result {
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            f.write_str(separator)?;
        }
        write_expr(f, part, parent)?;
    }
    Ok(())
}

/// 打印字符串字面量(含引号与转义)。
fn write_quoted(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in text.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\t' => f.write_str("\\t")?,
            '\r' => f.write_str("\\r")?,
            _ => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

/// 打印表达式主体(不含外层括号)。
fn write_inner(f: &mut fmt::Formatter<'_>, expr: &Expr) -> fmt::Result {
    match expr {
        Expr::Always => f.write_str("always"),
        Expr::Never => f.write_str("never"),
        // 空逻辑项按求值结果规约(空 and 恒真、空 or 恒假),保证打印可读回。
        Expr::And(parts) if parts.is_empty() => f.write_str("always"),
        Expr::And(parts) => write_joined(f, parts, " and ", 2),
        Expr::Or(parts) if parts.is_empty() => f.write_str("never"),
        Expr::Or(parts) => write_joined(f, parts, " or ", 1),
        Expr::Not(inner) => {
            f.write_str("not ")?;
            write_expr(f, inner, 3)
        }
        Expr::Cmp { op, field, val } => write!(f, "{field} {op} {val}"),
        // 空 `in` 恒不命中,打印为 `never`(与解析器至少要求一个取值一致)。
        Expr::In(_, vals) if vals.is_empty() => f.write_str("never"),
        Expr::In(field, vals) => {
            write!(f, "{field} in (")?;
            for (index, val) in vals.iter().enumerate() {
                if index > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{val}")?;
            }
            f.write_char(')')
        }
        Expr::Contains(field, val) => write!(f, "{field} contains {val}"),
        Expr::StartsWith(field, prefix) => {
            write!(f, "{field} startswith ")?;
            write_quoted(f, prefix)
        }
        Expr::EndsWith(field, suffix) => {
            write!(f, "{field} endswith ")?;
            write_quoted(f, suffix)
        }
        Expr::Glob(field, pattern) => {
            write!(f, "{field} ~ ")?;
            write_quoted(f, pattern)
        }
        Expr::Exists(field) => write!(f, "exists({field})"),
        Expr::IsNull(field) => write!(f, "is_null({field})"),
    }
}

impl fmt::Display for CmpOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
        })
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Val::Bool(value) => write!(f, "{value}"),
            Val::Int(value) => write!(f, "{value}"),
            Val::Num(value) => {
                let mut text = value.to_string();
                // 保证浮点常量读回仍是 Num:无小数点时补 ".0"。
                if value.is_finite() && !text.contains(['.', 'e', 'E']) {
                    text.push_str(".0");
                }
                f.write_str(&text)
            }
            Val::Str(text) => write_quoted(f, text),
            Val::Ts(ms) => write!(f, "ts\"{}\"", iso::format_iso8601_ms(*ms)),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_expr(f, self, 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::memory::pred::Expr;
    use crate::query::parse::parse_at;

    /// FC-QUERY-POST-002(打印 → 解析往返等价)
    #[test]
    fn display_roundtrips_through_parser() {
        for text in [
            r#"a == 1 and b == 2 or c == 3"#,
            r#"not (a == 1 or b == 2) and c != "x""#,
            r#"a.b >= 1.5 and s startswith "he" and t ~ "a*c?""#,
            r#"k in (1, 2, 3) and tags contains "x""#,
            r#"exists(kind) and not is_null(kind)"#,
            r#"created_at > ts"2024-06-01T00:00:00Z" and n <= -2.5"#,
            "always",
            "never",
            r#"v == true or w == false"#,
            r#"s == "a\"b\\c\n""#,
        ] {
            let expr = parse_at(text, 0).expect("parse");
            let printed = expr.to_string();
            let reparsed = parse_at(&printed, 0).unwrap_or_else(|error| {
                panic!("{text} -> {printed}: {error}");
            });
            assert_eq!(expr, reparsed, "往返不一致: {text} -> {printed}");
        }
    }

    /// FC-QUERY-POST-002(空逻辑项/空 `in` 打印为等价的真值常量,仍可读回)
    #[test]
    fn empty_lists_print_as_truth_constants() {
        let cases = [
            (
                Expr::And(Vec::new().into_boxed_slice()),
                "always",
                Expr::Always,
            ),
            (
                Expr::Or(Vec::new().into_boxed_slice()),
                "never",
                Expr::Never,
            ),
            (
                Expr::In("x".into(), Vec::new().into_boxed_slice()),
                "never",
                Expr::Never,
            ),
        ];
        for (expr, expected, decoded) in cases {
            let printed = expr.to_string();
            assert_eq!(printed, expected);
            let reparsed = parse_at(&printed, 0).expect("parse");
            assert_eq!(reparsed, decoded, "空列表规约后必须可读回");
        }
    }
}

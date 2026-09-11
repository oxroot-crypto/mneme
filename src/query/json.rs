//! 过滤 AST 与 JSON 的往返(设计 06 §1 的 `Meta ↔ Expr` 入口)。
//!
//! 编码为单键对象,便于 Agent 框架下发与持久化。schema:
//!
//! ```text
//! Expr = {"always":true} | {"never":true}
//!      | {"and":[...]} | {"or":[...]} | {"not":Expr}
//!      | {"cmp":{"op":"eq|ne|gt|ge|lt|le","field":...,"val":Val}}
//!      | {"in":{"field":...,"vals":[...]}}
//!      | {"contains":{"field":...,"val":Val}}
//!      | {"starts_with":{"field":...,"val":"..."}}
//!      | {"ends_with":{"field":...,"val":"..."}}
//!      | {"glob":{"field":...,"val":"..."}}
//!      | {"exists":"field"} | {"is_null":"field"}
//! Val  = {"bool":b} | {"int":i} | {"num":f} | {"str":"..."} | {"ts":毫秒}
//! ```

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{Meta, json};
use crate::memory::pred::{CmpOp, Expr, Val};

use super::iso;

/// 构造"JSON 结构非法"的过滤错误。
fn invalid(reason: impl std::fmt::Display) -> MnemeError {
    MnemeError::FilterParse(format!("过滤表达式 JSON 非法:{reason}"))
}

/// 取单键对象的唯一键值对。
fn single_entry<'a>(meta: &'a Meta, what: &str) -> Result<(&'a str, &'a Meta)> {
    let Some(map) = meta.as_object() else {
        return Err(invalid(format!("{what} 必须是对象")));
    };
    let mut iter = map.iter();
    let Some((key, value)) = iter.next() else {
        return Err(invalid(format!("{what} 不能为空对象")));
    };
    if iter.next().is_some() {
        return Err(invalid(format!("{what} 必须恰好一个键")));
    }
    Ok((key.as_str(), value))
}

/// 取对象中的指定字段。
fn field<'a>(map: &'a Meta, name: &str) -> Result<&'a Meta> {
    map.get(name)
        .ok_or_else(|| invalid(format!("缺少字段 `{name}`")))
}

/// 取对象中的字符串字段。
fn field_str<'a>(map: &'a Meta, name: &str) -> Result<&'a str> {
    field(map, name)?
        .as_str()
        .ok_or_else(|| invalid(format!("字段 `{name}` 必须是字符串")))
}

/// `CmpOp` → JSON 中的运算符名。
fn op_name(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "eq",
        CmpOp::Ne => "ne",
        CmpOp::Gt => "gt",
        CmpOp::Ge => "ge",
        CmpOp::Lt => "lt",
        CmpOp::Le => "le",
    }
}

/// JSON 中的运算符名 → `CmpOp`。
fn op_from_name(name: &str) -> Result<CmpOp> {
    match name {
        "eq" => Ok(CmpOp::Eq),
        "ne" => Ok(CmpOp::Ne),
        "gt" => Ok(CmpOp::Gt),
        "ge" => Ok(CmpOp::Ge),
        "lt" => Ok(CmpOp::Lt),
        "le" => Ok(CmpOp::Le),
        _ => Err(invalid(format!("未知比较运算符 `{name}`"))),
    }
}

/// `Val` → JSON。
fn val_to_meta(val: &Val) -> Meta {
    match val {
        Val::Bool(value) => json!({ "bool": value }),
        Val::Int(value) => json!({ "int": value }),
        Val::Num(value) => json!({ "num": value }),
        Val::Str(value) => json!({ "str": value.as_ref() }),
        Val::Ts(value) => json!({ "ts": value }),
    }
}

/// JSON → `Val`。
fn val_from_meta(meta: &Meta) -> Result<Val> {
    let (key, value) = single_entry(meta, "Val")?;
    match key {
        "bool" => value.as_bool().map(Val::Bool),
        "int" => value.as_i64().map(Val::Int),
        "num" => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(Val::Num),
        "str" => value.as_str().map(|text| Val::Str(Arc::from(text))),
        // 时间戳必须落在 `Display` 可往返的范围内,否则读回会失真。
        "ts" => value
            .as_i64()
            .filter(|ms| (iso::MIN_ROUNDTRIP_MS..=iso::MAX_ROUNDTRIP_MS).contains(ms))
            .map(Val::Ts),
        _ => None,
    }
    .ok_or_else(|| invalid(format!("Val 编码 `{key}` 与取值类型不符")))
}

/// 编码 `Expr` 列表为 JSON 数组。
fn exprs_to_meta(parts: &[Expr]) -> Vec<Meta> {
    parts.iter().map(Expr::to_meta).collect()
}

/// 解码 JSON 数组为 `Expr` 列表;空数组被显式拒绝(与 `Display` 的 `always`/`never`
/// 规约一致,避免同一表达式两种表示)。
fn exprs_from_meta(meta: &Meta) -> Result<Vec<Expr>> {
    let Some(items) = meta.as_array() else {
        return Err(invalid("逻辑运算子项必须是数组"));
    };
    if items.is_empty() {
        return Err(invalid("逻辑运算子项不能为空数组"));
    }
    items.iter().map(Expr::from_meta).collect()
}

/// 解码 `always` / `never` 真值常量。
fn decode_truth(key: &str, value: &Meta) -> Result<Expr> {
    if value.as_bool() != Some(true) {
        return Err(invalid(format!("`{key}` 的值必须是 true")));
    }
    Ok(if key == "always" {
        Expr::Always
    } else {
        Expr::Never
    })
}

/// 解码 `cmp` 节点。
fn decode_cmp(value: &Meta) -> Result<Expr> {
    let op = op_from_name(field_str(value, "op")?)?;
    let name = field_str(value, "field")?;
    let val = val_from_meta(field(value, "val")?)?;
    Ok(Expr::Cmp {
        op,
        field: name.to_string(),
        val,
    })
}

/// 解码 `in` 节点;`vals` 不得为空数组。
fn decode_in(value: &Meta) -> Result<Expr> {
    let name = field_str(value, "field")?;
    let Some(items) = field(value, "vals")?.as_array() else {
        return Err(invalid("`vals` 必须是数组"));
    };
    if items.is_empty() {
        return Err(invalid("`vals` 不能为空数组"));
    }
    let vals: Result<Vec<Val>> = items.iter().map(val_from_meta).collect();
    Ok(Expr::In(name.to_string(), vals?.into_boxed_slice()))
}

/// 解码 `{field, val}` 形态的节点(`contains`)。
fn decode_field_val(value: &Meta, build: fn(String, Val) -> Expr) -> Result<Expr> {
    Ok(build(
        field_str(value, "field")?.to_string(),
        val_from_meta(field(value, "val")?)?,
    ))
}

/// 解码 `{field, val:字符串}` 形态的节点(`starts_with` / `ends_with` / `glob`)。
fn decode_field_str(value: &Meta, build: fn(String, Arc<str>) -> Expr) -> Result<Expr> {
    Ok(build(
        field_str(value, "field")?.to_string(),
        Arc::from(field_str(value, "val")?),
    ))
}

/// 解码 `exists` / `is_null` 的路径参数。
fn decode_path(name: &str, value: &Meta, build: fn(String) -> Expr) -> Result<Expr> {
    let path = value
        .as_str()
        .ok_or_else(|| invalid(format!("`{name}` 的值必须是字符串")))?;
    Ok(build(path.to_string()))
}

impl Expr {
    /// 编码为 JSON(经 `core::meta`,宿主不接触 serde_json)。
    ///
    /// # Returns
    /// 单键对象形态的 [`Meta`],可经 [`Expr::from_meta`] 原样读回。
    ///
    /// # Examples
    /// ```
    /// use mneme::Expr;
    ///
    /// let expr = Expr::from_str(r#"kind == "preference""#).unwrap();
    /// let meta = expr.to_meta();
    /// assert_eq!(Expr::from_meta(&meta).unwrap(), expr);
    /// ```
    pub fn to_meta(&self) -> Meta {
        match self {
            Expr::Always => json!({ "always": true }),
            Expr::Never => json!({ "never": true }),
            // 空逻辑项/空 `in` 按求值结果规约,保证任意构造的 AST 都能往返。
            Expr::And(parts) if parts.is_empty() => json!({ "always": true }),
            Expr::And(parts) => json!({ "and": exprs_to_meta(parts) }),
            Expr::Or(parts) if parts.is_empty() => json!({ "never": true }),
            Expr::Or(parts) => json!({ "or": exprs_to_meta(parts) }),
            Expr::Not(inner) => json!({ "not": inner.to_meta() }),
            Expr::Cmp { op, field, val } => json!({
                "cmp": { "op": op_name(*op), "field": field, "val": val_to_meta(val) }
            }),
            Expr::In(_, vals) if vals.is_empty() => json!({ "never": true }),
            Expr::In(field, vals) => json!({
                "in": {
                    "field": field,
                    "vals": vals.iter().map(val_to_meta).collect::<Vec<_>>(),
                }
            }),
            Expr::Contains(field, val) => json!({
                "contains": { "field": field, "val": val_to_meta(val) }
            }),
            Expr::StartsWith(field, prefix) => json!({
                "starts_with": { "field": field, "val": prefix.as_ref() }
            }),
            Expr::EndsWith(field, suffix) => json!({
                "ends_with": { "field": field, "val": suffix.as_ref() }
            }),
            Expr::Glob(field, pattern) => json!({
                "glob": { "field": field, "val": pattern.as_ref() }
            }),
            Expr::Exists(field) => json!({ "exists": field }),
            Expr::IsNull(field) => json!({ "is_null": field }),
        }
    }

    /// 从 JSON 解码过滤表达式。
    ///
    /// # Arguments
    /// * `meta` - [`Expr::to_meta`] 产出的单键对象。
    ///
    /// # Returns
    /// 解码出的过滤表达式。
    ///
    /// # Errors
    /// 结构不符(多键/空对象/字段类型错误/未知运算符)时返回
    /// [`MnemeError::FilterParse`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Expr, json};
    ///
    /// let meta = json!({"exists": "kind"});
    /// assert_eq!(Expr::from_meta(&meta).unwrap(), Expr::Exists("kind".into()));
    /// assert!(Expr::from_meta(&json!({"unknown": 1})).is_err());
    /// ```
    pub fn from_meta(meta: &Meta) -> Result<Expr> {
        let (key, value) = single_entry(meta, "Expr")?;
        match key {
            "always" | "never" => decode_truth(key, value),
            "and" => Ok(Expr::And(exprs_from_meta(value)?.into_boxed_slice())),
            "or" => Ok(Expr::Or(exprs_from_meta(value)?.into_boxed_slice())),
            "not" => Ok(Expr::Not(Box::new(Expr::from_meta(value)?))),
            "cmp" => decode_cmp(value),
            "in" => decode_in(value),
            "contains" => decode_field_val(value, Expr::Contains),
            "starts_with" => decode_field_str(value, Expr::StartsWith),
            "ends_with" => decode_field_str(value, Expr::EndsWith),
            "glob" => decode_field_str(value, Expr::Glob),
            "exists" => decode_path("exists", value, Expr::Exists),
            "is_null" => decode_path("is_null", value, Expr::IsNull),
            _ => Err(invalid(format!("未知 Expr 键 `{key}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse_at;

    /// FC-QUERY-POST-002(JSON 往返)
    #[test]
    fn json_roundtrips_for_all_node_kinds() {
        for text in [
            r#"a == 1.5 and b <= -2 and c == true"#,
            r#"k in ("a", "b") or not exists(kind)"#,
            r#"tags contains "x" and s startswith "he" and t endswith "lo""#,
            r#"p ~ "a*c?" and is_null(x)"#,
            r#"created_at > ts"2024-06-01T00:00:00Z""#,
            "always",
            "never",
            r#"a.b.c != "x""#,
        ] {
            let expr = parse_at(text, 0).expect("parse");
            let meta = expr.to_meta();
            let decoded = Expr::from_meta(&meta).unwrap_or_else(|error| panic!("{text}: {error}"));
            assert_eq!(expr, decoded, "{text}");
        }
    }

    /// FC-QUERY-POST-002(非法 JSON 结构显式拒绝)
    #[test]
    fn json_rejects_malformed_shapes() {
        for meta in [
            json!({}),
            json!({"and": 1}),
            json!({"a": 1, "b": 2}),
            json!({"cmp": {"op": "unknown", "field": "x", "val": {"int": 1}}}),
            json!({"cmp": {"op": "eq", "field": 1, "val": {"int": 1}}}),
            json!({"in": {"field": "x", "vals": [1, 2]}}),
            json!({"exists": 1}),
            json!({"always": false}),
            json!({"str": "x"}),
            json!({"and": []}),
            json!({"or": []}),
            json!({"in": {"field": "x", "vals": []}}),
            json!({"cmp": {"op": "eq", "field": "x", "val": {"ts": i64::MAX}}}),
        ] {
            assert!(
                matches!(Expr::from_meta(&meta), Err(MnemeError::FilterParse(_))),
                "{meta} 应拒绝"
            );
        }
    }

    /// FC-QUERY-POST-002(程序构造的空逻辑项/空 `in` 编码为真值常量,仍可解码)
    #[test]
    fn json_encodes_empty_lists_as_truth_constants() {
        for (expr, expected) in [
            (Expr::And(Vec::new().into_boxed_slice()), Expr::Always),
            (Expr::Or(Vec::new().into_boxed_slice()), Expr::Never),
            (
                Expr::In("x".into(), Vec::new().into_boxed_slice()),
                Expr::Never,
            ),
        ] {
            let decoded = Expr::from_meta(&expr.to_meta()).expect("decode");
            assert_eq!(decoded, expected);
        }
    }
}

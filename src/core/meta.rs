//! L0 元数据(JSON)隔离区。
//!
//! 全库**只有本模块接触 `serde_json`**;其余代码只见 [`Meta`] 别名与辅助函数。
//! `Meta` 选择 JSON Value 而非强类型 schema:Agent 的记忆元数据是开放的
//! (不同应用记不同字段),schema-free 是需求而非妥协。若未来要换自研 JSON,
//! 只需改本文件。
//!
//! 深度与大小限额在写入入口校验(见设计 16 §8),本模块只做无副作用的读取辅助。

pub use serde_json::json;

/// 元数据:任意 JSON 值。
pub type Meta = serde_json::Value;

/// 按点路径 `"a.b.c"` 读取嵌套对象字段。
///
/// # Arguments
///
/// * `value` - 起始 JSON 值。
/// * `path` - 以 `.` 分隔的键路径;空字符串返回 `value` 本身。
///
/// # Returns
///
/// 路径存在且沿途均为对象时返回对应字段的引用;任一段缺失或当前值不是对象时返回
/// `None`。路径不解析数组下标。
///
/// # Examples
///
/// ```
/// use mneme::meta::get_path;
/// use mneme::json;
///
/// let v = json!({"a": {"b": 1}});
/// assert_eq!(get_path(&v, "a.b").and_then(|x| x.as_i64()), Some(1));
/// assert!(get_path(&v, "a.c").is_none());
/// ```
pub fn get_path<'v>(value: &'v Meta, path: &str) -> Option<&'v Meta> {
    if path.is_empty() {
        return Some(value);
    }
    let mut current = value;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// 若 `value` 是 JSON 数字,返回其 `f64` 形式。
///
/// # Arguments
///
/// * `value` - 待读取的 JSON 值。
///
/// # Returns
///
/// 是 JSON 数字时返回 `Some(f64)`,否则返回 `None`。
///
/// # Examples
///
/// ```
/// use mneme::json;
/// use mneme::meta::as_f64;
///
/// assert_eq!(as_f64(&json!(1.5)), Some(1.5));
/// assert_eq!(as_f64(&json!("x")), None);
/// ```
pub fn as_f64(value: &Meta) -> Option<f64> {
    value.as_f64()
}

/// 若 `value` 是 JSON 整数(或可无损转换的 `u64`),返回其 `i64` 形式。
///
/// # Arguments
///
/// * `value` - 待读取的 JSON 值。
///
/// # Returns
///
/// 是 JSON 整数时返回 `Some(i64)`,否则返回 `None`。
///
/// # Examples
///
/// ```
/// use mneme::json;
/// use mneme::meta::as_i64;
///
/// assert_eq!(as_i64(&json!(42)), Some(42));
/// assert_eq!(as_i64(&json!(1.5)), None);
/// ```
pub fn as_i64(value: &Meta) -> Option<i64> {
    value.as_i64()
}

/// 若 `value` 是 JSON 布尔值,返回其 `bool` 形式。
///
/// # Arguments
///
/// * `value` - 待读取的 JSON 值。
///
/// # Returns
///
/// 是 JSON 布尔值时返回 `Some(bool)`,否则返回 `None`。
///
/// # Examples
///
/// ```
/// use mneme::json;
/// use mneme::meta::as_bool;
///
/// assert_eq!(as_bool(&json!(true)), Some(true));
/// assert_eq!(as_bool(&json!(0)), None);
/// ```
pub fn as_bool(value: &Meta) -> Option<bool> {
    value.as_bool()
}

/// 若 `value` 是 JSON 字符串,返回其 `&str` 形式。
///
/// # Arguments
///
/// * `value` - 待读取的 JSON 值。
///
/// # Returns
///
/// 是 JSON 字符串时返回 `Some(&str)`,否则返回 `None`。
///
/// # Examples
///
/// ```
/// use mneme::json;
/// use mneme::meta::as_str;
///
/// assert_eq!(as_str(&json!("text")), Some("text"));
/// assert_eq!(as_str(&json!(1)), None);
/// ```
pub fn as_str(value: &Meta) -> Option<&str> {
    value.as_str()
}

/// 若 `value` 是 JSON 整数,按 Unix 毫秒时间戳返回。
///
/// 与 [`as_i64`] 等价,单独命名以表达时间语义。
///
/// # Arguments
///
/// * `value` - 待读取的 JSON 值。
///
/// # Returns
///
/// 是 JSON 整数时返回 `Some(i64)`(Unix 毫秒),否则返回 `None`。
///
/// # Examples
///
/// ```
/// use mneme::json;
/// use mneme::meta::as_ts;
///
/// assert_eq!(as_ts(&json!(1_700_000_000_000_i64)), Some(1_700_000_000_000));
/// ```
pub fn as_ts(value: &Meta) -> Option<i64> {
    value.as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_path_traverses_nested_objects() {
        let v = json!({"a": {"b": {"c": 7}}});
        assert_eq!(get_path(&v, "a.b.c").and_then(as_i64), Some(7));
    }

    #[test]
    fn get_path_missing_segment_returns_none() {
        let v = json!({"a": {"b": 1}});
        assert!(get_path(&v, "a.c").is_none());
        assert!(get_path(&v, "x").is_none());
    }

    #[test]
    fn get_path_through_non_object_returns_none() {
        let v = json!({"a": 1});
        assert!(get_path(&v, "a.b").is_none());
    }

    #[test]
    fn get_path_empty_returns_whole_value() {
        let v = json!({"a": 1});
        assert_eq!(get_path(&v, ""), Some(&v));
    }

    #[test]
    fn typed_accessors_match_json_types() {
        let v = json!({"s": "text", "i": 42, "f": 1.5, "b": true, "n": null});
        assert_eq!(as_str(&v["s"]), Some("text"));
        assert_eq!(as_i64(&v["i"]), Some(42));
        assert_eq!(as_f64(&v["f"]), Some(1.5));
        assert_eq!(as_bool(&v["b"]), Some(true));
        assert_eq!(as_ts(&v["i"]), Some(42));
        assert_eq!(as_str(&v["i"]), None);
        assert_eq!(as_i64(&v["f"]), None);
        assert_eq!(as_bool(&v["n"]), None);
    }
}

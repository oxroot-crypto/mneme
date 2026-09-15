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

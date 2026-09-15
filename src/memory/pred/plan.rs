//! 合取链的选择性预估与计划器重排。

use super::ast::{CmpOp, Expr};

/// 预估谓词选择性(命中率估计,`0` = 几乎不命中、`1` = 几乎全命中)。
///
/// 纯静态启发式(不查数据),只供计划器对 `And` 合取链排序;返回值不参与
/// 任何语义判定,排序结果只影响短路求值的顺序(FC-QUERY-POST-009)。
fn estimated_selectivity(expr: &Expr) -> f32 {
    match expr {
        Expr::Never => 0.0,
        Expr::Always => 1.0,
        Expr::Cmp {
            op: CmpOp::Eq,
            field,
            ..
        } => {
            if field == "key" || field == "rowid" {
                0.01
            } else {
                0.1
            }
        }
        Expr::Cmp { op: CmpOp::Ne, .. } => 0.9,
        Expr::Cmp { .. } => 0.3,
        Expr::In(_, vals) => (0.05 * vals.len() as f32).min(0.5),
        Expr::IsNull(_) => 0.05,
        Expr::Exists(_) => 0.8,
        Expr::Contains(..) | Expr::StartsWith(..) | Expr::EndsWith(..) | Expr::Glob(..) => 0.5,
        Expr::Not(_) => 0.5,
        Expr::And(parts) => parts.iter().map(estimated_selectivity).product(),
        Expr::Or(parts) => {
            let miss: f32 = parts
                .iter()
                .map(|part| 1.0 - estimated_selectivity(part))
                .product();
            (1.0 - miss).clamp(0.0, 1.0)
        }
    }
}

/// 计划器重排:对 `And` 合取链按预估选择性升序**稳定**排序(嵌套递归)。
///
/// 三值语义下 `And` 可交换可结合,重排只改变短路求值顺序、不改变候选集合
/// (FC-QUERY-POST-009);`Or`/`Not` 与叶子谓词保持原结构。
pub(crate) fn reorder_for_eval(expr: &Expr) -> Expr {
    match expr {
        Expr::And(parts) => {
            let mut parts: Vec<Expr> = parts.iter().map(reorder_for_eval).collect();
            parts.sort_by(|left, right| {
                estimated_selectivity(left).total_cmp(&estimated_selectivity(right))
            });
            Expr::And(parts.into_boxed_slice())
        }
        Expr::Or(parts) => Expr::Or(parts.iter().map(reorder_for_eval).collect()),
        Expr::Not(inner) => Expr::Not(Box::new(reorder_for_eval(inner))),
        other => other.clone(),
    }
}

//! 表达式构建器与运算符组合器。

use super::ast::{CmpOp, Expr, Val};

impl Expr {
    /// 字段组合器入口:`Expr::field("importance").gt(0.5)`。
    ///
    /// # Arguments
    /// * `name` - 字段名;内置保留字段优先于同名 metadata。
    ///
    /// # Returns
    /// 绑定 `name` 的 [`FieldBuilder`],供继续构造比较或集合条件。
    ///
    /// # Examples
    /// ```
    /// use mneme::Expr;
    /// let expr = Expr::field("importance").gt(0.5);
    /// let _ = expr & Expr::field("kind").eq("preference");
    /// ```
    pub fn field(name: &str) -> FieldBuilder {
        FieldBuilder {
            field: name.to_string(),
        }
    }
}

/// 字段组合器(`eq/ne/gt/ge/lt/le/is_in` 返回 [`Expr`])。
#[derive(Debug, Clone)]
pub struct FieldBuilder {
    field: String,
}

impl FieldBuilder {
    /// 构造 `field == val`。
    ///
    /// # Arguments
    /// * `val` - 比较取值;经 `Into<Val>` 转换,类型规则见 `FC-QUERY-POST-001`。
    ///
    /// # Returns
    /// 表示 `field == val` 的过滤表达式。
    pub fn eq(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Eq,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field != val`。
    ///
    /// # Arguments
    /// * `val` - 比较取值;经 `Into<Val>` 转换。
    ///
    /// # Returns
    /// 表示 `field != val` 的过滤表达式。
    pub fn ne(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Ne,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field > val`。
    ///
    /// # Arguments
    /// * `val` - 比较取值;经 `Into<Val>` 转换。
    ///
    /// # Returns
    /// 表示 `field > val` 的过滤表达式。
    pub fn gt(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Gt,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field >= val`。
    ///
    /// # Arguments
    /// * `val` - 比较取值;经 `Into<Val>` 转换。
    ///
    /// # Returns
    /// 表示 `field >= val` 的过滤表达式。
    pub fn ge(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Ge,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field < val`。
    ///
    /// # Arguments
    /// * `val` - 比较取值;经 `Into<Val>` 转换。
    ///
    /// # Returns
    /// 表示 `field < val` 的过滤表达式。
    pub fn lt(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Lt,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field <= val`。
    ///
    /// # Arguments
    /// * `val` - 比较取值;经 `Into<Val>` 转换。
    ///
    /// # Returns
    /// 表示 `field <= val` 的过滤表达式。
    pub fn le(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Le,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造集合判定 `field ∈ {vs}`(`in` 是关键字,故方法名为 `is_in`)。
    ///
    /// # Arguments
    /// * `vs` - 候选值集合;重复值自动去重。
    ///
    /// # Returns
    /// 表示 `field ∈ {vs}` 的过滤表达式;候选值按首次出现顺序去重。
    pub fn is_in(self, vs: impl IntoIterator<Item = Val>) -> Expr {
        let mut vals: Vec<Val> = Vec::new();
        for val in vs {
            if !vals.contains(&val) {
                vals.push(val);
            }
        }
        Expr::In(self.field, vals.into_boxed_slice())
    }
}

impl std::ops::BitAnd for Expr {
    type Output = Expr;

    fn bitand(self, rhs: Expr) -> Expr {
        Expr::And(vec![self, rhs].into_boxed_slice())
    }
}

impl std::ops::BitOr for Expr {
    type Output = Expr;

    fn bitor(self, rhs: Expr) -> Expr {
        Expr::Or(vec![self, rhs].into_boxed_slice())
    }
}

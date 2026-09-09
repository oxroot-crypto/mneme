//! 过滤 AST 与组合器(`pred.rs`)。
//!
//! 与 L4 共用同一套 AST;L1 提供 builder 组合器(`Expr::field("x").gt(0.5)`、
//! `&`/`|` 运算符重载),**字符串 DSL 解析器在 L4**([06 §1])。求值器见
//! [`pred_eval`](crate::memory::pred_eval)。

use std::sync::Arc;

use crate::memory::table::{AccessStat, SlotData};

pub(crate) use crate::memory::pred_eval::matches;
/// 比较运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
}

/// 过滤取值。
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// 布尔值。
    Bool(bool),
    /// 整数。
    Int(i64),
    /// 浮点数。
    Num(f64),
    /// 字符串。
    Str(Arc<str>),
    /// Unix 毫秒时间戳(只与 `Ts` 比较)。
    Ts(i64),
}

impl From<bool> for Val {
    fn from(value: bool) -> Self {
        Val::Bool(value)
    }
}

impl From<i64> for Val {
    fn from(value: i64) -> Self {
        Val::Int(value)
    }
}

impl From<i32> for Val {
    fn from(value: i32) -> Self {
        Val::Int(i64::from(value))
    }
}

impl From<f64> for Val {
    fn from(value: f64) -> Self {
        Val::Num(value)
    }
}

impl From<f32> for Val {
    fn from(value: f32) -> Self {
        Val::Num(f64::from(value))
    }
}

impl From<&str> for Val {
    fn from(value: &str) -> Self {
        Val::Str(Arc::from(value))
    }
}

impl From<String> for Val {
    fn from(value: String) -> Self {
        Val::Str(Arc::from(value))
    }
}

impl From<Arc<str>> for Val {
    fn from(value: Arc<str>) -> Self {
        Val::Str(value)
    }
}

/// 过滤表达式 AST。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// 字段与取值比较。
    Cmp {
        /// 比较运算符。
        op: CmpOp,
        /// 字段名(保留字段优先于同名 metadata)。
        field: String,
        /// 比较取值。
        val: Val,
    },
    /// 字段取值属于给定集合。
    In(String, Box<[Val]>),
    /// 数组含元素 / 字符串含子串。
    Contains(String, Val),
    /// 字符串以给定前缀开头。
    StartsWith(String, Arc<str>),
    /// 字符串以给定后缀结尾。
    EndsWith(String, Arc<str>),
    /// 通配符匹配:`*` 任意串、`?` 单字符。
    Glob(String, Arc<str>),
    /// 字段存在(可为 null)。
    Exists(String),
    /// 字段存在且为 JSON null。
    IsNull(String),
    /// 逻辑与(短路)。
    And(Box<[Expr]>),
    /// 逻辑或(短路)。
    Or(Box<[Expr]>),
    /// 逻辑非。
    Not(Box<Expr>),
    /// 恒真。
    Always,
    /// 恒假。
    Never,
}

impl Expr {
    /// 字段组合器入口:`Expr::field("importance").gt(0.5)`。
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
    pub fn eq(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Eq,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field != val`。
    pub fn ne(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Ne,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field > val`。
    pub fn gt(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Gt,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field >= val`。
    pub fn ge(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Ge,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field < val`。
    pub fn lt(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Lt,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造 `field <= val`。
    pub fn le(self, val: impl Into<Val>) -> Expr {
        Expr::Cmp {
            op: CmpOp::Le,
            field: self.field,
            val: val.into(),
        }
    }

    /// 构造集合判定 `field ∈ {vs}`(`in` 是关键字,故方法名为 `is_in`)。
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

/// 求值上下文:一条记录 + 其访问统计。
pub(crate) struct EvalCtx<'a> {
    /// 物理版本数据。
    pub(crate) slot: &'a SlotData,
    /// 访问统计(缺失时 `last_access` 为 Unknown、`access_count` 为 0)。
    pub(crate) access: Option<AccessStat>,
}

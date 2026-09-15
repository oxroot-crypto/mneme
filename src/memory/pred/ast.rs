//! 过滤 AST 的类型定义与求值上下文。

use std::sync::Arc;

use crate::memory::table::{AccessStat, SlotData};

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
    /// 表达式是否引用访问统计字段(`access_count` / `last_access`)。
    ///
    /// 扫描热路径据此按需填充求值上下文的访问统计:未引用时传 `None`,
    /// 省去每行一次访问统计查找;求值结果不变(两个字段只在引用时读取)。
    pub(crate) fn uses_access(&self) -> bool {
        match self {
            Expr::Always | Expr::Never => false,
            Expr::And(parts) | Expr::Or(parts) => parts.iter().any(Expr::uses_access),
            Expr::Not(inner) => inner.uses_access(),
            Expr::Cmp { field, .. }
            | Expr::In(field, _)
            | Expr::Contains(field, _)
            | Expr::StartsWith(field, _)
            | Expr::EndsWith(field, _)
            | Expr::Glob(field, _)
            | Expr::Exists(field)
            | Expr::IsNull(field) => field == "last_access" || field == "access_count",
        }
    }
}

/// 求值上下文:一条记录 + 其访问统计。
pub(crate) struct EvalCtx<'a> {
    /// 物理版本数据。
    pub(crate) slot: &'a SlotData,
    /// 访问统计(缺失时 `last_access` 为 Unknown、`access_count` 为 0)。
    pub(crate) access: Option<AccessStat>,
}

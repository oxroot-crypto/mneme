use std::sync::Arc;

use super::glob::glob_match;

/// 字符串模式匹配算子(`StartsWith`/`EndsWith`/`Glob` 三个分支同构,合并求值)。
pub(super) enum StringOp {
    StartsWith,
    EndsWith,
    Glob,
}

impl StringOp {
    pub(super) fn matches(self, text: &str, needle: &Arc<str>) -> bool {
        match self {
            StringOp::StartsWith => text.starts_with(&**needle),
            StringOp::EndsWith => text.ends_with(&**needle),
            StringOp::Glob => glob_match(needle, text),
        }
    }
}

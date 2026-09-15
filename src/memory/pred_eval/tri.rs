use std::cmp::Ordering;

use crate::memory::pred::{CmpOp, Val};

/// 三值逻辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    pub(crate) fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }

    pub(super) fn and(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::True, Tri::True) => Tri::True,
            _ => Tri::Unknown,
        }
    }

    pub(super) fn or(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::False, Tri::False) => Tri::False,
            _ => Tri::Unknown,
        }
    }
}

pub(super) fn cmp_vals(op: CmpOp, left: &Val, right: &Val) -> Option<bool> {
    match (left, right) {
        (Val::Int(a), Val::Int(b)) => Some(order(op, a.cmp(b))),
        (Val::Ts(a), Val::Ts(b)) => Some(order(op, a.cmp(b))),
        (Val::Str(a), Val::Str(b)) => Some(order(op, a.cmp(b))),
        (Val::Bool(a), Val::Bool(b)) => Some(order(op, a.cmp(b))),
        (Val::Num(a), Val::Num(b)) => partial_order(op, a.partial_cmp(b)),
        (Val::Int(a), Val::Num(b)) => partial_order(op, (*a as f64).partial_cmp(b)),
        (Val::Num(a), Val::Int(b)) => partial_order(op, a.partial_cmp(&(*b as f64))),
        _ => None,
    }
}

pub(super) fn order(op: CmpOp, ordering: std::cmp::Ordering) -> bool {
    match op {
        CmpOp::Eq => ordering == Ordering::Equal,
        CmpOp::Ne => ordering != Ordering::Equal,
        CmpOp::Gt => ordering == Ordering::Greater,
        CmpOp::Ge => ordering != Ordering::Less,
        CmpOp::Lt => ordering == Ordering::Less,
        CmpOp::Le => ordering != Ordering::Greater,
    }
}

fn partial_order(op: CmpOp, ordering: Option<std::cmp::Ordering>) -> Option<bool> {
    ordering.map(|ordering| order(op, ordering))
}

pub(super) fn tri_bool(value: bool) -> Tri {
    if value { Tri::True } else { Tri::False }
}

pub(super) fn tri_opt(value: Option<bool>) -> Tri {
    match value {
        Some(true) => Tri::True,
        Some(false) => Tri::False,
        None => Tri::Unknown,
    }
}

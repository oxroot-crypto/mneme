//! fuzz 专用解析入口(feature `fuzzing`;设计 14 §5)。
//!
//! 仅供 `fuzz/` 目标调用:把内部编解码器包装成「任意字节输入不 panic、失败返回
//! 结构化错误」的纯函数(`I7`/`I18`)。本模块不参与运行时路径,也不构成稳定 API;
//! 运行 fuzz 需要 nightly 工具链与 `cargo-fuzz`,详见 `fuzz/README.md`。

use crate::core::error::Result;

/// 解析并校验 `vsec` 向量段字节。
///
/// # Errors
/// 魔数/版本/头部 CRC/布局/qvec 畸形或 payload CRC 不符时返回结构化错误。
pub fn parse_vsec(bytes: &[u8]) -> Result<()> {
    let mut view = crate::persist::vsec::parse(bytes)?;
    view.verify_payload()
}

/// 解析并校验 `msec` 元数据段字节。
///
/// # Errors
/// 魔数/版本/区偏移/CRC 不符时返回结构化错误。
pub fn parse_msec(bytes: &[u8]) -> Result<()> {
    let mut view = crate::persist::msec::parse(bytes)?;
    view.verify_payload()
}

/// 解析 `hidx` 索引图字节。
///
/// # Errors
/// 魔数/版本/CRC/图布局/度数越界时返回结构化错误。
pub fn decode_hidx(bytes: &[u8]) -> Result<()> {
    crate::index::hidx::decode(bytes).map(|_decoded| ())
}

/// 回放任意 WAL 字节(以全新内存状态为基底);失败被吞掉,仅供不 panic 断言。
pub fn replay_wal(bytes: &[u8]) {
    let Ok(db) = crate::memory::Mneme::in_memory(2) else {
        return;
    };
    let mut ws = db.table.write();
    // reason: fuzz 目标只断言"任意输入不 panic";回放结果与错误由返回值丢弃。
    let _ = crate::persist::recover::replay_wal(&mut ws, bytes, 0).ok();
}

/// 解析过滤 DSL 字节;非 UTF-8 输入直接拒绝。
///
/// # Errors
/// 非 UTF-8 或 DSL 语法错误时返回结构化错误。
pub fn parse_dsl(bytes: &[u8]) -> Result<()> {
    let text =
        std::str::from_utf8(bytes).map_err(|_error| crate::core::error::MnemeError::Config {
            reason: "过滤 DSL 输入非 UTF-8",
        })?;
    crate::memory::pred::Expr::from_str(text).map(|_expr| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fuzz 入口的冒烟对照:极值与任意字节下都不 panic,失败即结构化错误。
    #[test]
    fn fuzz_entry_points_survive_arbitrary_bytes() {
        let samples: [&[u8]; 4] = [b"", b"\x00", b"VSC1\xff\xff", &[0xAB; 64]];
        for sample in samples {
            let _ = parse_vsec(sample);
            let _ = parse_msec(sample);
            let _ = decode_hidx(sample);
            let _ = parse_dsl(sample);
            replay_wal(sample);
        }
    }
}

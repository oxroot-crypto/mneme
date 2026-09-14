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
    crate::index::hidx::verify(bytes)
}

/// 解析 `wal` 文件头字节(I18 版本门控的一环)。
///
/// # Errors
/// 魔数/版本/头部长度/CRC 不符时返回结构化错误。
pub fn parse_wal_header(bytes: &[u8]) -> Result<()> {
    crate::persist::wal::parse_file_header(bytes).map(|_header| ())
}

/// 版本注入断言(I18,设计 14 §5):对可识别的段/WAL 文件头,
/// **改写 `format_version` 任意不同值必须返回 `UnsupportedVersion`**(绝不继续解析),
/// **改写魔数必须返回 `Corrupted`**;不可识别的输入不做任何事。
///
/// fuzz 目标在调用各解析入口之外再调用本函数,把"版本门控"从定向单测扩到
/// libFuzzer 的结构化输入上。
///
/// # Panics
/// 版本/魔数门控失效时 panic(fuzz 目标以此作为断言失败信号)。
pub fn version_injection(bytes: &[u8]) {
    if bytes.len() < 6 {
        return;
    }
    let parse: fn(&[u8]) -> Result<()> = if bytes[0..4] == crate::persist::vsec::MAGIC {
        parse_vsec
    } else if bytes[0..4] == crate::persist::msec::MAGIC {
        parse_msec
    } else if bytes[0..4] == crate::index::hidx::MAGIC {
        decode_hidx
    } else if bytes[0..4] == crate::persist::wal::MAGIC {
        parse_wal_header
    } else {
        return;
    };

    let current = u16::from_le_bytes([bytes[4], bytes[5]]);
    let mutated = current.wrapping_add(1);
    if mutated != current {
        let mut versioned = bytes.to_vec();
        versioned[4..6].copy_from_slice(&mutated.to_le_bytes());
        assert!(
            matches!(
                parse(&versioned),
                Err(crate::core::error::MnemeError::UnsupportedVersion { .. })
            ),
            "版本注入必须返回 UnsupportedVersion(I18)"
        );
    }

    let mut bad_magic = bytes.to_vec();
    bad_magic[0] ^= 0xFF;
    assert!(
        matches!(
            parse(&bad_magic),
            Err(crate::core::error::MnemeError::Corrupted { .. })
        ),
        "魔数改写必须返回 Corrupted(I18)"
    );
}

/// 回放任意 WAL 字节(以全新内存状态为基底);失败被吞掉,仅供不 panic 断言。
pub fn replay_wal(bytes: &[u8]) {
    let Ok(db) = crate::memory::Mneme::in_memory(2) else {
        return;
    };
    let mut ws = db.table.write();
    // reason: fuzz 目标只断言"任意输入不 panic";回放结果与错误由返回值丢弃。
    let _ = crate::persist::recover::replay_wal(&mut ws, bytes, 0, None).ok();
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
            version_injection(sample);
        }
    }

    /// 版本注入(I18):合法空 vsec 上改写版本 → `UnsupportedVersion`,
    /// 改写魔数 → `Corrupted`;不在已知魔数上的输入不做断言。
    #[test]
    fn version_injection_holds_for_known_magic() {
        use crate::core::metric::Metric;
        use crate::core::options::VectorFormat;
        use crate::persist::vsec::{VsecInput, encode};
        let bytes = encode(&VsecInput {
            dimension: 2,
            metric: Metric::Cosine,
            created_unix_ms: 0,
            vectors: &[],
            norms: &[],
            dead: &[],
            quant: VectorFormat::F32,
            quant_params: &[],
            quant_codes: &[],
        })
        .expect("encode");
        version_injection(&bytes);
        // 短于 6 字节的输入不越界。
        version_injection(b"VSC1");
    }
}

//! 文本/元数据压缩(L11,设计 11 §3)。
//!
//! 压缩作用于 msec 记录体的 `text`/`meta`/`provenance` 字段:每字段独立压缩,
//! 压缩流自描述 codec id,低于阈值(压缩后无收益)时自动存原文;`Compression::None`
//! 时字节布局与未压缩定义一致。内置 LZ4 风格 codec 零依赖(feature `compress`);
//! `Zstd` 需 feature `compress-zstd`。

#[cfg(feature = "compress")]
mod lz4;

use crate::core::error::{MnemeError, Result};
use crate::core::options::Compression;

#[cfg(feature = "compress")]
pub(crate) use lz4::Lz4Codec;

/// 压缩 codec 标识(落盘在压缩字段首字节,自描述)。
pub(crate) const CODEC_ID_LZ4: u8 = 1;
/// Zstd codec 标识。
pub(crate) const CODEC_ID_ZSTD: u8 = 2;

/// 单字段压缩/解压抽象。
pub(crate) trait Codec: Send + Sync {
    /// 压缩 `src`。
    fn compress(&self, src: &[u8]) -> Vec<u8>;

    /// 解压 `src`;`expected_len` 为原始长度(用于容量上界与完整性校验)。
    ///
    /// # Errors
    /// 流畸形、越界或长度不等于 `expected_len` 时返回 [`MnemeError::Corrupted`]。
    fn decompress(&self, src: &[u8], expected_len: usize) -> Result<Vec<u8>>;
}

/// 由配置返回写入用 codec;`None` 返回 `None`。
///
/// # Errors
/// 对应 feature 未开启时返回 [`MnemeError::Unsupported`](绝不静默降级)。
pub(crate) fn codec_for(compression: Compression) -> Result<Option<&'static dyn Codec>> {
    match compression {
        Compression::None => Ok(None),
        Compression::Lz4 => {
            #[cfg(feature = "compress")]
            {
                Ok(Some(&Lz4Codec))
            }
            #[cfg(not(feature = "compress"))]
            {
                Err(MnemeError::Unsupported {
                    feature: "compress",
                })
            }
        }
        Compression::Zstd => {
            #[cfg(feature = "compress-zstd")]
            {
                Ok(Some(&ZstdCodec))
            }
            #[cfg(not(feature = "compress-zstd"))]
            {
                Err(MnemeError::Unsupported {
                    feature: "compress-zstd",
                })
            }
        }
    }
}

/// 由落盘 codec id 返回解压用 codec。
///
/// # Errors
/// 未知 id 或对应 feature 未开启时返回结构化错误(绝不按原文静默返回)。
pub(crate) fn codec_by_id(id: u8) -> Result<&'static dyn Codec> {
    match id {
        CODEC_ID_LZ4 => {
            #[cfg(feature = "compress")]
            {
                Ok(&Lz4Codec)
            }
            #[cfg(not(feature = "compress"))]
            {
                Err(MnemeError::Unsupported {
                    feature: "compress",
                })
            }
        }
        CODEC_ID_ZSTD => {
            #[cfg(feature = "compress-zstd")]
            {
                Ok(&ZstdCodec)
            }
            #[cfg(not(feature = "compress-zstd"))]
            {
                Err(MnemeError::Unsupported {
                    feature: "compress-zstd",
                })
            }
        }
        _ => Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("compress: 未知 codec id {id}"),
        }),
    }
}

/// feature `compress-zstd` 的 Zstd codec。
#[cfg(feature = "compress-zstd")]
pub(crate) struct ZstdCodec;

#[cfg(feature = "compress-zstd")]
impl Codec for ZstdCodec {
    fn compress(&self, src: &[u8]) -> Vec<u8> {
        // reason: 级别 3 为 zstd 默认档;压缩失败(不可能,输入为内存字节)时
        // 返回空向量让调用方按"无收益"回退存原文,绝不 panic。
        zstd::bulk::compress(src, 3).unwrap_or_default()
    }

    fn decompress(&self, src: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let out =
            zstd::bulk::decompress(src, expected_len).map_err(|error| MnemeError::Corrupted {
                segment: None,
                reason: format!("compress: zstd 解压失败:{error}"),
            })?;
        if out.len() != expected_len {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: format!(
                    "compress: zstd 解压长度 {} 与期望 {expected_len} 不符",
                    out.len()
                ),
            });
        }
        Ok(out)
    }
}

/// 单字段压缩结果的容量上界(防解压炸弹;与记录体字段限额同量级)。
pub(crate) const MAX_FIELD_UNCOMPRESSED: usize = 16 * 1024 * 1024;

/// 把字段原文编码为落盘字节:`[u8 codec_id][u32 uncompressed_len][compressed]`。
///
/// 压缩无收益(压缩后不短于原文)或未配置 codec 时返回 `None`(调用方存原文)。
pub(crate) fn encode_field(codec: Option<&dyn Codec>, codec_id: u8, raw: &[u8]) -> Option<Vec<u8>> {
    let codec = codec?;
    let compressed = codec.compress(raw);
    if compressed.len() >= raw.len() {
        return None;
    }
    let mut blob = Vec::with_capacity(5 + compressed.len());
    blob.push(codec_id);
    blob.extend_from_slice(&u32::try_from(raw.len()).ok()?.to_le_bytes());
    blob.extend_from_slice(&compressed);
    Some(blob)
}

/// 解析压缩字段 blob(见 [`encode_field`]),返回原文。
///
/// # Errors
/// 长度前缀越界、codec 未知/未开启或解压失败时返回结构化错误。
pub(crate) fn decode_field(blob: &[u8]) -> Result<Vec<u8>> {
    let (codec_id, rest) = blob.split_first().ok_or_else(|| MnemeError::Corrupted {
        segment: None,
        reason: "compress: 压缩字段为空".to_string(),
    })?;
    if rest.len() < 4 {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "compress: 压缩字段长度前缀缺失".to_string(),
        });
    }
    let uncompressed_len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    if uncompressed_len > MAX_FIELD_UNCOMPRESSED {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("compress: 声明原始长度 {uncompressed_len} 超上限"),
        });
    }
    codec_by_id(*codec_id)?.decompress(&rest[4..], uncompressed_len)
}

#[cfg(all(test, feature = "compress"))]
mod tests {
    use super::*;

    /// FC-SEC-POST-002:内置 LZ4 codec 往返一致(短/长/重复/二进制输入)。
    #[test]
    fn lz4_roundtrip_various_inputs() {
        let cases: [Vec<u8>; 5] = [
            Vec::new(),
            b"a".to_vec(),
            b"hello hello hello hello hello".to_vec(),
            vec![0_u8; 10_000],
            (0..=255_u8).cycle().take(65_536).collect(),
        ];
        for case in cases {
            let compressed = Lz4Codec.compress(&case);
            let restored = Lz4Codec.decompress(&compressed, case.len()).expect("解压");
            assert_eq!(restored, case, "往返必须逐字节一致");
        }
    }

    /// FC-SEC-POST-002:压缩流畸形/长度不符 → `Corrupted`,绝不 panic。
    #[test]
    fn lz4_decompress_rejects_malformed_or_length_mismatch() {
        let compressed = Lz4Codec.compress(b"hello hello hello hello");
        assert!(matches!(
            Lz4Codec.decompress(&compressed, 999),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            Lz4Codec.decompress(&[0xFF, 0x00], 8),
            Err(MnemeError::Corrupted { .. })
        ));
        // 偏移越界(指向已输出之前不存在的位置)。
        assert!(matches!(
            Lz4Codec.decompress(&[0x41, b'x', 0x00, 0x00], 8),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// FC-SEC-POST-002:deflate 后无收益的字段回退原文(`encode_field` 返回 `None`)。
    #[test]
    fn incompressible_field_falls_back_to_raw() {
        let raw: Vec<u8> = (0..=255_u8).rev().collect();
        assert!(
            encode_field(Some(&Lz4Codec), CODEC_ID_LZ4, &raw).is_none(),
            "短且高熵的字段压缩无收益,必须存原文"
        );
    }
}

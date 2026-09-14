//! 惰性字节源与惰性向量/码流(`memory/lazy.rs`)。
//!
//! 段句柄惰性驻留的 L1 原语:此处只定义"字节从哪来"的抽象(L2 的
//! [`ByteFile`](crate::persist::source) 用 mmap 或整文件读入实现),以及
//! "首次访问才解码、此后缓存"的向量/定长行区载体。恢复路径不再逐行解
//! f32 或拷贝量化码,读语义与整段载入逐位一致(FC-PERSIST-INV-021)。

use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

/// 段内只读字节来源(L1 定义、L2 实现)。
///
/// 实现必须保证:一旦成功构造,底层映射或缓冲在句柄存活期内一直有效
/// (`slice_at` 不会因并发或文件删除而失效);段文件 write-once,句柄持有
/// 期间内容不变(FC-PERSIST-INV-021)。
pub(crate) trait ByteSource: Send + Sync + fmt::Debug {
    /// 返回文件内 `[offset, offset + len)` 的字节切片;越界返回 `None`。
    fn slice_at(&self, offset: usize, len: usize) -> Option<&[u8]>;
    /// 文件总字节数。
    fn byte_len(&self) -> usize;
}

/// 基于内存缓冲的字节源(测试与内存态构造用)。
#[derive(Debug)]
pub(crate) struct OwnedBytes {
    bytes: Box<[u8]>,
}

impl OwnedBytes {
    /// 以一段自有字节构造字节源。
    pub(crate) fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            bytes: bytes.into_boxed_slice(),
        })
    }
}

impl ByteSource for OwnedBytes {
    fn slice_at(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        self.bytes.get(offset..end)
    }

    fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

/// 文件内一段连续字节的逻辑视图(构造期完成区间校验)。
///
/// hidx 图与量化码区都以 `ByteSpan` 形式随索引长期持有,按需切片解码。
#[derive(Clone, Debug)]
pub(crate) struct ByteSpan {
    source: Arc<dyn ByteSource>,
    offset: usize,
    len: usize,
}

impl ByteSpan {
    /// 在 `source` 的 `[offset, offset + len)` 上建立视图;越界返回 `None`。
    pub(crate) fn new(source: Arc<dyn ByteSource>, offset: usize, len: usize) -> Option<Self> {
        let end = offset.checked_add(len)?;
        (end <= source.byte_len()).then_some(Self {
            source,
            offset,
            len,
        })
    }

    /// 把整段包成视图(偏移 0),隐式要求 `len == source.byte_len()`。
    pub(crate) fn whole(source: Arc<dyn ByteSource>) -> Option<Self> {
        let len = source.byte_len();
        Self::new(source, 0, len)
    }

    /// 视图内相对 `[relative, relative + len)` 的切片;越界返回 `None`。
    pub(crate) fn slice(&self, relative: usize, len: usize) -> Option<&[u8]> {
        let end = relative.checked_add(len)?;
        if end > self.len {
            return None;
        }
        self.source.slice_at(self.offset + relative, len)
    }

    /// 整段切片。
    // reason: 生产路径按相对偏移取切片;整段切片供测试与诊断(惰性图/编码回读)。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn slice_all(&self) -> Option<&[u8]> {
        self.source.slice_at(self.offset, self.len)
    }

    /// 视图字节长度。
    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

/// 段内一个向量的惰性存储:首次 [`Deref`] 时按小端解码并缓存。
///
/// `source` 在句柄存活期内始终有效(见 [`ByteSource`]),`offset`/`dimension`
/// 在构造期验证过区间,故解码路径不存在越界分支。
#[derive(Debug)]
pub(crate) struct LazyVector {
    source: Arc<dyn ByteSource>,
    offset: usize,
    dimension: usize,
    decoded: OnceLock<Box<[f32]>>,
}

impl LazyVector {
    /// 建立惰性向量视图;区间越界或维度为 0 时返回 `None`(调用方回退自有数据)。
    pub(crate) fn new(
        source: Arc<dyn ByteSource>,
        offset: usize,
        dimension: usize,
    ) -> Option<Self> {
        if dimension == 0 {
            return None;
        }
        let bytes = dimension.checked_mul(size_of::<f32>())?;
        let end = offset.checked_add(bytes)?;
        if end > source.byte_len() {
            return None;
        }
        Some(Self {
            source,
            offset,
            dimension,
            decoded: OnceLock::new(),
        })
    }

    /// 解码(首次)或取缓存向量。
    fn get(&self) -> &[f32] {
        self.decoded.get_or_init(|| {
            // 构造期已验证 `[offset, offset + dimension*4)` 在文件内;段句柄
            // 存活期内映射/缓冲有效且文件 write-once,故取切片不会失败
            // (FC-PERSIST-INV-021)。
            let bytes = self
                .source
                .slice_at(self.offset, self.dimension * size_of::<f32>())
                .expect("LazyVector 构造期已验证区间(FC-PERSIST-INV-021)");
            let mut vector = Vec::with_capacity(self.dimension);
            for chunk in bytes.chunks_exact(size_of::<f32>()) {
                vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            vector.into_boxed_slice()
        })
    }
}

/// 记录向量的存储形态:内存自有(新写入/未落盘)或段句柄惰性(已落盘)。
///
/// 统一经 [`Deref`] 暴露 `&[f32]`,读路径无需区分两种形态;`Arc` 共享使
/// 快照与索引节点复用同一缓存。`Debug` 不打印向量内容(避免日志膨胀)。
#[derive(Debug)]
pub(crate) enum VectorStorage {
    /// 自有堆数据(写入路径与测试)。
    Owned(Arc<[f32]>),
    /// 段文件内的惰性视图。
    Lazy(LazyVector),
}

impl VectorStorage {
    /// 包装自有向量。
    pub(crate) fn owned(vector: Arc<[f32]>) -> Arc<Self> {
        Arc::new(Self::Owned(vector))
    }

    /// 建立段内惰性向量;区间越界返回 `None`。
    pub(crate) fn lazy(
        source: Arc<dyn ByteSource>,
        offset: usize,
        dimension: usize,
    ) -> Option<Arc<Self>> {
        LazyVector::new(source, offset, dimension).map(|lazy| Arc::new(Self::Lazy(lazy)))
    }
}

impl Deref for VectorStorage {
    type Target = [f32];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owned(vector) => vector,
            Self::Lazy(lazy) => lazy.get(),
        }
    }
}

/// 定长行距的惰性行区(量化码流;行区在段文件内连续)。
///
/// 构造期校验 `count * stride <= span.len()`;`row` 只做乘法与切片,不分配。
#[derive(Clone, Debug)]
pub(crate) struct LazyRows {
    span: ByteSpan,
    stride: usize,
    count: usize,
}

impl LazyRows {
    /// 在 `span` 内建立 `count` 行、每行 `stride` 字节的行区。
    pub(crate) fn new(span: ByteSpan, stride: usize, count: usize) -> Option<Self> {
        if stride == 0 {
            return None;
        }
        let total = count.checked_mul(stride)?;
        (total <= span.len()).then_some(Self {
            span,
            stride,
            count,
        })
    }

    /// 以一段自有字节建立行区(flush 生成副本时用;语义与 [`LazyRows::new`] 同)。
    pub(crate) fn from_owned(bytes: Vec<u8>, stride: usize, count: usize) -> Option<Self> {
        Self::new(ByteSpan::whole(OwnedBytes::new(bytes))?, stride, count)
    }

    /// 行数。
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// 单行字节数。
    pub(crate) fn stride(&self) -> usize {
        self.stride
    }

    /// 第 `index` 行码流;越界返回 `None`。
    pub(crate) fn row(&self, index: usize) -> Option<&[u8]> {
        if index >= self.count {
            return None;
        }
        self.span.slice(index * self.stride, self.stride)
    }

    /// 逐行迭代码流(flush 重写 vsec 时按行借用)。
    pub(crate) fn iter(&self) -> impl Iterator<Item = &[u8]> + '_ {
        (0..self.count).filter_map(|index| self.row(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-PERSIST-INV-021:惰性向量首次解码后与自有向量逐位一致,且缓存复用
    /// (同一指针),不重复解码。
    #[test]
    fn lazy_vector_decodes_once_and_matches_owned() {
        let values: [f32; 4] = [1.5, -2.25, 0.0, 3.0];
        let mut bytes = vec![0xAA_u8; 8];
        bytes.extend(values.iter().flat_map(|value| value.to_le_bytes()));
        let source = OwnedBytes::new(bytes);

        let lazy =
            VectorStorage::lazy(Arc::clone(&source) as Arc<dyn ByteSource>, 8, 4).expect("区间内");
        let owned = VectorStorage::owned(Arc::from(values));
        assert_eq!(&lazy[..], &owned[..]);
        let first = lazy[..].as_ptr();
        let second = lazy[..].as_ptr();
        assert_eq!(first, second, "二次访问必须命中缓存");
    }

    /// FC-PERSIST-INV-021:越界区间在构造期即被拒绝,不建立惰性视图。
    #[test]
    fn lazy_vector_rejects_out_of_range() {
        let source = OwnedBytes::new(vec![0_u8; 8]);
        assert!(VectorStorage::lazy(Arc::clone(&source) as Arc<dyn ByteSource>, 8, 1).is_none());
        assert!(VectorStorage::lazy(source as Arc<dyn ByteSource>, 0, 0).is_none());
    }

    /// `ByteSpan` 相对切片与总长边界。
    #[test]
    fn byte_span_slices_with_offset() {
        let source = OwnedBytes::new(vec![0, 1, 2, 3, 4, 5]);
        let span = ByteSpan::new(source, 2, 3).expect("区间内");
        assert_eq!(span.len(), 3);
        assert_eq!(span.slice(1, 2), Some(&[3_u8, 4][..]));
        assert_eq!(span.slice(2, 2), None, "相对区间越界必须拒绝");
        assert_eq!(span.slice_all(), Some(&[2_u8, 3, 4][..]));
    }

    /// `LazyRows` 行切片与边界。
    #[test]
    fn lazy_rows_slice_rows_by_stride() {
        let source = OwnedBytes::new(vec![0, 1, 2, 3, 4, 5]);
        let span = ByteSpan::whole(source).expect("整段");
        let rows = LazyRows::new(span, 2, 3).expect("容量足够");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.stride(), 2);
        assert_eq!(rows.row(1), Some(&[2_u8, 3][..]));
        assert_eq!(rows.row(3), None);
        assert!(
            LazyRows::new(ByteSpan::new(OwnedBytes::new(vec![0]), 0, 1).unwrap(), 2, 1).is_none()
        );
    }
}

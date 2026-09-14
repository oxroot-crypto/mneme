//! 分块共享向量(`chunked.rs`)。
//!
//! L0 纯数据结构:无 I/O、无锁。固定块大小,块经 `Arc` 共享:克隆只复制块
//! 句柄(每块一次引用计数),追加只对末块做写时复制(COW),`get_mut` 同样在
//! 块级 COW——绝不把修改泄漏给仍持有旧快照的读者。
//!
//! 用于 L1 写状态的 `slots`/`slot_segment` 等追加型容器:把「每个写事务全量
//! 深拷 $O(N)$」降为「只拷末块 $O(\text{CHUNK})$」。读接口与平铺 `Vec<T>`
//! 语义全等(FC-CORE-POST-009)。

use std::ops::{Index, IndexMut};
use std::sync::Arc;

/// 单块元素数(固定;末块可能不足)。
pub(crate) const CHUNK: usize = 1024;

/// 分块共享的追加型向量。
pub(crate) struct ChunkedVec<T> {
    /// 块列表;除末块外每块恰好 [`CHUNK`] 个元素。
    chunks: Vec<Arc<Vec<T>>>,
    /// 元素总数。
    len: usize,
}

impl<T> ChunkedVec<T> {
    /// 构造空向量。
    pub(crate) fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    /// 元素总数(生产路径经迭代访问;供测试与内部断言)。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// 按下标读取;越界返回 `None`。
    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        // reason: index < len 保证块与块内下标均在界内;越界仅在释放构建的上层。
        Some(&self.chunks[index / CHUNK][index % CHUNK])
    }

    /// 按下标取可变引用;越界返回 `None`。
    ///
    /// 块被任何快照共享时先做块级 COW(仅克隆该块),语义与
    /// `Arc::make_mut(&mut Vec<T>)[index]` 全等。
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut T>
    where
        T: Clone,
    {
        if index >= self.len {
            return None;
        }
        let chunk = self.chunks.get_mut(index / CHUNK)?;
        Arc::make_mut(chunk).get_mut(index % CHUNK)
    }

    /// 顺序迭代表中元素的只读引用。
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }

    /// 在末尾追加一个元素;必要时新开一块。
    ///
    /// 末块被快照共享时先做块级 COW(仅克隆该块,至多 [`CHUNK`] 个元素)。
    pub(crate) fn push(&mut self, value: T)
    where
        T: Clone,
    {
        let offset = self.len % CHUNK;
        if offset == 0 {
            let mut chunk = Vec::with_capacity(CHUNK);
            chunk.push(value);
            self.chunks.push(Arc::new(chunk));
        } else {
            // reason: offset > 0 隐含已至少有一个块(未取模前长度非零)。
            let chunk = self.chunks.last_mut().expect("offset > 0 时必有末块");
            Arc::make_mut(chunk).push(value);
        }
        self.len += 1;
    }

    /// 预留至少 `additional` 个追加元素的块槽位(只预留外层块表,不预分配块内)。
    pub(crate) fn reserve(&mut self, additional: usize) {
        let needed = (self.len + additional).div_ceil(CHUNK);
        self.chunks
            .reserve(needed.saturating_sub(self.chunks.len()));
    }
}

impl<T> Clone for ChunkedVec<T> {
    /// 克隆块句柄(每块一次引用计数),不复制元素。
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            len: self.len,
        }
    }
}

impl<T> Default for ChunkedVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Index<usize> for ChunkedVec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        // reason: 与 `Vec<T>` 下标语义一致——越界即 panic;界内直取块内元素。
        &self.chunks[index / CHUNK][index % CHUNK]
    }
}

impl<T: Clone> IndexMut<usize> for ChunkedVec<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        // reason: 与 `Vec<T>` 下标语义一致——越界即 panic。
        self.get_mut(index).expect("ChunkedVec 下标越界")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-CORE-POST-009:任意长度(含 0、块边界、跨多块)下与 `Vec` 全等。
    #[test]
    fn matches_vec_semantics_across_chunk_boundaries() {
        for count in [0_usize, 1, CHUNK - 1, CHUNK, CHUNK + 1, CHUNK * 3 + 7] {
            let mut chunked = ChunkedVec::new();
            let mut flat = Vec::new();
            for index in 0..count {
                chunked.push(index);
                flat.push(index);
            }
            assert_eq!(chunked.len(), count);
            assert_eq!(chunked.len() == 0, count == 0);
            assert_eq!(chunked.iter().copied().collect::<Vec<_>>(), flat);
            for index in 0..count {
                assert_eq!(chunked.get(index), flat.get(index));
                assert_eq!(chunked[index], flat[index]);
            }
            assert_eq!(chunked.get(count), None);
            assert_eq!(chunked.get_mut(count), None);
        }
    }

    /// FC-CORE-POST-009:克隆只共享块,末尾追加不泄漏给克隆源。
    #[test]
    fn clone_shares_chunks_and_push_is_copy_on_write() {
        let mut original = ChunkedVec::new();
        for index in 0..CHUNK + 5 {
            original.push(index);
        }
        let snapshot = original.clone();
        original.push(9_999);
        assert_eq!(original.len(), CHUNK + 6);
        assert_eq!(snapshot.len(), CHUNK + 5, "快照长度不得被追加改变");
        assert_eq!(snapshot.get(CHUNK + 4), Some(&(CHUNK + 4)));
        assert_eq!(snapshot.get(CHUNK + 5), None);
        // 已封满的块完全共享:克隆后继续读旧块仍见原值。
        assert_eq!(snapshot[0], 0);
        assert_eq!(original[0], 0);
    }

    /// FC-CORE-POST-009:`get_mut` 块级 COW,修改不泄漏给已克隆视图。
    #[test]
    fn get_mut_is_copy_on_write_per_chunk() {
        let mut original = ChunkedVec::new();
        for index in 0..CHUNK + 3 {
            original.push(index);
        }
        let snapshot = original.clone();
        *original.get_mut(CHUNK + 1).expect("界内") = 777;
        assert_eq!(original[CHUNK + 1], 777);
        assert_eq!(snapshot[CHUNK + 1], CHUNK + 1, "快照不得被 get_mut 修改");
        // 未触碰的块仍共享:读值不变。
        assert_eq!(snapshot[CHUNK], CHUNK);
        assert_eq!(original[CHUNK], CHUNK);
    }

    /// FC-CORE-POST-009:`reserve` 只影响块表容量,不改变语义。
    #[test]
    fn reserve_keeps_semantics() {
        let mut chunked: ChunkedVec<u32> = ChunkedVec::new();
        chunked.reserve(CHUNK * 4);
        assert_eq!(chunked.len(), 0);
        for index in 0..10_u32 {
            chunked.push(index);
        }
        assert_eq!(
            chunked.iter().copied().collect::<Vec<_>>(),
            (0..10).collect::<Vec<_>>()
        );
    }
}

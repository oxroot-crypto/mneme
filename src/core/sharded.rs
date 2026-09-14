//! 分片共享哈希表(`sharded.rs`)。
//!
//! L0 纯数据结构:无 I/O、无锁。键为 [`RowId`],按行号取模分散到固定数量的分片,
//! 每分片一个 `Arc<HashMap>`:克隆只复制分片句柄,插入/修改/删除只对该键所在分片
//! 做写时复制(COW)。用于 L1 写状态的 `latest`/`versions` 等行号索引,把「每个
//! 写事务全量深拷 $O(N)$」降为「只拷命中分片」。
//!
//! 语义与 `HashMap<RowId, V>` 全等:`get`/`insert`/`remove`/`get_mut`/`iter`/`len`;
//! `iter` 顺序与 `HashMap` 一样不做保证(分片遍历序)。

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use crate::core::hash::{FnvBuildHasher, fnv1a_hash};

/// 分片数(固定)。
pub(crate) const SHARDS: usize = 256;

/// 分片共享的哈希表。
///
/// 键经哈希取模落到固定分片;克隆只复制分片句柄,写操作只对该键所在分片做 COW。
pub(crate) struct ShardedMap<K, V> {
    /// 固定长度的分片表。
    shards: Vec<Arc<HashMap<K, V, FnvBuildHasher>>>,
    /// 非空条目总数(与 `HashMap::len` 同口径)。
    count: usize,
}

impl<K: Hash + Eq + Clone, V> ShardedMap<K, V> {
    /// 构造空表。
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS)
                .map(|_| Arc::new(HashMap::with_hasher(FnvBuildHasher)))
                .collect(),
            count: 0,
        }
    }

    /// 键所属分片下标(FNV-1a;仅用于分散,不承担抗碰撞)。
    fn shard_index(key: &K) -> usize {
        (fnv1a_hash(key) as usize) % SHARDS
    }

    /// 读取键对应值。
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.shards
            .get(Self::shard_index(key))
            .and_then(|shard| shard.get(key))
    }

    /// 可变读取;分片被快照共享时做分片级 COW(只克隆该分片)。
    pub(crate) fn get_mut(&mut self, key: &K) -> Option<&mut V>
    where
        V: Clone,
    {
        let shard = self.shards.get_mut(Self::shard_index(key))?;
        Arc::make_mut(shard).get_mut(key)
    }

    /// 取键对应值的可变引用,不存在则插入 `V::default()`。
    ///
    /// 分片被快照共享时做分片级 COW(只克隆该分片)。
    pub(crate) fn get_or_insert_default(&mut self, key: K) -> &mut V
    where
        V: Clone + Default,
    {
        let shard = &mut self.shards[Self::shard_index(&key)];
        let map = Arc::make_mut(shard);
        let (entry, is_new) = match map.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => (entry.into_mut(), false),
            std::collections::hash_map::Entry::Vacant(slot) => (slot.insert(V::default()), true),
        };
        if is_new {
            self.count += 1;
        }
        entry
    }

    /// 插入键值,返回被覆盖的旧值。
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        V: Clone,
    {
        let shard = &mut self.shards[Self::shard_index(&key)];
        let previous = Arc::make_mut(shard).insert(key, value);
        if previous.is_none() {
            self.count += 1;
        }
        previous
    }

    /// 删除键,返回被删除的值。
    pub(crate) fn remove(&mut self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        let shard = &mut self.shards[Self::shard_index(key)];
        let removed = Arc::make_mut(shard).remove(key);
        if removed.is_some() {
            self.count -= 1;
        }
        removed
    }

    /// 迭代表中全部键值(分片序;与 `HashMap` 一样不保证顺序)。
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> + '_ {
        self.shards.iter().flat_map(|shard| shard.iter())
    }

    /// 迭代表中全部值(分片序;与 `HashMap` 一样不保证顺序)。
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> + '_ {
        self.iter().map(|(_, value)| value)
    }

    /// 非空条目总数(生产路径暂未使用;供测试与后续分片统计)。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// 预留各分片容量。
    ///
    /// 分片被快照共享时会对每个分片做 COW(等于整表拷贝);只应在表仍唯一持有
    /// (新建/恢复期)时调用。
    pub(crate) fn reserve(&mut self, additional: usize)
    where
        V: Clone,
    {
        let per_shard = additional.div_ceil(SHARDS);
        for shard in &mut self.shards {
            Arc::make_mut(shard).reserve(per_shard);
        }
    }
}

impl<K: Hash + Eq + Clone, V: Clone> FromIterator<(K, V)> for ShardedMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut map = Self::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl<K, V> Clone for ShardedMap<K, V> {
    /// 克隆分片句柄(每分片一次引用计数),不复制条目。
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            count: self.count,
        }
    }
}

impl<K: Hash + Eq + Clone, V> Default for ShardedMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::RowId;

    fn rowid(value: u64) -> RowId {
        RowId::new(value)
    }

    /// FC-CORE-POST-010:插入/读取/删除/长度与 `HashMap` 全等(含跨分片)。
    #[test]
    fn matches_hashmap_semantics_across_shards() {
        let mut sharded: ShardedMap<RowId, u32> = ShardedMap::new();
        let mut flat: HashMap<RowId, u32> = HashMap::new();
        for value in 0..(SHARDS as u64 * 3 + 7) {
            let key = rowid(value);
            assert_eq!(
                sharded.insert(key, value as u32),
                flat.insert(key, value as u32)
            );
        }
        assert_eq!(sharded.len(), flat.len());
        for value in 0..(SHARDS as u64 * 3 + 7) {
            let key = rowid(value);
            assert_eq!(sharded.get(&key), flat.get(&key));
        }
        // 覆盖写不改变长度。
        let key = rowid(5);
        sharded.insert(key, 999);
        assert_eq!(sharded.len(), flat.len());
        assert_eq!(sharded.get(&key), Some(&999));
        // 删除。
        assert_eq!(sharded.remove(&key), Some(999));
        assert_eq!(flat.remove(&key), Some(5));
        assert_eq!(sharded.len(), flat.len());
        assert_eq!(sharded.remove(&key), None);
        // 迭代集合全等(顺序不保证)。
        let mut got: Vec<(u64, u32)> = sharded.iter().map(|(k, v)| (k.get(), *v)).collect();
        let mut want: Vec<(u64, u32)> = flat.iter().map(|(k, v)| (k.get(), *v)).collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    /// FC-CORE-POST-010:克隆只共享分片,后续插入/删除不泄漏给克隆源。
    #[test]
    fn clone_shares_shards_and_writes_are_copy_on_write() {
        let mut original: ShardedMap<RowId, u32> = ShardedMap::new();
        for value in 0..1000 {
            original.insert(rowid(value), value as u32);
        }
        let snapshot = original.clone();
        original.insert(rowid(1), 111);
        original.remove(&rowid(2));
        assert_eq!(snapshot.get(&rowid(1)), Some(&1), "快照不得被覆盖写改变");
        assert_eq!(snapshot.get(&rowid(2)), Some(&2), "快照不得被删除改变");
        assert_eq!(snapshot.len(), 1000);
        assert_eq!(original.len(), 999);
        assert_eq!(original.get(&rowid(1)), Some(&111));
        assert_eq!(original.get(&rowid(2)), None);
    }

    /// FC-CORE-POST-010:`get_mut`/`get_or_insert_default` 分片级 COW,不泄漏给克隆源。
    #[test]
    fn get_mut_and_get_or_insert_are_copy_on_write() {
        let mut original: ShardedMap<RowId, String> = ShardedMap::new();
        original.insert(rowid(7), "a".to_string());
        let snapshot = original.clone();
        original.get_mut(&rowid(7)).expect("存在").push('b');
        assert_eq!(original.get(&rowid(7)).map(String::as_str), Some("ab"));
        assert_eq!(snapshot.get(&rowid(7)).map(String::as_str), Some("a"));
        *original.get_or_insert_default(rowid(9)) = "new".to_string();
        assert_eq!(original.get(&rowid(9)).map(String::as_str), Some("new"));
        assert_eq!(snapshot.get(&rowid(9)), None, "快照不得出现新键");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(original.len(), 2);
    }
}

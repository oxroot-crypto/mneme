//! HNSW 邻接图存储(`graph.rs`,设计 05 §3/§4)。
//!
//! 节点 id = 段内槽位下标(u32);每个节点持有 `0..=level` 层的邻居数组。
//! 第 0 层度数上界为 `M0`,上层为 `M`。图本身不含向量,距离计算在
//! [`HnswIndex`](super::hnsw::HnswIndex) 中完成。

/// 每层邻居表的初始容量(避免逐次扩容;非度数上界)。
const NEIGHBOR_PREALLOC: usize = 8;

/// 单节点的分层邻接表。
#[derive(Debug, Clone, Default)]
pub(crate) struct NodeLinks {
    /// `neighbors[l]` = 第 `l` 层邻居节点 id。
    pub(crate) neighbors: Vec<Vec<u32>>,
}

/// HNSW 邻接图:节点集、分层邻接与入口点。
#[derive(Debug, Clone)]
pub(crate) struct Graph {
    /// 每个节点的分层邻接表。
    pub(crate) nodes: Vec<NodeLinks>,
    /// 每个节点的最高层级(冗余便于快速访问)。
    pub(crate) levels: Vec<u8>,
    /// 段内入口节点 id。
    pub(crate) entry: u32,
    /// 入口节点层级。
    pub(crate) entry_level: u8,
}

impl Graph {
    /// 新建空图。
    pub(crate) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            levels: Vec::new(),
            entry: 0,
            entry_level: 0,
        }
    }

    /// 节点数。
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 追加一个最高层级为 `level` 的节点。
    pub(crate) fn push_node(&mut self, level: u8) {
        let mut links = NodeLinks::default();
        for _ in 0..=level {
            links.neighbors.push(Vec::with_capacity(NEIGHBOR_PREALLOC));
        }
        self.nodes.push(links);
        self.levels.push(level);
    }

    /// 第 `node` 节点在第 `level` 层的邻居(层级越界返回空)。
    pub(crate) fn neighbors(&self, node: u32, level: usize) -> &[u32] {
        self.nodes
            .get(node as usize)
            .and_then(|links| links.neighbors.get(level))
            .map_or(&[][..], Vec::as_slice)
    }

    /// 第 `node` 节点在第 `level` 层的度。
    pub(crate) fn degree(&self, node: u32, level: usize) -> usize {
        self.neighbors(node, level).len()
    }

    /// 覆盖第 `node` 节点在第 `level` 层的邻居集合。
    pub(crate) fn set_neighbors(&mut self, node: u32, level: usize, neighbors: Vec<u32>) {
        if let Some(links) = self.nodes.get_mut(node as usize)
            && let Some(slot) = links.neighbors.get_mut(level)
        {
            *slot = neighbors;
        }
    }

    /// 向第 `node` 节点在第 `level` 层追加一个邻居(去重)。
    pub(crate) fn add_neighbor(&mut self, node: u32, level: usize, other: u32) {
        if let Some(links) = self.nodes.get_mut(node as usize)
            && let Some(slot) = links.neighbors.get_mut(level)
            && !slot.contains(&other)
        {
            slot.push(other);
        }
    }
}

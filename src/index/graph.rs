//! HNSW 邻接图存储(`graph.rs`,设计 05 §3/§4)。
//!
//! 节点 id = 段内槽位下标(u32);每个节点持有 `0..=level` 层的邻居数组。
//! 第 0 层度数上界为 `M0`,上层为 `M`。图本身不含向量,距离计算在
//! [`HnswIndex`](super::hnsw::HnswIndex) 中完成。
//!
//! 两种形态共用 [`GraphStore`] 接口:构建期是内存 [`Graph`](堆上逐节点邻接表);
//! hidx 载入期是 [`MappedGraph`](只读头部 + `node_table` 视图,邻接字节按需从
//! 段句柄解码并缓存,FC-PERSIST-INV-021)。

use std::sync::OnceLock;

use crate::memory::lazy::ByteSpan;

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

/// [`MappedGraph::new`] 的输入参数(字段全部来自已校验的 hidx 头部与
/// `node_table`,由打开期 [`open`](super::hidx::open) 逐项校验后传入)。
#[derive(Debug)]
pub(crate) struct MappedGraphParts {
    /// hidx 文件整段视图(句柄存活期内有效)。
    pub(crate) span: ByteSpan,
    /// 节点数。
    pub(crate) count: usize,
    /// 每节点层级(`node_table` 的 level 列)。
    pub(crate) levels: Vec<u8>,
    /// 每节点邻接块在邻接区内的相对偏移(`node_table` 的 adj_off 列)。
    pub(crate) offsets: Vec<usize>,
    /// 邻接区在文件内的绝对起始偏移。
    pub(crate) adj_start: usize,
    /// 图参数(来自 hidx 头部)。
    pub(crate) params: GraphParams,
    /// 入口节点 id(载入期已验证 = 全图最高层)。
    pub(crate) entry: u32,
    /// 入口节点层级。
    pub(crate) entry_level: u8,
}

/// hidx 头部记录的图参数(m/M0/构建宽度/ml)。
#[derive(Debug, Clone, Copy)]
pub(crate) struct GraphParams {
    /// 上层度数上界。
    pub(crate) m: u16,
    /// 第 0 层度数上界。
    pub(crate) m0: u16,
    /// 构建期探查宽度。
    pub(crate) ef_construction: u16,
    /// 层级分布参数。
    pub(crate) ml: f32,
}

/// hidx 载入的惰性图:头部数据 + `node_table` 常驻,邻接字节按节点按需解码。
///
/// `node_table`(层级 + 邻接偏移)在打开期一次读出(5 B/节点);邻接区由
/// [`ByteSpan`] 挂在段句柄上,首次访问某节点时解码该节点各层邻接并缓存,
/// 未访问节点不占堆内存(设计 05 §10、FC-PERSIST-INV-021)。
#[derive(Debug)]
pub(crate) struct MappedGraph {
    /// hidx 文件整段视图(句柄存活期内有效)。
    span: ByteSpan,
    /// 节点数。
    count: usize,
    /// 每节点层级(`node_table` 的 level 列)。
    levels: Vec<u8>,
    /// 每节点邻接块在邻接区内的相对偏移(`node_table` 的 adj_off 列)。
    offsets: Vec<usize>,
    /// 邻接区在文件内的绝对起始偏移。
    adj_start: usize,
    /// 图参数(来自 hidx 头部)。
    params: GraphParams,
    /// 入口节点 + 层级(载入期已验证 = 全图最高层)。
    entry: u32,
    entry_level: u8,
    /// 逐节点惰性解码缓存(只缓存被访问过的节点)。
    cache: Vec<OnceLock<Box<[Vec<u32>]>>>,
}

impl MappedGraph {
    /// 由已校验的头部数据与句柄构造惰性图。
    pub(crate) fn new(parts: MappedGraphParts) -> Self {
        let MappedGraphParts {
            span,
            count,
            levels,
            offsets,
            adj_start,
            params,
            entry,
            entry_level,
        } = parts;
        Self {
            span,
            count,
            levels,
            offsets,
            adj_start,
            params,
            entry,
            entry_level,
            cache: (0..count).map(|_| OnceLock::new()).collect(),
        }
    }

    /// 图参数(来自 hidx 头部)。
    pub(crate) fn params(&self) -> GraphParams {
        self.params
    }

    /// 节点数。
    pub(crate) fn node_count(&self) -> usize {
        self.count
    }

    /// 全图最高层(载入期已验证等于入口层级)。
    pub(crate) fn max_level(&self) -> u8 {
        self.entry_level
    }

    /// 入口节点 id。
    pub(crate) fn entry(&self) -> u32 {
        self.entry
    }

    /// 入口层级。
    pub(crate) fn entry_level(&self) -> u8 {
        self.entry_level
    }

    /// 第 `node` 节点在第 `level` 层的邻居(首次访问解码并缓存)。
    pub(crate) fn neighbors(&self, node: u32, level: usize) -> &[u32] {
        let Some(cell) = self.cache.get(node as usize) else {
            return &[];
        };
        let links = cell.get_or_init(|| self.decode_node(node as usize));
        links.get(level).map_or(&[][..], Vec::as_slice)
    }

    /// 把整图物化为堆图(仅序列化/测试用;逐节点走惰性解码缓存)。
    pub(crate) fn to_graph(&self) -> Graph {
        let mut graph = Graph::new();
        for node in 0..self.count {
            graph.push_node(self.levels[node]);
        }
        for node in 0..self.count {
            let level = self.levels[node] as usize;
            for layer in 0..=level {
                let neighbors = self.neighbors(node as u32, layer).to_vec();
                graph.set_neighbors(node as u32, layer, neighbors);
            }
        }
        graph.entry = self.entry;
        graph.entry_level = self.entry_level;
        graph
    }

    /// 解码某节点的全部层邻接(载入期已校验布局,故按偏移直读)。
    fn decode_node(&self, node: usize) -> Box<[Vec<u32>]> {
        let level = self.levels[node] as usize;
        let mut links = Vec::with_capacity(level + 1);
        let mut cursor = self.offsets[node];
        for _ in 0..=level {
            // reason: 邻接区布局在打开期已逐节点校验(FC-INDEX-ERR-001),
            // 此处 `cursor` 必指向本节点该层的 2 字节度数头。
            let header = self
                .span
                .slice(self.adj_start + cursor, 2)
                .expect("hidx 邻接区载入期已校验(FC-INDEX-ERR-001)");
            let degree = usize::from(u16::from_le_bytes([header[0], header[1]]));
            let start = cursor + 2;
            // reason: 同一次打开期校验保证 `degree` 个邻居字节必在邻接区内
            // (越界/度数越界均在载入期被拒),故取切片不会失败。
            let bytes = self
                .span
                .slice(self.adj_start + start, degree * 4)
                .expect("hidx 邻接区载入期已校验(FC-INDEX-ERR-001)");
            let mut neighbors = Vec::with_capacity(degree);
            for chunk in bytes.chunks_exact(4) {
                neighbors.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            links.push(neighbors);
            cursor = start + degree * 4;
        }
        links.into_boxed_slice()
    }
}

/// 只读图访问(构建期 [`Graph`] 与 hidx 载入期 [`MappedGraph`] 共用)。
#[derive(Debug)]
pub(crate) enum GraphStore {
    /// 构建期内存图。
    Heap(Graph),
    /// hidx 载入的惰性图。
    Mapped(MappedGraph),
}

impl GraphStore {
    /// 取构建期可变图;仅构建路径可调用(`Mapped` 返回 `None`)。
    pub(crate) fn heap_mut(&mut self) -> Option<&mut Graph> {
        match self {
            Self::Heap(graph) => Some(graph),
            Self::Mapped(_) => None,
        }
    }

    /// 全图最高层。
    pub(crate) fn max_level(&self) -> u8 {
        match self {
            Self::Heap(graph) => graph.levels.iter().copied().max().unwrap_or(0),
            Self::Mapped(graph) => graph.max_level(),
        }
    }

    /// 入口节点 id。
    pub(crate) fn entry(&self) -> u32 {
        match self {
            Self::Heap(graph) => graph.entry,
            Self::Mapped(graph) => graph.entry(),
        }
    }

    /// 入口层级。
    pub(crate) fn entry_level(&self) -> u8 {
        match self {
            Self::Heap(graph) => graph.entry_level,
            Self::Mapped(graph) => graph.entry_level(),
        }
    }

    /// 第 `node` 节点在第 `level` 层的邻居。
    pub(crate) fn neighbors(&self, node: u32, level: usize) -> &[u32] {
        match self {
            Self::Heap(graph) => graph.neighbors(node, level),
            Self::Mapped(graph) => graph.neighbors(node, level),
        }
    }

    /// 第 `node` 节点在第 `level` 层的度。
    pub(crate) fn degree(&self, node: u32, level: usize) -> usize {
        self.neighbors(node, level).len()
    }

    /// 物化为堆图(序列化用;`Heap` 直接深拷贝)。
    pub(crate) fn to_graph(&self) -> Graph {
        match self {
            Self::Heap(graph) => graph.clone(),
            Self::Mapped(graph) => graph.to_graph(),
        }
    }
}

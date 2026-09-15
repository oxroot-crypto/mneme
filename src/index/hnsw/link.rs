//! 加边、修剪与可达性修复:批应用与启发式邻接维护。

use std::cmp::Ordering;

use crate::core::error::Result;
use crate::core::metric::Score;

use super::HnswIndex;
use super::model::LinkPlan;

impl HnswIndex {
    /// 串行应用一批计划:先全部加边(无距离计算),再把超员节点的修剪计算
    /// **按批并行求值**、按 `(节点, 层)` 序串行写回。
    ///
    /// 修剪只读批末图快照、各改各的邻接表,故结果与线程数无关(确定性不变);
    /// 相比逐节点边加边剪,批末统一修剪的输入含本批全部新边,修剪更彻底。
    ///
    /// # Errors
    /// 修剪计算线程 panic 时返回结构化错误,绝不把 panic 抛给调用方。
    pub(super) fn apply_batch(
        &mut self,
        start: usize,
        plans: &[LinkPlan],
        levels: &[u8],
        threads: usize,
    ) -> Result<()> {
        let mut touched: Vec<(u32, usize)> = Vec::new();
        for (offset, plan) in plans.iter().enumerate() {
            let position = start + offset;
            self.apply_links(position as u32, levels[position], plan, &mut touched);
        }
        touched.sort_unstable();
        touched.dedup();
        touched.retain(|&(node, layer)| {
            let max_conn = if layer == 0 { self.m0 } else { self.m };
            self.graph.degree(node, layer) > max_conn
        });
        let pruned = self.compute_prune_batch(&touched, threads)?;
        for ((node, layer), selected) in touched.into_iter().zip(pruned) {
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
            if let Some(graph) = self.graph.heap_mut() {
                graph.set_neighbors(node, layer, selected);
            }
        }
        Ok(())
    }

    /// 串行加边(无距离计算):双向连边、记录被触达的 `(节点, 层)`、必要时推进入口。
    fn apply_links(
        &mut self,
        node: u32,
        level: u8,
        plan: &LinkPlan,
        touched: &mut Vec<(u32, usize)>,
    ) {
        if node == 0 {
            // reason: 构建路径的图恒为 `Heap`,不可达分支显式忽略。
            if let Some(graph) = self.graph.heap_mut() {
                graph.entry = 0;
                graph.entry_level = level;
            }
            return;
        }
        for (layer, selected) in &plan.layers {
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
            let graph = self.graph.heap_mut().expect("构建路径恒为堆图");
            for &neighbor in selected {
                graph.add_neighbor(node, *layer, neighbor);
                graph.add_neighbor(neighbor, *layer, node);
            }
            touched.push((node, *layer));
            for &neighbor in selected {
                touched.push((neighbor, *layer));
            }
        }
        if level as usize > self.graph.entry_level() as usize {
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
            if let Some(graph) = self.graph.heap_mut() {
                graph.entry = node;
                graph.entry_level = level;
            }
        }
    }

    /// 并行计算一批超员节点的修剪后邻接(只读图快照;结果与线程数无关)。
    ///
    /// # Errors
    /// 修剪计算线程 panic 时返回结构化错误。
    fn compute_prune_batch(
        &self,
        targets: &[(u32, usize)],
        threads: usize,
    ) -> Result<Vec<Vec<u32>>> {
        let count = targets.len();
        let workers = threads.min(count).max(1);
        if workers <= 1 {
            return Ok(targets
                .iter()
                .map(|&(node, layer)| self.compute_pruned(node, layer))
                .collect());
        }
        self.compute_prune_parallel(targets, workers)?
            .into_iter()
            .map(|slot| {
                slot.ok_or(crate::core::error::MnemeError::Inconsistent {
                    reason: "HNSW 修剪结果缺失",
                })
            })
            .collect()
    }

    /// 多线程分派修剪计算:每线程经原子游标领取目标,结果按位置回填槽位。
    ///
    /// 只读批末图快照、各改各的邻接表,故结果与线程数无关。
    ///
    /// # Errors
    /// 修剪计算线程 panic 时返回结构化错误。
    fn compute_prune_parallel(
        &self,
        targets: &[(u32, usize)],
        workers: usize,
    ) -> Result<Vec<Option<Vec<u32>>>> {
        let count = targets.len();
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let mut slots: Vec<Option<Vec<u32>>> = (0..count).map(|_| None).collect();
        std::thread::scope(|scope| -> Result<()> {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| -> Vec<(usize, Vec<u32>)> {
                        let mut produced = Vec::new();
                        loop {
                            let index = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if index >= count {
                                break;
                            }
                            let (node, layer) = targets[index];
                            produced.push((index, self.compute_pruned(node, layer)));
                        }
                        produced
                    })
                })
                .collect();
            for handle in handles {
                let produced =
                    handle
                        .join()
                        .map_err(|_| crate::core::error::MnemeError::Inconsistent {
                            reason: "HNSW 修剪线程 panic",
                        })?;
                for (index, selected) in produced {
                    slots[index] = Some(selected);
                }
            }
            Ok(())
        })?;
        Ok(slots)
    }

    /// 计算 `node` 在 `layer` 层修剪后的邻接(只读;不写图)。
    fn compute_pruned(&self, node: u32, layer: usize) -> Vec<u32> {
        let max_conn = if layer == 0 { self.m0 } else { self.m };
        let current: Vec<u32> = self.graph.neighbors(node, layer).to_vec();
        let mut scored: Vec<(Score, u32)> = current
            .iter()
            .map(|&neighbor| (self.score_pair(node, neighbor), neighbor))
            .collect();
        scored.sort_by(|a, b| {
            if self.metric.better(a.0, b.0) {
                Ordering::Less
            } else if self.metric.better(b.0, a.0) {
                Ordering::Greater
            } else {
                a.1.cmp(&b.1)
            }
        });
        self.select_neighbors(node, &scored, max_conn)
    }

    /// 构建后可达性修复:把从入口沿出边不可达的节点重连到可达集合。
    ///
    /// 批内互不可见 + 邻边集中修剪可能留下极少量不可达节点(破坏 `ef→∞` 收敛的
    /// 图结构前提);本步在全部修剪完成后执行,按不可达节点的升序构造可达链
    /// `entry → u₀ → u₁ → …`:每条链边的宿主只被修改一次、并在保护式修剪下保留,
    /// 建立后不再被任何操作触碰,可达性严格成立。
    pub(super) fn repair_unreachable(&mut self) {
        // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
        let Some(graph) = self.graph.heap_mut() else {
            return;
        };
        let m0 = self.m0;
        let count = graph.node_count();
        if count == 0 {
            return;
        }
        let entry = graph.entry;
        let mut seen = vec![false; count];
        let mut stack = vec![entry];
        seen[entry as usize] = true;
        while let Some(node) = stack.pop() {
            let level = graph.levels[node as usize] as usize;
            for layer in 0..=level {
                for &neighbor in graph.neighbors(node, layer) {
                    if !seen[neighbor as usize] {
                        seen[neighbor as usize] = true;
                        stack.push(neighbor);
                    }
                }
            }
        }
        let unreachable: Vec<u32> = (0..count as u32)
            .filter(|&node| !seen[node as usize])
            .collect();
        if unreachable.is_empty() {
            return;
        }
        // 链式修复:首个不可达节点连到 entry,其后每个连到**上一个修复节点**,
        // 形成 `entry → u₀ → u₁ → …` 的可达链。每个宿主只被修改一次(修复其下一个
        // 节点时),边在**保护式修剪**下建立后不再被任何操作触碰,可达性严格成立。
        let mut previous: Option<u32> = None;
        for node in unreachable {
            let host = previous.unwrap_or(entry);
            if let Some(graph) = self.graph.heap_mut() {
                graph.add_neighbor(host, 0, node);
            }
            self.prune_protecting(host, 0, node);
            if let Some(graph) = self.graph.heap_mut()
                && graph.neighbors(node, 0).len() < m0
            {
                graph.add_neighbor(node, 0, host);
            }
            seen[node as usize] = true;
            previous = Some(node);
        }
    }

    /// `Hybrid` 档:按 f32 原向量重算候选与 owner 的距离并按精确分重排。
    ///
    /// 遍历候选来自 i8 近似分,重排保证 `select_neighbors`/`prune` 的输入
    /// 按精确分有序(同分按节点序号,与图构建的全序口径一致)。
    pub(super) fn refine_candidates(
        &self,
        owner: u32,
        candidates: Vec<(Score, u32)>,
    ) -> Vec<(Score, u32)> {
        let owner_node = &self.nodes[owner as usize];
        let mut refined: Vec<(Score, u32)> = candidates
            .into_iter()
            .map(|(_, node)| {
                let target = &self.nodes[node as usize];
                let score = self.metric.score(
                    &owner_node.vector,
                    &target.vector,
                    owner_node.norm_sq,
                    target.norm_sq,
                );
                (score, node)
            })
            .collect();
        refined.sort_by(|left, right| {
            self.metric
                .score_order(left.0, right.0)
                .then(left.1.cmp(&right.1))
        });
        refined
    }

    /// 启发式选邻(设计 05 §4.3):优先保留提供新方向的候选,不足则回填。
    pub(super) fn select_neighbors(
        &self,
        owner: u32,
        candidates: &[(Score, u32)],
        max_conn: usize,
    ) -> Vec<u32> {
        let mut selected: Vec<u32> = Vec::with_capacity(max_conn);
        for &(score_to_owner, candidate) in candidates {
            if selected.len() >= max_conn {
                break;
            }
            if candidate == owner {
                continue;
            }
            let mut keep = true;
            for &chosen in selected.iter().take(self.compare_cap) {
                let score_to_chosen = self.score_pair(candidate, chosen);
                // 候选离 owner 比离已选邻居更近 -> 提供新方向。
                if !self.metric.better(score_to_owner, score_to_chosen) {
                    keep = false;
                    break;
                }
            }
            if keep {
                selected.push(candidate);
            }
        }
        if selected.len() < max_conn {
            for &(_, candidate) in candidates {
                if selected.len() >= max_conn {
                    break;
                }
                if candidate != owner && !selected.contains(&candidate) {
                    selected.push(candidate);
                }
            }
        }
        selected
    }

    /// 修剪超员节点的邻集,并**强制保留**邻居 `protect`(修复路径用;必要时挤掉
    /// 选中序列的最后一位),度数恒在上界内。
    fn prune_protecting(&mut self, node: u32, level: usize, protect: u32) {
        let max_conn = if level == 0 { self.m0 } else { self.m };
        let mut selected = self.compute_pruned(node, level);
        if !selected.contains(&protect) {
            if selected.len() >= max_conn {
                selected.pop();
            }
            selected.push(protect);
        }
        // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
        if let Some(graph) = self.graph.heap_mut() {
            graph.set_neighbors(node, level, selected);
        }
    }
}

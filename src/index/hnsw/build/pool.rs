//! 批内并行建图的常驻 worker 池(`hnsw/build/pool.rs`)。
//!
//! 每次构建只经一次 `thread::scope` 生成固定数量的 worker,批次间复用(替代逐批
//! 创建/销毁线程):
//!
//! - **计划阶段**:worker 持图共享读锁计算批内节点选邻计划,结果按批内位置回填;
//! - **修剪阶段**:worker 持图共享读锁计算超员目标 `(节点,层)` 的修剪邻接,结果
//!   按目标位置回填;
//! - 主线程只在两阶段之间持写锁应用连边/写回,绝不与 worker 并发读写。
//!
//! 任务经 `AtomicUsize` 动态游标分派(负载均衡),结果经 channel 按位置回填;
//! 计划只依赖批开始图快照,故最终图与线程调度无关(确定性由
//! `FC-INDEX-POST-012` 约束)。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};

use crate::core::error::{MnemeError, Result};

use super::super::HnswIndex;
use super::super::model::LinkPlan;

/// worker 指令。
enum Task {
    /// 计算 `[start, start + batch)` 的选邻计划(动态游标分派)。
    Plan {
        /// 批起始节点位置。
        start: usize,
        /// 批内节点数。
        batch: usize,
        /// 共享游标(批内位置)。
        cursor: Arc<AtomicUsize>,
    },
    /// 计算超员目标的修剪邻接(动态游标分派)。
    Prune {
        /// 目标 `(节点, 层)` 列表。
        targets: Arc<Vec<(u32, usize)>>,
        /// 共享游标(目标位置)。
        cursor: Arc<AtomicUsize>,
    },
    /// 退出 worker 循环。
    Shutdown,
}

/// worker 结果(与 [`Task`] 一一对应;失败经 [`TaskOutput::Failed`] 上报)。
enum TaskOutput {
    /// `(批内位置, 选邻计划)`。
    Plans(Vec<(usize, LinkPlan)>),
    /// `(目标位置, 修剪后邻接)`。
    Pruned(Vec<(usize, Vec<u32>)>),
    /// 结构化失败(建图前置不满足等)。
    Failed(MnemeError),
}

/// 常驻 worker 池(生命周期绑定构建期的 `thread::scope`)。
pub(super) struct BuildPool {
    /// 各 worker 的任务发送端(下标 = worker 号)。
    tasks: Vec<Sender<Task>>,
    /// 各 worker 的结果接收端(与 `tasks` 同序)。
    results: Vec<Receiver<TaskOutput>>,
}

impl BuildPool {
    /// 生成 `threads` 个 worker 并常驻整个构建期。
    ///
    /// `lock` 为图访问锁:worker 只取读锁,主线程只在阶段之间取写锁;
    /// `levels` 为确定性层级表(只读共享)。
    pub(super) fn spawn<'scope, 'env>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        threads: usize,
        lock: &'env RwLock<&'env mut HnswIndex>,
        levels: &'env [u8],
    ) -> Self {
        let mut tasks = Vec::with_capacity(threads);
        let mut results = Vec::with_capacity(threads);
        for _ in 0..threads {
            let (task_tx, task_rx) = mpsc::channel::<Task>();
            let (result_tx, result_rx) = mpsc::channel::<TaskOutput>();
            scope.spawn(move || worker_loop(lock, levels, &task_rx, &result_tx));
            tasks.push(task_tx);
            results.push(result_rx);
        }
        Self { tasks, results }
    }

    /// 计算批 `[start, end)` 的全部选邻计划(按批内位置返回)。
    ///
    /// # Errors
    /// worker 退出(panic/通道断开)、结果位置越界或计划缺失时返回结构化错误。
    pub(super) fn plan_batch(&self, start: usize, end: usize) -> Result<Vec<LinkPlan>> {
        let batch = end.saturating_sub(start);
        if batch == 0 {
            return Ok(Vec::new());
        }
        let workers = self.tasks.len().min(batch).max(1);
        let cursor = Arc::new(AtomicUsize::new(0));
        for task in self.tasks.iter().take(workers) {
            task.send(Task::Plan {
                start,
                batch,
                cursor: Arc::clone(&cursor),
            })
            .map_err(|_| worker_gone())?;
        }
        let mut slots: Vec<Option<LinkPlan>> = (0..batch).map(|_| None).collect();
        for result in self.results.iter().take(workers) {
            match result.recv() {
                Ok(TaskOutput::Plans(produced)) => {
                    for (offset, plan) in produced {
                        let Some(slot) = slots.get_mut(offset) else {
                            return Err(inconsistent("建图计划位置越界"));
                        };
                        *slot = Some(plan);
                    }
                }
                Ok(TaskOutput::Failed(error)) => return Err(error),
                Ok(TaskOutput::Pruned(_)) => return Err(inconsistent("建图 worker 结果类型不符")),
                Err(_) => return Err(worker_gone()),
            }
        }
        slots
            .into_iter()
            .map(|slot| slot.ok_or_else(|| inconsistent("建图计划缺失")))
            .collect()
    }

    /// 计算超员目标的修剪邻接(按目标位置返回,顺序与 `targets` 一致)。
    ///
    /// # Errors
    /// worker 退出或结果位置越界时返回结构化错误。
    pub(super) fn prune_batch(&self, targets: &[(u32, usize)]) -> Result<Vec<Vec<u32>>> {
        let count = targets.len();
        if count == 0 {
            return Ok(Vec::new());
        }
        let workers = self.tasks.len().min(count).max(1);
        let shared = Arc::new(targets.to_vec());
        let cursor = Arc::new(AtomicUsize::new(0));
        for task in self.tasks.iter().take(workers) {
            task.send(Task::Prune {
                targets: Arc::clone(&shared),
                cursor: Arc::clone(&cursor),
            })
            .map_err(|_| worker_gone())?;
        }
        let mut slots: Vec<Option<Vec<u32>>> = (0..count).map(|_| None).collect();
        for result in self.results.iter().take(workers) {
            match result.recv() {
                Ok(TaskOutput::Pruned(produced)) => {
                    for (index, selected) in produced {
                        let Some(slot) = slots.get_mut(index) else {
                            return Err(inconsistent("修剪结果位置越界"));
                        };
                        *slot = Some(selected);
                    }
                }
                Ok(TaskOutput::Failed(error)) => return Err(error),
                Ok(TaskOutput::Plans(_)) => return Err(inconsistent("建图 worker 结果类型不符")),
                Err(_) => return Err(worker_gone()),
            }
        }
        slots
            .into_iter()
            .map(|slot| slot.ok_or_else(|| inconsistent("修剪结果缺失")))
            .collect()
    }

    /// 广播退出指令;worker 收到后结束循环。
    pub(super) fn shutdown(&self) {
        for task in &self.tasks {
            // reason: worker 已退出时发送失败无需处理,退出语义由通道断开兜底。
            let _ = task.send(Task::Shutdown);
        }
    }
}

/// worker 主循环:逐任务执行,直到收到退出指令或通道断开。
///
/// 任务 panic 经 `catch_unwind` 收敛为 [`TaskOutput::Failed`](结构化 `Inconsistent`),
/// 绝不让 panic 经 `thread::scope` 抛给调用方(FC-INDEX-POST-012)。
fn worker_loop(
    lock: &RwLock<&mut HnswIndex>,
    levels: &[u8],
    tasks: &Receiver<Task>,
    results: &Sender<TaskOutput>,
) {
    while let Ok(task) = tasks.recv() {
        if matches!(task, Task::Shutdown) {
            break;
        }
        let output = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_task(lock, levels, task)
        })) {
            Ok(output) => output,
            Err(_) => TaskOutput::Failed(inconsistent("HNSW 建图/修剪线程 panic")),
        };
        if results.send(output).is_err() {
            // 主线程已放弃收集(失败提前返回):直接退出,不阻塞作用域回收。
            break;
        }
    }
}

/// 执行一条任务(计划或修剪),返回结果。
fn execute_task(lock: &RwLock<&mut HnswIndex>, levels: &[u8], task: Task) -> TaskOutput {
    match task {
        Task::Plan {
            start,
            batch,
            cursor,
        } => plan_task(lock, levels, start, batch, &cursor),
        Task::Prune { targets, cursor } => prune_task(lock, &targets, &cursor),
        // reason: `Shutdown` 已在主循环处理;此处不可达,兜底返回空修剪结果。
        Task::Shutdown => TaskOutput::Pruned(Vec::new()),
    }
}

/// 计划任务:动态游标领取批内位置,逐个计算选邻计划。
fn plan_task(
    lock: &RwLock<&mut HnswIndex>,
    levels: &[u8],
    start: usize,
    batch: usize,
    cursor: &AtomicUsize,
) -> TaskOutput {
    let guard = lock.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut produced = Vec::new();
    loop {
        let offset = cursor.fetch_add(1, Ordering::Relaxed);
        if offset >= batch {
            return TaskOutput::Plans(produced);
        }
        let position = start + offset;
        match guard.plan_node(position as u32, levels[position]) {
            Ok(plan) => produced.push((offset, plan)),
            Err(error) => return TaskOutput::Failed(error),
        }
    }
}

/// 修剪任务:动态游标领取目标位置,逐个计算修剪后邻接。
fn prune_task(
    lock: &RwLock<&mut HnswIndex>,
    targets: &[(u32, usize)],
    cursor: &AtomicUsize,
) -> TaskOutput {
    let guard = lock.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut produced = Vec::new();
    loop {
        let index = cursor.fetch_add(1, Ordering::Relaxed);
        let Some(&(node, layer)) = targets.get(index) else {
            return TaskOutput::Pruned(produced);
        };
        produced.push((index, guard.compute_pruned(node, layer)));
    }
}

/// worker 通道断开(panic 或提前退出)的统一错误。
fn worker_gone() -> MnemeError {
    inconsistent("建图 worker 已退出")
}

/// 构造 `Inconsistent` 结构化错误。
fn inconsistent(reason: &'static str) -> MnemeError {
    MnemeError::Inconsistent { reason }
}

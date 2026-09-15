//! `open` 打开/重载流程个单元测试。

use super::*;
use crate::core::heap::TopK;
use crate::core::types::RowId;

/// 桩索引:测试只验证 `load_index` 的重排校验,本桩不会被真正调用。
struct StubIndex;

impl VectorIndex for StubIndex {
    fn node_count(&self) -> usize {
        0
    }

    fn max_level(&self) -> u8 {
        0
    }

    fn entry(&self) -> (SlotId, u8) {
        (SlotId::new(0), 0)
    }

    fn serialize(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn search(&self, _params: &crate::memory::index::IndexSearch<'_>) -> TopK<(RowId, SlotId)> {
        TopK::new(0, Metric::Dot)
    }
}

/// 桩工厂:不接触 `crate::index` 具体实现,保持 L2 不依赖 L3。
struct StubFactory;

impl IndexFactory for StubFactory {
    fn build(
        &self,
        _request: crate::memory::index::IndexBuildRequest<'_>,
    ) -> Result<Arc<dyn VectorIndex>> {
        Ok(Arc::new(StubIndex))
    }

    fn verify(&self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    fn load(&self, _request: IndexLoadRequest<'_>) -> Result<Arc<dyn VectorIndex>> {
        Ok(Arc::new(StubIndex))
    }
}

/// FC-PERSIST-ERR-009:载入期二次校验——重排映射指向不存在的槽位 → `Corrupted`,
/// 绝不静默映射到槽位 0(空状态上 `remap = [0]` 即越界)。
#[test]
fn load_index_rejects_remap_past_state_slots() {
    let state = WriterState::new();
    let factory: Arc<dyn IndexFactory> = Arc::new(StubFactory);
    let hidx = crate::persist::source::ByteFile::from_bytes(0, vec![1]);
    let error = load_index(LoadIndexInput {
        factory: &factory,
        hidx: &hidx,
        slots: SlotRemap {
            state: &state,
            remap: &[0],
        },
        metric: Metric::Dot,
        quant: None,
    })
    .err()
    .expect("重排映射越界必须拒绝载入");
    assert!(matches!(error, MnemeError::Corrupted { .. }));
}

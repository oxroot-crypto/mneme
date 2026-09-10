# Changelog

本项目的所有重要变更记录于此文件。

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added
- L1 内存引擎(`src/memory/`):全内存记忆库,覆盖写入/更新/删除/墓碑、暴力检索与三值过滤、
  关系联想、双时态 `as_of`、去重、遗忘与沉淀;公开 API 在 L1 冻结。

### Changed
- 错误分类矩阵(见 `docs/spec/contracts.md` §0.2):移除泛化 `Invalid(&'static str)`,
  拆为 `Closed`/`NonFinite`/`LimitExceeded`/`MetaTooDeep`/`Config`/`Unsupported`/`Inconsistent`
  (破坏性变更)。
- 契约追溯门禁由计划中的 `xtask check-contracts` 改为 `tests/contract_traceability.rs`,
  随 `cargo test` 机械校验契约 ↔ 测试双向映射。

### Fixed
- `insert_batch` 在预校验后仍失败(`Dedup::Merge` 回调产物超限、槽位溢出)时回滚整批,
  消除残留版本(FC-MEM-POST-002)。
- 内部辅助路径(dedup 判重、`stats` 计数、`consolidate` 候选、`forget` 目标)统一排除
  逻辑过期记录,与常规读路径对齐(FC-LIFE-INV-009)。
- 非有限策略参数(MMR `lambda`、`Retention::access_weight`)返回 `Config`,不再静默失效
  (FC-MEM-PRE-003 / FC-LIFE-POST-002)。

<!--
模板(发布时替换):

## [0.1.0] - YYYY-MM-DD

### Added
- ...

### Changed
- ...

### Fixed
- ...
-->

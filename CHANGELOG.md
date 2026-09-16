# Changelog

本项目的所有重要变更记录于此文件。

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [0.1.0] - 2026-09-16

首个版本:L0–L6 分层引擎完整落地。

- 内存表与 MVCC 读视图、过滤 AST 与三值逻辑;
- WAL 与不可变段、MANIFEST 与崩溃恢复;
- 自研 HNSW(含过滤三档与 hidx 持久化);
- 查询计划器与 BM25/RRF 混合检索;
- 生命周期与 size-tiered compaction;
- i8/f16 量化副本与两阶段精排、`async` 门面;
- 静态加密与压缩、`Storage` 后端与只读共享;
- 形式化契约矩阵(FC-Matrix)与逐层验收测试同步交付;
- 纯内存形态与持久形态同 API、同建图口径(内存库经 `flush` 建内存段)。

[0.1.0]: https://github.com/oxroot-crypto/mneme/releases/tag/v0.1.0

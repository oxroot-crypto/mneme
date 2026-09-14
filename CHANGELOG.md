# Changelog

本项目的所有重要变更记录于此文件。

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

<!-- 项目尚未发布;开发期里程碑记录在 [Unreleased] 段,正式发布时再按 Keep a Changelog 补齐版本号与日期。 -->

## [Unreleased]

### Added

- **L0–L6 核心**:`core/` 原语、`memory/` 内存引擎(公开 API 冻结)、
  `persist/` WAL/不可变段/MANIFEST/崩溃恢复、`index/` 自研 HNSW 与 hidx 持久化、
  `query/` 过滤 DSL/BM25/RRF/加权融合、`life/` TTL/遗忘曲线/size-tiered compaction/
  命名空间/快照备份、`quant/` i8/f16 量化副本与两阶段精排、`feature = "async"` 门面。
- **记忆模型与排序**:关系图与自定义关系注册表、双时态 `as_of`/`supersede`、
  来源/可信度、记忆沉淀;综合打分、候选放大与偏置路由、联想扩展、反馈闭环、MMR。
- **存储安全**(feature `encrypt`/`compress`/`compress-zstd`):AES-256-GCM
  整文件/整帧信封静态加密与密钥轮换(`Mneme::rotate_encryption_key`);
  记录体 `text`/`meta`/`provenance` 按字段压缩,无收益自动回退原文。
- **部署形态**:`Storage`/`FsStorage`/`MemStorage` 存储后端、多进程只读共享与
  周期刷新、`Observer` 事件钩子、`feature = "wasm"` 目标;段句柄惰性驻留。
- 工程化与文档:`.gitlab-ci.yml` 五档(fast/middle/heavy/fuzz/mutation)、
  `mutants.toml`、mdBook 设计文档、Rust 零基础教学(11 章)、
  FC-Matrix 契约矩阵与 `tests/contract_traceability.rs` 追溯门禁。

### Changed

- 文件格式版本推进到 `FORMAT_VERSION = 0x0006`:msec 记录体新增恒 0 的 `flags2`
  字节(承载 text/meta/provenance 压缩位;未压缩时字段编码与旧定义一致,仅多该字节)。

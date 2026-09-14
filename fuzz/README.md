# Mneme fuzz 目标

本目录是**独立 workspace**(`[workspace]` 空表),`cargo test` 不会编译它;
运行需要 nightly 工具链与 [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz)。

## 目标

| 目标 | 入口 | 断言 |
|---|---|---|
| `fuzz_vsec` | `mneme::fuzzing::parse_vsec` | 任意输入不 panic;失败返回结构化错误 |
| `fuzz_msec` | `mneme::fuzzing::parse_msec` | 同上 |
| `fuzz_hidx` | `mneme::fuzzing::decode_hidx` | 同上 |
| `fuzz_wal_replay` | `mneme::fuzzing::replay_wal` | 任意 WAL 字节回放不 panic(含极大 `rowid`/`ns_id`,`FC-PERSIST-ERR-012`) |
| `fuzz_dsl` | `mneme::fuzzing::parse_dsl` | 非 UTF-8 / 任意 DSL 不 panic(I7) |

入口经主 crate 的 `feature = "fuzzing"` 暴露(见 `src/fuzzing.rs`),不改变运行时行为。
各目标在解析入口之外统一调用 `mneme::fuzzing::version_injection`:**改写
`format_version` 必须返回 `UnsupportedVersion`,改写魔数必须返回 `Corrupted`**
(I18,设计 14 §5)。1h/24h 长跑用 `scripts/run_long.sh`(nightly + cargo-fuzz)。

## 运行

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run fuzz_vsec          # 单个目标
cargo +nightly fuzz list                   # 列出全部目标
DURATION=3600 ../fuzz/scripts/run_long.sh  # 全目标各 1h(夜跑)
```

仓库内快速兜底:`src/fuzzing.rs` 的冒烟单测随 `cargo test --features fuzzing` 运行
(含版本注入定向用例)。

## 产物

`target/`、`corpus/`、`artifacts/`、`coverage/`、`Cargo.lock` 均为本地生成,已在
`.gitignore` 忽略,不提交。

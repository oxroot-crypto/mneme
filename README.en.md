<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg" />
  <img src="docs/assets/logo-light.svg" alt="Mneme logo" width="96" />
</picture>

# Mneme

**Embedded vector store for AI-agent long-term memory · Rust · no server**

[![License](https://img.shields.io/badge/license-Unlicense-blue?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.93%2B-orange?style=flat-square)](Cargo.toml)
[![GitHub](https://img.shields.io/badge/GitHub-oxroot--crypto%2Fmneme-181717?style=flat-square&logo=github)](https://github.com/oxroot-crypto/mneme)

</div>

---

English | [简体中文](README.md)

**Mneme** (μνήμη, ancient Greek for "memory"; same root as Mnemosyne) is a pure-Rust **embedded vector store** built for the **long-term memory layer of AI agents**: it runs in-process, needs no server, and stays under control as data grows over years.

LLMs forget everything outside the context window once a conversation ends. An agent that runs for months needs an external memory that retrieves semantically, forgets trivia, and keeps working for a decade. Mneme is that engine.

**Status**: version `0.1.0`. L0–L6 are implemented and verified against formal contracts; the 1M×1536 performance gates and long fuzz runs are executed manually on a local or dedicated runner (not in CI; the official gate needs a ≥16GB runner). Design and acceptance criteria: [docs/DESIGN.md](docs/DESIGN.md).

## 📑 Table of Contents

- [Features](#-features)
- [Architecture](#-architecture)
- [Performance](#-performance)
- [Installation](#-installation)
- [Quick Start](#-quick-start)
- [Usage](#-usage)
- [Configuration](#-configuration)
- [Documentation](#-documentation)
- [Development](#-development)
- [Contributing](#-contributing)
- [License](#-license)
- [Acknowledgments](#-acknowledgments)

## ✨ Features

- **Embedded** — a local directory is the database; no server, no network, no ops.
- **Agent memory semantics** — namespaces, TTL, importance, forgetting curve and dedup are first-class.
- **Hybrid retrieval** — vector ANN plus BM25 and a filter DSL, fused with RRF or weighted scoring.
- **Memory-aware ranking** — similarity combined with recency, importance, access frequency and confidence.
- **Durable by design** — WAL, immutable segments, atomic MANIFEST; recoverable from any crash point.
- **Built for the long term** — O(log N) active segments, bounded memory, evolvable on-disk format.
- **Minimal dependencies** — four non-optional direct crates (`serde`, `serde_json`, `thiserror`, `crc32fast`; `mmap` adds `memmap2`); all heavy algorithms are in-house.

## 🏗️ Architecture

The engine is implemented as seven layers, each one usable on its own. Dependencies only point downwards (L0 → L6).

| Layer | Module | Provides |
|---|---|---|
| L0 | `src/core/` | IDs, errors, distance metrics, SIMD dot product, TopK heap, bitset, varint, metadata, option types |
| L1 | `src/memory/` | In-memory tables, brute-force search, filter AST, dedup, lifecycle, public API |
| L2 | `src/persist/` | WAL, immutable segment files, MANIFEST, crash recovery, incremental segment flush |
| L3 | `src/index/` | In-house HNSW (filter-aware), `hidx` persistence, mmap segment reads |
| L4 | `src/query/` | Filter DSL, query planner, BM25, RRF/weighted fusion, execution pipeline |
| L5 | `src/life/` | Size-tiered compaction, background maintenance, TTL pruning, snapshots, backups |
| L6 | `src/quant/` | i8/f16 quantized copies, two-stage rescoring, `AsyncNamespace` under `async` |

Capabilities in detail:

| Capability | Description |
|---|---|
| Vector search | Cosine / dot / Euclidean; in-house HNSW, automatic brute force for small segments |
| Hybrid retrieval | Filter DSL + BM25 + RRF/weighted fusion + result dedup + MMR diversity |
| Memory-aware ranking | Configurable mix of similarity, recency, importance, access count, confidence; associative expansion |
| Memory model | Relation graph, bi-temporal `as_of` time travel (history kept forever by default), `supersede` belief revision, provenance/confidence, consolidation |
| Lifecycle | Two-phase TTL expiry, exponential forgetting curve, access reinforcement, namespace isolation (auto-forget off by default) |
| Persistence | WAL + immutable segments + atomic MANIFEST; deletes never resurrect |
| Long-term growth | Size-tiered compaction; O(log N) write amplification; bounded active segment count |
| Quantization | i8 / f16 quantized copies with two-stage rescoring; query bandwidth ÷4 (i8) and ÷2 (f16); automatic fallback to f32 when sampled recall misses the floor |
| Storage security | AES-256-GCM encryption at rest (`encrypt`), key rotation, text/metadata compression (`compress` / `compress-zstd`) with fallback to raw bytes |
| Deployment | Multi-process read-only sharing (`Mneme::reload`), `Storage`/`FsStorage`/`MemStorage` backends for WASM/edge, `Observer` event hooks |

## 📊 Performance

> 50,000 rows × 128 dims · 1,000 queries · top-10 · L2² · `M=16` / `ef_construction=200` ·
> 4-core Intel Xeon 8255C. Medians over multiple interleaved rounds (round-to-round spread ±3–8%);
> **Mneme's build time is the real ingestion path** (`insert_batch` + `flush`: WAL, segment files,
> MANIFEST and graph persistence), while the other engines build purely in memory.
> Full methodology, recall tables and raw data: [`comparison/README.md`](comparison/README.md).

### Build time (4 threads, lower is better)

```text
usearch         ██ 3.76 s
hnsw_stable     ██████ 8.66 s
mneme i8        ███████ 10.52 s
hnsw_rs         ███████ 10.61 s
mneme (f32)     ███████ 10.85 s
instant_dist    ██████████████████████████████████████████████ 69.32 s
```

### Single-thread query latency, P50 (ef=128, lower is better)

```text
usearch         █████████████████ 189 µs
mneme i8        █████████████████ 193 µs
mneme (f32)     ███████████████████████████ 295 µs
hnsw_stable     ██████████████████████████████████ 377 µs
hnsw_rs         ████████████████████████████████████████ 441 µs
```

### 4-thread aggregate throughput (ef=128, higher is better)

```text
usearch         ████████████████████████████████████████ 17,020 QPS
mneme i8        ████████████████████████████████ 13,699 QPS
mneme (f32)     ████████████████████████ 10,095 QPS
hnsw_rs         █████████████ 5,599 QPS
hnsw_stable     █████████████ 5,502 QPS
```

### Key numbers

| Engine | ef | Recall@10 | P50 | 4-thread QPS | RSS delta |
| --- | ---: | ---: | ---: | ---: | ---: |
| usearch | 128 | 0.9996 | 189 µs | 17,020 | 37.2 MiB |
| **Mneme (i8, opt-in)** | 128 | 0.9980 | **193 µs** | **13,699** | 103.7 MiB |
| **Mneme (default f32)** | 128 | 0.9980 | 295 µs | 10,095 | 97.3 MiB |
| hnsw_stable | 128 | 1.0000 | 377 µs | 5,502 | 39.4 MiB |
| hnsw_rs | 128 | 0.9799 | 441 µs | 5,599 | 134.3 MiB |
| instant_distance | 64 | 0.9999 | 296 µs | 12,453 | 67.4 MiB |

### How to read this

- **Build**: Mneme (10.9 s) is on par with hnsw_rs (10.6 s), behind hnsw_stable (8.7 s) and
  usearch (3.8 s) — but it is the only engine that writes WAL, immutable segments, MANIFEST
  and the graph to disk and can recover from any crash point. `insert_batch` (WAL append)
  takes just 0.45 s; the rest is graph construction and encoding inside `flush`.
- **Query**: the default f32 mode matches hnsw_stable (295 µs vs 377 µs). Enabling the
  optional i8 quantized copy (`.quantization(VectorFormat::I8Rescored)`) brings P50 down to
  193 µs, essentially level with the fastest C++ implementation (usearch), for a recall cost
  of ≤0.001 and +36% 4-thread throughput.
- **Recall**: first tier — 0.9980 at ef=128 and 1.0000 at ef=256; hnsw_rs trails by ~2 points.
- **In-memory mode**: a `Builder` without `path` is a pure in-memory store. After
  `flush()` it builds an in-memory HNSW segment (no files, no quantized copy), so
  queries and recall match the persistent store while saving the persistence cost
  (build ~−7%, RSS ~−17%). Without `flush()` an in-memory store answers by exact
  brute force (20k×128: P50 ≈ 0.7 ms), so flush large in-memory stores first.
- On uniform random high-dimensional data (the hardest ANN regime) recall collapses for every
  engine as expected, and Mneme has the highest recall at every ef. See
  [`comparison/README.md`](comparison/README.md) for details.

## 📦 Installation

### Requirements

| Dependency | Minimum | Notes |
|---|---|---|
| Rust | 1.93 (edition 2024) | declared as `rust-version` in `Cargo.toml` |
| C toolchain | — | only for the optional `compress-zstd` feature (`zstd-sys` compiles C sources) |

### Add the dependency

The name `mneme` is already taken on crates.io, so the published package is `mneme-db`.
The library (crate) name remains `mneme`, so keep writing `use mneme::...` in code.

```toml
[dependencies]
mneme-db = "0.1"                                           # published name (library name: mneme)
# mneme-db = { path = "../mneme" }                         # or a local clone via path
# mneme-db = { git = "https://github.com/oxroot-crypto/mneme" }  # or via git
```

## 🚀 Quick Start

### 1. Create a demo crate

```bash
cargo new agent-memory && cd agent-memory
cargo add mneme-db                  # library name stays mneme; keep use mneme::...
```

### 2. Write `src/main.rs`

```rust
use mneme::{FsyncPolicy, Metric, Mneme, Record};
use std::time::Duration;

fn main() -> mneme::Result<()> {
    // Open (or create) a local directory as the memory store
    let db = Mneme::builder()
        .path("./agent_memory")                       // omit path for an in-memory (volatile) store
        .dimension(4)                                 // required on create; use your embedding dimension, e.g. 1536
        .metric(Metric::Cosine)                       // default: Cosine
        .fsync(FsyncPolicy::Batched(Duration::from_millis(20)))
        .build()?;

    let ns = db.namespace("agent-42/profile");        // hierarchical namespace

    // Remember two things (vectors come from your embedding model)
    ns.insert(
        Record::new(vec![1.0, 0.0, 0.0, 0.0])
            .key("pref.theme")                        // optional external key; duplicates upsert
            .text("the user prefers dark mode")       // optional; enables BM25 and text dedup
            .importance(0.8),
    )?;
    ns.insert(
        Record::new(vec![0.0, 1.0, 0.0, 0.0])
            .key("pref.os")
            .text("the user runs macOS")
            .importance(0.6),
    )?;

    // Recall related memories
    let hits = ns
        .search()
        .vector(&[1.0, 0.0, 0.0, 0.0])
        .top_k(2)
        .execute()?;
    for hit in &hits {
        let key = hit.key.as_ref().map(|k| k.as_str()).unwrap_or("-");
        println!("{key} {:.3}", hit.score);
    }

    db.close()?;                                      // graceful shutdown: flush + release the file lock
    Ok(())
}
```

### 3. Run it

```bash
cargo run
```

```text
pref.theme 1.000
pref.os 0.000
```

Full API semantics, configuration, error handling and backup/restore are documented in [16 Public API & Operations Reference](docs/design/16-api-reference.md).

## 📖 Usage

All snippets below reuse the `db` and `ns` handles from the quick start.

### Hybrid retrieval: vector + keywords + filter + composite scoring

```rust
use mneme::{filter, Diversity, Scoring};

// q comes from your embedding model
let hits = ns
    .search()
    .vector(&q)                                      // vector channel
    .text("dark mode")                               // BM25 channel; RRF fusion by default (k = 60)
    .filter(filter!(r#"kind == "preference""#))      // filter! for literals; Expr::from_str for runtime input
    .score(Scoring { w_recency: 0.2, w_importance: 0.3, ..Scoring::default() })
    .diversify(Diversity::Mmr { lambda: 0.7 })       // MMR diversity
    .top_k(10)
    .execute()?;
// hits are sorted by composite score; Hit carries no vector, use ns.get_vector(hit.rowid) if needed
```

### Lifecycle: TTL, reinforcement and deliberate forgetting

```rust
use mneme::{filter, Retention};
use std::time::Duration;

// TTL: relative duration at write time, stored as an absolute expires_at
ns.insert(
    Record::new(vec![0.0, 0.0, 1.0, 0.0])
        .key("scratch-1")
        .metadata(mneme::json!({"kind": "scratch"}))
        .ttl(Duration::from_secs(7 * 86400)),
)?;
ns.touch("pref.theme", Some(0.1))?;                  // access reinforcement
let forgotten = ns.forget(filter!(r#"kind == "scratch""#))?;   // tombstones; compaction reclaims space

// Forgetting curve: half-life + importance floor + protected set (auto-forget is off by default)
let report = ns.retain(
    Retention::new()
        .half_life(Duration::from_secs(14 * 86400))
        .min_importance(0.2)
        .protect(filter!(r#"kind == "decision""#)),
)?;
// forgotten == 1, report.forgotten == 0
```

### Time travel and belief revision

```rust
// Transaction-time snapshot: take a handle, keep writing, the handle stays fixed
let t1 = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_millis() as i64;
std::thread::sleep(Duration::from_millis(5));
ns.insert(Record::new(vec![0.0, 1.0, 0.0, 0.0]).key("pref.os"))?;

let old = db.as_of(t1)?;                             // snapshot handle, cheap to read repeatedly
let old_ns = old.namespace("agent-42/profile");
let old_hits = old_ns.search().vector(&[1.0, 0.0, 0.0, 0.0]).top_k(10).execute()?;
assert_eq!(old_hits.len(), 1);                       // only pref.theme existed at t1

// Belief revision: the previous version gets its valid_to closed; history is kept forever by default
ns.supersede(
    "pref.theme",
    Record::new(vec![0.0, 0.0, 0.0, 1.0]).valid_from(1_700_000_000_000),
)?;
```

### Encryption at rest and compression (features)

```toml
[dependencies]
mneme-db = { version = "0.1", features = ["encrypt", "compress"] }
```

```rust
use std::sync::Arc;
use mneme::{Cipher, Compression, CryptoKey, Encryption, KeyId, Keyring};

// Keyring is the built-in in-memory key provider; production can plug in env vars, keychain or KMS
let keyring = Arc::new(Keyring::new(KeyId(1), CryptoKey::generate()?));
let db = Mneme::builder()
    .path("./encrypted_memory")
    .dimension(1536)
    .encryption(Some(Encryption { provider: keyring.clone(), cipher: Cipher::Aes256Gcm }))
    .compression(Compression::Lz4)
    .build()?;
let new_key = db.rotate_encryption_key()?;           // key rotation: rewrites all segments
```

### End-to-end example: interactive memory store with a third-party embedding API (OpenAI protocol)

`examples/memory` is an interactive REPL: it calls an OpenAI-compatible embeddings endpoint
(OpenAI official / OpenRouter / vLLM / Ollama's `/v1` / LM Studio), stores `/add`-ed text into
Mneme, and serves `/search` with hybrid vector + BM25 retrieval. The `EmbeddingProvider` trait
is the extension point for other protocols.

```bash
# Put a .env at the repo root (gitignored) or export: EXAMPLE_EMBEDDING_API_KEY=sk-...
cargo run --example memory
# For compatible endpoints add: MNEME_EMBEDDING_BASE_URL / MNEME_EMBEDDING_MODEL
#
# REPL commands: /add, /search, /get, /list, /touch, /delete, /help, /quit
```

The example ships offline path tests (mock endpoint: shuffled responses, 401, malformed JSON,
connection loss, dimension mismatch, atomic batches, persistence reopen, command parsing):
`cargo test --example memory`.

## ⚙️ Configuration

Every knob has a default; tune after reading `db.stats()`. The full table, including `HnswParams`, `CompactionPolicy`, `Tuning` and `Limits`, lives in [16 §2](docs/design/16-api-reference.md).

| Option | Type | Default | Description |
|---|---|---|---|
| `.path` | `impl AsRef<Path>` | none (in-memory) | one directory = one store |
| `.dimension` | `u32` | required on create | 1..=65536, immutable after create |
| `.metric` | `Metric` | `Cosine` | `Cosine` / `Dot` / `Euclidean` |
| `.fsync` | `FsyncPolicy` | `Batched(20ms)` | `Always` / `Batched` / `OnFlush` / `Never` |
| `.insert_mode` | `InsertMode` | `Upsert` | same-key behavior: `Upsert` / `RejectDuplicate` |
| `.dedup` | `Dedup` | `Off` | `Off` / `Reject` / `Replace` / `KeepBoth` / `Merge` |
| `.dedup_threshold` | `f32` | `0.95` | near-duplicate threshold, always cosine-based |
| `.quantization` | `VectorFormat` | `F32` | `F32` / `F16` / `I8Rescored` (persistent stores only) |
| `.hnsw` | `HnswParams` | `m=16, m0=32, ef_construction=200, ef_search=64` | out-of-range values are rejected at build time |
| `.build_precision` | `BuildPrecision` | `Hybrid` | traversal on temporary i8 codes, neighbour selection rescored in f32; `F32` keeps the exact old behavior |
| `.compaction` | `CompactionPolicy` | see [16 §2](docs/design/16-api-reference.md) | `tier_ratio=4`, `dead_ratio=0.25`, `segment_rows=8192`, `history_horizon=None` (forever) |
| `.retention` | `Option<Retention>` | `None` | background auto-forget is off unless set |
| `.access_flush_interval` | `Duration` | `30s` | flush period for batched access counters |
| `.compression` | `Compression` | `None` | `Lz4` (`compress`) / `Zstd` (`compress-zstd`) |
| `.encryption` | `Option<Encryption>` | `None` | encryption at rest (needs feature `encrypt`) |
| `.storage` | `Arc<dyn Storage>` | `FsStorage` | custom backends such as `MemStorage` |
| `.read_only` | `bool` | `false` | multi-process read-only sharing |
| `.read_only_probe_interval` | `Duration` | `1s` | MANIFEST probe period for read-only instances; `ZERO` disables probing |
| `.relation_index` | `RelationIndex` | `Outgoing` | `Outgoing` / `Both` |
| `.parallelism` | `usize` | `0` (auto) | worker threads; 0 = available cores |
| `.maintenance` | `bool` | `true` | `false` gates the background maintenance thread (bulk import) |
| `.observer` | `Arc<dyn Observer>` | none | Query / Write / Flush / Compaction / Error event hooks |
| `.verify_on_open` | `bool` | `false` | full payload CRC verification on open (slow) |
| `.fail_fast_on_corruption` | `bool` | `false` | refuse to start on a corrupt segment instead of quarantining it |

### Cargo features

| Feature | Default | Description |
|---|---|---|
| `mmap` | on | zero-copy mmap segment reads; off falls back to `Read + Seek` |
| `async` | off | `AsyncNamespace` async facade (`spawn_blocking`); the core has zero tokio |
| `quant-f16` | off | f16 quantized copies; without it `F16` returns `Unsupported` at build time |
| `fuzzing` | off | parse entry points for the `fuzz/` targets; no runtime behavior change |
| `encrypt` | off | AES-256-GCM envelope encryption for segments/WAL/MANIFEST; see `Mneme::rotate_encryption_key` |
| `compress` | off | `text`/`meta`/`provenance` compression with the built-in LZ4-style codec (zero dependencies) |
| `compress-zstd` | off | optional stronger compression via `zstd` |
| `wasm` | off | disables mmap and background threads for WASM targets; pair with `MemStorage` |

## 📚 Documentation

| Document | Content |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | Entry point: layer map, reading paths, documentation conventions |
| [Developer Guide](docs/guide.md) | **For users**: from adding the dependency to running in production |
| [00 Fundamentals](docs/design/00-fundamentals.md) | Embeddings, similarity, ANN, WAL/MVCC and the rest of the background |
| [01 Overview](docs/design/01-overview.md) | Positioning, architecture, dependency whitelist, public API tour |
| [02–08 Layer designs](docs/design/02-l0-core.md) | L0 primitives → L6 quantization, with full mathematical derivations |
| [14 Testing & acceptance](docs/design/14-testing.md) | Crash injection, recall property tests, benchmarks, fuzz, long runs |
| [15 Glossary](docs/design/15-glossary.md) | Bilingual terms, symbols, complexity cheat sheet |
| [16 API & operations](docs/design/16-api-reference.md) | Full API, configuration table, errors/retries, thread safety, backup/restore |
| [09 Memory model](docs/design/09-memory-model.md) | Relation graph, bi-temporality, provenance/confidence, consolidation |
| [10 Memory-aware scoring](docs/design/10-scoring.md) | Composite scoring, associative expansion, feedback loop, MMR |
| [11 Security & storage](docs/design/11-security-storage.md) | Encryption at rest, text/metadata compression |
| [12 Deployment](docs/design/12-deployment.md) | Multi-process read-only, WASM, observability |
| [13 Cookbook](docs/design/13-cookbook.md) | Agent memory recipes you can copy directly |
| [spec/contracts.md](docs/spec/contracts.md) | Formal contract matrix (FC-Matrix) |
| [comparison/README.md](comparison/README.md) | Cross-engine benchmarks vs usearch / hnsw_rs / hnsw-stable / instant-distance (methodology, full tables, reproduction) |
| [CHANGELOG.md](CHANGELOG.md) | Release notes (Keep a Changelog format) |
| [rust/README.md](docs/rust/README.md) | **Rust from zero** (11 chapters) using this source tree as the textbook |

The design book is Chinese-only at the moment.

## 🧪 Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                       # unit + integration + doc tests
cargo doc --no-deps              # rustdoc gate: #![deny(missing_docs)]
cargo bench                      # criterion benchmarks (benches/hnsw.rs, benches/quant.rs)
```

Feature-specific test runs:

```bash
cargo test --features async
cargo test --features quant-f16
cargo test --all-features
```

Heavy gates (manual, not in CI; the official 1M×1536 gate needs a ≥16GB runner):

```bash
MNEME_HEAVY=1 cargo test --release --test cold_start --test heavy_gate -- --ignored
cargo test --release                       # FC-GLOBAL-CPLX-001 complexity operation counts
DURATION=3600 ./fuzz/scripts/run_long.sh   # long fuzz run (nightly + cargo-fuzz)
cargo mutants --no-shuffle --timeout 300   # mutation testing (mutants.toml)
```

Sizes can be overridden with `MNEME_HEAVY_ROWS` / `MNEME_HEAVY_DIM`; `MNEME_FLUSH_CHUNK_ROWS` / `MNEME_FLUSH_THREADS` tune bulk-import chunking and block-level parallelism.

Build the documentation book with [mdBook](https://rust-lang.github.io/mdBook/):

```bash
cargo install mdbook mdbook-mermaid
# Windows MSVC cannot compile mdbook-katex's default quick-js backend; use duktape instead:
cargo install mdbook-katex --no-default-features --features duktape
mdbook-mermaid install .   # first run: copies mermaid assets and updates book.toml
mdbook serve               # preview at http://localhost:3000
mdbook build               # output in book/
```

The project CI (`.gitlab-ci.yml` at the repository root) runs two light tiers: `fast` on every push (fmt + clippy + unit tests + feature matrix + rustdoc gate + wasm32 build check) and `middle` on pull requests (full L2–L6 contract integration plus f16/encrypt cross-feature matrices). The heavy gates, long fuzz runs and mutation testing stay manual.

MSRV is **1.93** (edition 2024), declared in `Cargo.toml`; CI builds and tests on the `rust:1.93` image.

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). In short:

1. Contract-first: any change to disk formats, API semantics, state transitions, concurrency or errors updates [spec/contracts.md](docs/spec/contracts.md) first, then tests, then code.
2. New external dependencies must be justified in the pull request; HNSW, BM25, quantization, bloom, compaction and tokenization are in-house by policy.
3. Commits follow [Conventional Commits](https://www.conventionalcommits.org/) with the first line ≤ 72 characters.

Issues and pull requests: [github.com/oxroot-crypto/mneme](https://github.com/oxroot-crypto/mneme/issues).

## 📄 License

[The Unlicense](LICENSE) — released into the public domain.

## 🙏 Acknowledgments

- **All** code and documentation in this repository were generated by AI: **deepseek-v4.1-flash** and **glm-5.3-flash**; the project name **Mneme** was chosen by **gemini-3.8-flash**; see [DISCLOSURE.en.md](DISCLOSURE.en.md).
- The name comes from **μνήμη** (*mnḗmē*), the Greek word for memory and the root of Mnemosyne.
- Optional capabilities lean on small, focused crates: [`memmap2`](https://crates.io/crates/memmap2), [`half`](https://crates.io/crates/half), [`tokio`](https://crates.io/crates/tokio) (`rt` only), [`aes-gcm`](https://crates.io/crates/aes-gcm), [`getrandom`](https://crates.io/crates/getrandom) and [`zstd`](https://crates.io/crates/zstd).
- Tests and benchmarks use [`proptest`](https://crates.io/crates/proptest), [`tempfile`](https://crates.io/crates/tempfile) and [`criterion`](https://crates.io/crates/criterion); the end-to-end example (`examples/memory`) calls the embeddings API via [`async-openai`](https://crates.io/crates/async-openai) (all dev-dependencies).

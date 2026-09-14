# 11 存储安全与压缩:静态加密、文本压缩

> **本章目标**:补齐产品化的两块存储能力——**数据静止加密**(Agent 记忆含用户隐私)
> 与**文本/元数据压缩**(超长期下文本是体积大头),同时守住"默认零成本、依赖可选"。
> **前置阅读**:[04](04-l2-persist.md)(文件布局/WAL/MANIFEST)、[02 §7](02-l0-core.md)(meta 隔离)、[01 §5](01-overview.md)(依赖白名单)。
> **本章你将学到**:威胁模型与边界 → 整文件/整帧 AEAD 加密与密钥提供者 → 密钥轮换 →
> 文本压缩与 BM25 的交互 → 层边界契约。
>
> 两者均为**可选 feature**,默认关闭时磁盘布局与 [04](04-l2-persist.md) 定义一致
> (压缩仅多一个恒 0 的 msec `flags2` 字节,`FORMAT_VERSION = 0x0006`),
> 依赖白名单不扩大。
>
> **落地状态(2026-09,已落地)**:`src/crypto/`(feature `encrypt`)提供
> `KeyId`/`Key`(导出名 `CryptoKey`)/`Cipher`/`KeyProvider`/`Encryption`/`Keyring`;
> 段/WAL/MANIFEST 写盘为**自描述整文件/整帧 AEAD 信封**(`[MNEC][版本][key_id]
> [明文长度][nonce][密文][tag]`,AAD 绑定用途/标识/格式版本),读路径自动解密;
> 加密段走自有缓冲(mmap 失效,与设计取舍一致)。`Mneme::rotate_encryption_key()`
> 以 provider 轮换 + 全量段重写完成迁移,`stats().storage` 报 `encryption` 与
> `migrated_segments/total_segments`。`src/compress/`(feature `compress` /
> `compress-zstd`)对记录体 `text`/`meta`/`provenance` 按字段压缩,自描述 codec 与
> 原始长度、无收益回退原文;`Compression::None` 语义不变(记录体新增恒 0 的
> `flags2` 字节,`FORMAT_VERSION = 0x0006`)。`FC-SEC-*` 均已转正,见
> `tests/security_contracts.rs`。
>
> **实现口径与目标设计的差异**(登记):采用**整文件/整帧信封**而非页级加密
> (加密段已放弃 mmap,页级零拷贝无收益);头部定长字段也随之密文化(而非保留明文),
> 版本/维度在解密后校验(`I18` 语义不变)。

模块:`src/crypto/mod.rs`(feature `encrypt`)、`src/compress/mod.rs` + `src/compress/lz4.rs`(feature `compress` / `compress-zstd`)

---

## 1. 威胁模型与默认

**保护什么**:磁盘/备份介质被非授权读取(离线攻击、误拷贝、云盘泄漏)时,记忆内容不可读。

**不保护什么**(诚实标注,与 [01 §1.2](01-overview.md) 一致):

- 运行中进程内存、`db` 句柄可读到的明文(需要 OS 级隔离);
- 拥有密钥的合法调用方;
- 侧信道(访问模式、段大小)与流量分析;
- 恶意篡改:AEAD 会检出不匹配,但本设计不承诺抗回滚攻击(攻击者用旧密文替换新密文);
  长期归档完整性可叠加 `backup_to` 多版本([07 §6](07-l5-life.md))。

**默认**:`Encryption = None`、`Compression = None`。开启加密需要显式提供 `KeyProvider`。

---

## 2. 静态加密(feature `encrypt`)

### 2.1 【直觉】把每个数据块锁进带校验的保险箱

AES-256-GCM 是**带认证的加密(AEAD)**:加密数据的同时生成认证标签,解密时校验
"密钥不对 / 数据被改"都会失败——天然满足不变量 I2(不静默返回错误数据)的加密版本。

### 2.2 单元与布局(整文件/整帧信封)

| 单元 | 加密粒度 | 说明 |
|---|---|---|
| 段文件(vsec/msec/hidx) | 整文件信封 | 固定头字段(维度/度量/版本)也密文化,版本/维度在解密后校验 |
| WAL 帧 | 每帧 payload 信封 | 帧头(crc/len/seqno/type)保留明文,便于撕裂写定位与回放 |
| MANIFEST | 整文件信封 | 段表/命名空间注册表/关系类型注册表随密文一并保护 |
| 关系段(edges) | 整文件信封 | 与段文件同口径 |

```text
信封布局:
[MNEC 4B][format_version u16][key_id u32][plaintext_len u32][nonce 12B][ciphertext][tag 16B]
AAD = (用途标签, 段号/版本标识, 上述定长头)   # 绑定位置与长度,防跨文件搬运
```

- **整文件/整帧信封**使加密段无法 mmap 零拷贝(需整段解密),开启加密后该段自动走
  自有缓冲解码路径([04 §11](04-l2-persist.md)),读吞吐下降(经验值 1.5–3×);
  这是安全换性能的显式取舍;[01 §1.1](01-overview.md)/[14 §4](14-testing.md) 的
  性能目标默认在**未加密**下衡量,加密开启后需重新基准;
- 版本/key_id/明文长度/nonce 全部参与 AAD:版本不符 → `UnsupportedVersion`,
  字段被改或密钥不对 → `Corrupted`,绝不按明文误读(I18);
- 页级方案(每 64 KiB 页一个 AEAD 记录)是设计初稿,已废弃,差异见本章开头
  "实现口径与目标设计的差异"。

### 2.3 密钥提供者

```rust
/// 密钥标识:写入信封头,解密时按 id 向 provider 取密钥。
pub struct KeyId(pub u32);
/// 32 字节对称密钥(刻意不实现打印明文内容的 `Debug`);以 `mneme::CryptoKey` 导出。
pub struct Key([u8; 32]);
impl Key {
    pub fn from_bytes(bytes: [u8; 32]) -> Self;
    #[cfg(feature = "encrypt")]
    pub fn generate() -> Result<Self>;            // OS 熵源(CSPRNG)
}
/// 支持的 AEAD 算法(当前仅 AES-256-GCM)。
pub enum Cipher { Aes256Gcm }

pub trait KeyProvider: Send + Sync {
    /// 当前用于写入的密钥及其 id(用于把 key_id 写入信封头,解密时按 id 取密钥)。
    fn active_key(&self) -> KeyId;
    fn key(&self, id: KeyId) -> Result<Key>;      // 32 字节
    fn rotate(&self) -> Result<KeyId>;            // 生成新密钥并设为 active(可选能力,默认 Unsupported)
}
pub struct Encryption { pub provider: Arc<dyn KeyProvider>, pub cipher: Cipher }  // Cipher::Aes256Gcm

/// 内存密钥环(测试与宿主便捷实现):多密钥共存、active 切换与退役。
pub struct Keyring { /* .. */ }
impl Keyring {
    pub fn new(id: KeyId, key: Key) -> Self;      // 单密钥即 active
    pub fn insert(&self, id: KeyId, key: Key);    // 登记历史密钥(不改变 active)
    pub fn retire(&self, id: KeyId) -> bool;      // 退役(active 密钥不可退役)
}
// `Keyring` 实现 `KeyProvider`,可直接用于 `Builder::encryption`。
```

- 引擎**不管理密钥文件**,只经 `KeyProvider` 取密钥;密钥可来自环境变量、OS keychain、
  KMS 或宿主自管(依赖不进入引擎);
- 每个段/WAL/MANIFEST 的**信封头**记录 `key_id`(见 §2.2 布局);
  解密按 id 查 provider,支持旧密钥仍可读(轮换迁移期)。

### 2.4 密钥轮换

```text
1. provider.rotate() → 新 key_id
2. Mneme::rotate_encryption_key() 以"全部活跃段"为计划强制重写:
   旧段解密 → 新密钥加密 → 提交新 MANIFEST(复用 compaction 段组替换流程,旧段入 trash/)
3. 全部段迁移完成后,宿主可 Keyring::retire(旧 key_id) 退役旧密钥
```

轮换**已落地**(`FC-SEC-POST-001`):`provider.rotate()` 获取新密钥后全量段重写,
迁移期间新旧密钥均可读(`KeyProvider` 需同时持有两者);`db.stats().storage` 的
`migrated_segments/total_segments` 以信封头 `key_id` 是否等于 active 实计。
纯内存库 → `Unsupported`,库未启用加密 → `Config`。项目未发布期不保留混合版本兼容。

### 2.5 复杂度与不变量

- 加密/解密吞吐取决于 AES-NI(经验值数 GB/s),相对磁盘带宽通常不是瓶颈;
- **不变量 I28**:开启加密后,磁盘上任何段/WAL/MANIFEST 的密文区不含明文记录字段;
  `db.check()` 校验每个信封的认证标签,篡改/错误密钥 → `Corrupted`,绝不返回错误数据。

---

## 3. 文本与元数据压缩(feature `compress`)

### 3.1 【直觉】文本比向量更占地方

[01 §1.2](01-overview.md) 已指出:向量只占 4 字节/分量,而对话原文、笔记、摘要动辄几百字节
到几 KB。超长期下 `text`/`meta` 才是体积大头。压缩直接放大"十年记忆"的容量上限。

### 3.2 编解码抽象

```rust
/// 单字段压缩/解压抽象(pub(crate) 内部细节,不是公开 API)。
pub(crate) trait Codec: Send + Sync {
    fn compress(&self, src: &[u8]) -> Vec<u8>;
    fn decompress(&self, src: &[u8], expected_len: usize) -> Result<Vec<u8>>;
}
pub enum Compression { None, Lz4, Zstd }   // 默认 None;`Lz4` 为内置自研实现(feature `compress`),`Zstd` 需 feature `compress-zstd`
```

- 压缩作用于**记录体内的 `text`/`meta`/`provenance` 字段**(见 [04 §2.2](04-l2-persist.md) 的 entry 变长区),
  按字段独立压缩,压缩 blob 自描述 codec id 并带 `uncompressed_len` 前缀;向量/norm 不压缩
  (已定长且量化另有手段);
- 记录体新增恒 0 的 `flags2` 字节标记三个字段是否压缩(`FORMAT_VERSION = 0x0006`);
  `Compression::None` 时字段布局与未压缩定义逐字节一致;
- 可选 feature `compress-zstd` 允许接入更强 codec,默认不引入依赖。

### 3.3 与 BM25 / 过滤的交互

- **倒排索引在 flush 时构建**,那时文本已解压,因此 BM25 检索**无需解压**——只读倒排;
- **残留谓词过滤**(行级 JSON)需要解压 meta;由于 zone map/bloom 已先剪枝
  ([04 §5](04-l2-persist.md)),只有候选块内的少量行会被解压;
- 返回 `RecordRef` 时,`text`/`metadata` 按需解压(可缓存最近解压结果,LRU 由宿主控制);
- 压缩使 `text` 上的 `contains`/`startswith` 过滤需要解压;若该字段频繁做子串过滤,
  建议为它建立列式索引区。

### 3.4 复杂度与收益

| 项 | 说明 |
|---|---|
| 压缩/解压时间 | 内置 LZ4 风格约数 GB/s(经验值),flush/读取时一次性,摊销进 compaction |
| 空间 | 对话/笔记类文本经验压缩率 2–4×;短文本可能因开销略增,低于阈值自动存原文 |
| 检索延迟 | 倒排不受影响;行级过滤仅对候选行解压 |

---

## 4. 层边界契约(产品能力层 → 上层)

**向上提供**:

1. `Encryption` + `KeyProvider`(feature `encrypt`)与密钥轮换;
2. `Compression` + `Codec`(feature `compress`)与内置 codec;
3. `Stats.storage` 暴露加密/压缩的配置值与迁移进度(均已随 L11 落地;见 [16 §5](16-api-reference.md))。

**依赖**:L0(类型)、L2(段/WAL/MANIFEST 布局、`SegmentSource`)、L5(后台迁移复用 compaction)。

**不变量**:I28(加密不落明文);I2 的加密版(认证失败绝不静默);默认关闭时磁盘布局不变。

## 本章小结

- 威胁模型:保护**静态介质**,不保护进程内存、密钥持有者、侧信道与回滚攻击。
- 整文件/整帧 AEAD 信封(段/WAL/MANIFEST/关系段)+ `KeyProvider` +
  `Mneme::rotate_encryption_key` 密钥轮换;加密段 mmap 失效,需重新基准。
- 文本/元数据压缩;倒排不受影响,只有行级过滤才解压。
- 默认关闭时磁盘布局与 [04](04-l2-persist.md) 定义一致(仅 msec 记录体多一个恒 0 的 `flags2` 字节)。
- **本章不变量**:I28(不落明文),以及 I2 的加密版。

## 下一章

[12-deployment.md](12-deployment.md):多进程只读、WASM/边缘与可观测性。

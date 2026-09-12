# 11 存储安全与压缩:静态加密、文本压缩

> **本章目标**:补齐产品化的两块存储能力——**数据静止加密**(Agent 记忆含用户隐私)
> 与**文本/元数据压缩**(超长期下文本是体积大头),同时守住"默认零成本、依赖可选"。
> **前置阅读**:[04](04-l2-persist.md)(文件布局/WAL/MANIFEST)、[02 §7](02-l0-core.md)(meta 隔离)、[01 §5](01-overview.md)(依赖白名单)。
> **本章你将学到**:威胁模型与边界 → 页级 AEAD 加密与密钥提供者 → 密钥轮换 →
> 文本压缩与 BM25 的交互 → 层边界契约。
>
> 两者均为**可选 feature**,默认关闭时磁盘布局与
> [04](04-l2-persist.md) 完全一致,依赖白名单不扩大。

模块:`crypto/{aead.rs, keyring.rs}`(feature `encrypt`)、`compress/{codec.rs, lz4.rs}`(feature `compress`)

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

### 2.2 单元与布局

| 单元 | 加密粒度 | 说明 |
|---|---|---|
| 段数据区 | 每 **64 KiB 页** 一个 AEAD 记录 | 页内明文连续;页头存 nonce(12B)+ tag(16B)+ 明文长度 |
| 段头/索引区 | 整块加密 | 头部定长字段(维度/度量/版本)保留**明文**,以便打开时判版本(I18)与维度校验 |
| WAL 帧 | 每帧 payload 加密 | 帧头(crc/len/seqno/type)明文,便于撕裂写定位与回放 |
| MANIFEST | 变长区加密 | 头部明文 |

```text
加密页布局:[u32 cipher_len][12B nonce][ciphertext][16B tag]
nonce = 随机 96 bit(每页独立;随机碰撞概率在 2^32 页内 < 2^-32,可接受)
AAD   = [segment_id | page_index | format_version]   # 绑定位置,防页重排/跨文件搬运
```

- **页级**加密使 mmap 零拷贝读失效(需解密),因此开启加密时该段自动走
  `FileSource` 解码路径([04 §11](04-l2-persist.md)),读吞吐下降(经验值 1.5–3×);
  这是安全换性能的显式取舍;[01 §1.1](01-overview.md)/[14 §4](14-testing.md) 的
  性能目标默认在**未加密**下衡量,加密开启后需重新基准;
- 页大小 64 KiB 是 OS 页(通常 4 KiB)的整数倍,便于对齐与复用解密缓冲区。

### 2.3 密钥提供者

```rust
/// 密钥标识:写入文件头,解密时按 id 向 provider 取密钥。
pub struct KeyId(pub u32);
/// 32 字节对称密钥(刻意不实现打印明文内容的 `Debug`)。
pub struct Key([u8; 32]);
/// 支持的 AEAD 算法。
pub enum Cipher { Aes256Gcm }

pub trait KeyProvider: Send + Sync {
    /// 当前用于写入的密钥及其 id(用于把 key_id 写入文件头,解密时按 id 取密钥)。
    fn active_key(&self) -> KeyId;
    fn key(&self, id: KeyId) -> Result<Key>;      // 32 字节
    fn rotate(&self) -> Result<KeyId>;            // 生成新密钥并设为 active(可选)
}
pub struct Encryption { pub provider: Arc<dyn KeyProvider>, pub cipher: Cipher }  // Cipher::Aes256Gcm
```

- 引擎**不管理密钥文件**,只经 `KeyProvider` 取密钥;密钥可来自环境变量、OS keychain、
  KMS 或宿主自管(依赖不进入引擎);
- 每个段/WAL/MANIFEST 头部记录 `key_id`(头部扩展区定义见 [04 §2.5](04-l2-persist.md));
  解密按 id 查 provider,支持旧密钥仍可读。

### 2.4 密钥轮换

```text
1. provider.rotate() → 新 key_id
2. 后台任务逐段重写:旧段解密 → 新密钥加密 → 提交新 MANIFEST(复用 compaction 流程)
3. 全部段迁移完成后,旧 key_id 可退役
```

轮换复用 compaction 的逐段重写流程(尚未落地,属后续层);项目未发布期不保留
混合版本兼容,`db.stats().storage` 的"已迁移段/总段"随该能力一并落地。

### 2.5 复杂度与不变量

- 加密/解密吞吐取决于 AES-NI(经验值数 GB/s),相对磁盘带宽通常不是瓶颈;
- **不变量 I28**:开启加密后,磁盘上任何段/WAL/MANIFEST 的密文区不含明文记录字段;
  `db.check()` 校验每个页的认证标签,篡改/错误密钥 → `Corrupted`,绝不返回错误数据。

---

## 3. 文本与元数据压缩(feature `compress`)

### 3.1 【直觉】文本比向量更占地方

[01 §1.2](01-overview.md) 已指出:向量只占 4 字节/分量,而对话原文、笔记、摘要动辄几百字节
到几 KB。超长期下 `text`/`meta` 才是体积大头。压缩直接放大"十年记忆"的容量上限。

### 3.2 编解码抽象

```rust
pub trait Codec: Send + Sync {
    fn compress(&self, src: &[u8], dst: &mut Vec<u8>);
    fn decompress(&self, src: &[u8], dst: &mut Vec<u8>) -> Result<()>;
}
pub enum Compression { None, Lz4, Zstd }   // 默认 None;`Lz4` 为内置自研实现(feature `compress`),`Zstd` 需 feature `compress-zstd`
```

- 压缩作用于**记录体内的 `text` 与 `meta` 字段**(`[04 §2.2](04-l2-persist.md)` entry 的变长区),
  按字段独立压缩并带 `uncompressed_len` 前缀;向量/norm 不压缩(已定长且量化另有手段);
- 每段头部记录所用 codec(头部扩展区定义见 [04 §2.5](04-l2-persist.md));`Compression::None` 时字节布局与未压缩定义逐字节一致;
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
3. `Stats.storage` 暴露加密/压缩生效状态与迁移进度。

**依赖**:L0(类型)、L2(段/WAL/MANIFEST 布局、`SegmentSource`)、L5(后台迁移复用 compaction)。

**不变量**:I28(加密不落明文);I2 的加密版(认证失败绝不静默);默认关闭时磁盘布局不变。

## 本章小结

- 威胁模型:保护**静态介质**,不保护进程内存、密钥持有者、侧信道与回滚攻击。
- 页级 AEAD 加密 + `KeyProvider` + 密钥轮换;加密时 mmap 失效,需重新基准。
- 文本/元数据压缩;倒排不受影响,只有行级过滤才解压。
- 默认关闭时磁盘布局与 [04](04-l2-persist.md) 完全一致。
- **本章不变量**:I28(不落明文),以及 I2 的加密版。

## 下一章

[12-deployment.md](12-deployment.md):多进程只读、WASM/边缘与可观测性。

# 11 异步与 tokio 最小封装

> **本章目标**:理解 `async fn` / `.await` / `Future` 的惰性与取消语义,看懂 L6 的
> `AsyncNamespace` 如何用 `tokio::task::spawn_blocking` 把同步阻塞 API 包成异步门面。
> **前置**:[04 章](04-borrowing-strings-slices.md)(借用与生命周期)、[05 章](05-errors.md)
> (`Result`)、[06 §3.3](06-generics-traits.md)(`Send`/`Sync`)。
> **对应源码**:[`src/memory/async_facade/`](../../src/memory/async_facade/)、
> [`src/memory/record/`](../../src/memory/record/)、[`src/memory/namespace.rs`](../../src/memory/namespace.rs)、
> [`src/lib.rs`](../../src/lib.rs)、[`tests/l6_contracts.rs`](../../tests/l6_contracts.rs)、
> [`Cargo.toml`](../../Cargo.toml)。

前 10 章读的都是同步代码:调用一个函数,它跑完才返回。L6 的 `AsyncNamespace` 提供了另一套
入口——同样的插入、点读、更新,但返回的是要 `.await` 的 future。本章讲清楚这层"最小异步
封装"背后的 Rust 机制,以及**为什么核心库一行 tokio 也不写**。

---

## 1. `async fn`、`.await` 与惰性 `Future`

mneme 的异步方法本身极短,几乎都是一句 `run_blocking(...).await`:

```rust
pub async fn insert(&self, rec: Record) -> Result<InsertOutcome> {
    let inner = self.inner.clone();
    run_blocking(move || inner.insert(rec)).await
}
```

见 [`src/memory/async_facade/write.rs:40-43`](../../src/memory/async_facade/)。三个语法点:

- `async fn f(...) -> T` 约等于 `fn f(...) -> impl Future<Output = T>`:调用它返回一个
  **future(未来值)**,函数体被编译器改写成状态机;
- 调用 async 函数**不会执行函数体**。future 造出来只是"准备了一个可以跑的步骤",只有
  `.await`(或把它交给执行器)它才开始推进——这叫**惰性(lazy)**;
- `.await` 是语法糖:当前 future 没就绪时把控制权交还执行器,就绪后从挂起点继续。因此
  `.await` 必须待在 async 上下文里,而驱动它的**执行器**(runtime)由调用方提供。

惰性最直观的验证是"造 future 不写库,`block_on` 才写":

```rust
let db = mneme::Mneme::in_memory(2).unwrap();
let ns = db.namespace("demo").into_async();

let future = ns.insert(mneme::Record::new(vec![1.0, 0.0]).key("a"));
// 到这里为止,库里一条记录都没有:future 还没被推进

let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
runtime.block_on(future).unwrap();   // 现在才真正执行 insert
```

编译器内部靠 `Future` trait 描述"怎么推进":

```rust
pub trait Future {
    type Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output>;
}
```

- `poll` 返回 `Poll::Ready(值)` 或 `Poll::Pending`(还没好,等通知);
- 参数是 `Pin<&mut Self>`:async 状态机可能"自己指向自己",一旦被移动指针就失效,
  `Pin` 用来禁止这种移动(是 [04 §4](04-borrowing-strings-slices.md) 借用规则在状态机上的延伸);
- `.await` 的展开就是"循环调用 `poll`,拿到 `Ready` 就把值取出来"。

> `async`/`.await` 是**语言内建**语法,`Future` 是**标准库** trait,而 `tokio` 只是提供
> **执行器与线程池**的第三方 crate——三者不同层次。mneme 只用了 tokio 最薄的一层。

---

## 2. `spawn_blocking`:把阻塞调用移进阻塞线程池

异步代码最忌讳在里面直接跑耗时同步操作:它会把执行器线程卡住,别的 future 全部挨饿。
mneme 的 L6 全是同步引擎代码,于是用 tokio 的**阻塞线程池**接住:

```rust
/// 把阻塞调用移入 tokio 阻塞线程池。
///
/// # Panics
///
/// 阻塞池任务异常终止(取消/panic)时 panic;按设计 16 §4 的文档化例外传播——
/// 核心库承诺不 panic,出现即属运行时故障,绝不静默吞掉。
async fn run_blocking<T, F>(task: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        // reason: 核心库不 panic(FC-GLOBAL-ERR-001);阻塞任务异常终止属运行时故障,
        // 按设计 16 §4 的文档化 panic 例外传播(同 `filter!` 宏的文档化例外)。
        .expect("async 门面:阻塞任务异常终止")
}
```

见 [`src/memory/async_facade/facade.rs:11-21`](../../src/memory/async_facade/facade.rs)。逐个概念:

- `spawn_blocking(task)` 把闭包交给**专门跑阻塞任务的线程池**,不占异步执行器;返回的
  `JoinHandle<T>` 本身是 future,`.await` 得到 `Result<T, JoinError>`;
- `JoinError` 表示任务 panic 或被取消。`run_blocking` 用 `.expect(...)` 把它转成 panic,
  这是源码里逐行注释过的**文档化例外**:核心库承诺不 panic,真出现就属运行时故障,
  绝不静默吞掉。

**两个约束从哪来?** 看 `spawn_blocking` 的签名,它接受 `F: FnOnce() -> T + Send + 'static`、
返回可跨线程等待的句柄,于是:

- `T: Send + 'static`:计算结果要从阻塞线程搬回调用线程,而且任务可能比当前调用活得久,
  所以结果必须能跨线程且不带借用;
- `F: Send + 'static`:闭包本身在别的线程执行,捕获的值也必须能跨线程;`'static` 则禁止
  它借用栈上的局部变量——这直接解释了门面里那些看似多余的 `clone()`。

看 `insert` 的实现(§1 的片段)与 `get`:

```rust
pub async fn get(&self, key: &str) -> Result<Option<StoredRecord>> {
    let inner = self.inner.clone();
    let key = key.to_string();
    run_blocking(move || {
        inner
            .get(&key)
            .map(|found| found.map(|record| record.to_stored()))
    })
    .await
}
```

见 [`src/memory/async_facade/read.rs:19-28`](../../src/memory/async_facade/)。为什么每个方法
都要先 `clone`:

- `self` 是 `&AsyncNamespace`。若直接 `move || self.inner.insert(rec)`,移进闭包的是**借用**,
  它的生命周期活不过 `'static`,编译直接拒绝;
- `Namespace` 是 `#[derive(Clone)]` 的轻量句柄,三个字段全是 `Arc`
  (见 [`src/memory/namespace.rs:42-47`](../../src/memory/namespace.rs))。`clone()` 只把
  引用计数 +1,**不复制底层表**,却让闭包拿到一份独立所有权——这是"跨线程前先 clone 句柄"
  的标准手法,和 [02 §3.7](02-values-and-ownership.md) 的 `Arc` 共享是同一件事;
- `key: &str` 同理:先 `key.to_string()` 变成 owned `String` 才能进 `'static` 闭包;
  `get_many` 甚至先把 `keys: &[&str]` 收成 `Vec<String>` 再整体 `move`
  (见 [`src/memory/async_facade/read.rs:48-60`](../../src/memory/async_facade/))。

由于字段都能跨线程,`AsyncNamespace` 本身是 `Send + Sync` 的,还能 `Clone` 到多个任务里
并发调用(见 [`src/memory/async_facade/facade.rs:28-32`](../../src/memory/async_facade/))。

---

## 3. tokio runtime 最小封装

门面只用到 tokio 的两样东西:`spawn_blocking` 和"跑 future"的 runtime。所以依赖开得极小:

```toml
# L6 async 门面的 `spawn_blocking`(设计 08 §6);只开 `rt`,不选 runtime、不引入 async I/O。
tokio = { version = "1", optional = true, default-features = false, features = ["rt"] }
```

见 [`Cargo.toml:22-23`](../../Cargo.toml) 的 feature 声明与
[`Cargo.toml:39-40`](../../Cargo.toml) 的依赖:tokio 是 **optional 依赖**,只有 feature `async`
打开才编译;tokio 本就没有默认 feature,`features = ["rt"]` 只开运行时核心,
不会带入 net/time/fs/macros 等组件。
对嵌入型库来说这很关键:不替宿主选运行时,依赖树也最小。门面类型本身也按 feature 重导出:
`#[cfg(feature = "async")] pub use crate::memory::AsyncNamespace;`
(见 [`src/lib.rs:67-68`](../../src/lib.rs)),关闭 feature 时 `AsyncNamespace` 根本不存在。

宿主自己起一个最小 runtime 的写法就是源码 doctest 里那种:

````rust,no_run
use mneme::{Mneme, Record};

let db = Mneme::in_memory(2).unwrap();
let ns = db.namespace("demo").into_async();
let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
runtime.block_on(async {
    ns.insert(Record::new(vec![1.0, 0.0]).key("a")).await.unwrap();
});
````

见 [`src/memory/async_facade/write.rs:30-39`](../../src/memory/async_facade/)。要点:

- `Builder::new_current_thread()` 建一个"单线程"运行时,`block_on(future)` 在当前线程把它
  一直跑到完成——嵌入场景最省事;
- 示例标成 `no_run`(文档测试只编译、不执行):doctest 里真起 runtime 会引入线程调度与
  随机失败,而示例的职责是展示 API 形态(见 [08 §5.3.1](08-modules-docs.md));
- 集成测试也是同一套路:构造一次 runtime,反复 `runtime.block_on(...)`
  (见 [`tests/l6_contracts.rs:620-626`](../../tests/l6_contracts.rs));
- mneme **没有** `#[tokio::main]`:那属于应用入口,库不该替使用者决定。

---

## 4. 取消语义:drop future 不会撤回已派发的阻塞任务

`async_facade/` 的文件头专门写了这条语义:

```rust
//! 所有方法经 `spawn_blocking` 执行,阻塞任务一经派发不可取消:drop 返回的
//! future 只丢弃等待结果,后台同步操作仍会执行完成(写入照常生效)。需要
//! 「取消即中止」的调用方应在业务层以句柄/标志做协作取消。
```

见 [`src/memory/async_facade/mod.rs:13-17`](../../src/memory/async_facade/mod.rs)(文件头)。理解:

- future 被 drop(比如 `select!` 输了、`timeout` 到点、作用域结束)只表示**等待方不等了**;
  阻塞线程池里的那个任务没有"可中断点",会继续跑完;
- 所以"插入到一半取消"这样的保证**不存在**:操作要么还没派发,要么最终生效;取消发生在
  派发之后,写入照常落地;
- 真要"取消即中止",得在业务层做**协作取消**:放一个 `AtomicBool`/句柄,任务在检查点自行退出
  (见 [04 §5.2](04-borrowing-strings-slices.md) 的原子类型);
- 这条语义与同步 API 等价(I14):同一底层句柄、同一把写锁、同一操作序列产生等价结果。
  异步门面是"搬运",不是"另一套执行模型"。

---

## 5. 借用返回值无法跨线程

同步 `get` 返回的是**只读借用视图**:

```rust
pub fn get(&self, key: &str) -> Result<Option<RecordRef<'_>>> { ... }
```

见 [`src/memory/namespace/query.rs:72`](../../src/memory/namespace/query.rs)。`RecordRef` 内部
虽然握着 `Arc`,但类型本身带生命周期标记,是"借来的":

```rust
pub struct RecordRef<'a> {
    slot_data: Arc<SlotData>,
    _marker: PhantomData<&'a ()>,
}
```

见 [`src/memory/record/view.rs:13-20`](../../src/memory/record/)。两个原因让它上不了
`spawn_blocking`:

- 它是在阻塞闭包**内部**、从局部 `inner` 借出来的,闭包一结束生命周期就到头,没法当返回值
  交出;编译器会报 `borrowed value does not live long enough`;
- `spawn_blocking` 要求 `T: Send + 'static`,带任何借用生命周期的类型都不满足。

异步版因此改成返回 **owned 快照** `StoredRecord`:

```rust
pub async fn get(&self, key: &str) -> Result<Option<StoredRecord>> { ... }
```

见 [`src/memory/async_facade/read.rs:19-28`](../../src/memory/async_facade/),转换在闭包里由
`to_stored()` 完成:

```rust
pub fn to_stored(&self) -> StoredRecord {
    StoredRecord::from_ref(self)
}
```

见 [`src/memory/record/view.rs:190-192`](../../src/memory/record/)。`StoredRecord` 把 key/text
复制成 `String`、向量复制成 `Vec<f32>`,字段全是 owned 类型
(见 [`src/memory/record/stored.rs:9-26`](../../src/memory/record/)),于是天然满足
`Send + 'static`。代价是一次深拷贝,换来的是"结果可以安全地跨线程回到 await 处"。

> 同样的理由,`iter`/`iter_with` 这类返回**迭代器**的同步方法没有异步版本:迭代器借自读
> 视图,无法跨线程移动(见 [`src/memory/async_facade/mod.rs:9-11`](../../src/memory/async_facade/mod.rs))。

---

## 6. 什么时候该用异步门面

| 操作 | 同步形态 | 异步怎么做 |
|---|---|---|
| `insert`/`get`/`update`/`delete`/`count`/`relate`… | `Namespace` 方法 | `ns.into_async()` 得到 `AsyncNamespace`,再 `.await` |
| `search()` 构建 | `SearchBuilder`(纯内存) | 直接用同步构建器,不必包装 |
| `builder.execute()` | 阻塞检索 | **宿主**自己 `spawn_blocking` 包起来 |
| `flush()`/`close()`/`backup_to()`/`snapshot()` | `Mneme` 方法 | 不在门面上,宿主按需 `spawn_blocking` 或直接同步调 |

- **推荐用门面**:宿主已经是 async 生态(web 服务、Agent 循环),不希望一次 `get` 把整个
  reactor 卡住;门面覆盖 `Namespace` 的全部阻塞入口(方法清单见
  [`src/memory/async_facade/`](../../src/memory/async_facade/))。
- **不必包装 `search()` 构建器**:`.vector()`/`.top_k()`/`.ef()` 只是往结构体里填字段,
  纯内存、不阻塞(见 [`src/memory/search_builder.rs:18-58`](../../src/memory/search_builder.rs)),
  用同步版构建最自然。
- **`execute()` 由宿主包装**:它是真正的检索,仍是阻塞调用;文件头明确给出分工
  "`search()` 构建器本身是轻量纯内存操作,`execute()` 为阻塞调用,异步场景请由宿主对
  `execute()` 自行 `spawn_blocking`"(见
  [`src/memory/async_facade/mod.rs:3-7`](../../src/memory/async_facade/mod.rs))。
- **`Mneme` 级操作不走门面**:`flush`/`close`/`backup_to`/`snapshot` 同样由宿主处理
  (理由同上)。门面只包 `Namespace`,不做"全库异步"这种过度设计。
- **别为异步而异步**:`spawn_blocking` 每次都有线程池调度开销;纯内存、微秒级的同步调用
  直接调更便宜。门面解决的是"阻塞了 reactor"的问题,不是"没有 async 就不酷"。

---

## 7. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `future cannot be sent between threads safely` | future 里持有非 `Send` 的值,任务不接受 | 在 `.await` 前把非 `Send` 值丢掉,或换成可跨线程的容器 |
| `borrowed value does not live long enough` / `'static` | `move` 闭包捕获了局部借用 | 先 `clone()` 句柄、`to_string()` 造 owned 值 |
| `the trait bound ... Send is not satisfied` | 返回值/捕获值不满足 `spawn_blocking` 约束 | 返回 owned `StoredRecord`,而不是借用 `RecordRef` |
| `.await` used outside of an async block | 在同步函数里写了 `.await` | 改用 `block_on`,或把调用方改成 `async fn` |
| runtime panic:`there is no reactor running` | 在 runtime 外调用了依赖它的 API | 先建 runtime 再 `block_on`(§3) |
| `cannot move out of ... which is behind a shared reference` | 想从 `&self` 里把字段移走 | 像门面那样先 `.clone()`(见 [02 §3.5](02-values-and-ownership.md)) |

---

## 8. 本章小结

- `async fn` 返回 future,函数体**惰性**执行,`.await` 才推进;执行器负责调度,`.await`
  必须在 async 上下文里。
- `spawn_blocking` 把同步阻塞调用移进阻塞线程池;`T: Send + 'static`、`F: Send + 'static`
  来自"结果与闭包都要跨线程"这一事实。
- 门面每个方法先 `clone` 句柄再 `move`,是因为 `&self` 的借用活不过 `'static`;
  `Arc` 句柄的 clone 只加计数,不是深拷贝。
- 核心库零 tokio runtime:只经 feature `async` 引入 `rt`,用
  `Builder::new_current_thread().build()` + `block_on` 最小驱动;文档示例用 `no_run`。
- drop future **不会**取消已派发的阻塞任务;需要中止时由业务层做协作取消。
- 借用视图(`RecordRef`)无法跨线程,异步点读返回 owned `StoredRecord`;`iter` 类方法因此
  没有异步版。
- `search()` 构建器纯内存、`execute()` 仍阻塞需宿主包装,`Mneme` 级操作不走门面。

## 动手练习

1. 用 `new_current_thread` runtime 跑一遍 `AsyncNamespace`:插入两条记录、`get` 一条、
   `count` 一次,打印结果。
2. 写一个 `async fn push(v: Arc<Mutex<Vec<i32>>>, x: i32)`,函数体里加锁 push;先造 future
   不 `.await`,断言 `Vec` 仍为空,再 `block_on` 看它变化(体会惰性)。
3. 在宿主 runtime 里把同步 `search().execute()` 用 `spawn_blocking` 包起来调用,和直接同步
   调用对拍,确认命中结果一致。
4. 读 [`src/memory/async_facade/read.rs:48-60`](../../src/memory/async_facade/) 的 `get_many`,
   解释为什么 `keys: &[&str]` 要先收集成 `Vec<String>`,以及 `to_stored()` 在返回值转换里
   的角色。

## 结语

到这里,`docs/rust/` 的教程告一段落。L6 的完整设计动机、量化收益与层边界契约见
[08 L6 打磨层:量化、两阶段检索与 async 门面](../design/08-l6-quant.md);
想换条路线继续读源码,回到 [README](README.md) 的总表即可。

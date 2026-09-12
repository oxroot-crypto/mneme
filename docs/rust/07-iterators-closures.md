# 07 迭代器与闭包

> **本章目标**:掌握 `for` 循环、迭代器链式调用(adapter)、闭包,以及 `sort_by` + `Ordering`。
> **前置**:[04 章](04-borrowing-strings-slices.md)(引用)、[06 章](06-generics-traits.md)(trait)。
> **对应源码**:[`src/core/simd.rs`](../../src/core/simd.rs)、[`src/core/heap.rs`](../../src/core/heap.rs)、
> [`src/core/varint.rs`](../../src/core/varint.rs)、[`src/memory/namespace/access.rs`](../../src/memory/namespace/access.rs)、
> [`src/index/hnsw.rs`](../../src/index/hnsw.rs)、[`src/index/filtered.rs`](../../src/index/filtered.rs)、
> [`src/query/bm25.rs`](../../src/query/bm25.rs)、[`src/query/fusion.rs`](../../src/query/fusion.rs)。

Rust 的迭代器是**惰性(lazy)**的:你写一串转换,只有到"消费"时(如 `sum`、`collect`、`for`)
才真正执行。它既表达力强,又能被编译器优化到和手写循环一样快。

---

## 1. `for` 循环与区间

```rust
for i in 0..5 {          // 0,1,2,3,4(左闭右开)
    println!("{i}");
}
for i in 0..=5 { ... }   // 0..=5 含 5(左闭右闭)
for i in (1..10).step_by(2) { ... }   // 1,3,5,7,9
```

`a..b` 和 `a..=b` 是 **Range**,本身就是迭代器。范围还能用于"是否包含":

```rust
if (Self::MIN..=Self::MAX).contains(&value) { ... }
```

见 [`src/core/options/dimension.rs:41`](../../src/core/options/dimension.rs)。

---

## 2. 迭代器的三种取得方式

| 方法 | 产生 | 说明 |
|---|---|---|
| `iter()` | `&T` | 只读借用,不消耗集合 |
| `iter_mut()` | `&mut T` | 可变借用 |
| `into_iter()` | `T` | 消耗集合,交出所有权 |

```rust
let v = vec![1, 2, 3];
for x in &v { }        // 等价于 v.iter(),x: &i32
for x in v.iter() { }
for x in v { }         // 等价于 v.into_iter(),x: i32,v 被消耗
```

> **edition 差异**:`for x in array` 自 Rust 1.53 起就按值迭代(`x: T`);而 `array.into_iter()`
> 这个**方法调用**在 2021 之前的 edition 里会解析成切片迭代、拿到 `&T`,2021+ 才按值。本项目用
> `edition = "2024"`,两者都按值。若看到旧教程说"数组迭代拿到引用",那说的是旧 edition 的方法调用。

---

## 3. 迭代器适配器(adapter)

适配器接收一个迭代器、返回一个新迭代器,可以链式组合。

```rust
a.iter()                       // &f32
 .zip(b.iter())                // (&f32, &f32)
 .map(|(x, y)| x * y)          // f32
 .sum::<f32>()                 // 消费:求和
```

这是 mneme 的标量点积实现,见 [`src/core/simd.rs:86-97`](../../src/core/simd.rs):

```rust
pub fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
```

> `zip` 在**较短的迭代器耗尽时停止**,所以 `dot_scalar` 对长度不等的切片按较短者计算,不会 panic。
> `dot`(SIMD 版)也显式用 `n = a.len().min(b.len())` 保持同样语义;两者只对等长向量在 debug 下断言。

常用适配器:

| 适配器 | 作用 |
|---|---|
| `map(f)` | 把每个元素变成另一个值 |
| `filter(f)` | 只保留 `f` 返回 `true` 的元素 |
| `zip(other)` | 把两个迭代器配成对 |
| `enumerate()` | 加上下标,产出 `(usize, T)` |
| `take(n)` / `skip(n)` | 取前 n 个 / 跳过前 n 个 |
| `rev()` | 反向迭代(`DoubleEndedIterator` 才有) |
| `copied()` | 把 `&T` 变成 `T`(`T: Copy`),省去 `|x| *x` |
| `flat_map(f)` | 每个元素展开成多个 |
| `peekable()` | 可以偷看下一个元素 |
| `find_map(f)` | `map` + `find` 合一:闭包返回 `Option`,第一个 `Some` 即为结果 |

消费器(终结适配器):

| 消费器 | 作用 |
|---|---|
| `collect()` | 收集成 `Vec`/`HashMap` 等 |
| `sum()` / `product()` | 求和 / 求积 |
| `fold(init, f)` | 带累加器的归约 |
| `count()` | 计数 |
| `any(f)` / `all(f)` | 是否任一 / 全部满足 |
| `find(f)` / `position(f)` | 找第一个满足的 |
| `min()` / `max()` | 极值(要求 `Ord`;`f32` 不能直接用,见 [03 §4.1](03-structs-enums-impl.md)) |
| `min_by_key(f)` / `max_by_key(f)` | 按"键"取极值,如 `max_by_key(\|item\| item.level)` |

`find_map` 是"逐个尝试、第一个成功就停"的组合:L4 用它把命名空间路径解析成 `NsId`,找不到就是
`None`(调用方退回空结果):

```rust
view.ns_registry.iter().find_map(|(id, path)| {
    if **path == *self.ns_path { Some(*id) } else { None }
})
```

见 [`src/query/exec.rs:195-202`](../../src/query/exec.rs)。闭包返回 `Option`,所以既能"过滤掉
不关心的项",又能顺手做转换;`**path` 的双重解引用见 [04 §3.3](04-borrowing-strings-slices.md)。

另外两个不属于迭代器、但总在链尾露脸的 `Vec` 方法:

- `Vec::extend(iter)`:把另一个迭代器(或 `Vec`)的元素追加进来。L4 解析器把第一个子表达式与
  收集到的其余项合并成一个列表再 `into_boxed_slice`,见
  [`src/query/parse.rs:191-194`](../../src/query/parse.rs);
- `Vec::truncate(n)`:只保留前 `n` 个元素(多出的直接丢掉)。L4 执行管线在融合排序后按 `top_k`
  截断,见 [`src/query/exec.rs:369`](../../src/query/exec.rs)。

### 3.1 `enumerate` 的例子

varint 解码要同时知道字节和它的位置:

```rust
for (index, &byte) in input.iter().enumerate() {
    ...
    if byte & CONTINUATION_BIT == 0 {
        return Ok((result, index + 1));
    }
}
```

见 [`src/core/varint.rs:96-108`](../../src/core/varint.rs)。`&byte` 是把 `&u8` 解构成 `u8`(因为 `u8` 是 `Copy`)。

### 3.2 `collect` 与类型标注

```rust
let v: Vec<i32> = (0..5).map(|x| x * x).collect();
```

`collect` 能收集成多种容器,所以常需要标注目标类型,或靠上下文推断。

### 3.3 `fold` 与函数指针:求极值

`fold(初值, f)` 从初值出发,对每个元素执行 `acc = f(acc, x)`。L4 的融合归一化要取
通道分数的最小 / 最大值:

```rust
let min = oriented.iter().copied().fold(f64::INFINITY, f64::min);
let max = oriented.iter().copied().fold(f64::NEG_INFINITY, f64::max);
```

见 [`src/query/fusion.rs:86-87`](../../src/query/fusion.rs)。三个细节:

- `f64::min` / `f64::max` 是**方法**,但签名是 `fn(f64, f64) -> f64`,正好吻合 `fold`
  需要的 `FnMut(f64, f64) -> f64`;凡是签名对得上,关联函数/方法都能直接当函数指针传
  (和 [04 §2.3](04-borrowing-strings-slices.md) 的 `Vec::as_slice`、[10 §4.6](10-testing.md)
  的 `Cell::get` 一样)。`f32` 有完全同名的版本,规则一致。
- 用 `f64::INFINITY` 当地基:任何有限值都能顶掉它,`fold` 完拿到的就是集合最小值;
  空集合则原样返回 `+∞`(L4 调用前已判空,不会遇到)。
- **为什么用 `f64` 而不是 `f32`**:两个通道的分数本身是 `f32`,但归一化要算 `max - min`;
  极端输入(`±f32::MAX`)的极差会在 `f32` 下溢出成 `inf`,再算 `inf/inf` 就得到 `NaN`。
  先把中间量升到 `f64` 再降回 `f32`,既不溢出也不丢序(见
  [`src/query/fusion.rs:76-106`](../../src/query/fusion.rs))。

### 3.4 再补几个高频组合子

维护代码时还会反复遇到这几个(知道"有它、长什么样"即可):

- **`HashMap` 的 `or_default()`**:entry API 的另一种收尾。`or_insert(v)` 要手写默认值,
  `or_default()` 直接用 `V::default()`(计数/列表就是 `0`/空容器):

  ```rust
  let bucket = self.terms.entry(ns_id).or_default();
  let postings = bucket.entry(Arc::from(token.as_str())).or_default();
  ```

  见 [`src/memory/analysis/inv.rs:48-52`](../../src/memory/analysis/inv.rs);默认值构造有
  成本时用惰性的 `or_insert_with(f)`。

- **`sort_by_key`**:按"提取出来的键"排序,比 `sort_by` 少写比较样板;键是元组时按
  字典序逐字段比较:

  ```rust
  versions.sort_by_key(|(row, _)| (row.rowid, row.seqno));
  ```

  见 [`src/persist/recover/state.rs:81`](../../src/persist/recover/state.rs)。要求键类型
  实现 `Ord`;键里含 `f32` 时不能直接用,得换成 `total_cmp` 版的 `sort_by`(见
  [03 §4.1](03-structs-enums-impl.md))。

- **`binary_search`**:在**已排序**切片上二分定位,返回 `Ok(下标)` 或 `Err(插入点)`;
  L5 的 compaction 用它把"按槽位排序的幸存列表"定位到目标槽位(见
  [`src/memory/engine_ops.rs:397`](../../src/memory/engine_ops.rs))。

- **`filter_map`**:`filter` + `map` 合一,闭包返回 `Option`,`None` 直接丢弃:

  ```rust
  self.slot_segment
      .iter()
      .enumerate()
      .filter_map(|(index, segment)| segment.is_none().then_some(index))
      .collect()
  ```

  见 [`src/memory/table/state.rs:495-499`](../../src/memory/table/state.rs)。和 `find_map`
  (§3)的区别是:它消费**整个**迭代器,而不是拿到第一个 `Some` 就停。

- **`windows(n)`**:滑动窗口,每次产出连续的 `n` 个元素的切片;CJK bigram 分词靠它:

  ```rust
  for pair in chars.windows(2) {
      let token: String = pair.iter().collect();
      ...
  }
  ```

  见 [`src/core/text.rs:47`](../../src/core/text.rs)。窗口是只读切片,不复制数据;
  `n = 0` 会 panic,`n > len` 时产出空迭代器(循环体一次也不执行)。

- **`peekable()`**(表里已列):包成"可偷看下一个而不消费"的 `Peekable`,适合解析器
  判断"还有没有下一个"。CJK 分词按连续字符成段时也先用它,再 `while let` 逐个消费
  (见 [`src/core/text.rs:57`](../../src/core/text.rs))。

---

## 4. 闭包(closure)

闭包是**可以捕获周围变量的匿名函数**:

```rust
let factor = 2.0;
let double = |x: f32| x * factor;   // 捕获了 factor
double(3.0);                         // 6.0
```

- `|参数| 表达式` 是闭包语法。
- 闭包能借用或移动它捕获的变量;用 `move` 强制把所有权移进闭包:

```rust
let s = String::from("hi");
let f = move || println!("{s}");     // s 被移动进闭包
```

### 4.1 闭包作为参数

`sort_by` 接收一个比较闭包:

```rust
self.heap.sort_by(|a, b| {
    if Self::is_better(metric, a.score, &a.payload, b.score, &b.payload) {
        std::cmp::Ordering::Less
    } else if Self::is_better(metric, b.score, &b.payload, a.score, &a.payload) {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
});
```

见 [`src/core/heap.rs:203-211`](../../src/core/heap.rs)。

- 闭包参数 `a`、`b` 是 `&Entry<T>`(因为 `sort_by` 传引用)。
- 返回值是 `std::cmp::Ordering`,三选一:`Less`(a 在前)、`Greater`(b 在前)、`Equal`。
- 这里不能用简单的 `a.score.partial_cmp(&b.score)`,因为 mneme 的排序方向由 `Metric::better` 决定,
  且同分要按载荷升序。

> **`sort_by` 的比较闭包必须是全序**,否则可能 panic 或结果错乱。这也是不能直接写
> `a.score.partial_cmp(&b.score).unwrap()` 的原因:`f32` 的 `partial_cmp` 遇到 `NaN` 返回 `None`,
> `unwrap()` 会 panic。mneme 绕开浮点比较,改用 `Metric::better` + `Ord` 载荷保证全序:
> 对任意 `a`、`b`,`is_better(a, b)` 与 `is_better(b, a)` 至多一个为真,相等时再用
> `a_payload < b_payload` 兜底。见 [`src/core/heap.rs:216-226`](../../src/core/heap.rs)。

### 4.2 闭包捕获与借用规则

闭包默认按"最小权限"捕获:只读就借 `&`,要改就借 `&mut`,要所有权就 `move`。
这依然受 [04 章](04-borrowing-strings-slices.md)的借用规则约束。

### 4.3 闭包作为回调:`FnOnce` 与写事务

上面是"写闭包";反过来,当**函数接收**一个闭包时,也要声明"我打算怎么调用它"。Rust 用三个
trait 表达调用次数与捕获方式:

| trait | 能调用几次 | 捕获方式 |
|---|---|---|
| `FnOnce` | 至多一次(可能消费捕获值) | 移动 / 借用 / 复制 |
| `FnMut` | 多次,且可改捕获值 | 可变借用 |
| `Fn` | 任意多次 | 只读借用 |

L1 的写事务 `Table::write_tx` 接收"执行一次、可失败"的闭包:

```rust
pub(crate) fn write_tx<T>(&self, f: impl FnOnce(&mut WriterState) -> Result<T>) -> Result<T> {
    let mut ws = self.write();
    let snapshot = ws.clone();
    match f(&mut ws) {
        Ok(value) => {
            self.publish(&ws);
            Ok(value)
        }
        Err(error) => {
            *ws = snapshot;
            Err(error)
        }
    }
}
```

见 [`src/memory/table/handle.rs`](../../src/memory/table/handle.rs)。`impl FnOnce(...)` 是 `F: FnOnce(...)`
的简写(见 [06 §2.3](06-generics-traits.md)):闭包只调用一次,所以用**最宽松**的 `FnOnce`;
若写成 `Fn`,那些会移动捕获值的闭包反而用不了。

调用方的惯例是 `move` 闭包 + 提前 `Arc::clone`:

```rust
let config = Arc::clone(&self.config);
let ns_path = Arc::clone(&self.ns_path);
self.table.write_tx(move |ws| {
    if ws.closed {
        return Err(MnemeError::Closed);
    }
    // ... 在闭包内完成整个写操作:失败时由 write_tx 统一回滚
})
```

见 [`src/memory/namespace/access.rs`](../../src/memory/namespace/access.rs)。`move` 把两个
`Arc` 的所有权移进闭包,闭包因此不借用 `self`,可以自由地和 `self.table` 的借用共存;
事务语义(失败回滚、成功发布读视图)则完全收敛在 `write_tx` 一处,各写方法只需关心业务。

> 编译器报 `closure may outlive the current function` 就是在提醒你:这个闭包可能活得比借用久,
> 需要 `move`(或调整生命周期)。

---

## 5. 优先队列:`BinaryHeap` 与 `Reverse`

[03 §4.2](03-structs-enums-impl.md) 给 `Cand` 手写 `Ord`,就是为了把它放进标准库的
**优先队列** `BinaryHeap<T: Ord>`——不用每次全排序,插入/取最值都是 $O(\log n)$:

```rust
use std::collections::BinaryHeap;

let mut heap = BinaryHeap::new();
heap.push(3);
heap.push(1);
heap.push(2);
assert_eq!(heap.peek(), Some(&3));   // 堆顶是最大值(只借看不弹出)
assert_eq!(heap.pop(), Some(3));     // 弹出最大值
assert_eq!(heap.pop(), Some(2));
```

- **`BinaryHeap` 是最大堆**:`peek()`/`pop()` 拿到的都是最大元素;
- 元素必须实现 `Ord`——所以 `f32` 不能直接放(见 [03 §4.1](03-structs-enums-impl.md)),
  得像 `Cand` 那样包一层并手写全序;
- `while let Some(x) = heap.pop()` 是"从大到小依次消费"的惯用写法(见 [05 §4.5](05-errors.md))。

**要最小堆,就包一层 `std::cmp::Reverse<T>`**——它把 `Ord` 整个反过来:

```rust
use std::cmp::Reverse;

let mut min_heap: BinaryHeap<Reverse<u32>> = BinaryHeap::new();
min_heap.push(Reverse(3));
min_heap.push(Reverse(1));
assert_eq!(min_heap.peek(), Some(&Reverse(1)));   // 堆顶变成最小值
```

L3 的 HNSW 搜索同时用了这两种堆:

```rust
let mut frontier: BinaryHeap<Cand> = BinaryHeap::new();                   // 最大堆
let mut results: BinaryHeap<std::cmp::Reverse<Cand>> = BinaryHeap::new(); // 最小堆

while let Some(current) = frontier.pop() {
    if results.len() >= ef {
        // results.peek() 看到的是"最差"候选;当前候选比它还差就不必继续探查
        let worst = results.peek().map_or(current, |rev| rev.0);
        if current.key.total_cmp(&worst.key) == Ordering::Less {
            break;
        }
    }
    for &neighbor in self.graph.neighbors(current.node, level) {
        ...
        if results.len() < ef {
            frontier.push(cand);
            results.push(std::cmp::Reverse(cand));
        } else if let Some(worst) = results.peek().map(|rev| rev.0)
            && cand.key.total_cmp(&worst.key) == Ordering::Greater
        {
            results.pop();                        // 淘汰最差
            results.push(std::cmp::Reverse(cand));
            frontier.push(cand);
        }
    }
}
```

见 [`src/index/hnsw.rs:243-266`](../../src/index/hnsw.rs)。两个堆的分工:

- `frontier`(最大堆):按"越近键越大",每次弹**最有希望**的候选继续扩展——best-first;
- `results`(最小堆 + `Reverse`):固定大小 `ef`,只淘汰**最差**。`Reverse` 让"堆顶 =
  最小值 = 最差",`pop()` 一步甩掉它,不必遍历找最小值。

> 为什么不用 L0 的 `TopK`?`TopK` 为"在线维护 top-k 结果"设计;图搜索还需要一个可继续
> 扩展的**前沿堆**和"当前最优 vs 当前最差"的提前终止判断,标准 `BinaryHeap` 更直接。
> 两者都是堆,职责不同。`Reverse` 是标准库的 newtype 包装(见 [03 §1.2](03-structs-enums-impl.md)),
> 只影响比较,不改变数据。

### 5.1 `HashSet`:O(1) 去重访问

搜索还要避免重复访问节点。`HashSet<u32>` 的 `insert` 返回 `bool`——**新插入为 `true`,
已存在为 `false`**,"检查 + 标记"因此一步完成:

```rust
let mut visited: HashSet<u32> = HashSet::new();
...
if !visited.insert(neighbor) {
    continue;   // 已经访问过,跳过
}
```

见 [`src/index/hnsw.rs:250-271`](../../src/index/hnsw.rs)。对照 `Vec<u32>` 的 `contains`
是 $O(n)$ 线性扫描;需要反复问"在不在集合里"时,`HashSet` 的期望 $O(1)$ 是数量级差别
(代价是哈希与额外内存)。要放进 `HashSet` 的类型必须实现 `Hash + Eq`(见
[03 §4](03-structs-enums-impl.md))。

> `Vec::contains` 也有用武之地:元素少时线性扫描比哈希更快,而且不要求 `Hash`。L3 的
> 邻接去重 `slot.contains(&other)` 就是这么用的(节点度数 ≤ 32,见
> [`src/index/graph.rs:79-86`](../../src/index/graph.rs))。

### 5.2 `HashMap` 的 entry API 与"排序去重"

L4 的融合要把两个通道的分数按 `RowId` 累加:同一个文档可能已经出现过,也可能第一次出现。
"先查再插"要查两次,`entry` API 一次搞定:

```rust
let mut fused: HashMap<RowId, (SlotId, f32)> = HashMap::new();
for (rank, hit) in channel.iter().enumerate() {
    let add = 1.0 / (k as f32 + (rank + 1) as f32);
    fused.entry(hit.rowid).or_insert((hit.slot, 0.0)).1 += add;
}
```

见 [`src/query/fusion.rs:45-53`](../../src/query/fusion.rs)。要点:

- `entry(key)` 返回 `Entry` 枚举("已在 / 不在"两种),**查找只发生一次**;
- `or_insert(init)` 在不存在时插入 `init` 并返回 `&mut V`,`.1 += add` 直接在槽位上累加。
  若 `init` 构造成本高,用惰性的 `or_insert_with(|| ...)`;
- 想"存在时顺便改一下"用 `and_modify(|v| ...)`;预知规模时 `HashMap::with_capacity(n)`
  预留容量(与 `Vec::with_capacity` 同理,见 §7)。
- 注意 `HashMap` 迭代顺序不确定;要稳定顺序必须像 L4 一样**显式排序**或按 key 取值。
- `keys()` 只借出键的视图(要值用 `values()`,要键值对直接 `iter()`);L4 统计 BM25 平均文档长度
  时 `for slot in docs.keys()`,见 [`src/query/bm25.rs:87-92`](../../src/query/bm25.rs)。
- `map[&key]` 是对 `HashMap` 实现 `Index` 的语法糖,等价于"`get` + 取不到就 panic";
  只在**已经证明键存在**时用——L4 的 `docs[slot]` 刚由同一轮 `keys()` 枚举出来,所以安全;
  拿不准时一律 `get(...).copied()` 配兜底,而不是靠 `[]`。

L4 的分词阶段还把"排序 + 去重"当固定搭配:

```rust
let mut terms = tokenize(query, stopwords);
terms.sort();
terms.dedup();
```

见 [`src/query/bm25.rs:62-68`](../../src/query/bm25.rs)。`Vec::dedup` 只删**相邻**重复,
所以必须先 `sort`——不排序时它几乎什么也不删。这两步合起来是"有序去重";`HashSet` 去重
更快但会丢顺序(见 §5.1)。

---

## 6. `Option` 与数组:都是"可迭代"的(常见组合)

`Option<T>` 实现了 `IntoIterator`(注意:不是 `Iterator`),产出 0 或 1 个元素。因此可以:

```rust
let v: Vec<i32> = [Some(1), None, Some(3)].into_iter().flatten().collect();
// v == [1, 3]
```

数组则经 `IntoIterator` 进入 `for` 循环(即 §2 表中的 `into_iter` 一行,拿到的是元素值)。
mneme 的测试里也常见 `for (score, id) in [...]` 直接遍历数组,见
[`src/core/heap.rs:287-296`](../../src/core/heap.rs)。

---

## 7. 性能提示

- 迭代器链是**惰性且零开销**的:编译器通常会内联成与手写循环等价的机器码。
- 但过度嵌套会降低可读性;mneme 规范允许在算法清晰性优先时使用显式 `while` 循环
  (如 SIMD 内核里为了控制分块,见 [09 章](09-cfg-unsafe-simd.md))。
- 预知结果规模时用 `Vec::with_capacity(n)` 预留容量,避免边 push 边扩容
  (L3 的 hidx 编码就这么做,见 [04 §2.3](04-borrowing-strings-slices.md))。

---

## 8. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `value moved here, in previous iteration` | 在循环里移动了集合元素 | 用 `.iter()` 借用,或每次 clone |
| `cannot infer type` / `type annotations needed` | `collect` 目标类型不明 | 加 `let v: Vec<_>` 标注 |
| `closure may outlive the current function` | 闭包借用了局部变量但会逃逸 | 加 `move` |
| `cannot borrow ... as mutable` | 闭包与外部同时借用冲突 | 调整借用顺序 |
| `no method named map found for &Option<T>` | 在 `&Option<T>` 上调了 `map`(需要 `Option<T>`) | 先 `as_ref()`,或改为持有 `Option<T>` |

---

## 9. 本章小结

- `for` + Range 是基本循环;`iter`/`iter_mut`/`into_iter` 决定借用还是消耗。
- 迭代器适配器(`map`/`filter`/`zip`/`enumerate`/`rev`/`copied`/`find_map`)+ 消费器
  (`sum`/`collect`/`fold`/`max_by_key`)链式组合,惰性零开销;`Vec::extend`/`Vec::truncate`
  常用来收尾。
- `fold(init, f)` 是带累加器的归约;`f64::min`/`f32::min` 这类方法只要签名对得上,就能当函数指针传给 `fold`。
- `HashMap::entry(key).or_insert(..)` 一次查找完成"查 / 插 / 改";`keys()` 遍历键;
  `map[&key]` 键不存在会 panic,拿不准时用 `get(..).copied()`;`sort` + `dedup` 是有序去重。
- 闭包 `|x| ...` 能捕获环境;`move` 强制转移所有权。
- 接收闭包的函数用 `FnOnce`/`FnMut`/`Fn` 声明调用方式;L1 写事务 `write_tx` 用 `FnOnce` 执行一次、
  失败整体回滚。
- `sort_by` 配 `Ordering::{Less, Greater, Equal}` 做自定义排序;mneme 的 `TopK` 借此实现度量感知排序。
- `BinaryHeap` 是最大堆,`Reverse` 把它反转成最小堆;L3 用"最大堆做前沿 + 最小堆淘汰最差"
  实现 best-first 图搜索。
- `HashSet::insert` 返回 `bool`,把"查重 + 标记"合并成一步;元素少时 `Vec::contains` 更划算。

## 动手练习

1. 用一行迭代器求 `vec![1, 2, 3, 4, 5]` 中所有偶数的平方和。
2. 用 `enumerate` 打印一个 `Vec<&str>` 的 `下标:值`。
3. 用 `sort_by` 把 `vec![(2, "b"), (1, "a")]` 按第一个元素升序排序。
4. 用 `BinaryHeap<Reverse<u32>>` 实现"流式取最大的 3 个":不断 `push`,堆里超过 3 个就弹出
   堆顶(当前最小值),最后把堆里的元素取出反转,就是从大到小的前三。
5. 用 `HashMap` 的 entry API 统计 `vec!["a", "b", "a"]` 里每个词出现的次数。
6. 用 `fold(f32::NEG_INFINITY, f32::max)` 求一个 `Vec<f32>` 的最大值,并解释为什么不能直接写
   `iter().copied().max()`(提示:见 [03 §4.1](03-structs-enums-impl.md))。

## 下一章

[08 模块、可见性与文档](08-modules-docs.md):代码怎么分文件、怎么暴露、怎么写文档。

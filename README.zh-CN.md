# CavityRank

[English](README.md)

CavityRank 是一个无第三方依赖的 Rust 研究实现，底层为每桶四槽的紧凑
Cuckoo Filter。它利用不影响查询结果的指纹排列编码逐出方向，不增加每桶
持久路由字节。

当前版本是单线程研究软件，不是并发生产库。配套 arXiv 论文取得编号后会在
此处补充链接。

## 核心机制

每桶把四个非零 `u16` 指纹装入一个 `u64`。Pair-CavityBit 使用第一对槽位
的方向；CavityRank 使用两对槽位方向编码 1–4 级截断剩余秩：

```text
let cavity_bit_rank = 1 + u8::from(slot1 < slot0);
let cavity_rank = 1 + u8::from(slot1 < slot0) + 2 * u8::from(slot3 < slot2);
```

查询仍只比较两个候选桶中的八个指纹，不解码 rank。逐出时，算法按备用桶
rank 选择最小边，删除受害边、加入 incoming 指纹的 predecessor 边，再把
更新后真实可解码的剩余 rank 写回桶内排列。

## 快速使用

```rust
use cavity_bit_filter::{Config, CuckooFilter, Policy};

fn main() -> Result<(), cavity_bit_filter::ConfigError> {
    let mut filter = CuckooFilter::new(Config {
        bucket_count: 1 << 19,
        policy: Policy::CavityRank4,
        seed: 42,
        max_kicks: 5_000,
        bfs_depth: 10,
        path: Default::default(),
    })?;

    let result = filter.insert(123);
    if !result.inserted {
        // 非 BFS 有界插入失败后，该过滤器不可继续使用。
        assert!(!result.filter_usable);
        return Ok(());
    }
    assert!(filter.contains(123));
    Ok(())
}
```

主要策略：

- `CavityBit`：两级隐式剩余路由。
- `CavityRank4`：四级核心方法。
- `DenseCavityRank4`：先使用 Rotor，达到 96% 时三遍扫描全表，再切换到
  CavityRank。
- `CavityRank4Path`：用于尾部实验的插入局部 path sketch。

仓库同时保留共享同一哈希、桶布局和统计口径的研究基线，以及
`cavity-bench` 实验命令行工具。

## 重要语义

- `bucket_count` 必须是至少 2 的二次幂。
- API 接受 `u64` 键；其他业务数据应先在调用侧哈希。
- 过滤器存在误报，不能充当权威集合。
- `remove` 只应用于已知存在的键；删除误报键可能移除共享指纹。
- 非 BFS 有界插入失败可能破坏当前实例。若
  `InsertStats::filter_usable == false`，必须丢弃或重建。
- 删除不会反向修复剩余 rank。
- Dense 准备阶段会 stop-the-world 扫描全表；对停顿敏感时应在批处理边界
  主动调用 `prepare_dense`。
- 内置 SplitMix 风格映射使用固定常量，不是密码学或对抗性 keyed hash。
  不可信输入应先经过应用自己控制的带密钥哈希。
- 当前性能证据只覆盖一台本地 Apple M4 Pro，不能据此推断 x86-64 性能。

## 构建与验证

支持 Rust 1.92 或更新版本。

```sh
cargo fmt --check
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
```

运行 `cargo run --release --bin cavity-bench -- help` 查看实验 CLI。
CLI 拒绝覆盖已有 CSV 和延迟 sidecar。`build --verify true` 会检查每个 seed，
`verify-samples` 是严格样本上限。query 只插入偶数键、只查询奇数键，保证每个
测量键都未插入；churn 与 query 行还会报告 `filter_usable`。

## 源码与实验工件边界

本目录有意不包含原始数据、编译后二进制、本机身份日志、论文构建产物和旧
Git 对象。源码来源与基线出处见 [PROVENANCE.md](PROVENANCE.md)，带校验和的
完整研究工件将独立发布。

## 许可与引用

软件使用 [MIT License](LICENSE)。引用信息见 [CITATION.cff](CITATION.cff)；
arXiv 编号分配后再补充。

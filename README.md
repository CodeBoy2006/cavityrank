# CavityRank

[![Paper](https://img.shields.io/badge/arXiv-2608.13970-b31b1b.svg)](https://arxiv.org/abs/2608.13970)

[Chinese translation](README.zh-CN.md)

CavityRank is a dependency-free, single-threaded Rust implementation of the
packed four-slot Cuckoo Filter described in
[CavityRank: Zero-Extra-Byte Residual Routing for Cuckoo Filters](https://arxiv.org/abs/2608.13970).
It stores relocation guidance in query-equivalent fingerprint order, adding no
routing bytes per bucket and leaving the two-bucket lookup unchanged.

## How it works

Each bucket packs four nonzero `u16` fingerprints into one `u64`. The two slot
pair orientations encode a residual rank from 1 to 4:

```text
let rank = 1 + u8::from(slot1 < slot0) + 2 * u8::from(slot3 < slot2);
```

Ranks guide relocation only. Lookup still compares the requested fingerprint
with the eight fingerprints in its two candidate buckets.

## Quick start

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
        // A failed bounded insertion may leave the filter unusable.
        assert!(!result.filter_usable);
        return Ok(());
    }

    assert!(filter.contains(123));
    Ok(())
}
```

`CavityRank4` is the paper's core policy. The crate also includes the two-level
`CavityBit`, high-load `DenseCavityRank4`, experimental `CavityRank4Path`,
research baselines, and the `cavity-bench` CLI.

## Important semantics

- `bucket_count` must be a power of two and at least two.
- Keys are `u64`; prehash untrusted input with an application-owned keyed hash.
- The filter has false positives. Call `remove` only for a known-present key.
- After a failed bounded insertion, discard or rebuild the filter when
  `InsertStats::filter_usable` is false.
- Dense preparation scans the full table and pauses insertion. Call
  `prepare_dense` at a controlled batch boundary when latency matters.

This is research software, not a concurrent production library.

## Build and verify

Rust 1.92 or newer is required.

```sh
cargo fmt --check
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
```

Run `cargo run --release --bin cavity-bench -- help` for the experiment CLI.

## Citation and license

Citation metadata is in [CITATION.cff](CITATION.cff), and source provenance is
documented in [PROVENANCE.md](PROVENANCE.md). The code is available under the
[MIT License](LICENSE).

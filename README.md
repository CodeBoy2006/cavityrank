# CavityRank

[简体中文](README.zh-CN.md)

CavityRank is a dependency-free Rust research implementation of a packed
four-slot Cuckoo Filter. It stores relocation guidance in query-equivalent
fingerprint order, adding no persistent routing bytes per bucket.

This release is single-threaded research software, not a concurrent production
library. The accompanying arXiv preprint will be linked here after it receives
an identifier.

## Core idea

Each bucket stores four nonzero `u16` fingerprints in one `u64`. Pair-CavityBit
uses the orientation of the first slot pair; CavityRank uses two pair
orientations to encode a truncated residual rank from 1 to 4:

```text
let cavity_bit_rank = 1 + u8::from(slot1 < slot0);
let cavity_rank = 1 + u8::from(slot1 < slot0) + 2 * u8::from(slot3 < slot2);
```

Lookup remains unchanged: it compares the requested fingerprint with the eight
fingerprints in the two candidate buckets and never decodes the rank.

During relocation, CavityRank scores each resident by its alternate bucket,
evicts a minimum-rank edge, removes that victim edge from the residual state,
adds the incoming predecessor edge, and re-encodes the realized rank in the
updated bucket.

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
        // A failed bounded non-BFS insertion leaves the filter unusable.
        assert!(!result.filter_usable);
        return Ok(());
    }
    assert!(filter.contains(123));
    Ok(())
}
```

The main routing policies are:

- `CavityBit`: two-level implicit residual routing.
- `CavityRank4`: the four-level core method.
- `DenseCavityRank4`: Rotor until 96% load, then a three-pass full-table
  preparation before using CavityRank.
- `CavityRank4Path`: the core method with an optional insertion-local path
  sketch for tail experiments.

The crate also contains the research baselines and the `cavity-bench` experiment
CLI used to compare them under a shared hash, bucket layout, and accounting
contract.

## Important semantics

- `bucket_count` must be a power of two and at least two.
- Keys are `u64`; hash arbitrary application data before calling this API.
- The filter has false positives. It is not an authoritative set.
- Call `remove` only for a key known to be present. Removing a false positive
  may delete a shared fingerprint.
- A failed bounded non-BFS insertion may leave the filter unusable. Check
  `InsertStats::filter_usable` and discard or rebuild the instance when false.
- Deletion does not run backward residual-rank repair.
- Dense preparation scans the full table and is stop-the-world. Call
  `prepare_dense` at a controlled batch boundary when pauses matter.
- The built-in SplitMix-style key mapping uses fixed constants and is not a
  cryptographic or adversarially keyed hash. Prehash untrusted inputs with an
  application-owned keyed hash.
- Performance evidence currently covers one local Apple M4 Pro. Do not infer
  x86-64 performance from those measurements.

## Build and verify

Rust 1.92 or newer is supported.

```sh
cargo fmt --check
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
```

Run `cargo run --release --bin cavity-bench -- help` for the experiment CLI.
The CLI refuses to overwrite existing CSV and latency sidecar files.
`build --verify true` checks every seed, and `verify-samples` is a strict upper
bound. Query runs insert even keys and probe odd keys, guaranteeing that every
measured query key is absent. Churn and query rows also report `filter_usable`.

## Source and artifact boundary

This source distribution intentionally excludes raw datasets, compiled
binaries, machine-identifying logs, generated paper files, and historical Git
objects. See [PROVENANCE.md](PROVENANCE.md) for the source origin and baseline
references. The checksum-bound research artifact is released separately.

## License and citation

The software is available under the [MIT License](LICENSE). Citation metadata
is provided in [CITATION.cff](CITATION.cff); the arXiv identifier will be added
after assignment.

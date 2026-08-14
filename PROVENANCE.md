# Source provenance

This directory is a clean source distribution derived from research repository
commit `9c43bc6dc0a0edbf6534ffb9410fd986b5869175` on 2026-08-14.

The six files under `src/` preserve the implementation from that commit, with
comments and API documentation refined for this distribution. Their hashes are
recorded in `SOURCE_SHA256SUMS`. `Cargo.toml` differs only by release metadata
and an explicit package allowlist.

## Included

- The packed Rust library, inline tests, exact Rust oracle, experiment helpers,
  and `cavity-bench` command-line program.
- Documentation, citation metadata, and the MIT license.

## Intentionally excluded

- Raw measurements, latency sidecars, compiled binaries, machine logs, and
  generated paper files.
- Historical pilot results and superseded experiments.
- Supplied research bundles and all original Git history.

The full, checksum-bound research artifact is distributed separately so that
ordinary source clones remain small and the frozen evidence is not rewritten.

## Research baseline provenance

- Better Choice follows the lower-occupancy initial-choice rule in the pinned
  [BCF author artifact](https://github.com/CGCL-codes/BCF/tree/8c03b6e7dfd452ee3758a29722af77e75fb62dd6).
- The LSA baseline is a four-slot partial-key adaptation of Khosla and Anand,
  [A Faster Algorithm for Cuckoo Insertion and Bipartite Matching in Large
  Graphs](https://arxiv.org/abs/1611.07786).
- No implementation is claimed for Local Minimum because a sufficiently
  detailed primary specification was unavailable.

No third-party source tree, compiled artifact, or external Rust dependency is
included in this distribution.

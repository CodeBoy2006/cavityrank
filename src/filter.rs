use crate::bucket::{Bucket, SLOTS};
use std::{error::Error, fmt, str::FromStr};

const KEY_HASH_SALT: u64 = 0xa076_1d64_78bd_642f;
const RNG_SALT: u64 = 0x243f_6a88_85a3_08d3;
const PATH_HASH_SALT: u64 = 0xd6e8_feb8_6659_fd93;
const GOLDEN_RATIO: u64 = 0x9e37_79b9_7f4a_7c15;
const NO_PARENT: u32 = u32::MAX;
const DENSE_LOAD_NUMERATOR: usize = 24;
const DENSE_LOAD_DENOMINATOR: usize = 25;

/// Insertion and relocation strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Policy {
    /// Chooses candidate buckets and eviction slots pseudo-randomly.
    Random,
    /// Prefers the less occupied candidate bucket, then uses random evictions.
    BetterChoice,
    /// Evicts the oldest slot by rotating each full bucket.
    Rotor,
    /// Prefers alternate buckets with lower persistent eviction counters.
    EvictionLabel,
    /// Uses persistent LSA residual labels to select an eviction edge.
    Lsa,
    /// Eight-state orientation baseline encoded by all four slot positions.
    CavityD4,
    /// Scans all four alternate buckets but treats every full bucket as rank 2.
    CavityScan,
    /// Pair-CavityBit: a two-level rank encoded by the first slot pair.
    CavityBit,
    /// CavityRank-4: a four-level rank encoded by two slot pairs.
    CavityRank4,
    /// CavityRank-4 with a lazily activated, insertion-local visited-bucket sketch.
    CavityRank4Path,
    /// Uses Rotor until automatic preparation at 96% load, then CavityRank-4.
    DenseCavityRank4,
    /// Finds a bounded augmenting path with reusable breadth-first-search state.
    Bfs,
}

impl fmt::Display for Policy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Random => "random",
            Self::BetterChoice => "better_choice",
            Self::Rotor => "rotor_queue",
            Self::EvictionLabel => "eviction_label",
            Self::Lsa => "lsa",
            Self::CavityD4 => "cavity_d4",
            Self::CavityScan => "cavity_scan",
            Self::CavityBit => "cavity_bit",
            Self::CavityRank4 => "cavity_rank4",
            Self::CavityRank4Path => "cavity_rank4_path",
            Self::DenseCavityRank4 => "dense_cr4",
            Self::Bfs => "bfs",
        })
    }
}

impl FromStr for Policy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "random" => Ok(Self::Random),
            "better_choice" => Ok(Self::BetterChoice),
            "rotor_queue" => Ok(Self::Rotor),
            "eviction_label" => Ok(Self::EvictionLabel),
            "lsa" => Ok(Self::Lsa),
            "cavity_d4" => Ok(Self::CavityD4),
            "cavity_scan" => Ok(Self::CavityScan),
            "cavity_bit" => Ok(Self::CavityBit),
            "cavity_rank4" => Ok(Self::CavityRank4),
            "cavity_rank4_path" => Ok(Self::CavityRank4Path),
            "dense_cr4" => Ok(Self::DenseCavityRank4),
            "bfs" => Ok(Self::Bfs),
            _ => Err(format!("unknown policy: {value}")),
        }
    }
}

/// Condition that activates the insertion-local path sketch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathActivation {
    /// Activate before relocation `N + 1`.
    After(u32),
    /// Activate when no target has a strictly smaller rank than the current
    /// bucket.
    NoDescent,
    /// Activate when the current bucket and all four candidate targets have rank 4.
    Rank4Plateau,
}

impl fmt::Display for PathActivation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::After(step) => write!(formatter, "after:{step}"),
            Self::NoDescent => formatter.write_str("no_descent"),
            Self::Rank4Plateau => formatter.write_str("rank4_plateau"),
        }
    }
}

impl FromStr for PathActivation {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(step) = value.strip_prefix("after:") {
            return step
                .parse()
                .map(Self::After)
                .map_err(|error| format!("invalid path activation {value}: {error}"));
        }
        match value {
            "no_descent" => Ok(Self::NoDescent),
            "rank4_plateau" => Ok(Self::Rank4Plateau),
            _ => Err(format!("unknown path activation: {value}")),
        }
    }
}

/// Strategy for clearing the insertion-local path sketch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathReset {
    /// Clear the full sketch when an insertion activates it.
    Full,
    /// Clear only words touched by the insertion when that insertion ends.
    Sparse,
    /// Advance a global generation and lazily clear each word when first written.
    Generational,
}

impl fmt::Display for PathReset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Full => "full",
            Self::Sparse => "sparse",
            Self::Generational => "generational",
        })
    }
}

impl FromStr for PathReset {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "full" => Ok(Self::Full),
            "sparse" => Ok(Self::Sparse),
            "generational" => Ok(Self::Generational),
            _ => Err(format!("unknown path reset: {value}")),
        }
    }
}

/// Configuration for [`Policy::CavityRank4Path`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathConfig {
    /// Bitset bytes; CR4-Path supports 512 or 2,048, excluding reset
    /// bookkeeping.
    pub bytes: usize,
    /// Condition that starts recording and checking the sketch.
    pub activation: PathActivation,
    /// Method used to clear state between activated insertions.
    pub reset: PathReset,
}

impl Default for PathConfig {
    fn default() -> Self {
        Self {
            bytes: 2_048,
            activation: PathActivation::After(128),
            reset: PathReset::Full,
        }
    }
}

/// Construction parameters for [`CuckooFilter`].
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Power-of-two bucket count in `2..=2^32`.
    pub bucket_count: usize,
    /// Insertion and relocation strategy.
    pub policy: Policy,
    /// Seed for policy-specific pseudo-random choices.
    pub seed: u64,
    /// Maximum relocations for bounded kick-based policies; must be nonzero.
    pub max_kicks: u32,
    /// Maximum path depth for [`Policy::Bfs`]; must be nonzero.
    pub bfs_depth: u8,
    /// Settings used only by [`Policy::CavityRank4Path`].
    pub path: PathConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bucket_count: 1 << 17,
            policy: Policy::CavityBit,
            seed: 0,
            max_kicks: 5_000,
            bfs_depth: 10,
            path: PathConfig::default(),
        }
    }
}

/// Invalid filter configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// `bucket_count` is smaller than two or is not a power of two.
    BucketCount,
    /// `bucket_count - 1` does not fit in the internal `u32` mask.
    BucketCountTooLarge,
    /// `max_kicks` is zero.
    MaxKicks,
    /// `bfs_depth` is zero.
    BfsDepth,
    /// CR4-Path was configured with an unsupported bitset size.
    PathBytes,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BucketCount => "bucket count must be a power of two and at least two",
            Self::BucketCountTooLarge => "bucket count must fit in 32-bit bucket indices",
            Self::MaxKicks => "max_kicks must be greater than zero",
            Self::BfsDepth => "bfs_depth must be greater than zero",
            Self::PathBytes => "path sketch bytes must be 512 or 2048",
        })
    }
}

impl Error for ConfigError {}

/// Outcome and logical bucket-access counts for one insertion attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub struct InsertStats {
    /// Whether the key was inserted.
    pub inserted: bool,
    /// Whether the filter remains safe to use after this attempt.
    ///
    /// Kick-based policies avoid an `O(max_kicks)` undo log. Their bounded
    /// failures therefore set this to `false`; discard or rebuild the filter.
    pub filter_usable: bool,
    /// Number of fingerprints moved between buckets.
    pub relocations: u32,
    /// Logical bucket reads charged by the insertion model.
    pub bucket_reads: u32,
    /// Logical bucket writes charged by the insertion model.
    pub bucket_writes: u32,
    /// Whether the CR4-Path sketch activated.
    pub path_activated: bool,
    /// One-based relocation step at activation; zero when the guard stayed
    /// inactive.
    pub path_activation_step: u32,
    /// Relocation steps protected by the active path sketch.
    pub path_guarded_steps: u32,
    /// Equal-rank candidate buckets checked against the sketch.
    pub path_checks: u32,
    /// Equal-rank candidates rejected because the sketch reported them seen.
    pub path_seen_candidates_rejected: u32,
}

impl Default for InsertStats {
    fn default() -> Self {
        Self {
            inserted: false,
            filter_usable: true,
            relocations: 0,
            bucket_reads: 0,
            bucket_writes: 0,
            path_activated: false,
            path_activation_step: 0,
            path_guarded_steps: 0,
            path_checks: 0,
            path_seen_candidates_rejected: 0,
        }
    }
}

/// Work performed by [`CuckooFilter::prepare_dense`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[must_use]
pub struct DensePreparationStats {
    /// Whether this call performed the one-time preparation.
    pub prepared: bool,
    /// Logical bucket reads during preparation.
    pub bucket_reads: u64,
    /// Logical bucket writes during preparation.
    pub bucket_writes: u64,
}

/// Single-threaded packed Cuckoo Filter over `u64` keys.
#[derive(Debug)]
pub struct CuckooFilter {
    buckets: Vec<Bucket>,
    eviction_labels: Vec<u8>,
    lsa_labels: Vec<u64>,
    policy: Policy,
    mask: u32,
    rng: FastRng,
    max_kicks: u32,
    size: usize,
    dense_mode: bool,
    bfs: Option<BfsScratch>,
    path: Option<PathSketch>,
}

impl CuckooFilter {
    /// Creates an empty filter after validating `config`.
    pub fn new(config: Config) -> Result<Self, ConfigError> {
        if config.bucket_count < 2 || !config.bucket_count.is_power_of_two() {
            return Err(ConfigError::BucketCount);
        }
        if config.bucket_count - 1 > u32::MAX as usize {
            return Err(ConfigError::BucketCountTooLarge);
        }
        if config.max_kicks == 0 {
            return Err(ConfigError::MaxKicks);
        }
        if config.bfs_depth == 0 {
            return Err(ConfigError::BfsDepth);
        }
        if config.policy == Policy::CavityRank4Path && !matches!(config.path.bytes, 512 | 2_048) {
            return Err(ConfigError::PathBytes);
        }

        let eviction_labels = if config.policy == Policy::EvictionLabel {
            vec![0; config.bucket_count]
        } else {
            Vec::new()
        };
        let lsa_labels = if config.policy == Policy::Lsa {
            vec![0; config.bucket_count]
        } else {
            Vec::new()
        };
        let bfs = (config.policy == Policy::Bfs)
            .then(|| BfsScratch::new(config.bucket_count, config.bfs_depth));
        let path = (config.policy == Policy::CavityRank4Path).then(|| PathSketch::new(config.path));

        Ok(Self {
            buckets: vec![Bucket::default(); config.bucket_count],
            eviction_labels,
            lsa_labels,
            policy: config.policy,
            mask: (config.bucket_count - 1) as u32,
            rng: FastRng(config.seed ^ RNG_SALT),
            max_kicks: config.max_kicks,
            size: 0,
            dense_mode: config.policy != Policy::DenseCavityRank4,
            bfs,
            path,
        })
    }

    /// Returns the number of successful insertions minus successful removals.
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Returns whether the filter contains no stored fingerprints.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Returns the physical slot count.
    ///
    /// Successful insertion near this limit is not guaranteed.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buckets.len() * SLOTS
    }

    /// Returns `len / capacity`.
    #[inline]
    pub fn load_factor(&self) -> f64 {
        self.size as f64 / self.capacity() as f64
    }

    /// Returns allocated bucket-payload bytes, excluding the `Vec` header.
    #[inline]
    pub fn payload_bytes(&self) -> usize {
        self.buckets.capacity() * size_of::<Bucket>()
    }

    /// Returns allocated persistent-label bytes from `Vec` capacities.
    ///
    /// Bucket payloads and `Vec` headers are excluded.
    #[inline]
    pub fn persistent_metadata_bytes(&self) -> usize {
        self.eviction_labels.capacity() * size_of::<u8>()
            + self.lsa_labels.capacity() * size_of::<u64>()
    }

    /// Returns allocated reusable BFS/path workspace from `Vec` capacities.
    ///
    /// `Vec` headers are excluded.
    #[inline]
    pub fn transient_insertion_workspace_bytes(&self) -> usize {
        self.bfs.as_ref().map_or(0, BfsScratch::storage_bytes)
            + self.path.as_ref().map_or(0, PathSketch::storage_bytes)
    }

    /// Returns `(full_buckets, alias_buckets)` for CR4 policies.
    ///
    /// Alias buckets contain three or four equal fingerprints and cannot encode
    /// all four ranks. Other policies return `(0, 0)`. This is a full-table scan
    /// and is never called from the insertion path.
    pub fn rank4_codec_counts(&self) -> (usize, usize) {
        if !matches!(
            self.policy,
            Policy::CavityRank4 | Policy::CavityRank4Path | Policy::DenseCavityRank4
        ) {
            return (0, 0);
        }

        let mut full_buckets = 0;
        let mut alias_buckets = 0;
        for &word in &self.buckets {
            if !word.is_full() {
                continue;
            }
            full_buckets += 1;
            let [a, b, c, d] = word.slots();
            alias_buckets += usize::from(
                (a == b && (a == c || a == d)) || (a == c && a == d) || (b == c && b == d),
            );
        }
        (full_buckets, alias_buckets)
    }

    /// Attempts to insert `key` and returns operation statistics.
    ///
    /// Always inspect [`InsertStats::filter_usable`] after a failed insertion.
    pub fn insert(&mut self, key: u64) -> InsertStats {
        if self.needs_dense_prepare() {
            let _ = self.prepare_dense();
        }
        let (fingerprint, first, second, hash) = key_parts(self.mask, key);
        self.insert_fingerprint(fingerprint, first, second, hash)
    }

    /// Prepares a [`Policy::DenseCavityRank4`] filter for ranked insertion.
    ///
    /// This synchronous three-pass full-table scan encodes exact residual
    /// distances 1..=3 and rank 4 otherwise. Three/four-equal fingerprint
    /// multisets retain CR4's rank-alias behavior. Already-prepared filters and
    /// other policies are no-ops.
    pub fn prepare_dense(&mut self) -> DensePreparationStats {
        let mut stats = DensePreparationStats::default();
        if self.policy != Policy::DenseCavityRank4 || self.dense_mode {
            return stats;
        }

        for pass in 0..3 {
            for bucket in 0..self.buckets.len() {
                stats.bucket_reads += 1;
                let word = self.buckets[bucket];
                if !word.is_full() || (pass > 0 && word.decode_rank4() < 4) {
                    continue;
                }

                let residents = word.slots();
                let mut best = 4;
                for fingerprint in residents {
                    let neighbor = alt_index(self.mask, bucket as u32, fingerprint);
                    let neighbor_word = self.bucket(neighbor);
                    stats.bucket_reads += 1;
                    let score = if !neighbor_word.is_full() {
                        0
                    } else if pass == 0 {
                        4
                    } else {
                        neighbor_word.decode_rank4()
                    };
                    best = best.min(score);
                    if best == 0 {
                        break;
                    }
                }

                let rank = (1 + best).min(4);
                if pass == 0 || rank < 4 {
                    self.buckets[bucket] = Bucket::encode_rank4(residents, rank);
                    stats.bucket_writes += 1;
                }
            }
        }

        self.dense_mode = true;
        stats.prepared = true;
        stats
    }

    #[inline]
    pub(crate) fn needs_dense_prepare(&self) -> bool {
        self.policy == Policy::DenseCavityRank4
            && !self.dense_mode
            && self.size >= self.capacity() * DENSE_LOAD_NUMERATOR / DENSE_LOAD_DENOMINATOR
    }

    /// Returns whether either candidate bucket contains `key`'s fingerprint.
    ///
    /// A `true` result may be a false positive.
    #[inline]
    pub fn contains(&self, key: u64) -> bool {
        let (fingerprint, first, second, _) = key_parts(self.mask, key);
        self.bucket(first).contains(fingerprint) || self.bucket(second).contains(fingerprint)
    }

    /// Removes one matching fingerprint, if present.
    ///
    /// Call this only for a key known to have been inserted: removing a false
    /// positive can delete a fingerprint shared with another key.
    pub fn remove(&mut self, key: u64) -> bool {
        let (fingerprint, first, second, _) = key_parts(self.mask, key);
        if self.remove_from(first, fingerprint)
            || (second != first && self.remove_from(second, fingerprint))
        {
            self.size -= 1;
            true
        } else {
            false
        }
    }

    fn remove_from(&mut self, bucket: u32, fingerprint: u16) -> bool {
        self.buckets[bucket as usize].remove_first(fingerprint)
    }

    #[inline]
    fn bucket(&self, index: u32) -> Bucket {
        self.buckets[index as usize]
    }

    #[inline]
    fn lsa_score(&self, bucket: u32, word: Bucket) -> u64 {
        if word.is_full() {
            self.lsa_labels[bucket as usize]
        } else {
            0
        }
    }

    fn initialize_lsa_full_bucket(
        &self,
        bucket: u32,
        residents: [u16; SLOTS],
        stats: &mut InsertStats,
    ) -> u64 {
        let mut best = u64::MAX;
        for fingerprint in residents {
            let neighbor = alt_index(self.mask, bucket, fingerprint);
            let neighbor_word = self.bucket(neighbor);
            stats.bucket_reads += 1;
            best = best.min(self.lsa_score(neighbor, neighbor_word));
        }
        best.checked_add(1).expect("LSA label overflow")
    }

    fn insert_fingerprint(
        &mut self,
        fingerprint: u16,
        first: u32,
        second: u32,
        key_hash: u64,
    ) -> InsertStats {
        if matches!(
            self.policy,
            Policy::CavityD4
                | Policy::CavityScan
                | Policy::CavityBit
                | Policy::CavityRank4
                | Policy::CavityRank4Path
                | Policy::DenseCavityRank4
        ) && (self.policy != Policy::DenseCavityRank4 || self.dense_mode)
        {
            return self.insert_ranked(fingerprint, first, second, key_hash);
        }

        let first_word = self.bucket(first);
        let second_word = self.bucket(second);
        let first_occupancy = first_word.occupancy();
        let second_occupancy = second_word.occupancy();
        let mut stats = InsertStats {
            bucket_reads: u32::from(first != second) + 1,
            ..InsertStats::default()
        };

        if first_occupancy < SLOTS || second_occupancy < SLOTS {
            let destination = match self.policy {
                Policy::Random => {
                    random_destination(first, second, first_occupancy, second_occupancy, key_hash)
                }
                Policy::BetterChoice => {
                    better_choice_destination(first, second, first_occupancy, second_occupancy)
                }
                _ => direct_destination(first, second, first_occupancy, second_occupancy, key_hash),
            };
            let mut word = if destination == first {
                first_word
            } else {
                second_word
            };
            assert!(word.append(fingerprint));
            if self.policy == Policy::Lsa && word.is_full() {
                self.lsa_labels[destination as usize] =
                    self.initialize_lsa_full_bucket(destination, word.slots(), &mut stats);
            }
            self.buckets[destination as usize] = word;
            stats.bucket_writes = 1;
            stats.inserted = true;
            self.size += 1;
            return stats;
        }

        if self.policy == Policy::Bfs {
            let mut bfs_stats = self.insert_bfs(fingerprint, first, second, key_hash);
            bfs_stats.bucket_reads += stats.bucket_reads;
            if bfs_stats.inserted {
                self.size += 1;
            }
            return bfs_stats;
        }

        let mut bucket = match self.policy {
            Policy::BetterChoice => first,
            Policy::Lsa => lsa_root(
                first,
                second,
                self.lsa_labels[first as usize],
                self.lsa_labels[second as usize],
            ),
            _ if (key_hash >> 33) & 1 == 1 => first,
            _ => second,
        };
        let mut lsa_predecessor = if self.policy == Policy::Lsa {
            let predecessor = if bucket == first { second } else { first };
            self.lsa_labels[predecessor as usize]
        } else {
            0
        };
        let mut carry = fingerprint;

        for step in 0..self.max_kicks {
            stats.bucket_reads += 1;
            let word = self.bucket(bucket);
            let victim = match self.policy {
                Policy::Random | Policy::BetterChoice => {
                    let slot = (self.rng.next() & 3) as usize;
                    let victim = word.slot(slot);
                    self.buckets[bucket as usize].replace(slot, carry);
                    victim
                }
                Policy::Rotor | Policy::DenseCavityRank4 => {
                    let victim = word.slot(0);
                    let mut residents = word.slots();
                    residents.rotate_left(1);
                    residents[3] = carry;
                    self.buckets[bucket as usize] = Bucket::from_slots(residents);
                    victim
                }
                Policy::EvictionLabel => {
                    let residents = word.sorted_slots();
                    let start = (splitmix64(
                        (u64::from(bucket) << 32) ^ (u64::from(carry) << 8) ^ u64::from(step),
                    ) & 3) as usize;
                    let mut best_slot = 0;
                    let mut best_score = u8::MAX;

                    for offset in 0..SLOTS {
                        let slot = (start + offset) & 3;
                        let neighbor = alt_index(self.mask, bucket, residents[slot]);
                        stats.bucket_reads += 1;
                        let neighbor_word = self.bucket(neighbor);
                        let score = if neighbor_word.is_full() {
                            self.eviction_labels[neighbor as usize]
                        } else {
                            0
                        };
                        if score < best_score {
                            best_score = score;
                            best_slot = slot;
                        }
                    }

                    let victim = residents[best_slot];
                    let mut updated = [0_u16; SLOTS];
                    let mut destination = 0;
                    for (slot, resident) in residents.into_iter().enumerate() {
                        if slot != best_slot {
                            updated[destination] = resident;
                            destination += 1;
                        }
                    }
                    updated[3] = carry;

                    updated.sort_unstable();
                    self.buckets[bucket as usize] = Bucket::from_slots(updated);
                    let label = &mut self.eviction_labels[bucket as usize];
                    *label = label.saturating_add(1);
                    victim
                }
                Policy::Lsa => {
                    let residents = word.sorted_slots();
                    let start = (splitmix64(
                        (u64::from(bucket) << 32) ^ (u64::from(carry) << 8) ^ u64::from(step),
                    ) & 3) as usize;
                    let mut scores = [0_u64; SLOTS];
                    let mut best_slot = start;
                    let mut best_score = u64::MAX;

                    for offset in 0..SLOTS {
                        let slot = (start + offset) & 3;
                        let neighbor = alt_index(self.mask, bucket, residents[slot]);
                        stats.bucket_reads += 1;
                        let score = self.lsa_score(neighbor, self.bucket(neighbor));
                        scores[slot] = score;
                        if score < best_score {
                            best_score = score;
                            best_slot = slot;
                        }
                    }

                    let victim = residents[best_slot];
                    let mut updated = [0_u16; SLOTS];
                    let mut destination = 0;
                    for (slot, resident) in residents.into_iter().enumerate() {
                        if slot != best_slot {
                            updated[destination] = resident;
                            destination += 1;
                        }
                    }
                    updated[3] = carry;
                    updated.sort_unstable();

                    let label = lsa_post_eviction_label(lsa_predecessor, scores, best_slot);
                    self.buckets[bucket as usize] = Bucket::from_slots(updated);
                    self.lsa_labels[bucket as usize] = label;
                    lsa_predecessor = label;
                    victim
                }
                Policy::CavityD4
                | Policy::CavityScan
                | Policy::CavityBit
                | Policy::CavityRank4
                | Policy::CavityRank4Path
                | Policy::Bfs => unreachable!(),
            };

            stats.bucket_writes += 1;
            stats.relocations += 1;
            carry = victim;
            let next = alt_index(self.mask, bucket, carry);
            bucket = next;

            stats.bucket_reads += 1;
            let mut word = self.bucket(bucket);
            if word.append(carry) {
                if self.policy == Policy::Lsa && word.is_full() {
                    self.lsa_labels[bucket as usize] =
                        self.initialize_lsa_full_bucket(bucket, word.slots(), &mut stats);
                }
                self.buckets[bucket as usize] = word;
                stats.bucket_writes += 1;
                stats.inserted = true;
                self.size += 1;
                return stats;
            }
        }
        stats.filter_usable = false;
        stats
    }

    #[inline]
    fn rank_cap(&self) -> u8 {
        match self.policy {
            Policy::CavityScan | Policy::CavityBit => 2,
            Policy::CavityRank4 | Policy::CavityRank4Path | Policy::DenseCavityRank4 => 4,
            Policy::CavityD4 => 8,
            Policy::Random
            | Policy::BetterChoice
            | Policy::Rotor
            | Policy::EvictionLabel
            | Policy::Lsa
            | Policy::Bfs => unreachable!(),
        }
    }

    #[inline]
    fn decode_rank(&self, word: Bucket) -> u8 {
        match self.policy {
            Policy::CavityScan => 2,
            Policy::CavityBit => word.decode_cavity(),
            Policy::CavityRank4 | Policy::CavityRank4Path | Policy::DenseCavityRank4 => {
                word.decode_rank4()
            }
            Policy::CavityD4 => word.decode_d4(),
            Policy::Random
            | Policy::BetterChoice
            | Policy::Rotor
            | Policy::EvictionLabel
            | Policy::Lsa
            | Policy::Bfs => unreachable!(),
        }
    }

    #[inline]
    fn encode_rank(&self, residents: [u16; SLOTS], rank: u8) -> Bucket {
        match self.policy {
            Policy::CavityScan => Bucket::encode_cavity(residents, 2),
            Policy::CavityBit => Bucket::encode_cavity(residents, rank),
            Policy::CavityRank4 | Policy::CavityRank4Path | Policy::DenseCavityRank4 => {
                Bucket::encode_rank4(residents, rank)
            }
            Policy::CavityD4 => Bucket::encode_d4(residents, rank),
            Policy::Random
            | Policy::BetterChoice
            | Policy::Rotor
            | Policy::EvictionLabel
            | Policy::Lsa
            | Policy::Bfs => unreachable!(),
        }
    }

    fn initialize_ranked_full_bucket(
        &self,
        bucket: u32,
        residents: [u16; SLOTS],
        incoming_slot: usize,
        predecessor_rank: u8,
        stats: &mut InsertStats,
    ) -> u8 {
        if self.policy == Policy::CavityScan {
            return 2;
        }
        let mut best = predecessor_rank;
        if best == 0 {
            return 1;
        }
        for (slot, fingerprint) in residents.into_iter().enumerate() {
            if slot == incoming_slot {
                continue;
            }
            let neighbor_word = self.bucket(alt_index(self.mask, bucket, fingerprint));
            stats.bucket_reads += 1;
            best = best.min(if neighbor_word.is_full() {
                self.decode_rank(neighbor_word)
            } else {
                0
            });
            if best == 0 {
                break;
            }
        }
        self.rank_cap().min(1 + best)
    }

    fn insert_ranked(
        &mut self,
        fingerprint: u16,
        first: u32,
        second: u32,
        key_hash: u64,
    ) -> InsertStats {
        let first_word = self.bucket(first);
        let second_word = self.bucket(second);
        let first_occupancy = first_word.branchless_occupancy();
        let second_occupancy = second_word.branchless_occupancy();
        let mut stats = InsertStats {
            bucket_reads: 2,
            ..InsertStats::default()
        };

        if first_occupancy < SLOTS || second_occupancy < SLOTS {
            let destination =
                direct_destination(first, second, first_occupancy, second_occupancy, key_hash);
            let other = if destination == first { second } else { first };
            let mut word = if destination == first {
                first_word
            } else {
                second_word
            };
            let other_word = if other == first {
                first_word
            } else {
                second_word
            };
            let incoming_slot = word.branchless_occupancy();
            word.replace(incoming_slot, fingerprint);
            if word.is_full() {
                let residents = word.slots();
                let predecessor_rank = if other_word.is_full() {
                    self.decode_rank(other_word)
                } else {
                    0
                };
                let rank = self.initialize_ranked_full_bucket(
                    destination,
                    residents,
                    incoming_slot,
                    predecessor_rank,
                    &mut stats,
                );
                word = self.encode_rank(residents, rank);
            }
            self.buckets[destination as usize] = word;
            stats.bucket_writes = 1;
            stats.inserted = true;
            self.size += 1;
            return stats;
        }

        let first_rank = self.decode_rank(first_word);
        let second_rank = self.decode_rank(second_word);
        let (mut bucket, mut current_word, mut predecessor_rank) = if first_rank < second_rank
            || (first_rank == second_rank && (key_hash >> 33) & 1 == 1)
        {
            (first, first_word, second_rank)
        } else {
            (second, second_word, first_rank)
        };

        let mut carry = fingerprint;
        let mut path_active = false;
        for step in 0..self.max_kicks {
            let mut residents = current_word.slots();
            let mut neighbors = [0_u32; SLOTS];
            let mut neighbor_words = [Bucket::default(); SLOTS];
            let mut scores = [0_u8; SLOTS];

            for slot in 0..SLOTS {
                let fingerprint = residents[slot];
                let neighbor = alt_index(self.mask, bucket, fingerprint);
                let neighbor_word = self.bucket(neighbor);
                stats.bucket_reads += 1;
                let score = if neighbor_word.is_full() {
                    self.decode_rank(neighbor_word)
                } else {
                    0
                };
                neighbors[slot] = neighbor;
                neighbor_words[slot] = neighbor_word;
                scores[slot] = score;
            }
            let (mut best_slot, tie_keys) =
                ranked_victim_slot(bucket, step, carry, residents, neighbors, scores);
            let best_score = scores[best_slot];

            if self.policy == Policy::CavityRank4Path {
                let current_rank = self.decode_rank(current_word);
                let sketch = self.path.as_mut().expect("path sketch must exist");
                if !path_active && sketch.activation.triggers(step, current_rank, best_score) {
                    sketch.activate();
                    path_active = true;
                    stats.path_activated = true;
                    stats.path_activation_step = step + 1;
                }
                if path_active {
                    sketch.record(bucket);
                    let (preferred, checks, rejected) =
                        sketch.prefer_unseen(best_score, &scores, &neighbors, &tie_keys, best_slot);
                    best_slot = preferred;
                    stats.path_guarded_steps += 1;
                    stats.path_checks += checks;
                    stats.path_seen_candidates_rejected += rejected;
                }
            }

            let victim = residents[best_slot];
            residents[best_slot] = carry;
            // Recompute after the swap: exclude the victim edge and include the
            // incoming edge's predecessor rank.
            let requested_rank = if self.policy == Policy::CavityScan {
                2
            } else {
                post_eviction_rank(self.rank_cap(), predecessor_rank, scores, best_slot)
            };
            let updated_word = self.encode_rank(residents, requested_rank);
            let realized_rank = self.decode_rank(updated_word);
            self.buckets[bucket as usize] = updated_word;
            stats.bucket_writes += 1;
            stats.relocations += 1;

            carry = victim;
            bucket = neighbors[best_slot];
            current_word = neighbor_words[best_slot];
            predecessor_rank = realized_rank;
            if !current_word.is_full() {
                let incoming_slot = current_word.branchless_occupancy();
                current_word.replace(incoming_slot, carry);
                if current_word.is_full() {
                    let residents = current_word.slots();
                    let rank = self.initialize_ranked_full_bucket(
                        bucket,
                        residents,
                        incoming_slot,
                        predecessor_rank,
                        &mut stats,
                    );
                    current_word = self.encode_rank(residents, rank);
                }
                self.buckets[bucket as usize] = current_word;
                stats.bucket_writes += 1;
                stats.inserted = true;
                self.size += 1;
                if path_active {
                    self.path
                        .as_mut()
                        .expect("path sketch must exist")
                        .finish_insertion();
                }
                return stats;
            }
        }
        if path_active {
            self.path
                .as_mut()
                .expect("path sketch must exist")
                .finish_insertion();
        }
        stats.filter_usable = false;
        stats
    }

    fn insert_bfs(&mut self, incoming: u16, first: u32, second: u32, key_hash: u64) -> InsertStats {
        let mut stats = InsertStats::default();
        let mask = self.mask;
        let buckets = &mut self.buckets;
        let scratch = self.bfs.as_mut().expect("BFS scratch must exist");

        scratch.generation = scratch.generation.wrapping_add(1);
        if scratch.generation == 0 {
            scratch.seen.fill(0);
            scratch.generation = 1;
        }
        let generation = scratch.generation;

        scratch.queue.clear();
        scratch.edges.clear();
        let mut starts = [first, second];
        if (key_hash >> 33) & 1 == 1 {
            starts.swap(0, 1);
        }
        for start in starts {
            if scratch.seen[start as usize] != generation {
                scratch.seen[start as usize] = generation;
                scratch.parent_bucket[start as usize] = NO_PARENT;
                scratch.depth[start as usize] = 0;
                scratch.queue.push(start);
            }
        }

        let mut terminal = None;
        let mut head = 0;
        while head < scratch.queue.len() {
            let bucket = scratch.queue[head];
            head += 1;
            stats.bucket_reads += 1;
            let word = buckets[bucket as usize];
            let depth = scratch.depth[bucket as usize];
            if depth >= scratch.max_depth {
                continue;
            }

            let start_slot =
                (splitmix64(key_hash ^ (u64::from(bucket) << 1) ^ u64::from(depth)) & 3) as usize;
            for offset in 0..SLOTS {
                let slot = (start_slot + offset) & 3;
                let fingerprint = word.slot(slot);
                debug_assert_ne!(fingerprint, 0);
                let neighbor = alt_index(mask, bucket, fingerprint);
                stats.bucket_reads += 1;
                if !buckets[neighbor as usize].is_full() {
                    terminal = Some((neighbor, bucket, slot as u8));
                    break;
                }
                if scratch.seen[neighbor as usize] != generation {
                    scratch.seen[neighbor as usize] = generation;
                    scratch.parent_bucket[neighbor as usize] = bucket;
                    scratch.parent_slot[neighbor as usize] = slot as u8;
                    scratch.depth[neighbor as usize] = depth + 1;
                    scratch.queue.push(neighbor);
                }
            }
            if terminal.is_some() {
                break;
            }
        }

        let Some((terminal_bucket, terminal_parent, terminal_slot)) = terminal else {
            return stats;
        };
        scratch.edges.push((terminal_parent, terminal_slot));
        let mut current = terminal_parent;
        while scratch.parent_bucket[current as usize] != NO_PARENT {
            let parent = scratch.parent_bucket[current as usize];
            scratch
                .edges
                .push((parent, scratch.parent_slot[current as usize]));
            current = parent;
        }
        scratch.edges.reverse();

        let mut carry = incoming;
        for &(from, slot) in &scratch.edges {
            stats.bucket_reads += 1;
            let mut word = buckets[from as usize];
            let victim = word.slot(slot as usize);
            word.replace(slot as usize, carry);
            buckets[from as usize] = word;
            stats.bucket_writes += 1;
            stats.relocations += 1;
            carry = victim;
        }
        stats.bucket_reads += 1;
        assert!(buckets[terminal_bucket as usize].append(carry));
        stats.bucket_writes += 1;
        stats.inserted = true;
        stats
    }
}

impl PathActivation {
    #[inline]
    fn triggers(self, step: u32, current_rank: u8, min_target_rank: u8) -> bool {
        match self {
            Self::After(threshold) => step >= threshold,
            Self::NoDescent => min_target_rank >= current_rank,
            Self::Rank4Plateau => current_rank == 4 && min_target_rank == 4,
        }
    }
}

#[derive(Debug)]
struct PathSketch {
    words: Vec<u64>,
    activation: PathActivation,
    reset: PathReset,
    touched: Vec<u16>,
    generations: Vec<u32>,
    generation: u32,
}

impl PathSketch {
    fn new(config: PathConfig) -> Self {
        let word_count = config.bytes / size_of::<u64>();
        Self {
            words: vec![0; word_count],
            activation: config.activation,
            reset: config.reset,
            touched: if config.reset == PathReset::Sparse {
                Vec::with_capacity(word_count)
            } else {
                Vec::new()
            },
            generations: if config.reset == PathReset::Generational {
                vec![0; word_count]
            } else {
                Vec::new()
            },
            generation: 0,
        }
    }

    fn storage_bytes(&self) -> usize {
        self.words.capacity() * size_of::<u64>()
            + self.touched.capacity() * size_of::<u16>()
            + self.generations.capacity() * size_of::<u32>()
    }

    fn activate(&mut self) {
        match self.reset {
            PathReset::Full => self.words.fill(0),
            PathReset::Sparse => debug_assert!(self.touched.is_empty()),
            PathReset::Generational => {
                self.generation = self.generation.wrapping_add(1);
                if self.generation == 0 {
                    self.generations.fill(0);
                    self.generation = 1;
                }
            }
        }
    }

    fn record(&mut self, bucket: u32) {
        let [first, second] = self.bit_positions(bucket);
        self.set(first);
        self.set(second);
    }

    fn contains(&self, bucket: u32) -> bool {
        self.bit_positions(bucket)
            .into_iter()
            .all(|bit| self.is_set(bit))
    }

    fn prefer_unseen(
        &self,
        best_score: u8,
        scores: &[u8; SLOTS],
        neighbors: &[u32; SLOTS],
        tie_keys: &[(u64, u32, u16); SLOTS],
        fallback: usize,
    ) -> (usize, u32, u32) {
        let mut checks = 0;
        let mut seen = 0;
        let mut preferred = None;
        for slot in 0..SLOTS {
            if scores[slot] != best_score {
                continue;
            }
            checks += 1;
            if self.contains(neighbors[slot]) {
                seen += 1;
            } else if preferred.is_none_or(|best| tie_keys[slot] < tie_keys[best]) {
                preferred = Some(slot);
            }
        }
        preferred.map_or((fallback, checks, 0), |slot| (slot, checks, seen))
    }

    fn finish_insertion(&mut self) {
        if self.reset == PathReset::Sparse {
            for word in self.touched.drain(..) {
                self.words[usize::from(word)] = 0;
            }
        }
    }

    fn bit_positions(&self, bucket: u32) -> [usize; 2] {
        let hash = splitmix64(u64::from(bucket) ^ PATH_HASH_SALT);
        let mask = self.words.len() * 64 - 1;
        let first = hash as usize & mask;
        let mut second = (hash >> 32) as usize & mask;
        if first == second {
            second = (second + 1) & mask;
        }
        [first, second]
    }

    fn set(&mut self, bit: usize) {
        let word = bit >> 6;
        if self.reset == PathReset::Generational && self.generations[word] != self.generation {
            self.generations[word] = self.generation;
            self.words[word] = 0;
        } else if self.reset == PathReset::Sparse && self.words[word] == 0 {
            self.touched.push(word as u16);
        }
        self.words[word] |= 1_u64 << (bit & 63);
    }

    fn is_set(&self, bit: usize) -> bool {
        let word = bit >> 6;
        (self.reset != PathReset::Generational || self.generations[word] == self.generation)
            && self.words[word] & (1_u64 << (bit & 63)) != 0
    }
}

#[derive(Debug)]
struct BfsScratch {
    seen: Vec<u32>,
    parent_bucket: Vec<u32>,
    parent_slot: Vec<u8>,
    depth: Vec<u8>,
    queue: Vec<u32>,
    edges: Vec<(u32, u8)>,
    generation: u32,
    max_depth: u8,
}

impl BfsScratch {
    fn new(bucket_count: usize, max_depth: u8) -> Self {
        Self {
            seen: vec![0; bucket_count],
            parent_bucket: vec![NO_PARENT; bucket_count],
            parent_slot: vec![0; bucket_count],
            depth: vec![0; bucket_count],
            queue: Vec::with_capacity(512),
            edges: Vec::with_capacity(usize::from(max_depth) + 1),
            generation: 0,
            max_depth,
        }
    }

    fn storage_bytes(&self) -> usize {
        self.seen.capacity() * size_of::<u32>()
            + self.parent_bucket.capacity() * size_of::<u32>()
            + self.parent_slot.capacity() * size_of::<u8>()
            + self.depth.capacity() * size_of::<u8>()
            + self.queue.capacity() * size_of::<u32>()
            + self.edges.capacity() * size_of::<(u32, u8)>()
    }
}

#[derive(Debug)]
struct FastRng(u64);

impl FastRng {
    #[inline]
    fn next(&mut self) -> u64 {
        self.0 = splitmix64(self.0);
        self.0
    }
}

#[inline]
pub(crate) fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(GOLDEN_RATIO);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[inline]
fn fingerprint_delta(mask: u32, fingerprint: u16) -> u32 {
    // With at least 16 low mask bits, the odd multiply and xorshift map every
    // nonzero u16 fingerprint to a unique nonzero bucket delta.
    let mut delta = u32::from(fingerprint).wrapping_mul(0x9e37_79b1) & mask;
    delta ^= delta >> 8;
    if delta == 0 { 1 } else { delta }
}

#[inline]
fn alt_index(mask: u32, bucket: u32, fingerprint: u16) -> u32 {
    bucket ^ fingerprint_delta(mask, fingerprint)
}

#[inline]
fn ranked_edge_key(
    current_bucket: u32,
    step: u32,
    carry: u16,
    neighbor: u32,
    fingerprint: u16,
) -> (u64, u32, u16) {
    let edge = (u64::from(neighbor) << 16) | u64::from(fingerprint);
    let context = ((u64::from(current_bucket) << 32) | u64::from(step)) ^ (u64::from(carry) << 48);
    (
        splitmix64(edge ^ splitmix64(context)),
        neighbor,
        fingerprint,
    )
}

fn ranked_victim_slot(
    current_bucket: u32,
    step: u32,
    carry: u16,
    residents: [u16; SLOTS],
    neighbors: [u32; SLOTS],
    scores: [u8; SLOTS],
) -> (usize, [(u64, u32, u16); SLOTS]) {
    let tie_keys = std::array::from_fn(|slot| {
        ranked_edge_key(
            current_bucket,
            step,
            carry,
            neighbors[slot],
            residents[slot],
        )
    });
    let best_slot = (0..SLOTS)
        .min_by_key(|&slot| (scores[slot], tie_keys[slot]))
        .unwrap();
    (best_slot, tie_keys)
}

#[inline]
fn key_parts(mask: u32, key: u64) -> (u16, u32, u32, u64) {
    let hash = splitmix64(key ^ KEY_HASH_SALT);
    let mut fingerprint = (hash >> 48) as u16;
    if fingerprint == 0 {
        fingerprint = 1;
    }
    let first = hash as u32 & mask;
    let second = alt_index(mask, first, fingerprint);
    (fingerprint, first, second, hash)
}

#[inline]
fn random_destination(
    first: u32,
    second: u32,
    first_occupancy: usize,
    second_occupancy: usize,
    key_hash: u64,
) -> u32 {
    if first_occupancy < SLOTS && second_occupancy < SLOTS {
        if (key_hash >> 32) & 1 == 1 {
            first
        } else {
            second
        }
    } else if first_occupancy < SLOTS {
        first
    } else {
        second
    }
}

#[inline]
fn better_choice_destination(
    first: u32,
    second: u32,
    first_occupancy: usize,
    second_occupancy: usize,
) -> u32 {
    if first_occupancy <= second_occupancy {
        first
    } else {
        second
    }
}

#[inline]
fn lsa_root(first: u32, second: u32, first_label: u64, second_label: u64) -> u32 {
    if first_label <= second_label {
        first
    } else {
        second
    }
}

#[inline]
fn direct_destination(
    first: u32,
    second: u32,
    first_occupancy: usize,
    second_occupancy: usize,
    key_hash: u64,
) -> u32 {
    if first_occupancy < SLOTS && second_occupancy < SLOTS {
        if first_occupancy < second_occupancy {
            first
        } else if second_occupancy < first_occupancy {
            second
        } else if (key_hash >> 32) & 1 == 1 {
            first
        } else {
            second
        }
    } else if first_occupancy < SLOTS {
        first
    } else {
        second
    }
}

#[inline]
fn lsa_post_eviction_label(
    predecessor_score: u64,
    scores: [u64; SLOTS],
    victim_slot: usize,
) -> u64 {
    debug_assert!(victim_slot < SLOTS);
    let mut best = predecessor_score;
    for (slot, score) in scores.into_iter().enumerate() {
        if slot != victim_slot {
            best = best.min(score);
        }
    }
    best.checked_add(1).expect("LSA label overflow")
}

#[inline]
fn post_eviction_rank(
    cap: u8,
    predecessor_rank: u8,
    scores: [u8; SLOTS],
    victim_slot: usize,
) -> u8 {
    debug_assert!(victim_slot < SLOTS);
    let mut best = predecessor_rank;
    for (slot, score) in scores.into_iter().enumerate() {
        if slot != victim_slot {
            best = best.min(score);
        }
    }
    cap.min(1 + best)
}

#[cfg(test)]
mod tests {
    use super::{
        Config, ConfigError, CuckooFilter, DENSE_LOAD_DENOMINATOR, DENSE_LOAD_NUMERATOR,
        PathActivation, PathConfig, PathReset, PathSketch, Policy, alt_index,
        better_choice_destination, fingerprint_delta, lsa_post_eviction_label, lsa_root,
        post_eviction_rank, random_destination, ranked_victim_slot, splitmix64,
    };
    use crate::bucket::{Bucket, SLOTS};

    fn config(policy: Policy) -> Config {
        Config {
            bucket_count: 1 << 11,
            policy,
            seed: 17,
            max_kicks: 5_000,
            bfs_depth: 10,
            path: PathConfig::default(),
        }
    }

    #[test]
    fn rejects_invalid_configuration() {
        let mut invalid = config(Policy::CavityBit);
        invalid.bucket_count = 3;
        assert!(matches!(
            CuckooFilter::new(invalid),
            Err(ConfigError::BucketCount)
        ));
        invalid = config(Policy::CavityBit);
        invalid.max_kicks = 0;
        assert!(matches!(
            CuckooFilter::new(invalid),
            Err(ConfigError::MaxKicks)
        ));
        invalid = config(Policy::CavityRank4Path);
        invalid.path.bytes = 1_024;
        assert!(matches!(
            CuckooFilter::new(invalid),
            Err(ConfigError::PathBytes)
        ));
    }

    #[test]
    fn alternate_bucket_is_an_involution() {
        let mask = (1 << 17) - 1;
        for fingerprint in [1, 2, 17, 257, u16::MAX] {
            for bucket in [0, 1, 12_345, mask] {
                let other = alt_index(mask, bucket, fingerprint);
                assert_ne!(other, bucket);
                assert_eq!(alt_index(mask, other, fingerprint), bucket);
            }
        }
    }

    #[test]
    fn post_eviction_rank_matches_slow_reference() {
        fn slow_reference(
            cap: u8,
            predecessor_rank: u8,
            scores: [u8; SLOTS],
            victim_slot: usize,
        ) -> u8 {
            let mut post_eviction_scores = vec![predecessor_rank];
            post_eviction_scores.extend(
                scores
                    .into_iter()
                    .enumerate()
                    .filter_map(|(slot, score)| (slot != victim_slot).then_some(score)),
            );
            post_eviction_scores.sort_unstable();
            cap.min(post_eviction_scores[0] + 1)
        }

        for cap in [2, 4, 8] {
            let radix = usize::from(cap) + 1;
            for predecessor_rank in 0..=cap {
                for victim_slot in 0..SLOTS {
                    for code in 0..radix.pow(SLOTS as u32) {
                        let mut remaining = code;
                        let scores = std::array::from_fn(|_| {
                            let score = (remaining % radix) as u8;
                            remaining /= radix;
                            score
                        });
                        assert_eq!(
                            post_eviction_rank(cap, predecessor_rank, scores, victim_slot),
                            slow_reference(cap, predecessor_rank, scores, victim_slot),
                            "cap={cap}, predecessor={predecessor_rank}, victim={victim_slot}, scores={scores:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ranked_victim_is_invariant_to_all_lane_permutations() {
        fn permutations(values: [u16; SLOTS]) -> Vec<[u16; SLOTS]> {
            let mut output = Vec::new();
            for a in 0..SLOTS {
                for b in 0..SLOTS {
                    for c in 0..SLOTS {
                        for d in 0..SLOTS {
                            if [a, b, c, d].into_iter().all(|index| {
                                [a, b, c, d].into_iter().filter(|&x| x == index).count() == 1
                            }) {
                                output.push([values[a], values[b], values[c], values[d]]);
                            }
                        }
                    }
                }
            }
            output
        }

        let mask = 255;
        let current = 0;
        let carry = 99;
        let scores_by_fingerprint = |fingerprint| match fingerprint {
            1..=3 => 2,
            4 => 3,
            _ => unreachable!(),
        };
        let mut expected = None;
        let distinct_permutations = permutations([1, 2, 3, 4]);
        assert_eq!(distinct_permutations.len(), 24);
        for residents in distinct_permutations {
            let neighbors = residents.map(|fingerprint| alt_index(mask, current, fingerprint));
            let scores = residents.map(scores_by_fingerprint);
            let (slot, _) = ranked_victim_slot(current, 7, carry, residents, neighbors, scores);
            let semantic_edge = (neighbors[slot], residents[slot]);
            assert_eq!(*expected.get_or_insert(semantic_edge), semantic_edge);
        }

        let mut duplicate_expected = None;
        for residents in permutations([1, 1, 2, 3]) {
            let neighbors = residents.map(|fingerprint| alt_index(mask, current, fingerprint));
            let (slot, _) = ranked_victim_slot(current, 7, carry, residents, neighbors, [2; SLOTS]);
            let semantic_edge = (neighbors[slot], residents[slot]);
            assert_eq!(
                *duplicate_expected.get_or_insert(semantic_edge),
                semantic_edge
            );
        }
    }

    #[test]
    fn rank4_alias_propagates_the_realized_rank_to_the_next_backup() {
        let mask = 255;
        let current = 0;
        let incoming = 13;
        let repeated = 11;
        let distinct = 17;
        let repeated_target = alt_index(mask, current, repeated);
        let distinct_target = alt_index(mask, current, distinct);
        let scores = [4, 4, 4, 0];
        let neighbors = [
            repeated_target,
            repeated_target,
            repeated_target,
            distinct_target,
        ];
        let residents = [repeated, repeated, repeated, distinct];
        let (victim_slot, _) =
            ranked_victim_slot(current, 0, incoming, residents, neighbors, scores);
        assert_eq!(victim_slot, 3);
        let requested = post_eviction_rank(4, 4, scores, victim_slot);
        assert_eq!(requested, 4);
        let mut updated = residents;
        updated[victim_slot] = incoming;
        let encoded = Bucket::encode_rank4(updated, requested);
        let realized = encoded.decode_rank4();
        assert_eq!(realized, 1);

        assert_eq!(post_eviction_rank(4, realized, [4; SLOTS], 0), 2);
        assert_eq!(post_eviction_rank(4, requested, [4; SLOTS], 0), 4);
    }

    #[test]
    fn lsa_post_eviction_label_matches_slow_reference() {
        fn slow_reference(predecessor_score: u64, scores: [u64; SLOTS], victim_slot: usize) -> u64 {
            let mut post_eviction_scores = vec![predecessor_score];
            post_eviction_scores.extend(
                scores
                    .into_iter()
                    .enumerate()
                    .filter_map(|(slot, score)| (slot != victim_slot).then_some(score)),
            );
            post_eviction_scores.sort_unstable();
            post_eviction_scores[0] + 1
        }

        let radix = 5_usize;
        for predecessor_score in 0..radix as u64 {
            for victim_slot in 0..SLOTS {
                for code in 0..radix.pow(SLOTS as u32) {
                    let mut remaining = code;
                    let scores = std::array::from_fn(|_| {
                        let score = (remaining % radix) as u64;
                        remaining /= radix;
                        score
                    });
                    assert_eq!(
                        lsa_post_eviction_label(predecessor_score, scores, victim_slot),
                        slow_reference(predecessor_score, scores, victim_slot)
                    );
                }
            }
        }
        assert_eq!(lsa_post_eviction_label(300, [400; SLOTS], 0), 301);
    }

    #[test]
    fn initial_choice_baselines_follow_their_pinned_rules() {
        let first = 3;
        let second = 5;
        assert_eq!(random_destination(first, second, 3, 0, 1 << 32), first);
        assert_eq!(random_destination(first, second, 0, 3, 0), second);
        assert_eq!(better_choice_destination(first, second, 3, 0), second);
        assert_eq!(better_choice_destination(first, second, 2, 2), first);

        for (policy, changed) in [(Policy::Random, 1), (Policy::BetterChoice, 0)] {
            let mut filter = CuckooFilter::new(Config {
                bucket_count: 2,
                policy,
                max_kicks: 1,
                ..config(policy)
            })
            .unwrap();
            filter.buckets = vec![
                Bucket::from_slots([1; SLOTS]),
                Bucket::from_slots([2; SLOTS]),
            ];
            filter.size = filter.capacity();
            let before = filter.buckets.clone();
            let _ = filter.insert_fingerprint(3, 0, 1, 0);
            assert_ne!(filter.buckets[changed], before[changed]);
            assert_eq!(filter.buckets[1 - changed], before[1 - changed]);
        }
    }

    #[test]
    fn lsa_initializes_newly_full_buckets_from_actual_alternates() {
        let mut filter = CuckooFilter::new(Config {
            bucket_count: 8,
            policy: Policy::Lsa,
            ..config(Policy::Lsa)
        })
        .unwrap();
        filter.buckets[0] = Bucket::from_slots([2, 2, 2, 0]);
        filter.buckets[1] = Bucket::from_slots([1; SLOTS]);
        filter.buckets[2] = Bucket::from_slots([2; SLOTS]);
        filter.lsa_labels[1] = 9;
        filter.lsa_labels[2] = 5;
        filter.size = 3 + 2 * SLOTS;

        let result = filter.insert_fingerprint(1, 0, 1, 0);

        assert!(result.inserted);
        assert_eq!(filter.lsa_labels[0], 6);
    }

    #[test]
    fn lsa_full_root_uses_minimum_label_and_ties_first() {
        assert_eq!(lsa_root(3, 5, 7, 9), 3);
        assert_eq!(lsa_root(3, 5, 9, 7), 5);
        assert_eq!(lsa_root(3, 5, 7, 7), 3);

        for (labels, changed) in [([9, 7], 1), ([7, 7], 0)] {
            let mut filter = CuckooFilter::new(Config {
                bucket_count: 2,
                policy: Policy::Lsa,
                max_kicks: 1,
                ..config(Policy::Lsa)
            })
            .unwrap();
            filter.buckets = vec![
                Bucket::from_slots([1; SLOTS]),
                Bucket::from_slots([2; SLOTS]),
            ];
            filter.lsa_labels.copy_from_slice(&labels);
            filter.size = filter.capacity();
            let before = filter.buckets.clone();

            let _ = filter.insert_fingerprint(3, 0, 1, 0);

            assert_ne!(filter.buckets[changed], before[changed]);
            assert_eq!(filter.buckets[1 - changed], before[1 - changed]);
        }
    }

    #[test]
    fn explicit_label_memory_is_accounted_in_bytes() {
        let mut lsa_config = config(Policy::Lsa);
        lsa_config.bucket_count = 64;
        assert_eq!(
            CuckooFilter::new(lsa_config)
                .unwrap()
                .persistent_metadata_bytes(),
            64 * size_of::<u64>()
        );

        let mut historical_config = config(Policy::EvictionLabel);
        historical_config.bucket_count = 64;
        assert_eq!(
            CuckooFilter::new(historical_config)
                .unwrap()
                .persistent_metadata_bytes(),
            64
        );
    }

    #[test]
    fn path_activation_rules_match_their_boundaries() {
        let after = PathActivation::After(64);
        assert!(!after.triggers(63, 4, 4));
        assert!(after.triggers(64, 1, 0));

        assert!(!PathActivation::NoDescent.triggers(0, 3, 2));
        assert!(PathActivation::NoDescent.triggers(0, 3, 3));
        assert!(PathActivation::NoDescent.triggers(0, 3, 4));

        assert!(PathActivation::Rank4Plateau.triggers(0, 4, 4));
        assert!(!PathActivation::Rank4Plateau.triggers(0, 3, 4));
        assert!(!PathActivation::Rank4Plateau.triggers(0, 4, 3));
    }

    #[test]
    fn path_configuration_strings_round_trip() {
        for activation in [
            PathActivation::After(0),
            PathActivation::After(64),
            PathActivation::NoDescent,
            PathActivation::Rank4Plateau,
        ] {
            assert_eq!(activation.to_string().parse(), Ok(activation));
        }
        for reset in [PathReset::Full, PathReset::Sparse, PathReset::Generational] {
            assert_eq!(reset.to_string().parse(), Ok(reset));
        }
        assert!("after:nope".parse::<PathActivation>().is_err());
        assert!("plateau".parse::<PathActivation>().is_err());
        assert!("lazy".parse::<PathReset>().is_err());
    }

    #[test]
    fn path_novelty_never_overrides_rank_or_balanced_order() {
        let mut sketch = PathSketch::new(PathConfig {
            bytes: 512,
            activation: PathActivation::After(0),
            reset: PathReset::Full,
        });
        sketch.activate();
        let seen = 7;
        sketch.record(seen);
        let unseen = (8..).find(|&bucket| !sketch.contains(bucket)).unwrap();
        let neighbors = [seen, unseen, 101, 102];
        let tie_keys = [(1, seen, 1), (2, unseen, 2), (3, 101, 3), (4, 102, 4)];

        assert_eq!(
            sketch.prefer_unseen(1, &[1, 2, 2, 2], &neighbors, &tie_keys, 0),
            (0, 1, 0)
        );
        assert_eq!(
            sketch.prefer_unseen(2, &[2, 2, 3, 3], &neighbors, &tie_keys, 0),
            (1, 2, 1)
        );

        sketch.record(unseen);
        assert_eq!(
            sketch.prefer_unseen(2, &[2, 2, 3, 3], &neighbors, &tie_keys, 1),
            (1, 2, 0)
        );
    }

    #[test]
    fn path_stats_use_one_based_activation_and_count_actual_probes() {
        let path_config = PathConfig {
            bytes: 512,
            activation: PathActivation::After(0),
            reset: PathReset::Full,
        };
        let mut probe = PathSketch::new(path_config);
        probe.activate();
        probe.record(0);
        let victim = (1..=u16::MAX)
            .find(|&fingerprint| {
                let neighbor = alt_index(7, 0, fingerprint);
                neighbor != 0 && neighbor != 1 && !probe.contains(neighbor)
            })
            .unwrap();

        let mut filter = CuckooFilter::new(Config {
            bucket_count: 8,
            policy: Policy::CavityRank4Path,
            max_kicks: 1,
            path: path_config,
            ..config(Policy::CavityRank4Path)
        })
        .unwrap();
        filter.buckets[0] = Bucket::from_slots([victim; SLOTS]);
        filter.buckets[1] = Bucket::from_slots([1; SLOTS]);
        filter.size = 2 * SLOTS;

        let stats = filter.insert_fingerprint(3, 0, 1, 1 << 33);

        assert!(stats.inserted);
        assert!(stats.path_activated);
        assert_eq!(stats.path_activation_step, 1);
        assert_eq!(stats.path_guarded_steps, 1);
        assert_eq!(stats.path_checks, 4);
        assert_eq!(stats.path_seen_candidates_rejected, 0);
    }

    #[test]
    fn path_reset_modes_are_logically_equivalent() {
        for reset in [PathReset::Full, PathReset::Sparse, PathReset::Generational] {
            let mut sketch = PathSketch::new(PathConfig {
                bytes: 512,
                activation: PathActivation::After(0),
                reset,
            });
            sketch.activate();
            sketch.record(17);
            assert!(sketch.contains(17));
            sketch.finish_insertion();
            assert_eq!(sketch.contains(17), reset != PathReset::Sparse);
            sketch.activate();
            assert!(!sketch.contains(17));
            sketch.record(19);
            assert!(sketch.contains(19));
        }

        let mut wrapped = PathSketch::new(PathConfig {
            bytes: 512,
            activation: PathActivation::After(0),
            reset: PathReset::Generational,
        });
        wrapped.activate();
        wrapped.record(23);
        wrapped.generation = u32::MAX;
        wrapped.activate();
        assert!(!wrapped.contains(23));
    }

    #[test]
    fn path_storage_matches_configured_sketch_and_reset() {
        assert_eq!(
            PathConfig::default(),
            PathConfig {
                bytes: 2_048,
                activation: PathActivation::After(128),
                reset: PathReset::Full,
            }
        );
        for bytes in [512, 2_048] {
            for (reset, reset_bytes_per_word) in [
                (PathReset::Full, 0),
                (PathReset::Sparse, size_of::<u16>()),
                (PathReset::Generational, size_of::<u32>()),
            ] {
                let mut path_config = config(Policy::CavityRank4Path);
                path_config.path = PathConfig {
                    bytes,
                    activation: PathActivation::After(64),
                    reset,
                };
                let path = CuckooFilter::new(path_config).unwrap();
                assert_eq!(
                    path.transient_insertion_workspace_bytes(),
                    bytes + bytes / size_of::<u64>() * reset_bytes_per_word
                );
            }
        }

        assert_eq!(
            CuckooFilter::new(config(Policy::CavityRank4))
                .unwrap()
                .transient_insertion_workspace_bytes(),
            0
        );
    }

    #[test]
    fn memory_accounting_uses_allocated_vector_capacity() {
        let mut lsa = CuckooFilter::new(Config {
            bucket_count: 64,
            policy: Policy::Lsa,
            ..config(Policy::Lsa)
        })
        .unwrap();
        lsa.buckets.reserve(65);
        lsa.lsa_labels.reserve(65);
        assert_eq!(
            lsa.payload_bytes(),
            lsa.buckets.capacity() * size_of::<Bucket>()
        );
        assert_eq!(
            lsa.persistent_metadata_bytes(),
            lsa.lsa_labels.capacity() * size_of::<u64>()
        );
        assert_eq!(lsa.transient_insertion_workspace_bytes(), 0);
    }

    #[test]
    fn path_policy_preserves_the_query_contract() {
        let mut path_config = config(Policy::CavityRank4Path);
        path_config.bucket_count = 256;
        path_config.path = PathConfig {
            bytes: 512,
            activation: PathActivation::After(0),
            reset: PathReset::Full,
        };
        let mut path = CuckooFilter::new(path_config).unwrap();
        assert_eq!(
            path.payload_bytes(),
            path_config.bucket_count * size_of::<u64>()
        );
        let target = path.capacity() * 9 / 10;
        let mut inserted = Vec::with_capacity(target);
        let mut key = splitmix64(31);
        while path.len() < target {
            key = splitmix64(key);
            assert!(path.insert(key).inserted);
            inserted.push(key);
            assert!(inserted.iter().all(|&resident| path.contains(resident)));
        }

        let sketch = path.path.as_ref().unwrap();
        let before = (
            sketch.words.clone(),
            sketch.touched.clone(),
            sketch.generations.clone(),
            sketch.generation,
        );
        assert!(before.0.iter().any(|&word| word != 0));
        for _ in 0..10_000 {
            key = splitmix64(key);
            let _ = path.contains(key);
        }
        let sketch = path.path.as_ref().unwrap();
        assert_eq!(
            before,
            (
                sketch.words.clone(),
                sketch.touched.clone(),
                sketch.generations.clone(),
                sketch.generation,
            )
        );
    }

    #[test]
    fn rank4_codec_counts_scan_only_full_ranked_buckets() {
        for policy in [
            Policy::CavityRank4,
            Policy::CavityRank4Path,
            Policy::DenseCavityRank4,
        ] {
            let mut filter = CuckooFilter::new(Config {
                bucket_count: 8,
                policy,
                ..config(policy)
            })
            .unwrap();
            filter.buckets[0] = Bucket::from_slots([7, 7, 7, 11]);
            filter.buckets[1] = Bucket::from_slots([1, 2, 3, 4]);
            filter.buckets[2] = Bucket::from_slots([5, 5, 5, 0]);
            assert_eq!(filter.rank4_codec_counts(), (2, 1));
        }

        let mut random = CuckooFilter::new(Config {
            bucket_count: 8,
            policy: Policy::Random,
            ..config(Policy::Random)
        })
        .unwrap();
        random.buckets[0] = Bucket::from_slots([7, 7, 7, 11]);
        assert_eq!(random.rank4_codec_counts(), (0, 0));
    }

    #[test]
    fn bfs_counts_initial_and_path_application_reads() {
        let mut filter = CuckooFilter::new(Config {
            bucket_count: 8,
            policy: Policy::Bfs,
            ..config(Policy::Bfs)
        })
        .unwrap();
        let incoming = 1;
        let victim = 2;
        filter.buckets[0] = Bucket::from_slots([victim; SLOTS]);
        filter.buckets[1] = Bucket::from_slots([incoming; SLOTS]);
        filter.size = 2 * SLOTS;

        let stats = filter.insert_fingerprint(incoming, 0, 1, 0);

        assert_eq!(
            stats,
            super::InsertStats {
                inserted: true,
                filter_usable: true,
                relocations: 1,
                bucket_reads: 6,
                bucket_writes: 2,
                ..Default::default()
            }
        );
        let scratch = filter.bfs.as_ref().unwrap();
        assert!(!scratch.queue.is_empty());
        assert!(!scratch.edges.is_empty());
        assert_eq!(
            filter.transient_insertion_workspace_bytes(),
            scratch.storage_bytes()
        );
    }

    #[test]
    fn bounded_failure_reports_whether_the_filter_is_still_usable() {
        let destructive = [
            Policy::Random,
            Policy::BetterChoice,
            Policy::Rotor,
            Policy::EvictionLabel,
            Policy::Lsa,
            Policy::CavityD4,
            Policy::CavityScan,
            Policy::CavityBit,
            Policy::CavityRank4,
            Policy::CavityRank4Path,
            Policy::DenseCavityRank4,
        ];
        for policy in destructive {
            let mut filter = CuckooFilter::new(Config {
                bucket_count: 2,
                policy,
                max_kicks: 1,
                ..config(policy)
            })
            .unwrap();
            filter.buckets = vec![
                Bucket::from_slots([1; SLOTS]),
                Bucket::from_slots([2; SLOTS]),
            ];
            filter.size = filter.capacity();
            filter.dense_mode = true;

            let result = filter.insert_fingerprint(3, 0, 1, 0);
            assert!(!result.inserted, "{policy} unexpectedly inserted");
            assert!(!result.filter_usable, "{policy} hid destructive failure");
        }

        let mut bfs = CuckooFilter::new(Config {
            bucket_count: 2,
            policy: Policy::Bfs,
            bfs_depth: 1,
            ..config(Policy::Bfs)
        })
        .unwrap();
        bfs.buckets = vec![
            Bucket::from_slots([1; SLOTS]),
            Bucket::from_slots([2; SLOTS]),
        ];
        bfs.size = bfs.capacity();
        let before = bfs.buckets.clone();

        let result = bfs.insert_fingerprint(3, 0, 1, 0);
        assert!(!result.inserted);
        assert!(result.filter_usable);
        assert_eq!(bfs.buckets, before);
    }

    #[test]
    fn fingerprint_deltas_are_unique_at_the_fingerprint_width() {
        let mask = u16::MAX as u32;
        let mut seen = vec![false; mask as usize + 1];
        for fingerprint in 1..=u16::MAX {
            let delta = fingerprint_delta(mask, fingerprint);
            assert_ne!(delta, 0);
            assert!(!seen[delta as usize]);
            seen[delta as usize] = true;
        }
    }

    #[test]
    fn all_policies_reach_high_load_without_false_negatives() {
        let policies = [
            Policy::Random,
            Policy::BetterChoice,
            Policy::Rotor,
            Policy::EvictionLabel,
            Policy::Lsa,
            Policy::CavityD4,
            Policy::CavityScan,
            Policy::CavityBit,
            Policy::CavityRank4,
            Policy::CavityRank4Path,
            Policy::DenseCavityRank4,
            Policy::Bfs,
        ];
        for policy in policies {
            let mut filter = CuckooFilter::new(config(policy)).unwrap();
            let target = (filter.capacity() as f64 * 0.95) as usize;
            let mut keys = Vec::with_capacity(target);
            let mut key = splitmix64(0x1234_5678_9abc_def0);
            while filter.len() < target {
                key = splitmix64(key);
                assert!(filter.insert(key).inserted, "{policy} failed early");
                keys.push(key);
            }
            for key in &keys {
                assert!(filter.contains(*key), "false negative for {policy}");
            }
            for key in keys.iter().step_by(23).take(64) {
                assert!(filter.remove(*key), "remove failed for {policy}");
            }
        }
    }

    #[test]
    fn dense_cr4_bootstrap_matches_truncated_residual_distances() {
        let mut dense = CuckooFilter::new(Config {
            bucket_count: 64,
            policy: Policy::DenseCavityRank4,
            ..config(Policy::DenseCavityRank4)
        })
        .unwrap();
        for bucket in 0..dense.buckets.len() {
            let first = (bucket * SLOTS + 1) as u16;
            let mut slots = [first, first + 1, first + 2, first + 3];
            if bucket == 0 {
                slots[3] = 0;
            }
            dense.buckets[bucket] = Bucket::from_slots(slots);
        }
        dense.size = dense.capacity() - 1;

        let before: Vec<_> = dense
            .buckets
            .iter()
            .map(|bucket| bucket.sorted_slots())
            .collect();
        let mut distances = vec![usize::MAX; dense.buckets.len()];
        distances[0] = 0;
        for _ in 0..dense.buckets.len() {
            let mut changed = false;
            for bucket in 1..dense.buckets.len() {
                let best = dense.buckets[bucket]
                    .slots()
                    .into_iter()
                    .map(|fingerprint| {
                        distances[alt_index(dense.mask, bucket as u32, fingerprint) as usize]
                    })
                    .min()
                    .unwrap();
                if best != usize::MAX && best + 1 < distances[bucket] {
                    distances[bucket] = best + 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let expected_writes = 63
            + distances
                .iter()
                .filter(|&&distance| distance == 2 || distance == 3)
                .count() as u64;
        let stats = dense.prepare_dense();
        assert!(stats.prepared);
        assert_eq!(stats.bucket_writes, expected_writes);
        assert_eq!(dense.persistent_metadata_bytes(), 0);
        assert_eq!(dense.transient_insertion_workspace_bytes(), 0);
        for bucket in 1..dense.buckets.len() {
            assert_eq!(
                dense.buckets[bucket].decode_rank4(),
                distances[bucket].min(4) as u8,
                "wrong rank for bucket {bucket}"
            );
            assert_eq!(dense.buckets[bucket].sorted_slots(), before[bucket]);
        }
        assert_eq!(dense.prepare_dense(), Default::default());
    }

    #[test]
    fn dense_cr4_waits_until_an_insert_would_cross_96_percent() {
        let mut dense = CuckooFilter::new(config(Policy::DenseCavityRank4)).unwrap();
        let mut rotor = CuckooFilter::new(config(Policy::Rotor)).unwrap();
        let mut key = 0;
        while dense.len() < dense.capacity() * 9 / 10 {
            key = splitmix64(key);
            assert_eq!(dense.insert(key), rotor.insert(key));
        }
        assert_eq!(dense.buckets, rotor.buckets);

        let trigger = dense.capacity() * DENSE_LOAD_NUMERATOR / DENSE_LOAD_DENOMINATOR;
        dense.size = trigger - 1;
        assert!(!dense.needs_dense_prepare());
        dense.size = trigger;
        assert!(dense.needs_dense_prepare());
    }
}

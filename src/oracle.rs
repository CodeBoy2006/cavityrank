//! Exact capacity-four orientation tooling for paired bucket endpoints.

use std::{collections::VecDeque, error::Error, fmt};

const CAPACITY: usize = 4;
const EMPTY: usize = usize::MAX;
const BUILD_KEY_SALT: u64 = 0x1234_5678_9abc_def0;
const KEY_HASH_SALT: u64 = 0xa076_1d64_78bd_642f;
const INDEPENDENT_SECOND_SALT: u64 = 0xe703_7ed1_a0b4_28db;
const GOLDEN_RATIO: u64 = 0x9e37_79b9_7f4a_7c15;

/// Endpoint-pair construction used by oracle experiments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphModel {
    /// Hashes the second endpoint independently and forces it to differ from
    /// the first.
    Independent,
    /// Derives the second endpoint by XORing the first with the fingerprint
    /// delta.
    Xor16,
}

/// One graph edge with two candidate buckets and its packed-filter fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Item {
    /// First candidate bucket.
    pub first: u32,
    /// Second candidate bucket.
    pub second: u32,
    /// Nonzero 16-bit fingerprint.
    pub fingerprint: u16,
}

/// Invalid oracle dimensions or endpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OracleError {
    /// The bucket count is invalid for the requested operation.
    BucketCount,
    /// An item endpoint lies outside the configured table.
    EndpointOutOfRange {
        /// Invalid endpoint.
        endpoint: u32,
        /// Configured number of buckets.
        bucket_count: usize,
    },
}

impl fmt::Display for OracleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BucketCount => formatter
                .write_str("bucket count must be nonzero and addressable by a 32-bit bucket index"),
            Self::EndpointOutOfRange {
                endpoint,
                bucket_count,
            } => write!(
                formatter,
                "bucket endpoint {endpoint} is outside a {bucket_count}-bucket table"
            ),
        }
    }
}

impl Error for OracleError {}

/// Generates deterministic nonzero fingerprints and distinct endpoint pairs.
///
/// `bucket_count` must be a power of two in `2..=2^32`.
pub fn generate_items(
    bucket_count: usize,
    count: usize,
    seed: u64,
    graph: GraphModel,
) -> Result<Vec<Item>, OracleError> {
    if bucket_count < 2 || !bucket_count.is_power_of_two() || bucket_count - 1 > u32::MAX as usize {
        return Err(OracleError::BucketCount);
    }

    let mask = (bucket_count - 1) as u32;
    let mut state = mix64(seed ^ BUILD_KEY_SALT);
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        state = mix64(state);
        let hash = mix64(state ^ KEY_HASH_SALT);
        let fingerprint = ((hash >> 48) as u16).max(1);
        let first = hash as u32 & mask;
        let second = match graph {
            GraphModel::Independent => {
                let candidate = mix64(state ^ INDEPENDENT_SECOND_SALT) as u32 & mask;
                if candidate == first {
                    first.wrapping_add(1) & mask
                } else {
                    candidate
                }
            }
            GraphModel::Xor16 => first ^ fingerprint_delta(mask, fingerprint),
        };
        items.push(Item {
            first,
            second,
            fingerprint,
        });
    }
    Ok(items)
}

/// Maintains an exact capacity-four assignment by augmenting the current
/// orientation for each inserted endpoint pair.
#[derive(Debug)]
pub struct ExactOracle {
    buckets: Vec<[usize; CAPACITY]>,
    items: Vec<Item>,
    seen: Vec<u32>,
    parent_bucket: Vec<usize>,
    parent_item: Vec<usize>,
    queue: VecDeque<usize>,
    generation: u32,
}

impl ExactOracle {
    /// Creates an empty oracle for `bucket_count` capacity-four buckets.
    pub fn new(bucket_count: usize) -> Result<Self, OracleError> {
        validate_bucket_count(bucket_count)?;
        Ok(Self {
            buckets: vec![[EMPTY; CAPACITY]; bucket_count],
            items: Vec::new(),
            seen: vec![0; bucket_count],
            parent_bucket: vec![EMPTY; bucket_count],
            parent_item: vec![EMPTY; bucket_count],
            queue: VecDeque::new(),
            generation: 0,
        })
    }

    /// Returns the number of feasible items currently assigned.
    #[inline]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns whether no items have been assigned.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns `false` without changing the orientation when the new prefix is
    /// infeasible.
    pub fn insert(&mut self, item: Item) -> Result<bool, OracleError> {
        validate_item(self.buckets.len(), item)?;
        let item_index = self.items.len();
        self.items.push(item);

        if append(&mut self.buckets[item.first as usize], item_index)
            || (item.second != item.first
                && append(&mut self.buckets[item.second as usize], item_index))
        {
            return Ok(true);
        }

        self.next_generation();
        let generation = self.generation;
        self.queue.clear();
        for root in [item.first as usize, item.second as usize] {
            if self.seen[root] != generation {
                self.seen[root] = generation;
                self.parent_bucket[root] = EMPTY;
                self.parent_item[root] = EMPTY;
                self.queue.push_back(root);
            }
        }

        let mut terminal = EMPTY;
        while let Some(bucket) = self.queue.pop_front() {
            for resident in self.buckets[bucket] {
                debug_assert_ne!(resident, EMPTY);
                let neighbor = other(self.items[resident], bucket);
                if self.seen[neighbor] == generation {
                    continue;
                }
                self.seen[neighbor] = generation;
                self.parent_bucket[neighbor] = bucket;
                self.parent_item[neighbor] = resident;
                if !is_full(&self.buckets[neighbor]) {
                    terminal = neighbor;
                    break;
                }
                self.queue.push_back(neighbor);
            }
            if terminal != EMPTY {
                break;
            }
        }

        if terminal == EMPTY {
            self.items.pop();
            return Ok(false);
        }

        let mut destination = terminal;
        while self.parent_bucket[destination] != EMPTY {
            let source = self.parent_bucket[destination];
            let resident = self.parent_item[destination];
            let appended = append(&mut self.buckets[destination], resident);
            assert!(appended, "augmenting-path destination must have a vacancy");
            remove(&mut self.buckets[source], resident);
            destination = source;
        }
        let appended = append(&mut self.buckets[destination], item_index);
        assert!(appended, "augmenting-path root must have a vacancy");
        Ok(true)
    }

    fn next_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.seen.fill(0);
            self.generation = 1;
        }
    }
}

/// Returns the length of the largest feasible prefix under capacity four.
pub fn exact_threshold(bucket_count: usize, items: &[Item]) -> Result<usize, OracleError> {
    let mut oracle = ExactOracle::new(bucket_count)?;
    for (index, &item) in items.iter().enumerate() {
        if !oracle.insert(item)? {
            return Ok(index);
        }
    }
    Ok(items.len())
}

/// Structurally independent small-instance cross-check using physical slots.
///
/// Expands each bucket into four right-side vertices and returns the largest
/// prefix accepted by bipartite augmenting matching.
pub fn slot_matching_threshold(bucket_count: usize, items: &[Item]) -> Result<usize, OracleError> {
    validate_bucket_count(bucket_count)?;
    let mut owners = vec![EMPTY; bucket_count * CAPACITY];
    let mut seen = vec![0_u32; owners.len()];
    let mut generation = 0_u32;

    for (index, &item) in items.iter().enumerate() {
        validate_item(bucket_count, item)?;
        generation = generation.wrapping_add(1);
        if generation == 0 {
            seen.fill(0);
            generation = 1;
        }
        if !augment_slot(index, items, &mut owners, &mut seen, generation) {
            return Ok(index);
        }
    }
    Ok(items.len())
}

// ponytail: recursive DFS is only for small oracle cross-checks; replace it
// with iterative Hopcroft-Karp if validation instances become large.
fn augment_slot(
    item_index: usize,
    items: &[Item],
    owners: &mut [usize],
    seen: &mut [u32],
    generation: u32,
) -> bool {
    let item = items[item_index];
    for (position, bucket) in [item.first as usize, item.second as usize]
        .into_iter()
        .enumerate()
    {
        if position == 1 && item.second == item.first {
            continue;
        }
        for lane in 0..CAPACITY {
            let slot = bucket * CAPACITY + lane;
            if seen[slot] == generation {
                continue;
            }
            seen[slot] = generation;
            let previous = owners[slot];
            if previous == EMPTY || augment_slot(previous, items, owners, seen, generation) {
                owners[slot] = item_index;
                return true;
            }
        }
    }
    false
}

#[inline]
fn append(bucket: &mut [usize; CAPACITY], item: usize) -> bool {
    let Some(slot) = bucket.iter_mut().find(|slot| **slot == EMPTY) else {
        return false;
    };
    *slot = item;
    true
}

fn remove(bucket: &mut [usize; CAPACITY], item: usize) {
    let position = bucket
        .iter()
        .position(|&resident| resident == item)
        .expect("augmenting path resident must be in its source bucket");
    bucket[position..].rotate_left(1);
    bucket[CAPACITY - 1] = EMPTY;
}

#[inline]
fn is_full(bucket: &[usize; CAPACITY]) -> bool {
    bucket[CAPACITY - 1] != EMPTY
}

#[inline]
fn other(item: Item, bucket: usize) -> usize {
    if item.first as usize == bucket {
        item.second as usize
    } else {
        item.first as usize
    }
}

fn validate_bucket_count(bucket_count: usize) -> Result<(), OracleError> {
    if bucket_count == 0 || bucket_count - 1 > u32::MAX as usize {
        Err(OracleError::BucketCount)
    } else {
        Ok(())
    }
}

fn validate_item(bucket_count: usize, item: Item) -> Result<(), OracleError> {
    for endpoint in [item.first, item.second] {
        if endpoint as usize >= bucket_count {
            return Err(OracleError::EndpointOutOfRange {
                endpoint,
                bucket_count,
            });
        }
    }
    Ok(())
}

#[inline]
fn fingerprint_delta(mask: u32, fingerprint: u16) -> u32 {
    let mut delta = u32::from(fingerprint).wrapping_mul(0x9e37_79b1) & mask;
    delta ^= delta >> 8;
    if delta == 0 { 1 } else { delta }
}

#[inline]
fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(GOLDEN_RATIO);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::{
        GraphModel, Item, exact_threshold, fingerprint_delta, generate_items,
        slot_matching_threshold,
    };

    #[test]
    fn incremental_oracle_matches_independent_slot_matching() {
        let mut instances = 0;
        for graph in [GraphModel::Independent, GraphModel::Xor16] {
            for seed in 0..512_u64 {
                let bucket_count = 4 << (seed as usize & 3);
                let mut items =
                    generate_items(bucket_count, bucket_count * 4 + 8, seed, graph).unwrap();

                match seed & 3 {
                    0 => {
                        items[1].fingerprint = items[0].fingerprint;
                    }
                    1 => {
                        items[1].second = items[1].first;
                        items[2].fingerprint = items[1].fingerprint;
                    }
                    2 => {
                        items[2] = items[1];
                    }
                    _ => {
                        items.extend(
                            [Item {
                                first: 0,
                                second: 0,
                                fingerprint: 7,
                            }; 6],
                        );
                    }
                }

                assert_eq!(
                    exact_threshold(bucket_count, &items).unwrap(),
                    slot_matching_threshold(bucket_count, &items).unwrap(),
                    "graph={graph:?}, seed={seed}, buckets={bucket_count}"
                );
                instances += 1;
            }
        }

        let xor = generate_items(16, 8, 0, GraphModel::Xor16).unwrap();
        let independent = generate_items(16, 8, 0, GraphModel::Independent).unwrap();
        assert_eq!(
            &xor[..4],
            &[
                Item {
                    first: 1,
                    second: 4,
                    fingerprint: 42_757,
                },
                Item {
                    first: 2,
                    second: 7,
                    fingerprint: 10_021,
                },
                Item {
                    first: 3,
                    second: 6,
                    fingerprint: 60_293,
                },
                Item {
                    first: 7,
                    second: 12,
                    fingerprint: 9_195,
                },
            ]
        );
        assert_eq!(
            independent[..4]
                .iter()
                .map(|item| item.second)
                .collect::<Vec<_>>(),
            [0, 5, 13, 2]
        );
        for (xor_item, independent_item) in xor.iter().zip(&independent) {
            assert_eq!(xor_item.first, independent_item.first);
            assert_eq!(xor_item.fingerprint, independent_item.fingerprint);
            assert_eq!(
                xor_item.second,
                xor_item.first ^ fingerprint_delta(15, xor_item.fingerprint)
            );
            assert_ne!(independent_item.first, independent_item.second);
        }
        assert_eq!(instances, 1_024);
    }
}

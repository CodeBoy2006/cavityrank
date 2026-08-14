//! Deterministic benchmark runners and CSV-oriented result types.
//!
//! The runners share hashing, accounting, and failure semantics, providing one
//! comparison contract across policies.

use crate::{Config, ConfigError, CuckooFilter, PathConfig, Policy, filter::splitmix64};
use std::{
    error::Error,
    fmt,
    hint::black_box,
    time::{Duration, Instant},
};

const BUILD_FILTER_SALT: u64 = 0xdead_beef_cafe_babe;
const BUILD_KEY_SALT: u64 = 0x1234_5678_9abc_def0;
const CHURN_FILTER_SALT: u64 = 0x81f1_f5aa_1234;
const CHURN_KEY_SALT: u64 = 0xe703_7ed1_a0b4_28db;
const CHURN_PICK_SALT: u64 = 0x9e37_79b9_7f4a_7c15;
const QUERY_KEY_SALT: u64 = 0x6a09_e667_f3bc_c909;

/// Interpolated summary of per-operation `u32` counters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SummaryStats {
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: u32,
}

impl SummaryStats {
    fn from_values(mut values: Vec<u32>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let sum: u64 = values.iter().map(|&value| u64::from(value)).sum();
        let mean = sum as f64 / values.len() as f64;
        values.sort_unstable();
        Self {
            mean,
            p50: quantile(&values, 0.5),
            p95: quantile(&values, 0.95),
            p99: quantile(&values, 0.99),
            p999: quantile(&values, 0.999),
            max: values[values.len() - 1],
        }
    }
}

/// Interpolated insertion-latency summary in nanoseconds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LatencyStats {
    pub mean_ns: f64,
    pub p50_ns: f64,
    pub p95_ns: f64,
    pub p99_ns: f64,
    pub p999_ns: f64,
    pub max_ns: u64,
}

impl LatencyStats {
    fn from_values(mut values: Vec<u64>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let sum: u128 = values.iter().map(|&value| u128::from(value)).sum();
        let mean_ns = sum as f64 / values.len() as f64;
        values.sort_unstable();
        Self {
            mean_ns,
            p50_ns: quantile_u64(&values, 0.5),
            p95_ns: quantile_u64(&values, 0.95),
            p99_ns: quantile_u64(&values, 0.99),
            p999_ns: quantile_u64(&values, 0.999),
            max_ns: values[values.len() - 1],
        }
    }
}

/// Parameters for a build-throughput run and its tail-load window.
#[derive(Clone, Copy, Debug)]
pub struct BuildRunConfig {
    pub bucket_count: usize,
    pub policy: Policy,
    pub seed: u64,
    pub target_load: f64,
    pub window_width: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub verify: bool,
}

/// CSV-ready measurements from one build-throughput run.
#[derive(Clone, Debug)]
pub struct BuildRow {
    pub policy: Policy,
    pub seed: u64,
    pub bucket_count: usize,
    pub target_load: f64,
    pub window_width: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub verify: bool,
    pub reached: bool,
    pub achieved_load: f64,
    pub inserted: usize,
    pub window_ops: usize,
    pub total_mops: f64,
    pub window_mops: f64,
    pub relocations: SummaryStats,
    pub reads: SummaryStats,
    pub writes: SummaryStats,
    pub relocation_fraction: f64,
    pub failed_attempt: bool,
    pub failed_at: usize,
    pub failed_relocations: u32,
    pub failed_reads: u32,
    pub failed_writes: u32,
    pub filter_usable: bool,
    pub failed_path_activated: bool,
    pub failed_path_activation_step: u32,
    pub failed_path_guarded_steps: u32,
    pub failed_path_checks: u32,
    pub failed_path_seen_candidates_rejected: u32,
    pub path_activated: usize,
    pub path_activation_fraction: f64,
    pub path_activation_step: SummaryStats,
    pub path_guarded_steps: SummaryStats,
    pub path_checks: SummaryStats,
    pub path_seen_candidates_rejected: SummaryStats,
    pub rank4_full_buckets: usize,
    pub rank4_alias_buckets: usize,
    pub rank4_alias_bucket_rate: f64,
    pub bootstrap_ms: f64,
    pub bootstrap_reads: u64,
    pub bootstrap_writes: u64,
    pub payload_bytes: usize,
    pub persistent_metadata_bytes: usize,
    pub transient_insertion_workspace_bytes: usize,
}

impl BuildRow {
    pub const CSV_HEADER: &'static str = "policy,seed,bucket_count,payload_mib,target_load,window,max_kicks,bfs_depth,path_bytes,path_activation,path_reset,verify,reached,achieved_load,inserted,window_ops,total_mops,window_mops,reloc_mean,reloc_p50,reloc_p95,reloc_p99,reloc_p999,reloc_max,reads_mean,reads_p95,reads_p99,reads_p999,reads_max,writes_mean,writes_p95,writes_p99,writes_p999,writes_max,relocation_fraction,failed_attempt,failed_at,failed_relocations,failed_reads,failed_writes,filter_usable,failed_path_activated,failed_path_activation_step,failed_path_guarded_steps,failed_path_checks,failed_path_seen_candidates_rejected,path_activated,path_activation_fraction,path_activation_step_mean,path_activation_step_p50,path_activation_step_p95,path_activation_step_p99,path_activation_step_p999,path_activation_step_max,path_guarded_steps_mean,path_guarded_steps_p95,path_guarded_steps_p99,path_guarded_steps_p999,path_guarded_steps_max,path_checks_mean,path_checks_p95,path_checks_p99,path_checks_p999,path_checks_max,path_seen_candidates_rejected_mean,path_seen_candidates_rejected_p95,path_seen_candidates_rejected_p99,path_seen_candidates_rejected_p999,path_seen_candidates_rejected_max,rank4_full_buckets,rank4_alias_buckets,rank4_alias_bucket_rate,bootstrap_ms,bootstrap_reads,bootstrap_writes,payload_bytes,persistent_metadata_bytes,transient_insertion_workspace_bytes";

    pub fn to_csv(&self) -> String {
        [
            self.policy.to_string(),
            self.seed.to_string(),
            self.bucket_count.to_string(),
            format!("{:.6}", self.bucket_count as f64 * 8.0 / (1024.0 * 1024.0)),
            format!("{:.6}", self.target_load),
            format!("{:.6}", self.window_width),
            self.max_kicks.to_string(),
            self.bfs_depth.to_string(),
            self.path.bytes.to_string(),
            self.path.activation.to_string(),
            self.path.reset.to_string(),
            u8::from(self.verify).to_string(),
            u8::from(self.reached).to_string(),
            format!("{:.6}", self.achieved_load),
            self.inserted.to_string(),
            self.window_ops.to_string(),
            format!("{:.6}", self.total_mops),
            format!("{:.6}", self.window_mops),
            format!("{:.6}", self.relocations.mean),
            format!("{:.6}", self.relocations.p50),
            format!("{:.6}", self.relocations.p95),
            format!("{:.6}", self.relocations.p99),
            format!("{:.6}", self.relocations.p999),
            self.relocations.max.to_string(),
            format!("{:.6}", self.reads.mean),
            format!("{:.6}", self.reads.p95),
            format!("{:.6}", self.reads.p99),
            format!("{:.6}", self.reads.p999),
            self.reads.max.to_string(),
            format!("{:.6}", self.writes.mean),
            format!("{:.6}", self.writes.p95),
            format!("{:.6}", self.writes.p99),
            format!("{:.6}", self.writes.p999),
            self.writes.max.to_string(),
            format!("{:.6}", self.relocation_fraction),
            u8::from(self.failed_attempt).to_string(),
            self.failed_at.to_string(),
            self.failed_relocations.to_string(),
            self.failed_reads.to_string(),
            self.failed_writes.to_string(),
            u8::from(self.filter_usable).to_string(),
            u8::from(self.failed_path_activated).to_string(),
            self.failed_path_activation_step.to_string(),
            self.failed_path_guarded_steps.to_string(),
            self.failed_path_checks.to_string(),
            self.failed_path_seen_candidates_rejected.to_string(),
            self.path_activated.to_string(),
            format!("{:.6}", self.path_activation_fraction),
            format!("{:.6}", self.path_activation_step.mean),
            format!("{:.6}", self.path_activation_step.p50),
            format!("{:.6}", self.path_activation_step.p95),
            format!("{:.6}", self.path_activation_step.p99),
            format!("{:.6}", self.path_activation_step.p999),
            self.path_activation_step.max.to_string(),
            format!("{:.6}", self.path_guarded_steps.mean),
            format!("{:.6}", self.path_guarded_steps.p95),
            format!("{:.6}", self.path_guarded_steps.p99),
            format!("{:.6}", self.path_guarded_steps.p999),
            self.path_guarded_steps.max.to_string(),
            format!("{:.6}", self.path_checks.mean),
            format!("{:.6}", self.path_checks.p95),
            format!("{:.6}", self.path_checks.p99),
            format!("{:.6}", self.path_checks.p999),
            self.path_checks.max.to_string(),
            format!("{:.6}", self.path_seen_candidates_rejected.mean),
            format!("{:.6}", self.path_seen_candidates_rejected.p95),
            format!("{:.6}", self.path_seen_candidates_rejected.p99),
            format!("{:.6}", self.path_seen_candidates_rejected.p999),
            self.path_seen_candidates_rejected.max.to_string(),
            self.rank4_full_buckets.to_string(),
            self.rank4_alias_buckets.to_string(),
            format!("{:.9}", self.rank4_alias_bucket_rate),
            format!("{:.6}", self.bootstrap_ms),
            self.bootstrap_reads.to_string(),
            self.bootstrap_writes.to_string(),
            self.payload_bytes.to_string(),
            self.persistent_metadata_bytes.to_string(),
            self.transient_insertion_workspace_bytes.to_string(),
        ]
        .join(",")
    }
}

/// Parameters for exhaustive successful-prefix membership checks.
#[derive(Clone, Copy, Debug)]
pub struct CorrectnessRunConfig {
    pub bucket_count: usize,
    pub policy: Policy,
    pub seed: u64,
    pub load: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
}

/// CSV-ready measurements from one correctness run.
#[derive(Clone, Debug)]
pub struct CorrectnessRow {
    pub policy: Policy,
    pub seed: u64,
    pub bucket_count: usize,
    pub load: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub reached: bool,
    pub achieved_load: f64,
    pub inserted: usize,
    pub successful_insertions_checked: usize,
    pub membership_checks: u64,
    pub false_negatives: usize,
    pub false_negative_detected_at: usize,
    pub first_false_negative_key: u64,
    pub failed_attempt: bool,
    pub failed_at: usize,
    pub failed_relocations: u32,
    pub failed_reads: u32,
    pub failed_writes: u32,
    pub filter_usable: bool,
    pub payload_bytes: usize,
    pub persistent_metadata_bytes: usize,
    pub transient_insertion_workspace_bytes: usize,
}

impl CorrectnessRow {
    pub const CSV_HEADER: &'static str = "policy,seed,bucket_count,payload_mib,load,max_kicks,bfs_depth,path_bytes,path_activation,path_reset,reached,achieved_load,inserted,successful_insertions_checked,membership_checks,false_negatives,false_negative_detected_at,first_false_negative_key,failed_attempt,failed_at,failed_relocations,failed_reads,failed_writes,filter_usable,payload_bytes,persistent_metadata_bytes,transient_insertion_workspace_bytes";

    pub fn to_csv(&self) -> String {
        [
            self.policy.to_string(),
            self.seed.to_string(),
            self.bucket_count.to_string(),
            format!("{:.6}", self.bucket_count as f64 * 8.0 / (1024.0 * 1024.0)),
            format!("{:.6}", self.load),
            self.max_kicks.to_string(),
            self.bfs_depth.to_string(),
            self.path.bytes.to_string(),
            self.path.activation.to_string(),
            self.path.reset.to_string(),
            u8::from(self.reached).to_string(),
            format!("{:.6}", self.achieved_load),
            self.inserted.to_string(),
            self.successful_insertions_checked.to_string(),
            self.membership_checks.to_string(),
            self.false_negatives.to_string(),
            self.false_negative_detected_at.to_string(),
            self.first_false_negative_key.to_string(),
            u8::from(self.failed_attempt).to_string(),
            self.failed_at.to_string(),
            self.failed_relocations.to_string(),
            self.failed_reads.to_string(),
            self.failed_writes.to_string(),
            u8::from(self.filter_usable).to_string(),
            self.payload_bytes.to_string(),
            self.persistent_metadata_bytes.to_string(),
            self.transient_insertion_workspace_bytes.to_string(),
        ]
        .join(",")
    }
}

/// Parameters for measuring successful insertion latency in a load window.
#[derive(Clone, Copy, Debug)]
pub struct LatencyRunConfig {
    pub bucket_count: usize,
    pub policy: Policy,
    pub seed: u64,
    pub target_load: f64,
    pub window_width: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
}

/// CSV-ready measurements and raw samples from one latency run.
#[derive(Clone, Debug)]
pub struct LatencyRow {
    pub policy: Policy,
    pub seed: u64,
    pub bucket_count: usize,
    pub target_load: f64,
    pub window_width: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub reached: bool,
    pub achieved_load: f64,
    pub inserted: usize,
    pub prefill_target: usize,
    pub latency_samples: usize,
    pub latency: LatencyStats,
    /// Successful insertion latencies in chronological order. The CLI may
    /// persist them as a raw sidecar; the values are not embedded in the CSV row.
    pub latency_samples_ns: Vec<u64>,
    pub latency_samples_file: String,
    pub failed_attempt: bool,
    pub failed_at: usize,
    pub failed_attempt_timed: bool,
    pub failed_latency_ns: u64,
    pub failed_relocations: u32,
    pub failed_reads: u32,
    pub failed_writes: u32,
    pub filter_usable: bool,
    pub payload_bytes: usize,
    pub persistent_metadata_bytes: usize,
    pub transient_insertion_workspace_bytes: usize,
}

impl LatencyRow {
    pub const CSV_HEADER: &'static str = "policy,seed,bucket_count,payload_mib,target_load,window,max_kicks,bfs_depth,path_bytes,path_activation,path_reset,reached,achieved_load,inserted,prefill_target,latency_samples,latency_mean_ns,latency_p50_ns,latency_p95_ns,latency_p99_ns,latency_p999_ns,latency_max_ns,failed_attempt,failed_at,failed_attempt_timed,failed_latency_ns,failed_relocations,failed_reads,failed_writes,filter_usable,payload_bytes,persistent_metadata_bytes,transient_insertion_workspace_bytes,latency_samples_file";

    pub fn to_csv(&self) -> String {
        [
            self.policy.to_string(),
            self.seed.to_string(),
            self.bucket_count.to_string(),
            format!("{:.6}", self.bucket_count as f64 * 8.0 / (1024.0 * 1024.0)),
            format!("{:.6}", self.target_load),
            format!("{:.6}", self.window_width),
            self.max_kicks.to_string(),
            self.bfs_depth.to_string(),
            self.path.bytes.to_string(),
            self.path.activation.to_string(),
            self.path.reset.to_string(),
            u8::from(self.reached).to_string(),
            format!("{:.6}", self.achieved_load),
            self.inserted.to_string(),
            self.prefill_target.to_string(),
            self.latency_samples.to_string(),
            format!("{:.3}", self.latency.mean_ns),
            format!("{:.3}", self.latency.p50_ns),
            format!("{:.3}", self.latency.p95_ns),
            format!("{:.3}", self.latency.p99_ns),
            format!("{:.3}", self.latency.p999_ns),
            self.latency.max_ns.to_string(),
            u8::from(self.failed_attempt).to_string(),
            self.failed_at.to_string(),
            u8::from(self.failed_attempt_timed).to_string(),
            self.failed_latency_ns.to_string(),
            self.failed_relocations.to_string(),
            self.failed_reads.to_string(),
            self.failed_writes.to_string(),
            u8::from(self.filter_usable).to_string(),
            self.payload_bytes.to_string(),
            self.persistent_metadata_bytes.to_string(),
            self.transient_insertion_workspace_bytes.to_string(),
            self.latency_samples_file.clone(),
        ]
        .join(",")
    }
}

/// Parameters for measuring repeated delete-insert cycles at a fixed load.
#[derive(Clone, Copy, Debug)]
pub struct ChurnRunConfig {
    pub bucket_count: usize,
    pub policy: Policy,
    pub seed: u64,
    pub load: f64,
    pub operations: usize,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub verify_samples: usize,
}

/// CSV-ready measurements from one churn run.
#[derive(Clone, Debug)]
pub struct ChurnRow {
    pub policy: Policy,
    pub seed: u64,
    pub bucket_count: usize,
    pub load: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub verify_samples: usize,
    pub completed: usize,
    pub reached: bool,
    pub churn_mops: f64,
    pub relocations: SummaryStats,
    pub insertion_reads: SummaryStats,
    pub insertion_writes: SummaryStats,
    pub false_negatives: usize,
    pub filter_usable: bool,
    pub payload_bytes: usize,
    pub persistent_metadata_bytes: usize,
    pub transient_insertion_workspace_bytes: usize,
}

impl ChurnRow {
    pub const CSV_HEADER: &'static str = "policy,seed,bucket_count,payload_mib,load,max_kicks,bfs_depth,path_bytes,path_activation,path_reset,verify_samples,churn_ops,reached,churn_mops,reloc_mean,reloc_p95,reloc_p99,reloc_p999,insertion_reads_mean,insertion_reads_p95,insertion_writes_mean,insertion_writes_p95,false_negatives,filter_usable,payload_bytes,persistent_metadata_bytes,transient_insertion_workspace_bytes";

    pub fn to_csv(&self) -> String {
        format!(
            "{},{},{},{:.6},{:.6},{},{},{},{},{},{},{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{},{},{},{},{}",
            self.policy,
            self.seed,
            self.bucket_count,
            self.bucket_count as f64 * 8.0 / (1024.0 * 1024.0),
            self.load,
            self.max_kicks,
            self.bfs_depth,
            self.path.bytes,
            self.path.activation,
            self.path.reset,
            self.verify_samples,
            self.completed,
            u8::from(self.reached),
            self.churn_mops,
            self.relocations.mean,
            self.relocations.p95,
            self.relocations.p99,
            self.relocations.p999,
            self.insertion_reads.mean,
            self.insertion_reads.p95,
            self.insertion_writes.mean,
            self.insertion_writes.p95,
            self.false_negatives,
            u8::from(self.filter_usable),
            self.payload_bytes,
            self.persistent_metadata_bytes,
            self.transient_insertion_workspace_bytes,
        )
    }
}

/// Parameters for measuring known-absent lookups after a fixed-load build.
#[derive(Clone, Copy, Debug)]
pub struct QueryRunConfig {
    pub bucket_count: usize,
    pub policy: Policy,
    pub seed: u64,
    pub load: f64,
    pub queries: usize,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub verify_samples: usize,
}

/// CSV-ready measurements from one query run.
#[derive(Clone, Debug)]
pub struct QueryRow {
    pub policy: Policy,
    pub seed: u64,
    pub bucket_count: usize,
    pub load: f64,
    pub max_kicks: u32,
    pub bfs_depth: u8,
    pub path: PathConfig,
    pub verify_samples: usize,
    pub reached: bool,
    pub inserted: usize,
    pub queries: usize,
    pub false_positives: usize,
    pub false_positive_rate: f64,
    pub query_mops: f64,
    pub false_negatives: usize,
    pub filter_usable: bool,
    pub payload_bytes: usize,
    pub persistent_metadata_bytes: usize,
    pub transient_insertion_workspace_bytes: usize,
}

impl QueryRow {
    pub const CSV_HEADER: &'static str = "policy,seed,bucket_count,payload_mib,load,max_kicks,bfs_depth,path_bytes,path_activation,path_reset,verify_samples,reached,inserted,queries,false_positives,false_positive_rate,query_mops,false_negatives,filter_usable,payload_bytes,persistent_metadata_bytes,transient_insertion_workspace_bytes";

    pub fn to_csv(&self) -> String {
        format!(
            "{},{},{},{:.6},{:.6},{},{},{},{},{},{},{},{},{},{},{:.9},{:.6},{},{},{},{},{}",
            self.policy,
            self.seed,
            self.bucket_count,
            self.bucket_count as f64 * 8.0 / (1024.0 * 1024.0),
            self.load,
            self.max_kicks,
            self.bfs_depth,
            self.path.bytes,
            self.path.activation,
            self.path.reset,
            self.verify_samples,
            u8::from(self.reached),
            self.inserted,
            self.queries,
            self.false_positives,
            self.false_positive_rate,
            self.query_mops,
            self.false_negatives,
            u8::from(self.filter_usable),
            self.payload_bytes,
            self.persistent_metadata_bytes,
            self.transient_insertion_workspace_bytes,
        )
    }
}

/// Invalid experiment input or a failed filter correctness check.
#[derive(Debug)]
pub enum ExperimentError {
    /// Filter construction failed.
    Filter(ConfigError),
    /// Requested load was outside `(0, 1)`.
    InvalidLoad(f64),
    /// Tail window was outside `(0, target_load]`.
    InvalidWindow(f64),
    /// The requested load rounds down to zero items.
    EmptyTarget,
    /// Verification found an inserted key that the filter no longer reports.
    FalseNegative {
        /// Policy under test.
        policy: Policy,
        /// Experiment seed.
        seed: u64,
        /// Missing inserted key.
        key: u64,
    },
}

impl fmt::Display for ExperimentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Filter(error) => error.fmt(formatter),
            Self::InvalidLoad(load) => write!(formatter, "load must be in (0, 1), got {load}"),
            Self::InvalidWindow(window) => {
                write!(
                    formatter,
                    "window width must be in (0, target_load], got {window}"
                )
            }
            Self::EmptyTarget => formatter.write_str("load produces an empty target table"),
            Self::FalseNegative { policy, seed, key } => {
                write!(
                    formatter,
                    "false negative for {policy}, seed {seed}, key {key}"
                )
            }
        }
    }
}

impl Error for ExperimentError {}

impl From<ConfigError> for ExperimentError {
    fn from(error: ConfigError) -> Self {
        Self::Filter(error)
    }
}

/// Builds a filter to `target_load` and measures total and tail-window throughput.
pub fn run_build(config: BuildRunConfig) -> Result<BuildRow, ExperimentError> {
    validate_load(config.target_load)?;
    validate_window(config.window_width, config.target_load)?;
    let mut filter = CuckooFilter::new(Config {
        bucket_count: config.bucket_count,
        policy: config.policy,
        seed: splitmix64(config.seed ^ BUILD_FILTER_SALT),
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
    })?;
    let target = (filter.capacity() as f64 * config.target_load).floor() as usize;
    if target == 0 {
        return Err(ExperimentError::EmptyTarget);
    }
    let window_begin = (filter.capacity() as f64
        * (config.target_load - config.window_width).max(0.0))
    .floor() as usize;
    let sample_stride = verification_stride(target, 200_000).expect("nonempty sample range");
    let mut samples = Vec::with_capacity(if config.verify {
        target.div_ceil(sample_stride)
    } else {
        0
    });
    let mut relocations = Vec::with_capacity(target - window_begin + 8);
    let mut reads = Vec::with_capacity(target - window_begin + 8);
    let mut writes = Vec::with_capacity(target - window_begin + 8);
    let mut path_activation_steps = Vec::with_capacity(target - window_begin + 8);
    let mut path_guarded_steps = Vec::with_capacity(target - window_begin + 8);
    let mut path_checks = Vec::with_capacity(target - window_begin + 8);
    let mut path_seen_candidates_rejected = Vec::with_capacity(target - window_begin + 8);
    let mut key_state = splitmix64(config.seed ^ BUILD_KEY_SALT);
    let total_start = Instant::now();
    let mut window_start = None;
    let mut failed_at = 0;
    let mut failed_attempt = false;
    let mut failed_relocations = 0;
    let mut failed_reads = 0;
    let mut failed_writes = 0;
    let mut filter_usable = true;
    let mut failed_path_activated = false;
    let mut failed_path_activation_step = 0;
    let mut failed_path_guarded_steps = 0;
    let mut failed_path_checks = 0;
    let mut failed_path_seen_candidates_rejected = 0;
    let mut path_activated = 0;
    let mut relocated_operations = 0_usize;
    let mut bootstrap_ms = 0.0;
    let mut bootstrap_reads = 0;
    let mut bootstrap_writes = 0;

    while filter.len() < target {
        if window_start.is_none() && filter.len() >= window_begin {
            window_start = Some(Instant::now());
        }
        key_state = splitmix64(key_state);
        if filter.needs_dense_prepare() {
            let start = Instant::now();
            let bootstrap = filter.prepare_dense();
            bootstrap_ms = start.elapsed().as_secs_f64() * 1_000.0;
            bootstrap_reads = bootstrap.bucket_reads;
            bootstrap_writes = bootstrap.bucket_writes;
        }
        let result = filter.insert(key_state);
        if !result.inserted {
            failed_attempt = true;
            failed_at = filter.len();
            failed_relocations = result.relocations;
            failed_reads = result.bucket_reads;
            failed_writes = result.bucket_writes;
            filter_usable = result.filter_usable;
            failed_path_activated = result.path_activated;
            failed_path_activation_step = result.path_activation_step;
            failed_path_guarded_steps = result.path_guarded_steps;
            failed_path_checks = result.path_checks;
            failed_path_seen_candidates_rejected = result.path_seen_candidates_rejected;
            break;
        }
        if config.verify && filter.len() % sample_stride == 1 % sample_stride {
            samples.push(key_state);
        }
        if filter.len() > window_begin {
            relocations.push(result.relocations);
            reads.push(result.bucket_reads);
            writes.push(result.bucket_writes);
            if result.path_activated {
                path_activated += 1;
                path_activation_steps.push(result.path_activation_step);
            }
            path_guarded_steps.push(result.path_guarded_steps);
            path_checks.push(result.path_checks);
            path_seen_candidates_rejected.push(result.path_seen_candidates_rejected);
            relocated_operations += usize::from(result.relocations != 0);
        }
    }
    let end = Instant::now();
    let (rank4_full_buckets, rank4_alias_buckets) = filter.rank4_codec_counts();

    if config.verify && !failed_attempt {
        for key in samples {
            if !filter.contains(key) {
                return Err(ExperimentError::FalseNegative {
                    policy: config.policy,
                    seed: config.seed,
                    key,
                });
            }
        }
    }

    let total_seconds = end.duration_since(total_start).as_secs_f64();
    let window_seconds = window_start.map_or(0.0, |start| end.duration_since(start).as_secs_f64());
    let window_ops = relocations.len();
    let payload_bytes = filter.payload_bytes();
    let persistent_metadata_bytes = filter.persistent_metadata_bytes();
    let transient_insertion_workspace_bytes = filter.transient_insertion_workspace_bytes();
    Ok(BuildRow {
        policy: config.policy,
        seed: config.seed,
        bucket_count: config.bucket_count,
        target_load: config.target_load,
        window_width: config.window_width,
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
        verify: config.verify,
        reached: !failed_attempt && filter.len() >= target,
        achieved_load: filter.load_factor(),
        inserted: filter.len(),
        window_ops,
        total_mops: throughput(filter.len(), total_seconds),
        window_mops: throughput(window_ops, window_seconds),
        relocations: SummaryStats::from_values(relocations),
        reads: SummaryStats::from_values(reads),
        writes: SummaryStats::from_values(writes),
        relocation_fraction: if window_ops == 0 {
            0.0
        } else {
            relocated_operations as f64 / window_ops as f64
        },
        failed_attempt,
        failed_at,
        failed_relocations,
        failed_reads,
        failed_writes,
        filter_usable,
        failed_path_activated,
        failed_path_activation_step,
        failed_path_guarded_steps,
        failed_path_checks,
        failed_path_seen_candidates_rejected,
        path_activated,
        path_activation_fraction: if window_ops == 0 {
            0.0
        } else {
            path_activated as f64 / window_ops as f64
        },
        path_activation_step: SummaryStats::from_values(path_activation_steps),
        path_guarded_steps: SummaryStats::from_values(path_guarded_steps),
        path_checks: SummaryStats::from_values(path_checks),
        path_seen_candidates_rejected: SummaryStats::from_values(path_seen_candidates_rejected),
        rank4_full_buckets,
        rank4_alias_buckets,
        rank4_alias_bucket_rate: if rank4_full_buckets == 0 {
            0.0
        } else {
            rank4_alias_buckets as f64 / rank4_full_buckets as f64
        },
        bootstrap_ms,
        bootstrap_reads,
        bootstrap_writes,
        payload_bytes,
        persistent_metadata_bytes,
        transient_insertion_workspace_bytes,
    })
}

/// Inserts to the requested load and verifies every successful prefix.
pub fn run_correctness(config: CorrectnessRunConfig) -> Result<CorrectnessRow, ExperimentError> {
    validate_load(config.load)?;
    let mut filter = CuckooFilter::new(Config {
        bucket_count: config.bucket_count,
        policy: config.policy,
        seed: splitmix64(config.seed ^ BUILD_FILTER_SALT),
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
    })?;
    let target = (filter.capacity() as f64 * config.load).floor() as usize;
    if target == 0 {
        return Err(ExperimentError::EmptyTarget);
    }

    let mut keys = Vec::with_capacity(target);
    let mut key_state = splitmix64(config.seed ^ BUILD_KEY_SALT);
    let mut successful_insertions_checked = 0;
    let mut membership_checks = 0_u64;
    let mut false_negatives = 0;
    let mut false_negative_detected_at = 0;
    let mut first_false_negative_key = 0;
    let mut failed_attempt = false;
    let mut failed_at = 0;
    let mut failed_relocations = 0;
    let mut failed_reads = 0;
    let mut failed_writes = 0;
    let mut filter_usable = true;

    while keys.len() < target {
        key_state = splitmix64(key_state);
        let result = filter.insert(key_state);
        if !result.inserted {
            failed_attempt = true;
            failed_at = keys.len();
            failed_relocations = result.relocations;
            failed_reads = result.bucket_reads;
            failed_writes = result.bucket_writes;
            filter_usable = result.filter_usable;
            break;
        }

        keys.push(key_state);
        let mut prefix_is_present = true;
        // ponytail: exhaustive prefix checks are O(n²); sample if runs grow large.
        for &key in &keys {
            membership_checks += 1;
            if !filter.contains(key) {
                false_negatives = 1;
                false_negative_detected_at = keys.len();
                first_false_negative_key = key;
                prefix_is_present = false;
                break;
            }
        }
        if !prefix_is_present {
            break;
        }
        successful_insertions_checked += 1;
    }

    Ok(CorrectnessRow {
        policy: config.policy,
        seed: config.seed,
        bucket_count: config.bucket_count,
        load: config.load,
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
        reached: !failed_attempt && false_negatives == 0 && keys.len() >= target,
        achieved_load: filter.load_factor(),
        inserted: keys.len(),
        successful_insertions_checked,
        membership_checks,
        false_negatives,
        false_negative_detected_at,
        first_false_negative_key,
        failed_attempt,
        failed_at,
        failed_relocations,
        failed_reads,
        failed_writes,
        filter_usable,
        payload_bytes: filter.payload_bytes(),
        persistent_metadata_bytes: filter.persistent_metadata_bytes(),
        transient_insertion_workspace_bytes: filter.transient_insertion_workspace_bytes(),
    })
}

/// Prefills a filter, then records successful insertion latencies in order.
pub fn run_latency(config: LatencyRunConfig) -> Result<LatencyRow, ExperimentError> {
    validate_load(config.target_load)?;
    validate_window(config.window_width, config.target_load)?;
    let mut filter = CuckooFilter::new(Config {
        bucket_count: config.bucket_count,
        policy: config.policy,
        seed: splitmix64(config.seed ^ BUILD_FILTER_SALT),
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
    })?;
    let target = (filter.capacity() as f64 * config.target_load).floor() as usize;
    if target == 0 {
        return Err(ExperimentError::EmptyTarget);
    }
    let prefill_target = (filter.capacity() as f64
        * (config.target_load - config.window_width).max(0.0))
    .floor() as usize;
    let mut key_state = splitmix64(config.seed ^ BUILD_KEY_SALT);
    let mut failed_attempt = false;
    let mut failed_at = 0;
    let mut failed_attempt_timed = false;
    let mut failed_latency_ns = 0;
    let mut failed_relocations = 0;
    let mut failed_reads = 0;
    let mut failed_writes = 0;
    let mut filter_usable = true;

    while filter.len() < prefill_target {
        key_state = splitmix64(key_state);
        let result = filter.insert(key_state);
        if !result.inserted {
            failed_attempt = true;
            failed_at = filter.len();
            failed_relocations = result.relocations;
            failed_reads = result.bucket_reads;
            failed_writes = result.bucket_writes;
            filter_usable = result.filter_usable;
            break;
        }
    }

    let mut latencies = Vec::with_capacity(target.saturating_sub(prefill_target));
    while !failed_attempt && filter.len() < target {
        key_state = splitmix64(key_state);
        let start = Instant::now();
        let result = filter.insert(key_state);
        let elapsed_ns = duration_ns(start.elapsed());
        if !result.inserted {
            failed_attempt = true;
            failed_at = filter.len();
            failed_attempt_timed = true;
            failed_latency_ns = elapsed_ns;
            failed_relocations = result.relocations;
            failed_reads = result.bucket_reads;
            failed_writes = result.bucket_writes;
            filter_usable = result.filter_usable;
            break;
        }
        latencies.push(elapsed_ns);
    }

    let latency_samples = latencies.len();
    let latency = LatencyStats::from_values(latencies.clone());
    Ok(LatencyRow {
        policy: config.policy,
        seed: config.seed,
        bucket_count: config.bucket_count,
        target_load: config.target_load,
        window_width: config.window_width,
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
        reached: !failed_attempt && filter.len() >= target,
        achieved_load: filter.load_factor(),
        inserted: filter.len(),
        prefill_target,
        latency_samples,
        latency,
        latency_samples_ns: latencies,
        latency_samples_file: String::new(),
        failed_attempt,
        failed_at,
        failed_attempt_timed,
        failed_latency_ns,
        failed_relocations,
        failed_reads,
        failed_writes,
        filter_usable,
        payload_bytes: filter.payload_bytes(),
        persistent_metadata_bytes: filter.persistent_metadata_bytes(),
        transient_insertion_workspace_bytes: filter.transient_insertion_workspace_bytes(),
    })
}

/// Prefills a filter and measures repeated delete-insert cycles.
pub fn run_churn(config: ChurnRunConfig) -> Result<ChurnRow, ExperimentError> {
    validate_load(config.load)?;
    let mut filter = CuckooFilter::new(Config {
        bucket_count: config.bucket_count,
        policy: config.policy,
        seed: splitmix64(config.seed ^ CHURN_FILTER_SALT),
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
    })?;
    let target = (filter.capacity() as f64 * config.load).floor() as usize;
    if target == 0 {
        return Err(ExperimentError::EmptyTarget);
    }
    let mut active = Vec::with_capacity(target);
    let mut key_state = splitmix64(config.seed ^ CHURN_KEY_SALT);
    let mut reached = true;
    let mut filter_usable = true;
    let mut false_negatives = 0;
    while active.len() < target {
        key_state = splitmix64(key_state);
        let result = filter.insert(key_state);
        if !result.inserted {
            reached = false;
            filter_usable = result.filter_usable;
            break;
        }
        active.push(key_state);
    }

    let mut chooser = splitmix64(config.seed ^ CHURN_PICK_SALT);
    let mut relocations = Vec::with_capacity(config.operations);
    let mut reads = Vec::with_capacity(config.operations);
    let mut writes = Vec::with_capacity(config.operations);
    let start = Instant::now();
    let mut completed = 0;
    while reached && completed < config.operations {
        chooser = splitmix64(chooser);
        let index = chooser as usize % active.len();
        if !filter.remove(active[index]) {
            reached = false;
            filter_usable = false;
            false_negatives = 1;
            break;
        }
        key_state = splitmix64(key_state);
        let result = filter.insert(key_state);
        if !result.inserted {
            active.swap_remove(index);
            reached = false;
            filter_usable = result.filter_usable;
            break;
        }
        active[index] = key_state;
        relocations.push(result.relocations);
        reads.push(result.bucket_reads);
        writes.push(result.bucket_writes);
        completed += 1;
    }
    let seconds = start.elapsed().as_secs_f64();
    if false_negatives == 0 {
        false_negatives = sampled_false_negatives(&filter, &active, config.verify_samples);
        filter_usable &= false_negatives == 0;
    }
    let payload_bytes = filter.payload_bytes();
    let persistent_metadata_bytes = filter.persistent_metadata_bytes();
    let transient_insertion_workspace_bytes = filter.transient_insertion_workspace_bytes();

    Ok(ChurnRow {
        policy: config.policy,
        seed: config.seed,
        bucket_count: config.bucket_count,
        load: config.load,
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
        verify_samples: config.verify_samples,
        completed,
        reached,
        churn_mops: throughput(completed, seconds),
        relocations: SummaryStats::from_values(relocations),
        insertion_reads: SummaryStats::from_values(reads),
        insertion_writes: SummaryStats::from_values(writes),
        false_negatives,
        filter_usable,
        payload_bytes,
        persistent_metadata_bytes,
        transient_insertion_workspace_bytes,
    })
}

/// Builds a filter and measures lookups from a disjoint, known-absent key domain.
pub fn run_queries(config: QueryRunConfig) -> Result<QueryRow, ExperimentError> {
    validate_load(config.load)?;
    let mut filter = CuckooFilter::new(Config {
        bucket_count: config.bucket_count,
        policy: config.policy,
        seed: splitmix64(config.seed ^ BUILD_FILTER_SALT),
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
    })?;
    let target = (filter.capacity() as f64 * config.load).floor() as usize;
    if target == 0 {
        return Err(ExperimentError::EmptyTarget);
    }
    let sample_stride = verification_stride(target, config.verify_samples);
    let mut samples = Vec::with_capacity(sample_stride.map_or(0, |stride| target.div_ceil(stride)));
    let mut reached = true;
    let mut filter_usable = true;
    while filter.len() < target {
        let key = query_domain_key(config.seed, BUILD_KEY_SALT, filter.len(), false);
        let result = filter.insert(key);
        if !result.inserted {
            reached = false;
            filter_usable = result.filter_usable;
            break;
        }
        if let Some(stride) = sample_stride
            && filter.len() % stride == 1 % stride
        {
            samples.push(key);
        }
    }
    let false_negatives = samples.iter().filter(|&&key| !filter.contains(key)).count();
    filter_usable &= false_negatives == 0;

    let mut false_positives = 0;
    let start = Instant::now();
    if reached {
        for index in 0..config.queries {
            let key = query_domain_key(config.seed, QUERY_KEY_SALT, index, true);
            false_positives += usize::from(filter.contains(black_box(key)));
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    let measured_queries = if reached { config.queries } else { 0 };

    Ok(QueryRow {
        policy: config.policy,
        seed: config.seed,
        bucket_count: config.bucket_count,
        load: config.load,
        max_kicks: config.max_kicks,
        bfs_depth: config.bfs_depth,
        path: config.path,
        verify_samples: config.verify_samples,
        reached,
        inserted: filter.len(),
        queries: measured_queries,
        false_positives,
        false_positive_rate: if measured_queries == 0 {
            0.0
        } else {
            false_positives as f64 / measured_queries as f64
        },
        query_mops: throughput(measured_queries, seconds),
        false_negatives,
        filter_usable,
        payload_bytes: filter.payload_bytes(),
        persistent_metadata_bytes: filter.persistent_metadata_bytes(),
        transient_insertion_workspace_bytes: filter.transient_insertion_workspace_bytes(),
    })
}

fn sampled_false_negatives(filter: &CuckooFilter, keys: &[u64], max_samples: usize) -> usize {
    let Some(stride) = verification_stride(keys.len(), max_samples) else {
        return 0;
    };
    keys.iter()
        .step_by(stride)
        .filter(|&&key| !filter.contains(key))
        .count()
}

fn verification_stride(item_count: usize, max_samples: usize) -> Option<usize> {
    (item_count > 0 && max_samples > 0).then(|| item_count.div_ceil(max_samples))
}

#[inline]
fn query_domain_key(seed: u64, salt: u64, index: usize, absent: bool) -> u64 {
    const DOMAIN_MASK: u64 = u64::MAX >> 1;
    let value = (splitmix64(seed ^ salt) & DOMAIN_MASK).wrapping_add(index as u64) & DOMAIN_MASK;
    value << 1 | u64::from(absent)
}

fn validate_load(load: f64) -> Result<(), ExperimentError> {
    if load > 0.0 && load < 1.0 {
        Ok(())
    } else {
        Err(ExperimentError::InvalidLoad(load))
    }
}

fn validate_window(window: f64, target_load: f64) -> Result<(), ExperimentError> {
    if window > 0.0 && window <= target_load {
        Ok(())
    } else {
        Err(ExperimentError::InvalidWindow(window))
    }
}

fn throughput(operations: usize, seconds: f64) -> f64 {
    if seconds > 0.0 {
        operations as f64 / seconds / 1_000_000.0
    } else {
        0.0
    }
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn quantile(sorted: &[u32], probability: f64) -> f64 {
    let index = probability * (sorted.len() - 1) as f64;
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    let fraction = index - lower as f64;
    f64::from(sorted[lower]) * (1.0 - fraction) + f64::from(sorted[upper]) * fraction
}

fn quantile_u64(sorted: &[u64], probability: f64) -> f64 {
    let index = probability * (sorted.len() - 1) as f64;
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    let fraction = index - lower as f64;
    sorted[lower] as f64 * (1.0 - fraction) + sorted[upper] as f64 * fraction
}

#[cfg(test)]
mod tests {
    use super::{
        BUILD_KEY_SALT, BuildRow, BuildRunConfig, ChurnRow, ChurnRunConfig, CorrectnessRow,
        CorrectnessRunConfig, ExperimentError, LatencyRow, LatencyRunConfig, LatencyStats,
        QUERY_KEY_SALT, QueryRow, QueryRunConfig, SummaryStats, query_domain_key, run_build,
        run_churn, run_correctness, run_latency, run_queries, validate_window, verification_stride,
    };
    use crate::{PathActivation, PathConfig, PathReset, Policy};

    #[test]
    fn summary_matches_interpolated_quantiles() {
        let stats = SummaryStats::from_values(vec![0, 1, 2, 3, 4]);
        assert_eq!(stats.mean, 2.0);
        assert_eq!(stats.p50, 2.0);
        assert!((stats.p95 - 3.8).abs() < 1e-12);
        assert!((stats.p99 - 3.96).abs() < 1e-12);
        assert_eq!(stats.max, 4);

        let latency = LatencyStats::from_values(vec![0, 10, 20, 30, 40]);
        assert_eq!(latency.mean_ns, 20.0);
        assert_eq!(latency.p50_ns, 20.0);
        assert!((latency.p95_ns - 38.0).abs() < 1e-12);
        assert!((latency.p999_ns - 39.96).abs() < 1e-12);
        assert_eq!(latency.max_ns, 40);
    }

    #[test]
    fn validation_sampling_and_query_domains_cover_boundaries() {
        assert!(matches!(
            validate_window(f64::NAN, 0.9),
            Err(ExperimentError::InvalidWindow(value)) if value.is_nan()
        ));
        assert!(validate_window(0.01, 0.9).is_ok());
        assert!(validate_window(0.0, 0.9).is_err());
        assert!(validate_window(0.91, 0.9).is_err());

        assert_eq!(verification_stride(10, 6), Some(2));
        assert_eq!(verification_stride(10, 0), None);
        assert_eq!(verification_stride(0, 6), None);

        for index in 0..1_024 {
            let present = query_domain_key(7, BUILD_KEY_SALT, index, false);
            let absent = query_domain_key(7, QUERY_KEY_SALT, index, true);
            assert_eq!(present & 1, 0);
            assert_eq!(absent & 1, 1);
            assert_ne!(
                present,
                query_domain_key(7, BUILD_KEY_SALT, index + 1, false)
            );
        }
    }

    #[test]
    fn correctness_checks_every_successful_prefix() {
        let row = run_correctness(CorrectnessRunConfig {
            bucket_count: 32,
            policy: Policy::CavityRank4Path,
            seed: 0,
            load: 0.5,
            max_kicks: 1_000,
            bfs_depth: 8,
            path: PathConfig {
                bytes: 512,
                activation: PathActivation::After(0),
                reset: PathReset::Full,
            },
        })
        .unwrap();

        assert!(row.reached);
        assert_eq!(row.inserted, 64);
        assert_eq!(row.successful_insertions_checked, row.inserted);
        assert_eq!(
            row.membership_checks,
            row.inserted as u64 * (row.inserted as u64 + 1) / 2
        );
        assert_eq!(row.false_negatives, 0);
        assert!(!row.failed_attempt);
        assert_eq!(
            CorrectnessRow::CSV_HEADER.split(',').count(),
            row.to_csv().split(',').count()
        );

        let failed = run_correctness(CorrectnessRunConfig {
            bucket_count: 64,
            policy: Policy::Random,
            seed: 0,
            load: 0.99,
            max_kicks: 1,
            bfs_depth: 1,
            path: Default::default(),
        })
        .unwrap();
        assert!(!failed.reached);
        assert!(failed.failed_attempt);
        assert_eq!(failed.inserted, 182);
        assert_eq!(failed.successful_insertions_checked, failed.inserted);
        assert_eq!(
            failed.membership_checks,
            failed.inserted as u64 * (failed.inserted as u64 + 1) / 2
        );
        assert_eq!(failed.false_negatives, 0);
        assert!(!failed.filter_usable);
    }

    #[test]
    fn latency_samples_are_ordered_and_exclude_failed_attempts() {
        let row = run_latency(LatencyRunConfig {
            bucket_count: 256,
            policy: Policy::CavityRank4,
            seed: 0,
            target_load: 0.9,
            window_width: 0.1,
            max_kicks: 1_000,
            bfs_depth: 8,
            path: Default::default(),
        })
        .unwrap();
        assert!(row.reached);
        assert_eq!(row.inserted, 921);
        assert_eq!(row.prefill_target, 819);
        assert_eq!(row.latency_samples, 102);
        assert!(row.latency.p50_ns <= row.latency.p95_ns);
        assert!(row.latency.p95_ns <= row.latency.p99_ns);
        assert!(row.latency.p99_ns <= row.latency.p999_ns);
        assert!(row.latency.p999_ns <= row.latency.max_ns as f64);
        assert_eq!(
            LatencyRow::CSV_HEADER.split(',').count(),
            row.to_csv().split(',').count()
        );

        let failed = run_latency(LatencyRunConfig {
            bucket_count: 64,
            policy: Policy::Random,
            seed: 0,
            target_load: 0.99,
            window_width: 0.99,
            max_kicks: 1,
            bfs_depth: 1,
            path: Default::default(),
        })
        .unwrap();
        assert!(!failed.reached);
        assert!(failed.failed_attempt);
        assert!(failed.failed_attempt_timed);
        assert_eq!(failed.prefill_target, 0);
        assert_eq!(failed.latency_samples, failed.failed_at);
        assert_eq!(failed.inserted, failed.latency_samples);
        assert!(!failed.filter_usable);
    }

    #[test]
    fn experiment_smoke_test() {
        let build = run_build(BuildRunConfig {
            bucket_count: 256,
            policy: Policy::CavityBit,
            seed: 0,
            target_load: 0.9,
            window_width: 0.01,
            max_kicks: 1_000,
            bfs_depth: 8,
            path: Default::default(),
            verify: true,
        })
        .unwrap();
        assert!(build.reached);
        assert_eq!(build.inserted, 921);
        assert_eq!(
            BuildRow::CSV_HEADER.split(',').count(),
            build.to_csv().split(',').count()
        );

        let churn = run_churn(ChurnRunConfig {
            bucket_count: 256,
            policy: Policy::CavityBit,
            seed: 0,
            load: 0.9,
            operations: 1_000,
            max_kicks: 1_000,
            bfs_depth: 8,
            path: Default::default(),
            verify_samples: 1_000,
        })
        .unwrap();
        assert!(churn.reached);
        assert_eq!(churn.false_negatives, 0);
        assert!(churn.filter_usable);
        assert_eq!(
            ChurnRow::CSV_HEADER.split(',').count(),
            churn.to_csv().split(',').count()
        );

        let query = run_queries(QueryRunConfig {
            bucket_count: 256,
            policy: Policy::CavityBit,
            seed: 0,
            load: 0.9,
            queries: 10_000,
            max_kicks: 1_000,
            bfs_depth: 8,
            path: Default::default(),
            verify_samples: 1_000,
        })
        .unwrap();
        assert!(query.reached);
        assert_eq!(query.false_negatives, 0);
        assert!(query.filter_usable);
        assert_eq!(query.persistent_metadata_bytes, 0);
        assert_eq!(query.transient_insertion_workspace_bytes, 0);
        assert_eq!(
            QueryRow::CSV_HEADER.split(',').count(),
            query.to_csv().split(',').count()
        );

        let failed_query = run_queries(QueryRunConfig {
            bucket_count: 64,
            policy: Policy::Random,
            seed: 0,
            load: 0.99,
            queries: 10,
            max_kicks: 1,
            bfs_depth: 1,
            path: Default::default(),
            verify_samples: 200,
        })
        .unwrap();
        assert!(!failed_query.reached);
        assert!(!failed_query.filter_usable);
    }

    #[test]
    fn build_preserves_failed_attempt_and_path_tail_counters() {
        let failed = run_build(BuildRunConfig {
            bucket_count: 64,
            policy: Policy::Random,
            seed: 0,
            target_load: 0.99,
            window_width: 0.01,
            max_kicks: 1,
            bfs_depth: 1,
            path: PathConfig::default(),
            verify: false,
        })
        .unwrap();
        assert!(!failed.reached);
        assert!(failed.failed_attempt);
        assert_eq!(failed.failed_at, 182);
        assert_eq!(failed.failed_relocations, 1);
        assert_eq!(failed.failed_reads, 4);
        assert_eq!(failed.failed_writes, 1);
        assert!(!failed.filter_usable);

        let failed_path = run_build(BuildRunConfig {
            bucket_count: 64,
            policy: Policy::CavityRank4Path,
            seed: 0,
            target_load: 0.99,
            window_width: 0.01,
            max_kicks: 1,
            bfs_depth: 1,
            path: PathConfig {
                bytes: 512,
                activation: PathActivation::After(0),
                reset: PathReset::Full,
            },
            verify: false,
        })
        .unwrap();
        assert!(failed_path.failed_attempt);
        assert!(!failed_path.filter_usable);
        assert!(failed_path.failed_path_activated);
        assert_eq!(failed_path.failed_path_activation_step, 1);
        assert_eq!(failed_path.failed_path_guarded_steps, 1);
        assert!(failed_path.failed_path_checks > 0);

        let guarded = run_build(BuildRunConfig {
            bucket_count: 256,
            policy: Policy::CavityRank4Path,
            seed: 0,
            target_load: 0.9,
            window_width: 0.1,
            max_kicks: 1_000,
            bfs_depth: 8,
            path: PathConfig {
                bytes: 512,
                activation: PathActivation::After(0),
                reset: PathReset::Full,
            },
            verify: true,
        })
        .unwrap();
        assert!(guarded.reached);
        assert!(!guarded.failed_attempt);
        assert!(guarded.path_activated > 0);
        assert_eq!(guarded.path_activation_step.mean, 1.0);
        assert_eq!(guarded.path_activation_step.max, 1);
        assert!(guarded.path_guarded_steps.mean > 0.0);
        assert!(guarded.path_checks.mean >= guarded.path_guarded_steps.mean);
        assert_eq!(guarded.transient_insertion_workspace_bytes, 512);
        assert!(guarded.rank4_full_buckets > 0);
        assert_eq!(
            guarded.rank4_alias_bucket_rate,
            guarded.rank4_alias_buckets as f64 / guarded.rank4_full_buckets as f64
        );
    }

    #[test]
    fn build_counters_regression() {
        let expected = [
            (Policy::Random, 17.884_146, 100.92, 37.768_293, 18.884_146),
            (
                Policy::BetterChoice,
                15.957_317,
                113.79,
                33.914_634,
                16.957_317,
            ),
            (Policy::Rotor, 5.378_049, 30.11, 12.756_098, 6.378_049),
            (Policy::Lsa, 1.884_146, 8.37, 16.5, 2.884_146),
            (Policy::CavityScan, 4.487_805, 25.0, 19.951_220, 5.487_805),
            (Policy::CavityBit, 1.859_756, 9.0, 11.469_512, 2.859_756),
            (Policy::CavityRank4, 1.878_049, 9.37, 11.560_976, 2.878_049),
            (Policy::CavityD4, 1.878_049, 9.37, 11.560_976, 2.878_049),
            (Policy::Bfs, 1.292_683, 3.0, 23.969_512, 2.292_683),
        ];
        for (policy, relocations, p99, reads, writes) in expected {
            let row = run_build(BuildRunConfig {
                bucket_count: 4_096,
                policy,
                seed: 0,
                target_load: 0.95,
                window_width: 0.01,
                max_kicks: 5_000,
                bfs_depth: 10,
                path: Default::default(),
                verify: true,
            })
            .unwrap();
            assert!(row.reached);
            assert_eq!(row.inserted, 15_564);
            assert_eq!(row.window_ops, 164);
            assert_close(row.relocations.mean, relocations);
            assert_close(row.relocations.p99, p99);
            assert_close(row.reads.mean, reads);
            assert_close(row.writes.mean, writes);
        }
    }

    #[test]
    fn churn_counters_regression() {
        let expected = [
            (Policy::Random, 41.151_4, 221.02, 84.302_8, 42.151_4),
            (Policy::BetterChoice, 38.499_2, 204.01, 78.998_4, 39.499_2),
            (Policy::Rotor, 25.040_8, 130.0, 52.081_6, 26.040_8),
            (Policy::EvictionLabel, 6.233_2, 32.0, 39.399_2, 7.233_2),
            (Policy::Lsa, 5.177_2, 26.0, 36.759_2, 6.177_2),
            (Policy::CavityBit, 8.063_2, 43.0, 36.921_2, 9.063_2),
            (Policy::CavityRank4, 7.380_8, 43.0, 34.188_4, 8.380_8),
            (Policy::CavityD4, 6.131_6, 32.0, 29.170_8, 7.131_6),
            (Policy::Bfs, 1.767_2, 4.0, 47.477_8, 2.767_2),
        ];
        for (policy, relocations, p99, reads, writes) in expected {
            let row = run_churn(ChurnRunConfig {
                bucket_count: 4_096,
                policy,
                seed: 0,
                load: 0.97,
                operations: 5_000,
                max_kicks: 5_000,
                bfs_depth: 10,
                path: Default::default(),
                verify_samples: 20_000,
            })
            .unwrap();
            assert!(row.reached);
            assert_eq!(row.completed, 5_000);
            assert_eq!(row.false_negatives, 0);
            assert_close(row.relocations.mean, relocations);
            assert_close(row.relocations.p99, p99);
            assert_close(row.insertion_reads.mean, reads);
            assert_close(row.insertion_writes.mean, writes);
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000_000_5,
            "{actual} != {expected}"
        );
    }
}

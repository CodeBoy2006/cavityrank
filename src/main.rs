use cavity_bit_filter::{
    PathConfig, Policy,
    experiment::{
        BuildRow, BuildRunConfig, ChurnRow, ChurnRunConfig, CorrectnessRow, CorrectnessRunConfig,
        LatencyRow, LatencyRunConfig, QueryRow, QueryRunConfig, run_build, run_churn,
        run_correctness, run_latency, run_queries,
    },
};
use std::{
    collections::HashMap,
    env, fmt,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
};

const MAIN_POLICIES: &[Policy] = &[
    Policy::Random,
    Policy::BetterChoice,
    Policy::Rotor,
    Policy::Lsa,
    Policy::CavityBit,
    Policy::CavityRank4,
    Policy::CavityRank4Path,
    Policy::CavityD4,
    Policy::Bfs,
];
const CHURN_POLICIES: &[Policy] = &[
    Policy::Random,
    Policy::BetterChoice,
    Policy::Rotor,
    Policy::EvictionLabel,
    Policy::Lsa,
    Policy::CavityBit,
    Policy::CavityRank4,
    Policy::CavityRank4Path,
    Policy::CavityD4,
    Policy::Bfs,
];
const CORRECTNESS_POLICIES: &[Policy] = &[
    Policy::Random,
    Policy::BetterChoice,
    Policy::Rotor,
    Policy::EvictionLabel,
    Policy::Lsa,
    Policy::CavityScan,
    Policy::CavityBit,
    Policy::CavityRank4,
    Policy::CavityRank4Path,
    Policy::DenseCavityRank4,
    Policy::CavityD4,
    Policy::Bfs,
];

const USAGE: &str = r#"CavityBit Filter experiment runner

Usage:
  cavity-bench build [options]
  cavity-bench churn [options]
  cavity-bench query [options]
  cavity-bench correctness [options]
  cavity-bench latency [options]

Common options:
  --buckets N[,N...]       powers of two (default: 524288)
  --seed-start N           first absolute seed (default: 0)
  --seeds N                seeds per configuration (default: 6)
  --policies P[,P...]      random,better_choice,rotor_queue,eviction_label,lsa,cavity_scan,cavity_bit,cavity_rank4,cavity_rank4_path,dense_cr4,cavity_d4,bfs
  --max-kicks N            relocation bound (default: 5000)
  --bfs-depth N            BFS depth bound (default: 10)
  --path-bytes N           CR4-Path sketch bytes: 512 or 2048 (default: 2048)
  --path-activation RULE   after:N, no_descent, rank4_plateau (default: after:128)
  --path-reset MODE        full, sparse, generational (default: full)
  --output PATH            CSV destination

Build options:
  --loads F[,F...]         target loads (default: 0.90,0.95,0.97)
  --window F               measured final load window (default: 0.01)
  --verify true|false      sample every seed for false negatives (default: true)

Churn options:
  --load F                 load (default: 0.97)
  --operations N           delete-insert cycles per run (default: 500000)
  --verify-samples N       active-key samples (default: 200000)

Query options:
  --load F                 load (default: 0.97)
  --queries N              absent-key queries per run (default: 1000000)
  --verify-samples N       inserted-key samples (default: 200000)

Correctness options:
  --buckets N[,N...]       small powers of two (default: 256)
  --seeds N                seeds per configuration (default: 1)
  --load F                 load (default: 0.90)

Latency options:
  --loads F[,F...]         target loads (default: 0.90,0.95,0.97)
  --window F               timed final load window (default: 0.01)
  --samples-output DIR     raw successful latencies as chronological u64-le sidecars
"#;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        print!("{USAGE}");
        return Ok(());
    };
    if command == "help" || command == "--help" || command == "-h" {
        print!("{USAGE}");
        return Ok(());
    }
    let options = parse_options(arguments)?;
    match command.as_str() {
        "build" => run_build_matrix(&options),
        "churn" => run_churn_matrix(&options),
        "query" => run_query_matrix(&options),
        "correctness" => run_correctness_matrix(&options),
        "latency" => run_latency_matrix(&options),
        _ => Err(format!("unknown command: {command}\n\n{USAGE}")),
    }
}

fn run_build_matrix(options: &HashMap<String, String>) -> Result<(), String> {
    reject_unknown(
        options,
        &[
            "buckets",
            "seed-start",
            "seeds",
            "policies",
            "max-kicks",
            "bfs-depth",
            "path-bytes",
            "path-activation",
            "path-reset",
            "output",
            "loads",
            "window",
            "verify",
        ],
    )?;
    let buckets = list(options, "buckets", "524288")?;
    let seeds = seed_range(options, 6)?;
    let policies = policies(options, MAIN_POLICIES)?;
    let max_kicks = value(options, "max-kicks", 5_000_u32)?;
    let bfs_depth = value(options, "bfs-depth", 10_u8)?;
    let path_config = path_config(options)?;
    let loads = list(options, "loads", "0.90,0.95,0.97")?;
    let window = value(options, "window", 0.01_f64)?;
    let verify = value(options, "verify", true)?;
    let output = path(options, "output", "results/build.csv");
    let mut writer = csv_writer(&output, BuildRow::CSV_HEADER)?;

    for bucket_count in buckets {
        for target_load in &loads {
            // Run same-seed policies back-to-back to reduce drift in paired timings.
            for seed in seeds.clone() {
                for &policy in &policies {
                    let row = run_build(BuildRunConfig {
                        bucket_count,
                        policy,
                        seed,
                        target_load: *target_load,
                        window_width: window,
                        max_kicks,
                        bfs_depth,
                        path: path_config,
                        verify,
                    })
                    .map_err(|error| error.to_string())?;
                    emit(&mut writer, &row.to_csv())?;
                }
            }
        }
    }
    Ok(())
}

fn run_churn_matrix(options: &HashMap<String, String>) -> Result<(), String> {
    reject_unknown(
        options,
        &[
            "buckets",
            "seed-start",
            "seeds",
            "policies",
            "max-kicks",
            "bfs-depth",
            "path-bytes",
            "path-activation",
            "path-reset",
            "output",
            "load",
            "operations",
            "verify-samples",
        ],
    )?;
    let buckets = list(options, "buckets", "524288")?;
    let seeds = seed_range(options, 6)?;
    let policies = policies(options, CHURN_POLICIES)?;
    let max_kicks = value(options, "max-kicks", 5_000_u32)?;
    let bfs_depth = value(options, "bfs-depth", 10_u8)?;
    let path_config = path_config(options)?;
    let load = value(options, "load", 0.97_f64)?;
    let operations = value(options, "operations", 500_000_usize)?;
    let verify_samples = value(options, "verify-samples", 200_000_usize)?;
    let output = path(options, "output", "results/churn.csv");
    let mut writer = csv_writer(&output, ChurnRow::CSV_HEADER)?;

    for bucket_count in buckets {
        for seed in seeds.clone() {
            for &policy in &policies {
                let row = run_churn(ChurnRunConfig {
                    bucket_count,
                    policy,
                    seed,
                    load,
                    operations,
                    max_kicks,
                    bfs_depth,
                    path: path_config,
                    verify_samples,
                })
                .map_err(|error| error.to_string())?;
                emit(&mut writer, &row.to_csv())?;
            }
        }
    }
    Ok(())
}

fn run_query_matrix(options: &HashMap<String, String>) -> Result<(), String> {
    reject_unknown(
        options,
        &[
            "buckets",
            "seed-start",
            "seeds",
            "policies",
            "max-kicks",
            "bfs-depth",
            "path-bytes",
            "path-activation",
            "path-reset",
            "output",
            "load",
            "queries",
            "verify-samples",
        ],
    )?;
    let buckets = list(options, "buckets", "524288")?;
    let seeds = seed_range(options, 6)?;
    let policies = policies(options, MAIN_POLICIES)?;
    let max_kicks = value(options, "max-kicks", 5_000_u32)?;
    let bfs_depth = value(options, "bfs-depth", 10_u8)?;
    let path_config = path_config(options)?;
    let load = value(options, "load", 0.97_f64)?;
    let queries = value(options, "queries", 1_000_000_usize)?;
    let verify_samples = value(options, "verify-samples", 200_000_usize)?;
    let output = path(options, "output", "results/query.csv");
    let mut writer = csv_writer(&output, QueryRow::CSV_HEADER)?;

    for bucket_count in buckets {
        for seed in seeds.clone() {
            for &policy in &policies {
                let row = run_queries(QueryRunConfig {
                    bucket_count,
                    policy,
                    seed,
                    load,
                    queries,
                    max_kicks,
                    bfs_depth,
                    path: path_config,
                    verify_samples,
                })
                .map_err(|error| error.to_string())?;
                emit(&mut writer, &row.to_csv())?;
            }
        }
    }
    Ok(())
}

fn run_correctness_matrix(options: &HashMap<String, String>) -> Result<(), String> {
    reject_unknown(
        options,
        &[
            "buckets",
            "seed-start",
            "seeds",
            "policies",
            "max-kicks",
            "bfs-depth",
            "path-bytes",
            "path-activation",
            "path-reset",
            "output",
            "load",
        ],
    )?;
    let buckets = list(options, "buckets", "256")?;
    let seeds = seed_range(options, 1)?;
    let policies = policies(options, CORRECTNESS_POLICIES)?;
    let max_kicks = value(options, "max-kicks", 5_000_u32)?;
    let bfs_depth = value(options, "bfs-depth", 10_u8)?;
    let path_config = path_config(options)?;
    let load = value(options, "load", 0.90_f64)?;
    let output = path(options, "output", "results/correctness.csv");
    let mut writer = csv_writer(&output, CorrectnessRow::CSV_HEADER)?;

    for bucket_count in buckets {
        for seed in seeds.clone() {
            for &policy in &policies {
                let row = run_correctness(CorrectnessRunConfig {
                    bucket_count,
                    policy,
                    seed,
                    load,
                    max_kicks,
                    bfs_depth,
                    path: path_config,
                })
                .map_err(|error| error.to_string())?;
                emit(&mut writer, &row.to_csv())?;
            }
        }
    }
    Ok(())
}

fn run_latency_matrix(options: &HashMap<String, String>) -> Result<(), String> {
    reject_unknown(
        options,
        &[
            "buckets",
            "seed-start",
            "seeds",
            "policies",
            "max-kicks",
            "bfs-depth",
            "path-bytes",
            "path-activation",
            "path-reset",
            "output",
            "loads",
            "window",
            "samples-output",
        ],
    )?;
    let buckets = list(options, "buckets", "524288")?;
    let seeds = seed_range(options, 6)?;
    let policies = policies(options, MAIN_POLICIES)?;
    let max_kicks = value(options, "max-kicks", 5_000_u32)?;
    let bfs_depth = value(options, "bfs-depth", 10_u8)?;
    let path_config = path_config(options)?;
    let loads = list(options, "loads", "0.90,0.95,0.97")?;
    let window = value(options, "window", 0.01_f64)?;
    let output = path(options, "output", "results/latency.csv");
    let samples_output = options.get("samples-output").map(PathBuf::from);
    let mut writer = csv_writer(&output, LatencyRow::CSV_HEADER)?;

    for bucket_count in buckets {
        for target_load in &loads {
            for seed in seeds.clone() {
                for &policy in &policies {
                    let mut row = run_latency(LatencyRunConfig {
                        bucket_count,
                        policy,
                        seed,
                        target_load: *target_load,
                        window_width: window,
                        max_kicks,
                        bfs_depth,
                        path: path_config,
                    })
                    .map_err(|error| error.to_string())?;
                    if let Some(directory) = &samples_output {
                        row.latency_samples_file = write_latency_samples(directory, &row)?;
                    }
                    emit(&mut writer, &row.to_csv())?;
                }
            }
        }
    }
    Ok(())
}

fn parse_options(
    arguments: impl Iterator<Item = String>,
) -> Result<HashMap<String, String>, String> {
    let arguments: Vec<_> = arguments.collect();
    if arguments.iter().any(|argument| argument == "--help") {
        print!("{USAGE}");
        std::process::exit(0);
    }
    if arguments.len() % 2 != 0 {
        return Err("every option needs a value".to_owned());
    }
    let mut options = HashMap::new();
    for pair in arguments.chunks_exact(2) {
        let Some(key) = pair[0].strip_prefix("--") else {
            return Err(format!("expected --option, got {}", pair[0]));
        };
        if options.insert(key.to_owned(), pair[1].clone()).is_some() {
            return Err(format!("duplicate option: --{key}"));
        }
    }
    Ok(options)
}

fn reject_unknown(options: &HashMap<String, String>, allowed: &[&str]) -> Result<(), String> {
    if let Some(key) = options.keys().find(|key| !allowed.contains(&key.as_str())) {
        Err(format!("unknown option: --{key}"))
    } else {
        Ok(())
    }
}

fn value<T>(options: &HashMap<String, String>, key: &str, default: T) -> Result<T, String>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    options.get(key).map_or(Ok(default), |raw| {
        raw.parse()
            .map_err(|error| format!("invalid --{key}: {error}"))
    })
}

fn seed_range(
    options: &HashMap<String, String>,
    default_count: u64,
) -> Result<std::ops::Range<u64>, String> {
    let start = value(options, "seed-start", 0_u64)?;
    let count = value(options, "seeds", default_count)?;
    if count == 0 {
        return Err("--seeds must be greater than zero".to_owned());
    }
    let end = start
        .checked_add(count)
        .ok_or_else(|| "--seed-start plus --seeds overflows u64".to_owned())?;
    Ok(start..end)
}

fn list<T>(options: &HashMap<String, String>, key: &str, default: &str) -> Result<Vec<T>, String>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    options
        .get(key)
        .map_or(default, String::as_str)
        .split(',')
        .map(|raw| {
            raw.parse()
                .map_err(|error| format!("invalid --{key} value {raw}: {error}"))
        })
        .collect()
}

fn policies(options: &HashMap<String, String>, defaults: &[Policy]) -> Result<Vec<Policy>, String> {
    options.get("policies").map_or_else(
        || Ok(defaults.to_vec()),
        |raw| raw.split(',').map(str::parse).collect(),
    )
}

fn path_config(options: &HashMap<String, String>) -> Result<PathConfig, String> {
    let defaults = PathConfig::default();
    let config = PathConfig {
        bytes: value(options, "path-bytes", defaults.bytes)?,
        activation: value(options, "path-activation", defaults.activation)?,
        reset: value(options, "path-reset", defaults.reset)?,
    };
    if !matches!(config.bytes, 512 | 2_048) {
        return Err("invalid --path-bytes: expected 512 or 2048".to_owned());
    }
    Ok(config)
}

fn path(options: &HashMap<String, String>, key: &str, default: &str) -> PathBuf {
    options
        .get(key)
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn csv_writer(path: &Path, header: &str) -> Result<BufWriter<File>, String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writeln!(writer, "{header}").map_err(|error| error.to_string())?;
    println!("{header}");
    Ok(writer)
}

fn emit(writer: &mut BufWriter<File>, row: &str) -> Result<(), String> {
    writeln!(writer, "{row}").map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())?;
    println!("{row}");
    Ok(())
}

const LATENCY_SIDECAR_MAGIC: &[u8; 8] = b"CBLAT001";

fn latency_sample_file_name(row: &LatencyRow) -> String {
    let load_ppm = (row.target_load * 1_000_000.0).round() as u64;
    let window_ppm = (row.window_width * 1_000_000.0).round() as u64;
    format!(
        "b{}-l{}-w{}-s{}-{}.u64le",
        row.bucket_count, load_ppm, window_ppm, row.seed, row.policy
    )
}

fn write_latency_stream(writer: &mut impl Write, samples: &[u64]) -> std::io::Result<()> {
    writer.write_all(LATENCY_SIDECAR_MAGIC)?;
    writer.write_all(&(samples.len() as u64).to_le_bytes())?;
    for &sample in samples {
        writer.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

fn write_latency_samples(directory: &Path, row: &LatencyRow) -> Result<String, String> {
    if row.latency_samples_ns.len() != row.latency_samples {
        return Err("latency sample count mismatch".to_owned());
    }
    fs::create_dir_all(directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let name = latency_sample_file_name(row);
    let final_path = directory.join(&name);
    let temporary_path = directory.join(format!("{name}.tmp"));
    if final_path.exists() || temporary_path.exists() {
        return Err(format!(
            "refusing to overwrite latency sidecar: {}",
            final_path.display()
        ));
    }
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .map_err(|error| format!("cannot create {}: {error}", temporary_path.display()))?;
    let mut writer = BufWriter::new(file);
    write_latency_stream(&mut writer, &row.latency_samples_ns)
        .and_then(|()| writer.flush())
        .map_err(|error| format!("cannot write {}: {error}", temporary_path.display()))?;
    fs::rename(&temporary_path, &final_path).map_err(|error| {
        format!(
            "cannot rename {} to {}: {error}",
            temporary_path.display(),
            final_path.display()
        )
    })?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::{
        LATENCY_SIDECAR_MAGIC, csv_writer, parse_options, seed_range, write_latency_stream,
    };
    use std::{env, fs, process};

    #[test]
    fn seed_range_uses_absolute_start_and_rejects_empty_ranges() {
        let options = parse_options(
            ["--seed-start", "41", "--seeds", "2"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(seed_range(&options, 6).unwrap(), 41..43);

        let empty = parse_options(["--seeds", "0"].into_iter().map(str::to_owned)).unwrap();
        assert_eq!(
            seed_range(&empty, 6).unwrap_err(),
            "--seeds must be greater than zero"
        );
    }

    #[test]
    fn latency_sidecar_stream_is_counted_u64_little_endian() {
        let mut bytes = Vec::new();
        write_latency_stream(&mut bytes, &[7, 0x0102_0304_0506_0708]).unwrap();
        assert_eq!(&bytes[..8], LATENCY_SIDECAR_MAGIC);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 2);
        assert_eq!(u64::from_le_bytes(bytes[16..24].try_into().unwrap()), 7);
        assert_eq!(
            u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            0x0102_0304_0506_0708
        );
    }

    #[test]
    fn csv_writer_refuses_to_overwrite_existing_output() {
        let path = env::temp_dir().join(format!("cavity-bench-{}.csv", process::id()));
        let _ = fs::remove_file(&path);
        fs::write(&path, "sentinel\n").unwrap();

        assert!(csv_writer(&path, "header").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "sentinel\n");

        fs::remove_file(path).unwrap();
    }
}

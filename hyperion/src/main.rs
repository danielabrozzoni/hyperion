use std::path::PathBuf;
use std::str::FromStr;

use clap::Parser;
use rand::{rng, RngCore};
use serde::Serialize;
use simple_logger::SimpleLogger;

use hyper_lib::simulator::Simulator;
use hyperion::cli::Cli;

#[derive(Serialize)]
struct DayRow {
    day: u64,
    avg_addrman_size: f64,
    address_coverage: f64,
    stale_7d: usize,
    stale_30d: usize,
    stale_departed: usize,
    fingerprint_pairs: usize,
    fingerprint_fpr: f64,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let log_level = cli.log_level();

    SimpleLogger::new()
        .with_level(log::LevelFilter::Warn)
        .with_module_level("hyper_lib", log_level)
        .with_module_level("hyperion", log_level)
        .init()
        .unwrap();

    let seed = cli.seed.unwrap_or_else(|| rng().next_u64());
    log::info!("RNG seed: {seed}");

    let output_file = cli.output_file.clone();
    let config = cli.into_config();

    log::info!("Building network and running simulation...");
    let mut simulator = Simulator::new(config, seed);
    simulator.run();

    let stats = &simulator.stats;
    let days = stats.avg_addrman_size.len();

    let rows: Vec<DayRow> = (0..days)
        .map(|i| DayRow {
            day: stats.staleness_per_day[i].day,
            avg_addrman_size: stats.avg_addrman_size[i],
            address_coverage: stats.address_coverage[i],
            stale_7d: stats.staleness_per_day[i].addresses_older_than_7_days,
            stale_30d: stats.staleness_per_day[i].addresses_older_than_30_days,
            stale_departed: stats.staleness_per_day[i].addresses_of_departed_nodes,
            fingerprint_pairs: stats.fingerprint_results[i].node_pairs_same_fingerprint,
            fingerprint_fpr: stats.fingerprint_results[i].false_positive_rate,
        })
        .collect();

    for row in &rows {
        log::info!(
            "Day {:>2}: addrman_avg={:.1}  coverage={:.4}  stale_7d={}  stale_30d={}  \
             departed={}  fp_pairs={}  fp_rate={:.6}",
            row.day,
            row.avg_addrman_size,
            row.address_coverage,
            row.stale_7d,
            row.stale_30d,
            row.stale_departed,
            row.fingerprint_pairs,
            row.fingerprint_fpr,
        );
    }

    if let Some(of) = output_file {
        let mut path = PathBuf::from_str(&of)?;
        if let Some(rest) = of.strip_prefix("~/") {
            path = home::home_dir().unwrap().join(rest);
        }
        log::info!("Writing results to {}", path.display());
        let mut wtr = csv::WriterBuilder::new()
            .has_headers(!std::fs::exists(&path)?)
            .from_writer(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?,
            );
        for row in rows {
            wtr.serialize(row)?;
        }
        wtr.flush()?;
    }

    Ok(())
}

use std::path::PathBuf;
use std::str::FromStr;

use clap::Parser;
use rand::{rng, RngCore};
use serde::Serialize;
use simple_logger::SimpleLogger;

use hyper_lib::simulator::Simulator;
use hyperion::cli::Cli;
use hyperion::tui;

#[derive(Serialize)]
struct DayRow {
    day: u64,
    avg_addrman_size: f64,
    avg_addrman_live: f64,
    address_coverage: f64,
    avg_stale_7d: f64,
    avg_stale_30d: f64,
    avg_departed: f64,
    avg_departed_fresh: f64,
    fp_dual_stack_nodes: usize,
    fp_avg_overlap: f64,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = cli.log_level();

    SimpleLogger::new()
        .with_level(log::LevelFilter::Warn)
        .with_module_level("hyper_lib", level)
        .with_module_level("hyperion", level)
        .init()
        .unwrap();

    let seed = cli.seed.unwrap_or_else(|| rng().next_u64());
    log::info!("RNG seed: {seed}");

    let interactive = cli.interactive;
    let output_file = cli.output_file.clone();
    let config = cli.into_config();

    if interactive {
        let simulator = Simulator::new(config, seed);
        return tui::run(simulator);
    }

    log::info!("Building network and running simulation...");
    let mut simulator = Simulator::new(config, seed);
    simulator.run();

    let stats = &simulator.stats;
    let days = stats.avg_addrman_size.len();

    let rows: Vec<DayRow> = (0..days)
        .map(|i| DayRow {
            day: stats.staleness_per_day[i].day,
            avg_addrman_size: stats.avg_addrman_size[i],
            avg_addrman_live: stats.avg_addrman_live[i],
            address_coverage: stats.address_coverage[i],
            avg_stale_7d: stats.staleness_per_day[i].avg_older_than_7_days,
            avg_stale_30d: stats.staleness_per_day[i].avg_older_than_30_days,
            avg_departed: stats.staleness_per_day[i].avg_departed,
            avg_departed_fresh: stats.staleness_per_day[i].avg_departed_fresh,
            fp_dual_stack_nodes: stats.fingerprint_results[i].nodes_sampled,
            fp_avg_overlap: stats.fingerprint_results[i].avg_overlap,
        })
        .collect();

    if let Some(of) = output_file {
        let mut path = PathBuf::from_str(&of)?;
        if let Some(rest) = of.strip_prefix("~/") {
            path = home::home_dir().expect("$HOME is not set").join(rest);
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

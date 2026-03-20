use clap::{Parser, ValueEnum};
use log::LevelFilter;

use hyper_lib::node::GetaddrCacheAlgorithm;
use hyper_lib::SimulationConfig;

#[derive(Clone, ValueEnum)]
pub enum CacheAlgo {
    Current,
    FixedOffset,
    NetworkBased,
}

impl From<CacheAlgo> for GetaddrCacheAlgorithm {
    fn from(a: CacheAlgo) -> Self {
        match a {
            CacheAlgo::Current => GetaddrCacheAlgorithm::Current,
            CacheAlgo::FixedOffset => GetaddrCacheAlgorithm::FixedOffset,
            CacheAlgo::NetworkBased => GetaddrCacheAlgorithm::NetworkBased,
        }
    }
}

#[derive(Parser)]
#[command(name = "hyperion-addr", version, about = "Bitcoin P2P address relay simulator")]
pub struct Cli {
    /// Onion-only nodes
    #[clap(long, default_value_t = 1000)]
    pub onion: usize,

    /// Clearnet-only nodes
    #[clap(long, default_value_t = 8000)]
    pub clearnet: usize,

    /// Dual-stack nodes (one address per network)
    #[clap(long, default_value_t = 1000)]
    pub dual_stack: usize,

    /// Percentage of clearnet addresses accepting inbound connections
    #[clap(long, default_value_t = 15)]
    pub reachable_clearnet_pct: u8,

    /// Percentage of onion addresses accepting inbound connections
    #[clap(long, default_value_t = 50)]
    pub reachable_onion_pct: u8,

    /// Outbound connections per node
    #[clap(long, default_value_t = 8)]
    pub outbounds: usize,

    /// Days to simulate
    #[clap(long, default_value_t = 30)]
    pub days: u64,

    /// Nodes joining per day
    #[clap(long, default_value_t = 100)]
    pub joins_per_day: usize,

    /// Nodes leaving per day
    #[clap(long, default_value_t = 100)]
    pub leaves_per_day: usize,

    /// Start with empty addrmans (default is warm-start: pre-populated)
    #[clap(long)]
    pub cold_start: bool,

    /// GETADDR cache timestamp algorithm
    #[clap(long, default_value = "current")]
    pub cache_algo: CacheAlgo,

    /// CSV output file
    #[clap(long)]
    pub output_file: Option<String>,

    /// Verbose output (debug log level)
    #[clap(long, short)]
    pub verbose: bool,

    /// RNG seed for reproducibility
    #[clap(long, short)]
    pub seed: Option<u64>,
}

impl Cli {
    pub fn log_level(&self) -> LevelFilter {
        if self.verbose {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        }
    }

    pub fn into_config(self) -> SimulationConfig {
        SimulationConfig {
            onion: self.onion,
            clearnet: self.clearnet,
            dual_stack: self.dual_stack,
            reachable_clearnet_pct: self.reachable_clearnet_pct,
            reachable_onion_pct: self.reachable_onion_pct,
            outbounds: self.outbounds,
            days: self.days,
            joins_per_day: self.joins_per_day,
            leaves_per_day: self.leaves_per_day,
            warm_start: !self.cold_start,
            cache_algo: self.cache_algo.into(),
        }
    }
}

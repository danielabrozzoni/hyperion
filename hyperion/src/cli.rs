use clap::{Parser, ValueEnum};
use log::LevelFilter;

use hyper_lib::node::GetaddrCacheAlgorithm;
use hyper_lib::{SimulationConfig, StartMode};

#[derive(Clone, ValueEnum)]
pub enum LogLevel {
    Info,
    Debug,
    Trace,
}

#[derive(Clone, ValueEnum)]
pub enum CliStartMode {
    /// Pre-populate every node's addrman with all network addresses.
    Warm,
    /// Start with empty addrmans; 30-day burn-in applied by default.
    Cold,
    /// Start with only directly connected peers' addresses in each addrman.
    Peers,
    /// Start with a random sample of network addresses, timestamps 3–7 days old (DNS seed model).
    Dns,
}

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
    #[clap(long, default_value_t = 60)]
    pub days: u64,

    /// Nodes joining per day
    #[clap(long, default_value_t = 100)]
    pub joins_per_day: usize,

    /// Nodes leaving per day
    #[clap(long, default_value_t = 100)]
    pub leaves_per_day: usize,

    /// Addrman initialisation: warm (all addresses), cold (empty), peers (connected peers only)
    #[clap(long, default_value = "warm")]
    pub start: CliStartMode,

    /// Days to skip before recording statistics (default: 30 for cold, 0 otherwise)
    #[clap(long)]
    pub burn_in: Option<u64>,

    /// Percentage of network addresses seeded into each addrman for --start dns (default: 1)
    #[clap(long, default_value_t = 1)]
    pub seed_sample_pct: u8,

    /// GETADDR cache timestamp algorithm
    #[clap(long, default_value = "current")]
    pub cache_algo: CacheAlgo,

    /// CSV output file
    #[clap(long)]
    pub output_file: Option<String>,

    /// Log level: info (default), debug (protocol/topology events), trace (every event + relay detail)
    #[clap(long, default_value = "info")]
    pub log_level: LogLevel,

    /// RNG seed for reproducibility
    #[clap(long, short)]
    pub seed: Option<u64>,

    /// Launch interactive TUI instead of running the full simulation
    #[clap(long, short = 'i', conflicts_with = "output_file")]
    pub interactive: bool,
}

impl Cli {
    pub fn log_level(&self) -> LevelFilter {
        match self.log_level {
            LogLevel::Info => LevelFilter::Info,
            LogLevel::Debug => LevelFilter::Debug,
            LogLevel::Trace => LevelFilter::Trace,
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
            start_mode: match self.start {
                CliStartMode::Warm => StartMode::Warm,
                CliStartMode::Cold => StartMode::Cold,
                CliStartMode::Peers => StartMode::Peers,
                CliStartMode::Dns => StartMode::Dns,
            },
            dns_sample_pct: self.seed_sample_pct,
            burn_in_days: self.burn_in.unwrap_or(match self.start {
                CliStartMode::Cold => 30,
                _ => 0,
            }),
            cache_algo: self.cache_algo.into(),
        }
    }
}

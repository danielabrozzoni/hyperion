pub mod address;
pub mod addrman;
pub mod fingerprint;
pub mod network;
pub mod node;
pub mod simulator;
pub mod statistics;

use crate::node::GetaddrCacheAlgorithm;

#[derive(Clone, Copy)]
pub enum StartMode {
    /// Every node's addrman is pre-populated with all addresses in the network.
    Warm,
    /// Every node's addrman starts empty.
    Cold,
    /// Every node's addrman starts with only its directly connected peers' addresses.
    Peers,
    /// Every node's addrman starts with a random sample of the network, with timestamps
    /// 3–7 days old, mirroring what Bitcoin Core nodes receive from DNS seeds at bootstrap.
    Dns,
}

pub struct SimulationConfig {
    pub onion: usize,
    pub clearnet: usize,
    pub dual_stack: usize,
    pub reachable_clearnet_pct: u8,
    pub reachable_onion_pct: u8,
    pub outbounds: usize,
    pub days: u64,
    pub burn_in_days: u64,
    pub joins_per_day: usize,
    pub leaves_per_day: usize,
    pub start_mode: StartMode,
    /// Fraction of network addresses seeded into each addrman for `StartMode::Dns` (0–100).
    pub dns_sample_pct: u8,
    pub cache_algo: GetaddrCacheAlgorithm,
}

pub mod address;
pub mod addrman;
pub mod fingerprint;
pub mod network;
pub mod node;
pub mod simulator;
pub mod statistics;

use crate::node::GetaddrCacheAlgorithm;

pub struct SimulationConfig {
    pub onion: usize,
    pub clearnet: usize,
    pub dual_stack: usize,
    pub reachable_clearnet_pct: u8,
    pub reachable_onion_pct: u8,
    pub outbounds: usize,
    pub days: u64,
    pub joins_per_day: usize,
    pub leaves_per_day: usize,
    pub warm_start: bool,
    pub cache_algo: GetaddrCacheAlgorithm,
}

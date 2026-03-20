/// Per-node message counters for address relay.
#[derive(Default)]
pub struct NodeStatistics {
    pub getaddr_sent: u64,
    pub getaddr_received: u64,
    pub addr_sent: u64,
    pub addr_received: u64,
    pub addr_announce_sent: u64,
    pub addr_announce_received: u64,
}

/// Simulation-wide statistics collected once per simulated day.
pub struct SimulationStatistics {
    pub fingerprint_results: Vec<FingerprintResult>,
    pub staleness_per_day: Vec<StaleAddressStats>,
    pub avg_addrman_size: Vec<f64>,
    pub address_coverage: Vec<f64>,
}

pub struct FingerprintResult {
    pub day: u64,
    pub node_pairs_same_fingerprint: usize,
    pub false_positive_rate: f64,
}

pub struct StaleAddressStats {
    pub day: u64,
    pub addresses_older_than_7_days: usize,
    pub addresses_older_than_30_days: usize,
    pub addresses_of_departed_nodes: usize,
}

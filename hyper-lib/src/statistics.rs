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
    /// Average fraction of (addr, ts) pairs shared between a dual-stack node's
    /// clearnet and onion GETADDR caches. High overlap means an attacker can
    /// link the node's clearnet and Tor identities by comparing responses.
    pub avg_overlap: f64,
    /// Number of dual-stack nodes that had both caches populated this day.
    pub nodes_sampled: usize,
}

pub struct StaleAddressStats {
    pub day: u64,
    pub addresses_older_than_7_days: usize,
    pub addresses_older_than_30_days: usize,
    pub addresses_of_departed_nodes: usize,
}

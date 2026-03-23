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

pub struct ChurnStats {
    pub joined_onion: usize,
    pub joined_clearnet: usize,
    pub joined_dual: usize,
    pub left_onion: usize,
    pub left_clearnet: usize,
    pub left_dual: usize,
    pub total_onion: usize,
    pub total_clearnet: usize,
    pub total_dual: usize,
}

/// Simulation-wide statistics collected once per simulated day.
pub struct SimulationStatistics {
    pub fingerprint_results: Vec<FingerprintResult>,
    pub staleness_per_day: Vec<StaleAddressStats>,
    pub churn_per_day: Vec<ChurnStats>,
    pub avg_addrman_size: Vec<f64>,
    /// Average number of non-terrible addrman entries per node (visible to the protocol).
    pub avg_addrman_live: Vec<f64>,
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
    /// Average number of addrman entries per node with timestamp older than 7 days.
    pub avg_older_than_7_days: f64,
    /// Average number of addrman entries per node with timestamp older than 30 days.
    pub avg_older_than_30_days: f64,
    /// Average number of addrman entries per node pointing to departed nodes (any timestamp).
    pub avg_departed: f64,
    /// Average number of addrman entries per node pointing to departed nodes that are still
    /// fresh (not terrible) — these are the ones that cause live connection attempts to fail.
    pub avg_departed_fresh: f64,
}

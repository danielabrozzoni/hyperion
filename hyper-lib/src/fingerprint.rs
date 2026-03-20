use std::collections::HashMap;

use crate::address::{AddressId, NetworkType};
use crate::node::{AddrPayload, NodeId};
use crate::statistics::FingerprintResult;

pub struct FingerprintAnalyzer {
    clearnet: HashMap<NodeId, Vec<(AddressId, u64)>>,
    onion: HashMap<NodeId, Vec<(AddressId, u64)>>,
}

impl FingerprintAnalyzer {
    pub fn new() -> Self {
        FingerprintAnalyzer {
            clearnet: HashMap::new(),
            onion: HashMap::new(),
        }
    }

    /// Record a node's GETADDR cache for a given network.
    pub fn record(&mut self, node_id: NodeId, network: NetworkType, cache: &[AddrPayload]) {
        let mut pairs: Vec<_> = cache.iter().map(|p| (p.address, p.timestamp)).collect();
        pairs.sort_unstable();
        match network {
            NetworkType::Clearnet => self.clearnet.insert(node_id, pairs),
            NetworkType::Onion => self.onion.insert(node_id, pairs),
        };
    }

    /// For each dual-stack node (one with both a clearnet and onion cache),
    /// compute what fraction of (addr, ts) pairs appear in both responses.
    /// Returns the average overlap across all such nodes.
    pub fn analyze(&self, day: u64) -> FingerprintResult {
        let mut total_overlap = 0.0;
        let mut nodes_sampled = 0;

        for (node_id, clearnet_cache) in &self.clearnet {
            if let Some(onion_cache) = self.onion.get(node_id) {
                total_overlap += Self::overlap(clearnet_cache, onion_cache);
                nodes_sampled += 1;
            }
        }

        let avg_overlap = if nodes_sampled > 0 {
            total_overlap / nodes_sampled as f64
        } else {
            0.0
        };

        FingerprintResult {
            day,
            avg_overlap,
            nodes_sampled,
        }
    }

    /// Fraction of pairs from `a` that also appear in `b`.
    /// Both slices must be sorted. Returns 0.0 if `a` is empty.
    pub fn overlap(a: &[(AddressId, u64)], b: &[(AddressId, u64)]) -> f64 {
        if a.is_empty() {
            return 0.0;
        }
        let mut intersection = 0usize;
        let mut ai = 0;
        let mut bi = 0;
        while ai < a.len() && bi < b.len() {
            match a[ai].cmp(&b[bi]) {
                std::cmp::Ordering::Equal => {
                    intersection += 1;
                    ai += 1;
                    bi += 1;
                }
                std::cmp::Ordering::Less => ai += 1,
                std::cmp::Ordering::Greater => bi += 1,
            }
        }
        intersection as f64 / a.len() as f64
    }
}

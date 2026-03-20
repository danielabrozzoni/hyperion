use std::collections::HashMap;

use crate::address::AddressId;
use crate::node::{AddrPayload, NodeId};
use crate::statistics::FingerprintResult;

pub struct FingerprintAnalyzer {
    /// Sorted (address, timestamp) pairs per node, collected daily.
    responses: HashMap<NodeId, Vec<(AddressId, u64)>>,
}

impl FingerprintAnalyzer {
    pub fn new() -> Self {
        FingerprintAnalyzer {
            responses: HashMap::new(),
        }
    }

    /// Record a node's GETADDR cache for today's sample.
    pub fn record(&mut self, node_id: NodeId, cache: &[AddrPayload]) {
        let mut pairs: Vec<_> = cache.iter().map(|p| (p.address, p.timestamp)).collect();
        pairs.sort_unstable();
        self.responses.insert(node_id, pairs);
    }

    /// Compare all pairs of nodes, return fingerprint collision stats.
    /// "Same fingerprint" = Jaccard similarity == 1.0 (identical caches).
    pub fn analyze(&self, day: u64) -> FingerprintResult {
        let nodes: Vec<&Vec<(AddressId, u64)>> = self.responses.values().collect();
        let n = nodes.len();
        let total_pairs = n.saturating_sub(1) * n / 2;
        let mut same_fingerprint = 0usize;

        for i in 0..n {
            for j in (i + 1)..n {
                if Self::similarity(nodes[i], nodes[j]) == 1.0 {
                    same_fingerprint += 1;
                }
            }
        }

        let false_positive_rate = if total_pairs > 0 {
            same_fingerprint as f64 / total_pairs as f64
        } else {
            0.0
        };

        FingerprintResult {
            day,
            node_pairs_same_fingerprint: same_fingerprint,
            false_positive_rate,
        }
    }

    /// Jaccard similarity over sorted (address, timestamp) pairs.
    pub fn similarity(a: &[(AddressId, u64)], b: &[(AddressId, u64)]) -> f64 {
        if a.is_empty() && b.is_empty() {
            return 1.0;
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
        let union = a.len() + b.len() - intersection;
        if union == 0 {
            1.0
        } else {
            intersection as f64 / union as f64
        }
    }
}

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};

use rand::rngs::StdRng;
use rand::seq::IteratorRandom;
use rand::{Rng, SeedableRng};

use crate::address::NetworkType;
use crate::fingerprint::FingerprintAnalyzer;
use crate::network::Network;
use crate::node::{Event, NetworkMessage};
use crate::statistics::{SimulationStatistics, StaleAddressStats};
use crate::SimulationConfig;

pub struct ScheduledEvent {
    pub inner: Event,
    time: Reverse<u64>,
}

impl ScheduledEvent {
    pub fn new(event: Event, time: u64) -> Self {
        ScheduledEvent {
            inner: event,
            time: Reverse(time),
        }
    }

    pub fn time(&self) -> u64 {
        self.time.0
    }
}

impl PartialEq for ScheduledEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

impl Eq for ScheduledEvent {}

impl Ord for ScheduledEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time.cmp(&other.time)
    }
}

impl PartialOrd for ScheduledEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct Simulator {
    pub network: Network,
    pub event_queue: BinaryHeap<ScheduledEvent>,
    pub rng: StdRng,
    pub config: SimulationConfig,
    pub stats: SimulationStatistics,
}

impl Simulator {
    pub fn new(config: SimulationConfig, seed: u64) -> Self {
        let mut sim = Self {
            network: Network::new(),
            event_queue: BinaryHeap::new(),
            rng: StdRng::seed_from_u64(seed),
            config,
            stats: SimulationStatistics {
                fingerprint_results: vec![],
                staleness_per_day: vec![],
                avg_addrman_size: vec![],
                address_coverage: vec![],
            },
        };
        sim.build_initial_network();
        sim
    }

    fn build_initial_network(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Copy all config values to locals to avoid borrow conflicts.
        let onion = self.config.onion;
        let clearnet = self.config.clearnet;
        let dual_stack = self.config.dual_stack;
        let reachable_clearnet_pct = self.config.reachable_clearnet_pct as usize;
        let reachable_onion_pct = self.config.reachable_onion_pct as usize;
        let outbounds = self.config.outbounds;
        let algo = self.config.cache_algo;
        let warm_start = self.config.warm_start;

        let clearnet_reachable = clearnet * reachable_clearnet_pct / 100;
        let onion_reachable = onion * reachable_onion_pct / 100;
        let dual_clearnet_reachable = dual_stack * reachable_clearnet_pct / 100;
        let dual_onion_reachable = dual_stack * reachable_onion_pct / 100;

        let total = onion + clearnet + dual_stack;
        log::info!(
            "Building network: {} nodes ({} onion, {} clearnet, {} dual-stack), {} outbounds each",
            total, onion, clearnet, dual_stack, outbounds
        );

        let t_build = std::time::Instant::now();

        for i in 0..onion {
            let reachable_on = if i < onion_reachable {
                [NetworkType::Onion].into()
            } else {
                HashSet::new()
            };
            let (_, events) = self.network.add_node(
                vec![NetworkType::Onion],
                reachable_on,
                outbounds,
                algo,
                now,
                &mut self.rng,
            );
            for e in events {
                self.add_event(e);
            }
        }
        log::debug!("Added {} onion-only nodes ({} reachable)", onion, onion_reachable);

        for i in 0..clearnet {
            let reachable_on = if i < clearnet_reachable {
                [NetworkType::Clearnet].into()
            } else {
                HashSet::new()
            };
            let (_, events) = self.network.add_node(
                vec![NetworkType::Clearnet],
                reachable_on,
                outbounds,
                algo,
                now,
                &mut self.rng,
            );
            for e in events {
                self.add_event(e);
            }
        }
        log::debug!("Added {} clearnet-only nodes ({} reachable)", clearnet, clearnet_reachable);

        for i in 0..dual_stack {
            let mut reachable_on = HashSet::new();
            if i < dual_onion_reachable {
                reachable_on.insert(NetworkType::Onion);
            }
            if i < dual_clearnet_reachable {
                reachable_on.insert(NetworkType::Clearnet);
            }
            let (_, events) = self.network.add_node(
                vec![NetworkType::Onion, NetworkType::Clearnet],
                reachable_on,
                outbounds,
                algo,
                now,
                &mut self.rng,
            );
            for e in events {
                self.add_event(e);
            }
        }
        log::debug!(
            "Added {} dual-stack nodes ({} onion-reachable, {} clearnet-reachable)",
            dual_stack, dual_onion_reachable, dual_clearnet_reachable
        );
        log::info!(
            "Network topology built in {:.2?}: {} nodes, {} addresses, {} queued events",
            t_build.elapsed(),
            self.network.nodes.len(),
            self.network.registry.addresses.len(),
            self.event_queue.len()
        );

        if warm_start {
            let addr_count = self.network.registry.addresses.len();
            let node_count = self.network.nodes.len();
            log::info!(
                "Warm-starting addrmans: {} inserts ({} nodes × {} addresses)...",
                node_count * addr_count,
                node_count,
                addr_count,
            );
            let t_warm = std::time::Instant::now();
            let all_addrs: Vec<_> = self
                .network
                .registry
                .addresses
                .values()
                .map(|a| a.id)
                .collect();
            for (i, node) in self.network.nodes.values_mut().enumerate() {
                for &addr in &all_addrs {
                    node.addrman.add(addr, now, 0, now);
                }
                if (i + 1) % 1000 == 0 {
                    log::debug!(
                        "  warm-start: {}/{} nodes done ({:.2?} elapsed)",
                        i + 1,
                        node_count,
                        t_warm.elapsed()
                    );
                }
            }
            log::info!("Warm-start done in {:.2?}", t_warm.elapsed());
        }
    }

    pub fn run(&mut self) {
        for day in 0..self.config.days {
            log::info!(
                "Day {}/{} — queue: {} events, nodes: {}",
                day + 1,
                self.config.days,
                self.event_queue.len(),
                self.network.nodes.len()
            );
            self.schedule_churn(day);
            self.run_until(day * 86400 + 86399);
            self.collect_statistics(day);
            let s = self.stats.staleness_per_day.last().unwrap();
            let f = self.stats.fingerprint_results.last().unwrap();
            log::debug!(
                "  addrman_avg={:.0}  stale_7d={}  stale_30d={}  departed={}  \
                 fp_overlap={:.4}  fp_nodes={}",
                self.stats.avg_addrman_size.last().unwrap(),
                s.addresses_older_than_7_days,
                s.addresses_older_than_30_days,
                s.addresses_of_departed_nodes,
                f.avg_overlap,
                f.nodes_sampled,
            );
        }
    }

    fn schedule_churn(&mut self, day: u64) {
        let day_start = day * 86400;
        let joins = self.config.joins_per_day;
        let leaves = self.config.leaves_per_day;

        for _ in 0..joins {
            let at = day_start + self.rng.random_range(0..86400u64);
            self.add_event(Event::NodeJoin { at });
        }
        for _ in 0..leaves {
            if let Some(&node_id) = self.network.nodes.keys().choose(&mut self.rng) {
                let at = day_start + self.rng.random_range(0..86400u64);
                self.add_event(Event::NodeLeave { node_id, at });
            }
        }
    }

    fn run_until(&mut self, end_time: u64) {
        while let Some(se) = self.event_queue.peek() {
            if se.time() > end_time {
                break;
            }
            let se = self.event_queue.pop().unwrap();
            let at = se.time();
            let new_events = self.process(se.inner, at);
            for e in new_events {
                self.add_event(e);
            }
        }
    }

    fn process(&mut self, event: Event, _at: u64) -> Vec<Event> {
        match event {
            Event::NodeJoin { at } => {
                let algo = self.config.cache_algo;
                let outbounds = self.config.outbounds;
                let (_, events) = self.network.add_node(
                    vec![NetworkType::Clearnet],
                    HashSet::new(),
                    outbounds,
                    algo,
                    at,
                    &mut self.rng,
                );
                events
            }
            Event::NodeLeave { node_id, at } => {
                if self.network.nodes.contains_key(&node_id) {
                    self.network.remove_node(node_id, at)
                } else {
                    vec![]
                }
            }
            Event::SelfAnnounce {
                node_id,
                peer_addr,
                at,
            } => {
                if self.network.nodes.contains_key(&node_id) {
                    self.network
                        .nodes
                        .get_mut(&node_id)
                        .unwrap()
                        .self_announce(peer_addr, at, &mut self.rng)
                } else {
                    vec![]
                }
            }
            Event::SendMessage { from, to, msg, at } => {
                let to_node_id = match self.network.registry.addresses.get(&to) {
                    Some(a) => a.owner_node,
                    None => return vec![],
                };
                if !self.network.nodes.contains_key(&to_node_id) {
                    return vec![];
                }
                match msg {
                    NetworkMessage::GetAddr => self
                        .network
                        .nodes
                        .get_mut(&to_node_id)
                        .unwrap()
                        .receive_getaddr(from, at, &mut self.rng),
                    NetworkMessage::Addr(addrs) => {
                        self.network
                            .nodes
                            .get_mut(&to_node_id)
                            .unwrap()
                            .receive_addr(addrs, at);
                        vec![]
                    }
                    NetworkMessage::AddrAnnounce(addrs) => {
                        let registry = &self.network.registry;
                        self.network
                            .nodes
                            .get_mut(&to_node_id)
                            .unwrap()
                            .receive_addr_announce(from, addrs, at, registry, &mut self.rng)
                    }
                }
            }
        }
    }

    fn collect_statistics(&mut self, day: u64) {
        let now = day * 86400 + 86399;
        let mut analyzer = FingerprintAnalyzer::new();
        let mut total_addrman = 0usize;
        let mut stale_7d = 0usize;
        let mut stale_30d = 0usize;
        let mut departed = 0usize;
        let node_count = self.network.nodes.len();

        for (node_id, node) in &self.network.nodes {
            for (network, cache) in &node.getaddr_cache {
                if !cache.entries.is_empty() {
                    analyzer.record(*node_id, *network, &cache.entries);
                }
            }
            for entry in node.addrman.entries.values() {
                total_addrman += 1;
                let age = now.saturating_sub(entry.timestamp);
                if age > 7 * 86400 {
                    stale_7d += 1;
                }
                if age > 30 * 86400 {
                    stale_30d += 1;
                }
                if !self.network.registry.is_active(entry.address) {
                    departed += 1;
                }
            }
        }

        self.stats.fingerprint_results.push(analyzer.analyze(day));
        self.stats.staleness_per_day.push(StaleAddressStats {
            day,
            addresses_older_than_7_days: stale_7d,
            addresses_older_than_30_days: stale_30d,
            addresses_of_departed_nodes: departed,
        });

        let avg_size = if node_count > 0 {
            total_addrman as f64 / node_count as f64
        } else {
            0.0
        };
        self.stats.avg_addrman_size.push(avg_size);

        let total_registered = self.network.registry.addresses.len();
        let coverage = if total_registered > 0 && node_count > 0 {
            total_addrman as f64 / (total_registered * node_count) as f64
        } else {
            0.0
        };
        self.stats.address_coverage.push(coverage);
    }

    pub fn add_event(&mut self, event: Event) {
        let at = event_time(&event);
        self.event_queue.push(ScheduledEvent::new(event, at));
    }

    pub fn get_next_event(&mut self) -> Option<ScheduledEvent> {
        self.event_queue.pop()
    }

    pub fn get_next_event_time(&mut self) -> Option<u64> {
        self.event_queue.peek().map(|se| se.time())
    }
}

fn event_time(event: &Event) -> u64 {
    match event {
        Event::SendMessage { at, .. } => *at,
        Event::NodeJoin { at, .. } => *at,
        Event::NodeLeave { at, .. } => *at,
        Event::SelfAnnounce { at, .. } => *at,
    }
}

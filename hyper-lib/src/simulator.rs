use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};

use rand::rngs::StdRng;
use rand::seq::IteratorRandom;
use rand::{Rng, SeedableRng};

use crate::address::NetworkType;
use crate::fingerprint::FingerprintAnalyzer;
use crate::network::Network;
use crate::node::{Event, NetworkMessage};
use crate::statistics::{ChurnStats, SimulationStatistics, StaleAddressStats};
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
    /// Unix timestamp at which the simulation starts. All event times and
    /// day boundaries are offsets from this value.
    pub start_time: u64,
    /// Nodes that joined this day by type: [onion, clearnet, dual].
    day_joined: [usize; 3],
    /// Nodes that left this day by type: [onion, clearnet, dual].
    day_left: [usize; 3],
}

impl Simulator {
    pub fn new(config: SimulationConfig, seed: u64) -> Self {
        let start_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut sim = Self {
            network: Network::new(),
            event_queue: BinaryHeap::new(),
            rng: StdRng::seed_from_u64(seed),
            config,
            stats: SimulationStatistics {
                fingerprint_results: vec![],
                staleness_per_day: vec![],
                churn_per_day: vec![],
                avg_addrman_size: vec![],
                avg_addrman_live: vec![],
                address_coverage: vec![],
            },
            start_time,
            day_joined: [0; 3],
            day_left: [0; 3],
        };
        sim.build_initial_network();
        sim
    }

    fn build_initial_network(&mut self) {
        let now = self.start_time;

        // Copy all config values to locals to avoid borrow conflicts.
        let onion = self.config.onion;
        let clearnet = self.config.clearnet;
        let dual_stack = self.config.dual_stack;
        let reachable_clearnet_pct = self.config.reachable_clearnet_pct as usize;
        let reachable_onion_pct = self.config.reachable_onion_pct as usize;
        let outbounds = self.config.outbounds;
        let algo = self.config.cache_algo;
        let start_mode = self.config.start_mode;

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

        match start_mode {
            crate::StartMode::Warm => {
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
            crate::StartMode::Peers => {
                log::info!("Peers-start: seeding each addrman with directly connected peers...");
                let node_ids: Vec<_> = self.network.nodes.keys().copied().collect();
                for node_id in node_ids {
                    let peer_addrs: Vec<_> = {
                        let node = &self.network.nodes[&node_id];
                        node.out_peers.keys().chain(node.in_peers.keys()).copied().collect()
                    };
                    let node = self.network.nodes.get_mut(&node_id).unwrap();
                    for addr in peer_addrs {
                        node.addrman.add(addr, now, 0, now);
                    }
                }
                log::info!("Peers-start done.");
            }
            crate::StartMode::Dns => {
                // Mirrors Bitcoin Core's ThreadDNSAddressSeed: each node receives a random
                // sample of the network with timestamps 3–7 days old (nTime = now - rand(3..=7) days).
                let pct = self.config.dns_sample_pct as usize;
                let all_addrs: Vec<_> = self
                    .network
                    .registry
                    .addresses
                    .values()
                    .map(|a| a.id)
                    .collect();
                let sample_size = ((all_addrs.len() * pct) / 100).max(1);
                log::info!(
                    "DNS-start: seeding each addrman with ~{} addresses (~{}% of {}) at 3–7 day old timestamps...",
                    sample_size, pct, all_addrs.len()
                );
                let node_ids: Vec<_> = self.network.nodes.keys().copied().collect();
                for node_id in node_ids {
                    use rand::seq::SliceRandom;
                    let mut sample = all_addrs.clone();
                    sample.shuffle(&mut self.rng);
                    sample.truncate(sample_size);
                    let node = self.network.nodes.get_mut(&node_id).unwrap();
                    for addr in sample {
                        // Bitcoin Core assigns timestamps now - rand(3..=7) days for DNS seed entries.
                        let age_days = self.rng.random_range(3u64..=7);
                        let ts = now.saturating_sub(age_days * 86400);
                        node.addrman.add(addr, ts, 0, now);
                    }
                }
                log::info!("DNS-start done.");
            }
            crate::StartMode::Cold => {
                log::info!("Cold-start: addrmans left empty.");
            }
        }
    }

    pub fn run(&mut self) {
        let burn_in = self.config.burn_in_days;
        let total_days = burn_in + self.config.days;

        for day in 0..total_days {
            if day < burn_in {
                log::info!(
                    "Burn-in {}/{} — queue: {} events, nodes: {}",
                    day + 1,
                    burn_in,
                    self.event_queue.len(),
                    self.network.nodes.len()
                );
            } else {
                let sim_day = day - burn_in;
                log::info!(
                    "Day {}/{} — queue: {} events, nodes: {}",
                    sim_day + 1,
                    self.config.days,
                    self.event_queue.len(),
                    self.network.nodes.len()
                );
            }

            self.day_joined = [0; 3];
            self.day_left = [0; 3];
            self.schedule_churn(day);
            self.run_until(self.start_time + day * 86400 + 86399);

            if day >= burn_in {
                let sim_day = day - burn_in;
                self.collect_statistics(sim_day);
                let s = self.stats.staleness_per_day.last().unwrap();
                let f = self.stats.fingerprint_results.last().unwrap();
                let c = self.stats.churn_per_day.last().unwrap();
                log::info!(
                    "  onion   +{} -{} = {}  |  clearnet +{} -{} = {}  |  dual +{} -{} = {}",
                    c.joined_onion, c.left_onion, c.total_onion,
                    c.joined_clearnet, c.left_clearnet, c.total_clearnet,
                    c.joined_dual, c.left_dual, c.total_dual,
                );
                log::info!(
                    "  addrman_avg={:.1}  addrman_live={:.1}  coverage={:.4}  stale_7d={:.1}  stale_30d={:.1}  \
                     departed={:.1}  departed_fresh={:.1}  fp_overlap={:.4}  fp_nodes={}",
                    self.stats.avg_addrman_size.last().unwrap(),
                    self.stats.avg_addrman_live.last().unwrap(),
                    self.stats.address_coverage.last().unwrap(),
                    s.avg_older_than_7_days,
                    s.avg_older_than_30_days,
                    s.avg_departed,
                    s.avg_departed_fresh,
                    f.avg_overlap,
                    f.nodes_sampled,
                );
            }
        }
    }

    fn schedule_churn(&mut self, day: u64) {
        let day_start = self.start_time + day * 86400;
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
        log_event(&event);
        match event {
            Event::NodeJoin { at } => {
                let algo = self.config.cache_algo;
                let outbounds = self.config.outbounds;
                let total = self.config.onion + self.config.clearnet + self.config.dual_stack;
                let (networks, reachable_on) = if total > 0 {
                    let roll = self.rng.random_range(0..total);
                    if roll < self.config.onion {
                        let reachable = roll < self.config.onion * self.config.reachable_onion_pct as usize / 100;
                        let r = if reachable { [NetworkType::Onion].into() } else { HashSet::new() };
                        (vec![NetworkType::Onion], r)
                    } else if roll < self.config.onion + self.config.clearnet {
                        let reachable = (roll - self.config.onion) < self.config.clearnet * self.config.reachable_clearnet_pct as usize / 100;
                        let r = if reachable { [NetworkType::Clearnet].into() } else { HashSet::new() };
                        (vec![NetworkType::Clearnet], r)
                    } else {
                        let i = roll - self.config.onion - self.config.clearnet;
                        let mut r = HashSet::new();
                        if i < self.config.dual_stack * self.config.reachable_onion_pct as usize / 100 {
                            r.insert(NetworkType::Onion);
                        }
                        if i < self.config.dual_stack * self.config.reachable_clearnet_pct as usize / 100 {
                            r.insert(NetworkType::Clearnet);
                        }
                        (vec![NetworkType::Onion, NetworkType::Clearnet], r)
                    }
                } else {
                    (vec![NetworkType::Clearnet], HashSet::new())
                };
                let idx = node_type_idx(&networks);
                let (_, events) = self.network.add_node(
                    networks,
                    reachable_on,
                    outbounds,
                    algo,
                    at,
                    &mut self.rng,
                );
                self.day_joined[idx] += 1;
                events
            }
            Event::NodeLeave { node_id, at } => {
                if let Some(node) = self.network.nodes.get(&node_id) {
                    let idx = node_type_idx(&node.addresses.iter().map(|a| a.network).collect::<Vec<_>>());
                    self.day_left[idx] += 1;
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
                    NetworkMessage::GetAddr => {
                        // Count on the sender when the message is actually delivered.
                        let from_node_id = self.network.registry.addresses.get(&from).map(|a| a.owner_node);
                        if let Some(fid) = from_node_id {
                            if let Some(n) = self.network.nodes.get_mut(&fid) {
                                n.node_statistics.getaddr_sent += 1;
                            }
                        }
                        self.network
                            .nodes
                            .get_mut(&to_node_id)
                            .unwrap()
                            .receive_getaddr(from, at, &mut self.rng)
                    }
                    NetworkMessage::Addr(addrs) => {
                        // Count addr_sent on the sender at delivery time (mirrors getaddr_sent above).
                        let from_node_id = self.network.registry.addresses.get(&from).map(|a| a.owner_node);
                        if let Some(fid) = from_node_id {
                            if let Some(n) = self.network.nodes.get_mut(&fid) {
                                n.node_statistics.addr_sent += 1;
                            }
                        }
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
        let now = self.start_time + day * 86400 + 86399;
        let mut analyzer = FingerprintAnalyzer::new();
        let mut total_addrman = 0usize;
        let mut total_addrman_live = 0usize;
        let mut stale_7d = 0usize;
        let mut stale_30d = 0usize;
        let mut departed = 0usize;
        let mut departed_fresh = 0usize;
        let node_count = self.network.nodes.len();
        let mut total_onion = 0usize;
        let mut total_clearnet = 0usize;
        let mut total_dual = 0usize;

        for (node_id, node) in &self.network.nodes {
            match node_type_idx(&node.addresses.iter().map(|a| a.network).collect::<Vec<_>>()) {
                0 => total_onion += 1,
                1 => total_clearnet += 1,
                _ => total_dual += 1,
            }
            for (network, cache) in &node.getaddr_cache {
                if !cache.entries.is_empty() {
                    analyzer.record(*node_id, *network, &cache.entries);
                }
            }
            for entry in node.addrman.entries.values() {
                total_addrman += 1;
                if !entry.is_terrible(now) {
                    total_addrman_live += 1;
                }
                let age = now.saturating_sub(entry.timestamp);
                if age > 7 * 86400 {
                    stale_7d += 1;
                }
                if age > 30 * 86400 {
                    stale_30d += 1;
                }
                if !self.network.registry.is_active(entry.address) {
                    departed += 1;
                    if !entry.is_terrible(now) {
                        departed_fresh += 1;
                    }
                }
            }
        }

        let avg = |n: usize| if node_count > 0 { n as f64 / node_count as f64 } else { 0.0 };

        self.stats.avg_addrman_live.push(avg(total_addrman_live));
        self.stats.fingerprint_results.push(analyzer.analyze(day));
        self.stats.churn_per_day.push(ChurnStats {
            joined_onion: self.day_joined[0],
            joined_clearnet: self.day_joined[1],
            joined_dual: self.day_joined[2],
            left_onion: self.day_left[0],
            left_clearnet: self.day_left[1],
            left_dual: self.day_left[2],
            total_onion,
            total_clearnet,
            total_dual,
        });
        self.stats.staleness_per_day.push(StaleAddressStats {
            day,
            avg_older_than_7_days: avg(stale_7d),
            avg_older_than_30_days: avg(stale_30d),
            avg_departed: avg(departed),
            avg_departed_fresh: avg(departed_fresh),
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

    /// Process one event from the queue. Returns (event, timestamp) or None if queue is empty.
    pub fn step(&mut self) -> Option<(Event, u64)> {
        let se = self.event_queue.pop()?;
        let at = se.time();
        let event = se.inner.clone();
        let new_events = self.process(se.inner, at);
        for e in new_events {
            self.add_event(e);
        }
        Some((event, at))
    }

    pub fn add_event(&mut self, event: Event) {
        let at = event_time(&event);
        self.event_queue.push(ScheduledEvent::new(event, at));
    }


}

/// Returns 0 for onion-only, 1 for clearnet-only, 2 for dual-stack.
fn node_type_idx(networks: &[NetworkType]) -> usize {
    match networks {
        [NetworkType::Onion] => 0,
        [NetworkType::Clearnet] => 1,
        _ => 2,
    }
}

fn log_event(event: &Event) {
    match event {
        Event::NodeJoin { at } => {
            log::trace!(target: "hyper_lib::event", "t={at} NodeJoin");
        }
        Event::NodeLeave { node_id, at } => {
            log::trace!(target: "hyper_lib::event", "t={at} NodeLeave node={node_id}");
        }
        Event::SelfAnnounce { node_id, peer_addr, at } => {
            log::trace!(target: "hyper_lib::event", "t={at} SelfAnnounce node={node_id} peer={peer_addr:?}");
        }
        Event::SendMessage { from, to, msg, at } => {
            let kind = match msg {
                NetworkMessage::GetAddr => "GetAddr",
                NetworkMessage::Addr(_) => "Addr",
                NetworkMessage::AddrAnnounce(_) => "AddrAnnounce",
            };
            log::trace!(target: "hyper_lib::event", "t={at} {kind} from={from:?} to={to:?}");
        }
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

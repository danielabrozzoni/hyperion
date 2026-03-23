# Address Relay Simulator — Implementation Plan

See `ADDRESS_RELAY_SPEC.md` for the goals, Bitcoin Core behavior reference, and design rationale. This document specifies exactly what to build.

---

## Data Structures

### `address.rs`

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AddressId {
    pub id: u64,
    pub network: NetworkType,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetworkType {
    Onion,
    Clearnet,
}

/// Stored in the global registry only — not in addrman.
pub struct Address {
    pub id: AddressId,
    pub owner_node: NodeId,
    pub is_active: bool,
    pub is_reachable: bool,   // static; set at creation from --reachable-pct
}

/// Global bookkeeping: maps addresses to nodes and vice versa.
/// Used to mark all addresses of a departing node as inactive.
pub struct AddressRegistry {
    pub addresses: HashMap<AddressId, Address>,
    pub node_addresses: HashMap<NodeId, Vec<AddressId>>,
}

impl AddressRegistry {
    pub fn register(&mut self, node_id: NodeId, address_id: AddressId, is_reachable: bool) { ... }

    /// Mark all addresses of a departing node inactive.
    pub fn deactivate_node(&mut self, node_id: NodeId) {
        for addr_id in &self.node_addresses[&node_id] {
            self.addresses.get_mut(addr_id).unwrap().is_active = false;
        }
    }

    pub fn is_active(&self, addr_id: AddressId) -> bool {
        self.addresses[&addr_id].is_active
    }

    /// True if the owner node of this address accepts inbound connections.
    pub fn is_reachable(&self, addr_id: AddressId) -> bool {
        self.addresses[&addr_id].is_reachable
    }
}
```

### `addrman.rs`

```rust
const ADDRMAN_HORIZON: u64           = 30 * 24 * 3600;
const TIMESTAMP_UPDATE_INTERVAL: u64 = 20 * 60;

pub struct AddrmanEntry {
    pub address: AddressId,
    pub timestamp: u64,   // nTime
}

impl AddrmanEntry {
    /// Only timestamp-based checks for now.
    /// Bitcoin Core also gates on last_try/attempts/last_success, but those fields
    /// are not modeled in this simulator.
    pub fn is_terrible(&self, now: u64) -> bool {
        if self.timestamp > now + 600 { return true; }
        if now.saturating_sub(self.timestamp) > ADDRMAN_HORIZON { return true; }
        false
    }

    /// Called on disconnect. Mirrors AddrMan::Connected() in Bitcoin Core, which is called
    /// from FinalizeNode() (net_processing.cpp) when a peer connection is torn down.
    /// Despite the name, Connected() records the fact that we *were* connected — it runs
    /// at disconnect, not at connect. Updates nTime at most every 20 minutes.
    pub fn record_connected(&mut self, now: u64) {
        if now.saturating_sub(self.timestamp) > TIMESTAMP_UPDATE_INTERVAL {
            self.timestamp = now;
        }
    }
}

pub struct Addrman {
    entries: HashMap<AddressId, AddrmanEntry>,
}

impl Addrman {
    /// Returns a random selection of non-terrible entries for a GETADDR response.
    /// Count is min(1000, addrman_size * 23%) — MAX_PCT_ADDR_TO_SEND = 23.
    pub fn get_addr(&self, now: u64, rng: &mut impl Rng) -> Vec<&AddrmanEntry> {
        let candidates: Vec<_> = self.entries.values()
            .filter(|e| !e.is_terrible(now))
            .collect();
        let n = (candidates.len() * 23 / 100).min(1000);
        candidates.into_iter().choose_multiple(rng, n)
    }

    /// Add a new address or update an existing one.
    ///
    /// penalty: 0 for own-address self-announcements (sender == announced address),
    ///          2*3600 (2h) for all other received addresses.
    ///
    /// Stored value: incoming_timestamp - penalty
    /// Update condition: stored < incoming - update_interval - penalty, where:
    ///   incoming < 24h old  → update_interval = 1h  (effective bar: 3h for relayed, 1h for own)
    ///   incoming >= 24h old → update_interval = 24h (effective bar: 26h for relayed, 24h for own)
    pub fn add(&mut self, address: AddressId, incoming_timestamp: u64, penalty: u64, now: u64) {
        let update_interval = if now.saturating_sub(incoming_timestamp) < 24 * 3600 {
            3600   // 1 hour
        } else {
            86400  // 24 hours
        };
        let stored_value = incoming_timestamp.saturating_sub(penalty);

        match self.entries.get_mut(&address) {
            None => {
                self.entries.insert(address, AddrmanEntry {
                    address,
                    timestamp: stored_value,
                });
            }
            Some(entry) => {
                // Update only if penalty-adjusted value exceeds stored by at least update_interval
                if stored_value > entry.timestamp + update_interval {
                    entry.timestamp = stored_value;
                }
            }
        }
    }
}
```

### `node.rs` — structs

```rust
const GETADDR_CACHE_LIFETIME_BASE: u64 = 21 * 3600;
const GETADDR_CACHE_LIFETIME_RAND: u64 = 6 * 3600;
const SELF_ANNOUNCE_DELAY: u64 = 30;      // seconds after connect for first announcement
const DAYS: u64 = 86400;
const HOURS: u64 = 3600;

pub struct Node {
    pub node_id: NodeId,
    pub addresses: Vec<AddressId>,          // 1 or 2 (one per network)
    pub reachable_networks: HashSet<NetworkType>, // networks on which this node accepts inbound

    pub out_peers: BTreeMap<AddressId, Peer>,
    pub in_peers: BTreeMap<AddressId, Peer>,

    pub addrman: Addrman,
    pub getaddr_cache: HashMap<NetworkType, GetaddrCache>,
    pub cache_algo: GetaddrCacheAlgorithm,

    pub node_statistics: NodeStatistics,
}

pub struct Peer {
    pub addr: AddressId,
    pub getaddr_sent: bool,      // we sent GETADDR to this peer
    pub getaddr_recvd: bool,     // we already answered their GETADDR
}

/// GETADDR cache for one network type. Timestamps are frozen at build time.
pub struct GetaddrCache {
    pub entries: Vec<AddrPayload>,
    pub expires_at: u64,
}

pub enum GetaddrCacheAlgorithm {
    /// Real timestamp from addrman (Bitcoin Core baseline).
    Current,
    /// now - rand_uniform(8, 12) days, sampled independently per address.
    FixedOffset,
    /// Same network as the GETADDR requester → real timestamp; cross-network → now - 5 days.
    NetworkBased,
}

pub enum NetworkMessage {
    GetAddr,
    Addr(Vec<AddrPayload>),         // GETADDR response (up to 1000); never relayed
    AddrAnnounce(Vec<AddrPayload>), // self-announcement or relayed gossip (≤10)
}

pub struct AddrPayload {
    pub address: AddressId,
    pub timestamp: u64,
}

pub enum Event {
    SendMessage { from: AddressId, to: AddressId, msg: NetworkMessage, at: u64 },
    NodeJoin { at: u64 },
    NodeLeave { node_id: NodeId, at: u64 },
    SelfAnnounce { node_id: NodeId, peer_addr: AddressId, at: u64 },
}
```

---

## Node Methods

### Connection lifecycle

```rust
impl Node {
    /// Called when this node establishes a connection to a peer.
    /// is_outbound = true means we initiated the connection.
    pub fn on_connect(&mut self, peer_addr: AddressId, is_outbound: bool, now: u64) -> Vec<Event> {
        let peer = Peer { addr: peer_addr, getaddr_sent: false, getaddr_recvd: false };
        let mut events = vec![];

        if is_outbound {
            self.out_peers.insert(peer_addr, peer);
            // Send GETADDR immediately
            events.push(Event::SendMessage {
                from: self.own_addr_for_network(peer_addr.network),
                to: peer_addr,
                msg: NetworkMessage::GetAddr,
                at: now,
            });
            self.out_peers.get_mut(&peer_addr).unwrap().getaddr_sent = true;
        } else {
            self.in_peers.insert(peer_addr, peer);
        }

        // Schedule first self-announcement (~30s after connect)
        events.push(Event::SelfAnnounce {
            node_id: self.node_id,
            peer_addr,
            at: now + SELF_ANNOUNCE_DELAY,
        });

        events
    }

    /// Called when a connection to a peer is dropped.
    /// Bitcoin Core calls AddrMan::Connected() from FinalizeNode() at this point
    /// (net_processing.cpp:1737), which updates nTime if >20 min have passed since the
    /// last update. Verified against addrman.cpp:Connected_(). The name is misleading —
    /// it records a past successful connection, it does not fire at connect time.
    pub fn on_disconnect(&mut self, peer_addr: AddressId, now: u64) {
        self.out_peers.remove(&peer_addr);
        self.in_peers.remove(&peer_addr);
        if let Some(entry) = self.addrman.entries.get_mut(&peer_addr) {
            entry.record_connected(now);
        }
    }

    /// Called when a connection attempt to a peer fails (target node has departed).
    pub fn on_connect_failed(&mut self, _peer_addr: AddressId, _now: u64) {}

    /// Returns this node's own address on the given network, if it has one.
    fn own_addr_for_network(&self, network: NetworkType) -> AddressId {
        self.addresses.iter().find(|a| a.network == network)
            .copied()
            .expect("connected on a network we don't have an address for")
    }
}
```

### GETADDR handling

```rust
impl Node {
    pub fn receive_getaddr(&mut self, from: AddressId, now: u64, rng: &mut impl Rng) -> Vec<Event> {
        // GETADDR is only routed here after suitable_outbound_candidates selected this node's
        // address as reachable — so receiving it on a non-listening network is a simulator bug.
        assert!(self.reachable_networks.contains(&from.network),
            "received GETADDR on network {:?} but node is not reachable on it", from.network);

        // Nodes don't misbehave in this simulation: sending GETADDR twice is a bug.
        let peer = self.peer_mut(from);
        assert!(!peer.getaddr_recvd, "peer {from:?} sent GETADDR twice on the same connection");
        peer.getaddr_recvd = true;

        // Rebuild cache for this network if missing or expired
        let network = from.network;
        if self.getaddr_cache.get(&network).map_or(true, |c| now >= c.expires_at) {
            self.build_cache(network, now, rng);
        }

        let entries = self.getaddr_cache[&network].entries.clone();
        vec![Event::SendMessage {
            from: self.own_addr_for_network(network),
            to: from,
            msg: NetworkMessage::Addr(entries),
            at: now,
        }]
    }

    fn build_cache(&mut self, network: NetworkType, now: u64, rng: &mut impl Rng) {
        let selected = self.addrman.get_addr(now, rng);
        let entries = selected.into_iter()
            .map(|e| AddrPayload {
                address: e.address,
                timestamp: self.cache_timestamp(e, network, now, rng),
            })
            .collect();

        self.getaddr_cache.insert(network, GetaddrCache {
            entries,
            expires_at: now + GETADDR_CACHE_LIFETIME_BASE + rng.gen_range(0..GETADDR_CACHE_LIFETIME_RAND),
        });
    }

    /// `cache_network`: the network the GETADDR came from (i.e. which cache is being built).
    fn cache_timestamp(&self, entry: &AddrmanEntry, cache_network: NetworkType, now: u64, rng: &mut impl Rng) -> u64 {
        match self.cache_algo {
            GetaddrCacheAlgorithm::Current => entry.timestamp,
            GetaddrCacheAlgorithm::FixedOffset => now - rng.gen_range(8..=12) * DAYS,
            GetaddrCacheAlgorithm::NetworkBased => {
                if entry.address.network == cache_network {
                    entry.timestamp
                } else {
                    now - 5 * DAYS
                }
            }
        }
    }
}
```

### ADDR and AddrAnnounce handling

```rust
impl Node {
    /// Handle GETADDR response: populate addrman, never relay.
    /// Always a relayed address → penalty = 2h.
    pub fn receive_addr(&mut self, addrs: Vec<AddrPayload>, now: u64) {
        const PENALTY: u64 = 2 * HOURS;
        for payload in addrs {
            self.addrman.add(payload.address, payload.timestamp, PENALTY, now);
        }
    }

    /// Handle gossip announcement: populate addrman, relay eligible entries.
    pub fn receive_addr_announce(
        &mut self,
        from: AddressId,
        addrs: Vec<AddrPayload>,
        now: u64,
        registry: &AddressRegistry,
        rng: &mut impl Rng,
    ) -> Vec<Event> {
        let batch_size = addrs.len();
        let mut events = vec![];

        for payload in &addrs {
            // penalty = 0 if sender is announcing its own address directly; 2h otherwise
            let penalty = if payload.address == from { 0 } else { 2 * HOURS };
            self.addrman.add(payload.address, payload.timestamp, penalty, now);
        }

        // Only relay if the batch is small enough
        if batch_size > 10 { return events; }

        let peer_sent_getaddr = self.peer(from).getaddr_sent;

        for payload in addrs {
            // Relay conditions
            if payload.timestamp < now.saturating_sub(10 * 60) { continue; } // older than 10 min
            if peer_sent_getaddr { continue; }                                // solicited
            if !registry.is_active(payload.address) { continue; }            // not routable proxy

            // Select 1 or 2 relay targets via SipHash (excluding source peer)
            let n_relay = self.relay_count(payload.address, registry);
            let targets = self.select_relay_peers(from, payload.address, n_relay, now);
            for target in targets {
                events.push(Event::SendMessage {
                    from: self.own_addr_for_network(target.network),
                    to: target,
                    msg: NetworkMessage::AddrAnnounce(vec![payload.clone()]),
                    at: now,
                });
            }
        }

        events
    }

    /// Reachable → 2; unreachable → low bit of SipHash of address decides (1 or 2).
    fn relay_count(&self, addr: AddressId, registry: &AddressRegistry) -> usize {
        if registry.is_reachable(addr) { 2 } else { (SipHasher::hash(addr.id) & 1) as usize + 1 }
    }

    /// Select top-N peers by SipHash(addr_hash, time_window, peer_id), excluding source.
    /// Key rotates every 24h per-address (time_window = (now_secs + addr_hash) / 86400),
    /// so different addresses rotate at slightly different times.
    fn select_relay_peers(&self, exclude: AddressId, addr: AddressId, n: usize, now: u64) -> Vec<AddressId> {
        let addr_hash = addr.id; // stand-in for CServiceHash
        let time_window = (now + addr_hash) / 86400;

        let mut scored: Vec<(u64, AddressId)> = self.out_peers.keys()
            .chain(self.in_peers.keys())
            .filter(|&&p| p != exclude)
            .map(|&p| {
                let score = siphash(addr_hash, time_window, p.id); // SipHash(addr, time, peer)
                (score, p)
            })
            .collect();

        // Top-N by score (highest wins)
        scored.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(n).map(|(_, p)| p).collect()
    }
}
```

### Self-announcement

```rust
impl Node {
    /// Send this node's own address to one peer, then schedule the next announcement.
    pub fn self_announce(&mut self, peer_addr: AddressId, now: u64, rng: &mut impl Rng) -> Vec<Event> {
        let mut events = vec![];

        // Only announce if reachable on this network (fListen gating).
        // Match peer's network; skip if not reachable on that network.
        let own_addr = self.addresses.iter()
            .find(|a| a.network == peer_addr.network && self.reachable_networks.contains(&a.network));
        if let Some(&addr) = own_addr {
            events.push(Event::SendMessage {
                from: addr,
                to: peer_addr,
                msg: NetworkMessage::AddrAnnounce(vec![AddrPayload { address: addr, timestamp: now }]),
                at: now,
            });
        }

        // Schedule next announcement: exponential distribution, mean 24h
        let next = now + sample_exponential(rng, 24 * HOURS);
        events.push(Event::SelfAnnounce { node_id: self.node_id, peer_addr, at: next });

        events
    }
}
```

---

## Network and Simulator

### `network.rs`

```rust
pub struct Network {
    pub nodes: HashMap<NodeId, Node>,
    pub registry: AddressRegistry,
    next_node_id: NodeId,
    next_addr_id: u64,
}

impl Network {
    /// Add a new node with the given network types and reachability.
    /// Connects it to `n_outbound` randomly selected existing peers.
    /// `reachable_on`: which of this node's networks accept inbound connections.
    /// Determined by the caller from `--reachable-clearnet-pct` / `--reachable-onion-pct`.
    pub fn add_node(
        &mut self,
        networks: Vec<NetworkType>,
        reachable_on: HashSet<NetworkType>,
        n_outbound: usize,
        cache_algo: GetaddrCacheAlgorithm,
        now: u64,
        rng: &mut impl Rng,
    ) -> (NodeId, Vec<Event>) {
        let node_id = self.next_node_id;
        self.next_node_id += 1;

        // Assign one AddressId per network; reachability is per-address
        let addresses: Vec<AddressId> = networks.iter().map(|&net| {
            let addr = AddressId { id: self.next_addr_id, network: net };
            self.next_addr_id += 1;
            self.registry.register(node_id, addr, reachable_on.contains(&net));
            addr
        }).collect();

        let node = Node {
            node_id,
            addresses: addresses.clone(),
            reachable_networks: reachable_on,
            out_peers: BTreeMap::new(),
            in_peers: BTreeMap::new(),
            addrman: Addrman { entries: HashMap::new() },
            getaddr_cache: HashMap::new(),
            cache_algo,
            node_statistics: NodeStatistics::default(),
        };
        self.nodes.insert(node_id, node);

        // Connect outbound to n_outbound random existing nodes
        let mut events = vec![];
        let candidates = self.suitable_outbound_candidates(node_id, &addresses, rng, n_outbound);
        for peer_addr in candidates {
            events.extend(self.connect(node_id, peer_addr, now));
        }

        (node_id, events)
    }

    /// Remove a node: disconnect all its peers, mark its addresses inactive.
    pub fn remove_node(&mut self, node_id: NodeId, now: u64) -> Vec<Event> {
        self.registry.deactivate_node(node_id);
        let peers: Vec<AddressId> = {
            let node = &self.nodes[&node_id];
            node.out_peers.keys().chain(node.in_peers.keys()).copied().collect()
        };
        let mut events = vec![];
        for peer_addr in peers {
            // Notify the peer's node of the disconnect
            let peer_node_id = self.node_id_for_addr(peer_addr);
            events.extend(self.nodes.get_mut(&peer_node_id).unwrap()
                .on_disconnect(self.own_addr_of(node_id, peer_addr.network), now));
        }
        self.nodes.remove(&node_id);
        events
    }

    fn connect(&mut self, from_node: NodeId, to_addr: AddressId, now: u64) -> Vec<Event> {
        let from_addr = self.own_addr_of(from_node, to_addr.network);
        let to_node = self.node_id_for_addr(to_addr);
        let mut events = vec![];
        events.extend(self.nodes.get_mut(&from_node).unwrap().on_connect(to_addr, true, now));
        events.extend(self.nodes.get_mut(&to_node).unwrap().on_connect(from_addr, false, now));
        events
    }

    /// Return up to `n` addresses from reachable nodes whose network overlaps
    /// with at least one of `own_addresses`, excluding the joining node itself.
    /// Candidates are chosen uniformly at random from the eligible pool.
    fn suitable_outbound_candidates(
        &self,
        joining: NodeId,
        own_addresses: &[AddressId],
        rng: &mut impl Rng,
        n: usize,
    ) -> Vec<AddressId> {
        // Eligible: address belongs to a node that is reachable (is_reachable == true),
        // is not the joining node itself, and shares a network with one of own_addresses.
        let own_networks: HashSet<NetworkType> = own_addresses.iter().map(|a| a.network).collect();
        let eligible: Vec<AddressId> = self.registry.addresses.values()
            .filter(|addr| {
                addr.is_reachable
                    && addr.owner_node != joining
                    && addr.is_active
                    && own_networks.contains(&addr.id.network)
            })
            .map(|addr| addr.id)
            .collect();
        eligible.into_iter().choose_multiple(rng, n)
    }
}
```

### `simulator.rs`

```rust
pub struct Simulator {
    pub network: Network,
    pub event_queue: BinaryHeap<ScheduledEvent>,
    pub rng: StdRng,
    pub config: SimulationConfig,
    pub stats: SimulationStatistics,
}

impl Simulator {
    pub fn new(config: SimulationConfig, seed: u64) -> Self {
        let mut sim = Self { /* ... */ };
        sim.build_initial_network();
        sim
    }

    fn build_initial_network(&mut self) {
        // Create all nodes (onion-only, clearnet-only, dual-stack)
        // Establish initial outbound connections
        // If warm-start: pre-populate all addrmans with all registered addresses at timestamp=now
        // If cold-start: leave addrmans empty
        // Schedule initial self-announcement events (staggered randomly across first 24h)
    }

    pub fn run(&mut self) {
        for day in 0..self.config.days {
            self.schedule_churn(day);
            self.run_until(day_end(day));
            self.collect_statistics(day);
        }
    }

    fn schedule_churn(&mut self, day: u64) {
        let day_start = day * 86400;
        for _ in 0..self.config.joins_per_day {
            let at = day_start + self.rng.gen_range(0..86400);
            self.push(Event::NodeJoin { at }); // node_id assigned at processing time
        }
        for _ in 0..self.config.leaves_per_day {
            let node_id = self.random_existing_node();
            let at = day_start + self.rng.gen_range(0..86400);
            self.push(Event::NodeLeave { node_id, at });
        }
    }

    fn run_until(&mut self, end_time: u64) {
        while let Some(event) = self.event_queue.peek() {
            if event.at > end_time { break; }
            let event = self.event_queue.pop().unwrap();
            let new_events = self.process(event);
            for e in new_events { self.event_queue.push(e); }
        }
    }

    fn process(&mut self, event: Event) -> Vec<Event> {
        match event {
            Event::NodeJoin { at } => {
                // node_id is assigned inside add_node from network.next_node_id
                let (_, events) = self.network.add_node(/* config-driven params */, at, &mut self.rng);
                events
            }
            Event::NodeLeave { node_id, at } => self.network.remove_node(node_id, at),
            Event::SelfAnnounce { node_id, peer_addr, at } => {
                self.network.nodes.get_mut(&node_id).unwrap()
                    .self_announce(peer_addr, at, &mut self.rng)
            }
            Event::SendMessage { from, to, msg, at } => {
                let to_node = self.network.node_id_for_addr(to);
                match msg {
                    NetworkMessage::GetAddr => {
                        self.network.nodes.get_mut(&to_node).unwrap()
                            .receive_getaddr(from, at, &mut self.rng)
                    }
                    NetworkMessage::Addr(addrs) => {
                        self.network.nodes.get_mut(&to_node).unwrap()
                            .receive_addr(addrs, at);
                        vec![]
                    }
                    NetworkMessage::AddrAnnounce(addrs) => {
                        self.network.nodes.get_mut(&to_node).unwrap()
                            .receive_addr_announce(from, addrs, at, &self.network.registry, &mut self.rng)
                    }
                }
            }
        }
    }
}
```

---

## Statistics

```rust
// statistics.rs
pub struct SimulationStatistics {
    pub fingerprint_results: Vec<FingerprintResult>,
    pub staleness_per_day: Vec<StaleAddressStats>,
    pub avg_addrman_size: Vec<f64>,
    pub address_coverage: Vec<f64>,
}

pub struct FingerprintResult {
    pub day: u64,
    /// Number of dual-stack nodes whose clearnet and onion caches share at least one address.
    pub dual_stack_nodes_with_shared_addresses: usize,
    /// Among shared addresses, fraction that also have matching timestamps across both caches.
    /// High overlap means the two network identities are linkable (high fingerprinting surface).
    pub cross_network_timestamp_overlap: f64,
}

pub struct StaleAddressStats {
    pub day: u64,
    pub addresses_older_than_7_days: usize,
    pub addresses_older_than_30_days: usize,
    pub addresses_of_departed_nodes: usize,
}

// fingerprint.rs
//
// The fingerprinting attack: an observer connects to a dual-stack node once via clearnet and
// once via onion, issues GETADDR on each connection, and compares the (address, timestamp) pairs.
// If timestamps for shared addresses match across both caches, the observer can link the two
// network identities to the same physical node. FingerprintAnalyzer measures this exposure.
pub struct FingerprintAnalyzer {
    /// Per-node, per-network sorted (address, timestamp) pairs, collected daily.
    /// Only dual-stack nodes (those with entries for both Clearnet and Onion) are analysed.
    responses: HashMap<NodeId, HashMap<NetworkType, Vec<(AddressId, u64)>>>,
}

impl FingerprintAnalyzer {
    /// Record one network's GETADDR cache for a node.
    /// Call once for Clearnet and once for Onion on each dual-stack node per day.
    pub fn record(&mut self, node_id: NodeId, network: NetworkType, cache: &[AddrPayload]) {
        let mut pairs: Vec<_> = cache.iter().map(|p| (p.address, p.timestamp)).collect();
        pairs.sort();
        self.responses.entry(node_id).or_default().insert(network, pairs);
    }

    /// For each dual-stack node with both a Clearnet and Onion cache, compute the fraction of
    /// shared addresses that also have matching timestamps. Returns aggregate stats for the day.
    pub fn analyze(&self) -> FingerprintResult { ... }

    /// Fraction of addresses appearing in both slices that have identical timestamps.
    /// Input slices must be sorted by AddressId.
    pub fn cross_network_overlap(
        clearnet: &[(AddressId, u64)],
        onion: &[(AddressId, u64)],
    ) -> f64 { ... }
}
```

---

## Simulation Flow

### Initialization

```
1. Parse CLI config
2. Seed RNG

3. Build network:
   Reachability is per-network, applied independently:
     clearnet_reachable = floor(clearnet_addr_count × reachable_clearnet_pct / 100)
     onion_reachable    = floor(onion_addr_count    × reachable_onion_pct    / 100)
   The first N addresses of each network type are marked reachable; the rest are not.

   For each onion-only node:
     reachable_on = {Onion} if this node is among the reachable onion quota, else {}
     add_node([Onion], reachable_on, ...)
   For each clearnet-only node:
     reachable_on = {Clearnet} if this node is among the reachable clearnet quota, else {}
     add_node([Clearnet], reachable_on, ...)
   For each dual-stack node:
     reachable_on = union of {Onion} and/or {Clearnet} based on each network's quota
     add_node([Onion, Clearnet], reachable_on, ...)

4. Bootstrap addrmans:
   warm-start: for each node, for each registered address, call addrman.add(addr, now, /*penalty=*/0, now)
   cold-start: leave addrmans empty

5. All initial on_connect() calls produce events (GetAddr, SelfAnnounce) — add to queue
```

### Daily Loop

```
FOR day IN 0..config.days:
    schedule_churn(day)          // NodeJoin + NodeLeave events
    run_until(day * 86400 + 86399)

    // Daily statistics snapshot:
    for each node:
        for each network this node is active on:
            record that network's GETADDR cache into FingerprintAnalyzer
        count addrman entries by timestamp age and active/departed status
    // FingerprintAnalyzer.analyze() only considers dual-stack nodes (those with
    // both a Clearnet and Onion cache recorded). It measures what fraction of
    // shared addresses have matching timestamps across the two caches.
    compute and store FingerprintResult + StaleAddressStats for this day

output results
```

---

## Implementation Order

**Step 0 — Remove files and dependencies that cannot be reused:**
- Delete `hyper-lib/src/txreconciliation.rs` — Erlay-specific, no overlap with address relay
- Delete `hyper-lib/src/graph.rs` — only exports the topology to GraphML via `graphrs`; topology tracking moves into `Node.out_peers`/`Node.in_peers`
- Remove `graphrs` and `itertools` from `hyper-lib/Cargo.toml`; keep `rand`, `serde`, `log`, `simple_logger`

**Steps 1–10 — Implement in dependency order:**

1. `address.rs` (NEW) — `AddressId`, `NetworkType`, `Address`, `AddressRegistry`

2. `addrman.rs` (NEW) — `AddrmanEntry` (`is_terrible` timestamp-only, `record_connected`), `Addrman` (`get_addr`, `add`)

3. `node.rs` (REWRITE — keep `NodeId` type alias and `debug_log!` macro) — Replace all structs and methods.
   Write structs first: `Node`, `Peer`, `GetaddrCache`, `GetaddrCacheAlgorithm`, `AddrPayload`.
   Then methods in dependency order: `own_addr_for_network`, `cache_timestamp`, `build_cache`,
   `receive_getaddr`, `receive_addr`, `receive_addr_announce`, `relay_count`, `select_relay_peers`,
   `self_announce`, `on_connect`, `on_disconnect`

4. `statistics.rs` (REWRITE) — Replace all existing stats with `SimulationStatistics`,
   `NodeStatistics`, `StaleAddressStats`. Remove all tx/Erlay message tracking.

5. `fingerprint.rs` (NEW) — `FingerprintAnalyzer` with `record`, `analyze`, `similarity`

6. `network.rs` (REWRITE) — Delete `Link` struct and all existing code. Write new `NetworkMessage`
   enum, then `Network` with `add_node`, `remove_node`, `connect`, `suitable_outbound_candidates`

7. `simulator.rs` (PARTIAL REWRITE — keep event queue infrastructure) — Keep `ScheduledEvent`
   struct, its `Ord`/`PartialOrd`/`From` impls, `get_next_event`, `get_next_event_time`, and
   `add_event` (drop the network-latency branch — latency is not modeled). Replace `Event` enum
   entirely (new variants: `SendMessage`, `NodeJoin`, `NodeLeave`, `SelfAnnounce`). Rewrite
   `Simulator` struct and all its methods: `new`, `build_initial_network`, `process`, `run`,
   `schedule_churn`. Remove `schedule_set_reconciliation`, `get_random_nodeid`, `cached_node_id`.

8. `lib.rs` (REWRITE) — Update module declarations (add `address`, `addrman`, `fingerprint`;
   remove `txreconciliation`, `graph`). Replace `SimulationParameters` and `OutputResult` with
   `SimulationConfig`. Remove `MAX_OUTBOUND_CONNECTIONS` and `SECS_TO_NANOS`.

9. `cli.rs` (REWRITE — keep the clap `Parser` derive pattern and `--output-file`/`--seed`/
   `--verbose` plumbing) — Replace all arguments with the new CLI spec.

10. `main.rs` (REWRITE — keep logger setup and CSV output logic) — Replace the single-transaction
    event loop with the daily simulation loop: parse CLI → build `SimulationConfig` → run simulator
    → collect and display statistics → write CSV.

---

## Files

| File | Action | What to keep | What changes |
|------|--------|--------------|--------------|
| `hyper-lib/src/txreconciliation.rs` | **DELETE** | — | Removed entirely |
| `hyper-lib/src/graph.rs` | **DELETE** | — | Removed entirely |
| `hyper-lib/src/address.rs` | **NEW** | — | `AddressId`, `NetworkType`, `Address`, `AddressRegistry` |
| `hyper-lib/src/addrman.rs` | **NEW** | — | `AddrmanEntry`, `Addrman` |
| `hyper-lib/src/fingerprint.rs` | **NEW** | — | `FingerprintAnalyzer` |
| `hyper-lib/src/node.rs` | **REWRITE** | `NodeId` type alias, `debug_log!` macro | Everything else replaced: new structs, new message types, new methods |
| `hyper-lib/src/network.rs` | **REWRITE** | Nothing | `Link` deleted; `NetworkMessage` replaced; `Network` rewritten for dynamic topology |
| `hyper-lib/src/simulator.rs` | **PARTIAL REWRITE** | `ScheduledEvent` + queue ordering impls; `get_next_event`, `get_next_event_time`, `add_event` | `Event` enum replaced; `Simulator` struct and all methods rewritten; Erlay/latency/reconciliation code removed |
| `hyper-lib/src/statistics.rs` | **REWRITE** | Nothing | All tx/Erlay tracking replaced with address-relay stats |
| `hyper-lib/src/lib.rs` | **REWRITE** | Module declaration pattern | Module list updated; `SimulationParameters`/`OutputResult` replaced by `SimulationConfig` |
| `hyperion/src/cli.rs` | **REWRITE** | `clap` derive pattern; `--output-file`/`--seed`/`--verbose` | All domain arguments replaced |
| `hyperion/src/main.rs` | **REWRITE** | Logger setup; CSV serialization | Event loop replaced with daily simulation loop |

---

## CLI

```
hyperion-addr [OPTIONS]

Network:
  --onion <N>           Onion-only nodes [default: 1000]
  --clearnet <N>        Clearnet-only nodes [default: 8000]
  --dual-stack <N>      Dual-stack nodes (2 addresses each) [default: 1000]
  --reachable-clearnet-pct <N>  % of clearnet addresses accepting inbound [default: 15]
  --reachable-onion-pct <N>     % of onion addresses accepting inbound [default: 50]
  --outbounds <N>       Outbound connections per node [default: 8]

Simulation:
  --days <N>            Days to simulate [default: 30]
  --joins-per-day <N>   Nodes joining per day [default: 100]
  --leaves-per-day <N>  Nodes leaving per day [default: 100]
  --warm-start          Pre-populate addrmans at startup [default]
  --cold-start          Start with empty addrmans

Algorithm:
  --cache-algo <ALG>    GETADDR cache timestamp algorithm [default: current]
                          current        Real timestamps from addrman
                          fixed-offset   now - rand(8,12) days per address
                          network-based  Same network → real; cross-network → now - 5 days

Output:
  --output-file <PATH>  CSV output file
  -v, --verbose         Verbose output

Reproducibility:
  -s, --seed <N>        RNG seed
```

**Address count:** `onion + clearnet + (2 × dual-stack)`. Example: 1000 + 8000 + 1000×2 = 11,000 addresses from 10,000 nodes.

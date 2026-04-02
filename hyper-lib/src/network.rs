use std::collections::{BTreeMap, HashMap, HashSet};

use rand::seq::IteratorRandom;
use rand::Rng;

use crate::address::{AddressId, AddressRegistry, NetworkType};
use crate::addrman::Addrman;
use crate::node::{Event, GetaddrCacheAlgorithm, Node, NodeId};
use crate::statistics::NodeStatistics;

pub struct Network {
    pub nodes: HashMap<NodeId, Node>,
    pub registry: AddressRegistry,
    next_node_id: NodeId,
    next_addr_id: u64,
}

impl Network {
    pub fn new() -> Self {
        Network {
            nodes: HashMap::new(),
            registry: AddressRegistry::new(),
            next_node_id: 0,
            next_addr_id: 0,
        }
    }

    /// Add a new node with the given network types and reachability.
    /// Connects it to `n_outbound` randomly selected existing reachable peers.
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

        let addresses: Vec<AddressId> = networks
            .iter()
            .map(|&net| {
                let addr = AddressId {
                    id: self.next_addr_id,
                    network: net,
                };
                self.next_addr_id += 1;
                self.registry.register(node_id, addr, reachable_on.contains(&net));
                addr
            })
            .collect();

        log::trace!(
            target: "hyper_lib::topology",
            "t={now} add_node id={node_id} networks={networks:?} reachable={reachable_on:?}"
        );

        let node = Node {
            node_id,
            addresses: addresses.clone(),
            reachable_networks: reachable_on,
            out_peers: BTreeMap::new(),
            in_peers: BTreeMap::new(),
            addrman: Addrman::new(),
            getaddr_cache: HashMap::new(),
            cache_algo,
            node_statistics: NodeStatistics::default(),
        };
        self.nodes.insert(node_id, node);

        let mut events = vec![];
        let candidates =
            self.suitable_outbound_candidates(node_id, &addresses, rng, n_outbound);
        for peer_addr in candidates {
            events.extend(self.connect(node_id, peer_addr, now));
        }

        (node_id, events)
    }

    /// Remove a node: notify its peers of the disconnect and mark its addresses inactive.
    /// Returns NodeReconnect events for any peer that lost an outbound connection.
    pub fn remove_node(&mut self, node_id: NodeId, now: u64) -> Vec<Event> {
        self.registry.deactivate_node(node_id);
        let peers: Vec<AddressId> = {
            let node = &self.nodes[&node_id];
            node.out_peers
                .keys()
                .chain(node.in_peers.keys())
                .copied()
                .collect()
        };
        log::debug!(
            target: "hyper_lib::topology",
            "t={now} remove_node id={node_id} peers={}",
            peers.len()
        );
        let mut events = vec![];
        for peer_addr in peers {
            let peer_node_id = self.node_id_for_addr(peer_addr);
            let departing_addr = self.own_addr_of(node_id, peer_addr.network);
            if let Some(peer_node) = self.nodes.get_mut(&peer_node_id) {
                events.extend(peer_node.on_disconnect(departing_addr, now));
            }
        }
        self.nodes.remove(&node_id);
        events
    }

    /// Find a new outbound peer for `node_id` on `network` and connect to it.
    /// Excludes nodes the caller is already connected to (inbound or outbound).
    pub fn reconnect_outbound(
        &mut self,
        node_id: NodeId,
        network: NetworkType,
        now: u64,
        rng: &mut impl Rng,
    ) -> Vec<Event> {
        let already_connected: HashSet<AddressId> = {
            let node = &self.nodes[&node_id];
            node.out_peers.keys().chain(node.in_peers.keys()).copied().collect()
        };
        let candidate = self
            .registry
            .addresses
            .values()
            .filter(|addr| {
                addr.is_reachable
                    && addr.owner_node != node_id
                    && addr.is_active
                    && addr.id.network == network
                    && !already_connected.contains(&addr.id)
            })
            .map(|addr| addr.id)
            .choose(rng);

        if let Some(peer_addr) = candidate {
            log::debug!(
                target: "hyper_lib::topology",
                "t={now} reconnect_outbound node={node_id} net={network:?} → {peer_addr:?}"
            );
            self.connect(node_id, peer_addr, now)
        } else {
            log::debug!(
                target: "hyper_lib::topology",
                "t={now} reconnect_outbound node={node_id} net={network:?} — no candidate found"
            );
            vec![]
        }
    }

    fn connect(&mut self, from_node: NodeId, to_addr: AddressId, now: u64) -> Vec<Event> {
        let from_addr = self.own_addr_of(from_node, to_addr.network);
        let to_node = self.node_id_for_addr(to_addr);
        log::debug!(
            target: "hyper_lib::topology",
            "t={now} connect node={from_node} → node={to_node} (net={:?})",
            to_addr.network
        );
        let mut events = vec![];
        events.extend(
            self.nodes
                .get_mut(&from_node)
                .unwrap()
                .on_connect(to_addr, true, now),
        );
        events.extend(
            self.nodes
                .get_mut(&to_node)
                .unwrap()
                .on_connect(from_addr, false, now),
        );
        events
    }

    /// Top up a node's outbound connections to `target` if it currently has fewer.
    /// Safe to call after the network is built; excludes already-connected peers.
    pub fn top_up_outbounds(
        &mut self,
        node_id: NodeId,
        target: usize,
        now: u64,
        rng: &mut impl Rng,
    ) -> Vec<Event> {
        let current = self.nodes[&node_id].out_peers.len();
        let needed = target.saturating_sub(current);
        if needed == 0 {
            return vec![];
        }
        let already_connected: HashSet<AddressId> = {
            let node = &self.nodes[&node_id];
            node.out_peers.keys().chain(node.in_peers.keys()).copied().collect()
        };
        let own_networks: HashSet<NetworkType> =
            self.nodes[&node_id].addresses.iter().map(|a| a.network).collect();
        let candidates: Vec<AddressId> = self
            .registry
            .addresses
            .values()
            .filter(|addr| {
                addr.is_reachable
                    && addr.owner_node != node_id
                    && addr.is_active
                    && own_networks.contains(&addr.id.network)
                    && !already_connected.contains(&addr.id)
            })
            .map(|addr| addr.id)
            .choose_multiple(rng, needed);
        let mut events = vec![];
        for peer_addr in candidates {
            events.extend(self.connect(node_id, peer_addr, now));
        }
        events
    }

    /// Return up to `n` addresses from reachable nodes whose network overlaps
    /// with at least one of `own_addresses`, excluding the joining node itself.
    fn suitable_outbound_candidates(
        &self,
        joining: NodeId,
        own_addresses: &[AddressId],
        rng: &mut impl Rng,
        n: usize,
    ) -> Vec<AddressId> {
        let own_networks: HashSet<NetworkType> =
            own_addresses.iter().map(|a| a.network).collect();
        let eligible: Vec<AddressId> = self
            .registry
            .addresses
            .values()
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

    pub fn node_id_for_addr(&self, addr: AddressId) -> NodeId {
        self.registry.addresses[&addr].owner_node
    }

    fn own_addr_of(&self, node_id: NodeId, network: NetworkType) -> AddressId {
        self.nodes[&node_id]
            .addresses
            .iter()
            .find(|a| a.network == network)
            .copied()
            .expect("node has no address on the given network")
    }
}

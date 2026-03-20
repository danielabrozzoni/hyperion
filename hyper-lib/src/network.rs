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
        for peer_addr in peers {
            let peer_node_id = self.node_id_for_addr(peer_addr);
            let departing_addr = self.own_addr_of(node_id, peer_addr.network);
            if let Some(peer_node) = self.nodes.get_mut(&peer_node_id) {
                peer_node.on_disconnect(departing_addr, now);
            }
        }
        self.nodes.remove(&node_id);
        vec![]
    }

    fn connect(&mut self, from_node: NodeId, to_addr: AddressId, now: u64) -> Vec<Event> {
        let from_addr = self.own_addr_of(from_node, to_addr.network);
        let to_node = self.node_id_for_addr(to_addr);
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

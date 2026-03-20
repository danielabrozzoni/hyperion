use std::collections::{BTreeMap, HashMap, HashSet};

use rand::seq::IteratorRandom;
use rand::Rng;
use rand_distr::{Distribution, Exp};

use crate::address::{AddressId, AddressRegistry, NetworkType};
use crate::addrman::{Addrman, AddrmanEntry};
use crate::statistics::NodeStatistics;

pub type NodeId = usize;

macro_rules! debug_log {
    ($time:tt, $id:expr, $($arg:tt)*)
    =>
    (log::debug!("{}: [Node: {}] {}", $time, $id, &format!($($arg)*)));
}

const GETADDR_CACHE_LIFETIME_BASE: u64 = 21 * 3600;
const GETADDR_CACHE_LIFETIME_RAND: u64 = 6 * 3600;
const SELF_ANNOUNCE_DELAY: u64 = 30;
const DAYS: u64 = 86400;
const HOURS: u64 = 3600;

pub struct Node {
    pub node_id: NodeId,
    pub addresses: Vec<AddressId>,
    pub reachable_networks: HashSet<NetworkType>,

    pub out_peers: BTreeMap<AddressId, Peer>,
    pub in_peers: BTreeMap<AddressId, Peer>,

    pub addrman: Addrman,
    pub getaddr_cache: HashMap<NetworkType, GetaddrCache>,
    pub cache_algo: GetaddrCacheAlgorithm,

    pub node_statistics: NodeStatistics,
}

pub struct Peer {
    pub addr: AddressId,
    pub getaddr_sent: bool,
    pub getaddr_recvd: bool,
}

/// GETADDR cache for one network type. Timestamps are frozen at build time.
pub struct GetaddrCache {
    pub entries: Vec<AddrPayload>,
    pub expires_at: u64,
}

#[derive(Clone, Copy)]
pub enum GetaddrCacheAlgorithm {
    /// Real timestamp from addrman (Bitcoin Core baseline).
    Current,
    /// now - rand_uniform(8, 12) days, sampled independently per address.
    FixedOffset,
    /// Same network as the GETADDR requester → real timestamp; cross-network → now - 5 days.
    NetworkBased,
}

#[derive(Clone)]
pub struct AddrPayload {
    pub address: AddressId,
    pub timestamp: u64,
}

pub enum NetworkMessage {
    GetAddr,
    Addr(Vec<AddrPayload>),
    AddrAnnounce(Vec<AddrPayload>),
}

pub enum Event {
    SendMessage { from: AddressId, to: AddressId, msg: NetworkMessage, at: u64 },
    NodeJoin { at: u64 },
    NodeLeave { node_id: NodeId, at: u64 },
    SelfAnnounce { node_id: NodeId, peer_addr: AddressId, at: u64 },
}

impl Node {
    fn own_addr_for_network(&self, network: NetworkType) -> AddressId {
        self.addresses
            .iter()
            .find(|a| a.network == network)
            .copied()
            .expect("connected on a network we don't have an address for")
    }

    fn peer(&self, addr: AddressId) -> &Peer {
        self.out_peers
            .get(&addr)
            .or_else(|| self.in_peers.get(&addr))
            .expect("peer not found")
    }

    fn peer_mut(&mut self, addr: AddressId) -> &mut Peer {
        if let Some(p) = self.out_peers.get_mut(&addr) {
            return p;
        }
        self.in_peers.get_mut(&addr).expect("peer not found")
    }

    fn cache_timestamp(
        &self,
        entry: &AddrmanEntry,
        cache_network: NetworkType,
        now: u64,
        rng: &mut impl Rng,
    ) -> u64 {
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

    fn build_cache(&mut self, network: NetworkType, now: u64, rng: &mut impl Rng) {
        let selected = self.addrman.get_addr(now, rng);
        let entries = selected
            .into_iter()
            .map(|e| AddrPayload {
                address: e.address,
                timestamp: self.cache_timestamp(e, network, now, rng),
            })
            .collect();

        self.getaddr_cache.insert(
            network,
            GetaddrCache {
                entries,
                expires_at: now
                    + GETADDR_CACHE_LIFETIME_BASE
                    + rng.gen_range(0..GETADDR_CACHE_LIFETIME_RAND),
            },
        );
    }

    pub fn receive_getaddr(
        &mut self,
        from: AddressId,
        now: u64,
        rng: &mut impl Rng,
    ) -> Vec<Event> {
        assert!(
            self.reachable_networks.contains(&from.network),
            "received GETADDR on network {:?} but node is not reachable on it",
            from.network
        );

        let peer = self.peer_mut(from);
        assert!(
            !peer.getaddr_recvd,
            "peer {from:?} sent GETADDR twice on the same connection"
        );
        peer.getaddr_recvd = true;

        let network = from.network;
        if self
            .getaddr_cache
            .get(&network)
            .map_or(true, |c| now >= c.expires_at)
        {
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

    pub fn receive_addr(&mut self, addrs: Vec<AddrPayload>, now: u64) {
        const PENALTY: u64 = 2 * HOURS;
        for payload in addrs {
            self.addrman
                .add(payload.address, payload.timestamp, PENALTY, now);
        }
    }

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
            let penalty = if payload.address == from { 0 } else { 2 * HOURS };
            self.addrman
                .add(payload.address, payload.timestamp, penalty, now);
        }

        if batch_size > 10 {
            return events;
        }

        let peer_sent_getaddr = self.peer(from).getaddr_sent;

        for payload in addrs {
            if payload.timestamp < now.saturating_sub(10 * 60) {
                continue;
            }
            if peer_sent_getaddr {
                continue;
            }
            if !registry.is_active(payload.address) {
                continue;
            }

            let n_relay = self.relay_count(payload.address, registry, rng);
            let targets = self.select_relay_peers(from, n_relay, rng);
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

    fn relay_count(
        &self,
        addr: AddressId,
        registry: &AddressRegistry,
        rng: &mut impl Rng,
    ) -> usize {
        if registry.is_reachable(addr) {
            2
        } else {
            rng.gen_range(1..=2)
        }
    }

    fn select_relay_peers(
        &self,
        exclude: AddressId,
        n: usize,
        rng: &mut impl Rng,
    ) -> Vec<AddressId> {
        self.out_peers
            .keys()
            .chain(self.in_peers.keys())
            .filter(|&&p| p != exclude)
            .copied()
            .choose_multiple(rng, n)
    }

    pub fn self_announce(
        &mut self,
        peer_addr: AddressId,
        now: u64,
        rng: &mut impl Rng,
    ) -> Vec<Event> {
        let mut events = vec![];

        let own_addr = self
            .addresses
            .iter()
            .find(|a| {
                a.network == peer_addr.network && self.reachable_networks.contains(&a.network)
            })
            .copied();

        if let Some(addr) = own_addr {
            events.push(Event::SendMessage {
                from: addr,
                to: peer_addr,
                msg: NetworkMessage::AddrAnnounce(vec![AddrPayload {
                    address: addr,
                    timestamp: now,
                }]),
                at: now,
            });
        }

        let next = now + sample_exponential(rng, 24 * HOURS);
        events.push(Event::SelfAnnounce {
            node_id: self.node_id,
            peer_addr,
            at: next,
        });

        events
    }

    pub fn on_connect(
        &mut self,
        peer_addr: AddressId,
        is_outbound: bool,
        now: u64,
    ) -> Vec<Event> {
        let peer = Peer {
            addr: peer_addr,
            getaddr_sent: false,
            getaddr_recvd: false,
        };
        let mut events = vec![];

        if is_outbound {
            self.out_peers.insert(peer_addr, peer);
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

        events.push(Event::SelfAnnounce {
            node_id: self.node_id,
            peer_addr,
            at: now + SELF_ANNOUNCE_DELAY,
        });

        events
    }

    pub fn on_disconnect(&mut self, peer_addr: AddressId, now: u64) {
        self.out_peers.remove(&peer_addr);
        self.in_peers.remove(&peer_addr);
        if let Some(entry) = self.addrman.entries.get_mut(&peer_addr) {
            entry.record_connected(now);
        }
    }

    pub fn on_connect_failed(&mut self, _peer_addr: AddressId, _now: u64) {}
}

fn sample_exponential(rng: &mut impl Rng, mean: u64) -> u64 {
    let exp = Exp::new(1.0 / mean as f64).unwrap();
    exp.sample(rng) as u64
}

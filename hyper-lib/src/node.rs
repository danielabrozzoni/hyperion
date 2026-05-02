use std::collections::{BTreeMap, HashMap, HashSet};

use rand::seq::IteratorRandom;
use rand::Rng;

use crate::address::{AddressId, AddressRegistry, NetworkType};
use crate::addrman::{Addrman, AddrmanEntry};
use crate::statistics::NodeStatistics;

pub type NodeId = usize;

macro_rules! protocol_log {
    ($time:expr, $id:expr, $($arg:tt)*) => {
        log::debug!(target: "hyper_lib::protocol", "t={} node={} {}", $time, $id, format!($($arg)*));
    };
}

macro_rules! protocol_trace {
    ($time:expr, $id:expr, $($arg:tt)*) => {
        log::trace!(target: "hyper_lib::protocol", "t={} node={} {}", $time, $id, format!($($arg)*));
    };
}

macro_rules! topology_log {
    ($time:expr, $id:expr, $($arg:tt)*) => {
        log::debug!(target: "hyper_lib::topology", "t={} node={} {}", $time, $id, format!($($arg)*));
    };
}

macro_rules! topology_trace {
    ($time:expr, $id:expr, $($arg:tt)*) => {
        log::trace!(target: "hyper_lib::topology", "t={} node={} {}", $time, $id, format!($($arg)*));
    };
}

const GETADDR_CACHE_LIFETIME_BASE: u64 = 21 * 3600;
const GETADDR_CACHE_LIFETIME_RAND: u64 = 6 * 3600;
const DAYS: u64 = 86400;
const HOURS: u64 = 3600;
/// Per-hop relay delay (seconds). Models Bitcoin Core's ~30 s addr-send batching window.
/// With the 10-minute freshness filter this caps relay chains at ~20 hops, matching
/// real-network behaviour where an addr reaches the whole network or dies within
/// 10 minutes of its timestamp.

pub struct Node {
    pub node_id: NodeId,
    pub addresses: Vec<AddressId>,
    /// Networks on which this node accepts inbound connections (has a listener).
    pub reachable_networks: HashSet<NetworkType>,
    /// Networks this node initiates outbound connections to.
    /// Tor-proxy nodes have {Clearnet, Tor}; -onlynet=onion nodes have {Tor}.
    pub outbound_networks: HashSet<NetworkType>,

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
    /// Addresses this peer already knows; checked before queuing relays to them.
    /// Reset before each periodic self-announcement so it always goes out.
    pub addr_known: HashSet<AddressId>,
    /// Addresses queued for relay to this peer, flushed every 30 s.
    /// Mirrors Bitcoin Core's per-peer m_addrs_to_send vector.
    pub pending_relay: Vec<AddrPayload>,
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

#[derive(Clone)]
pub enum NetworkMessage {
    GetAddr,
    Addr(Vec<AddrPayload>),
    AddrAnnounce(Vec<AddrPayload>),
}

#[derive(Clone)]
pub enum Event {
    SendMessage { from: AddressId, to: AddressId, msg: NetworkMessage, at: u64 },
    NodeJoin { at: u64 },
    NodeLeave { node_id: NodeId, at: u64 },
    /// An outbound peer disconnected; the node should find a replacement on that network.
    NodeReconnect { node_id: NodeId, network: NetworkType, at: u64 },
    /// Fire self-announcement to a single peer; rescheduled exponentially (~24 h mean).
    SelfAnnounce { node_id: NodeId, peer_addr: AddressId, at: u64 },
    /// Flush the pending relay queue to a peer; rescheduled every 30 s.
    /// Mirrors Bitcoin Core's per-peer m_next_addr_send timer in SendMessages.
    FlushAddrQueue { node_id: NodeId, peer_addr: AddressId, at: u64 },
}

impl Node {
    fn own_addr_for_network(&self, network: NetworkType) -> AddressId {
        self.addresses
            .iter()
            .find(|a| a.network == network)
            .copied()
            .expect("connected on a network we don't have an address for")
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
            GetaddrCacheAlgorithm::FixedOffset => now - rng.random_range(8..=12) * DAYS,
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
        protocol_trace!(now, self.node_id, "build_cache net={network:?} size={}", selected.len());
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
                    + rng.random_range(0..GETADDR_CACHE_LIFETIME_RAND),
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
        self.node_statistics.getaddr_received += 1;

        let network = from.network;
        let cache_hit = self
            .getaddr_cache
            .get(&network)
            .map_or(false, |c| now < c.expires_at);
        if !cache_hit {
            self.build_cache(network, now, rng);
        }
        let entries = self.getaddr_cache[&network].entries.clone();
        protocol_log!(
            now, self.node_id,
            "GETADDR from={from:?} → {} addrs (cache {})",
            entries.len(),
            if cache_hit { "hit" } else { "miss" }
        );
        vec![Event::SendMessage {
            from: self.own_addr_for_network(network),
            to: from,
            msg: NetworkMessage::Addr(entries),
            at: now,
        }]
    }

    pub fn receive_addr(&mut self, addrs: Vec<AddrPayload>, now: u64) {
        protocol_log!(now, self.node_id, "ADDR {} entries", addrs.len());
        self.node_statistics.addr_received += 1;
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
        protocol_log!(now, self.node_id, "AddrAnnounce {batch_size} entries from={from:?}");
        self.node_statistics.addr_announce_received += 1;
        let events = vec![];

        for payload in &addrs {
            let penalty = if payload.address == from { 0 } else { 2 * HOURS };
            self.addrman
                .add(payload.address, payload.timestamp, penalty, now);
        }

        // Mark the sender as knowing all addresses they just sent us.
        // Mirrors Bitcoin Core's AddAddressKnown() called in ProcessAddrs.
        if let Some(peer) = self.out_peers.get_mut(&from).or_else(|| self.in_peers.get_mut(&from)) {
            for payload in &addrs {
                peer.addr_known.insert(payload.address);
            }
        }

        if batch_size > 10 {
            return events;
        }

        let peer_sent_getaddr = self.out_peers.get(&from)
            .or_else(|| self.in_peers.get(&from))
            .map_or(false, |p| p.getaddr_sent);

        for payload in addrs {
            if payload.timestamp < now.saturating_sub(10 * 60) {
                continue;
            }
            if peer_sent_getaddr {
                continue;
            }

            let n_relay = self.relay_count(payload.address, registry, rng);
            let targets = self.select_relay_peers(from, n_relay, rng);
            for target in targets {
                // Skip peers that already know this address; mark known at queue time.
                // Mirrors Bitcoin Core's PushAddress preliminary addr_known check.
                if self.out_peers.get(&target).map_or(false, |p| p.addr_known.contains(&payload.address)) {
                    continue;
                }
                if let Some(peer) = self.out_peers.get_mut(&target) {
                    peer.addr_known.insert(payload.address);
                    peer.pending_relay.push(payload.clone());
                    protocol_trace!(now, self.node_id, "  queued relay addr={:?} → peer={target:?}", payload.address);
                }
            }
        }

        events
    }

    /// Flush all pending relay addresses to `peer_addr` in one batch message.
    /// Mirrors Bitcoin Core's m_next_addr_send flush in SendMessages.
    pub fn flush_addr_queue(&mut self, peer_addr: AddressId, now: u64) -> Vec<Event> {
        let peer = match self.out_peers.get_mut(&peer_addr).or_else(|| self.in_peers.get_mut(&peer_addr)) {
            Some(p) => p,
            None => return vec![],
        };
        let batch: Vec<AddrPayload> = peer.pending_relay.drain(..).collect();
        if batch.is_empty() {
            return vec![];
        }
        protocol_log!(now, self.node_id, "FlushAddrQueue {} addrs → peer={peer_addr:?}", batch.len());
        self.node_statistics.addr_announce_sent += 1;
        vec![Event::SendMessage {
            from: self.own_addr_for_network(peer_addr.network),
            to: peer_addr,
            msg: NetworkMessage::AddrAnnounce(batch),
            at: now,
        }]
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
            rng.random_range(1..=2)
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

    /// Send our own address to one peer. Mirrors Bitcoin Core's per-peer
    /// m_next_local_addr_send timer in SendMessages (net_processing.cpp:5448).
    /// Returns an empty Vec if this node is not reachable on the peer's network.
    pub fn self_announce_to_peer(&mut self, peer_addr: AddressId, now: u64) -> Vec<Event> {
        protocol_log!(now, self.node_id, "SelfAnnounce peer={peer_addr:?}");

        // Reset addr_known so the announcement always goes out even if the peer's
        // filter has seen our address before. Mirrors Bitcoin Core's m_addr_known->reset()
        // in SendMessages (net_processing.cpp:5457-5459).
        if let Some(peer) = self.out_peers.get_mut(&peer_addr).or_else(|| self.in_peers.get_mut(&peer_addr)) {
            peer.addr_known.clear();
        }

        let own_addr = self
            .addresses
            .iter()
            .find(|a| a.network == peer_addr.network && self.reachable_networks.contains(&a.network))
            .copied();

        if let Some(addr) = own_addr {
            self.node_statistics.addr_announce_sent += 1;
            vec![Event::SendMessage {
                from: addr,
                to: peer_addr,
                msg: NetworkMessage::AddrAnnounce(vec![AddrPayload {
                    address: addr,
                    timestamp: now,
                }]),
                at: now,
            }]
        } else {
            vec![]
        }
    }

    pub fn on_connect(
        &mut self,
        peer_addr: AddressId,
        is_outbound: bool,
        now: u64,
    ) -> Vec<Event> {
        topology_trace!(
            now, self.node_id,
            "{} peer={peer_addr:?}",
            if is_outbound { "→connect" } else { "←connect" }
        );
        let peer = Peer {
            addr: peer_addr,
            getaddr_sent: false,
            getaddr_recvd: false,
            addr_known: HashSet::new(),
            pending_relay: vec![],
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

        events
    }

    pub fn on_disconnect(&mut self, peer_addr: AddressId, now: u64) -> Vec<Event> {
        topology_log!(now, self.node_id, "disconnect peer={peer_addr:?}");
        let was_outbound = self.out_peers.remove(&peer_addr).is_some();
        self.in_peers.remove(&peer_addr);
        if let Some(entry) = self.addrman.entries.get_mut(&peer_addr) {
            entry.record_connected(now);
        }
        if was_outbound {
            vec![Event::NodeReconnect {
                node_id: self.node_id,
                network: peer_addr.network,
                at: now,
            }]
        } else {
            vec![]
        }
    }

    pub fn on_connect_failed(&mut self, _peer_addr: AddressId, _now: u64) {}
}


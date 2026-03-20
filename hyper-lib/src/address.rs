use std::collections::HashMap;

use crate::node::NodeId;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AddressId {
    pub id: u64,
    pub network: NetworkType,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NetworkType {
    Onion,
    Clearnet,
}

/// Stored in the global registry only — not in addrman.
pub struct Address {
    pub id: AddressId,
    pub owner_node: NodeId,
    pub is_active: bool,
    pub is_reachable: bool,
}

/// Global bookkeeping: maps addresses to nodes and vice versa.
/// Used to mark all addresses of a departing node as inactive.
pub struct AddressRegistry {
    pub addresses: HashMap<AddressId, Address>,
    pub node_addresses: HashMap<NodeId, Vec<AddressId>>,
}

impl AddressRegistry {
    pub fn new() -> Self {
        AddressRegistry {
            addresses: HashMap::new(),
            node_addresses: HashMap::new(),
        }
    }

    pub fn register(&mut self, node_id: NodeId, address_id: AddressId, is_reachable: bool) {
        self.addresses.insert(
            address_id,
            Address {
                id: address_id,
                owner_node: node_id,
                is_active: true,
                is_reachable,
            },
        );
        self.node_addresses
            .entry(node_id)
            .or_default()
            .push(address_id);
    }

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

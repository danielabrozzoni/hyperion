use std::collections::HashMap;

use rand::seq::IteratorRandom;
use rand::Rng;

use crate::address::AddressId;

const ADDRMAN_HORIZON: u64 = 30 * 24 * 3600;
const TIMESTAMP_UPDATE_INTERVAL: u64 = 20 * 60;

pub struct AddrmanEntry {
    pub address: AddressId,
    pub timestamp: u64,
}

impl AddrmanEntry {
    /// Only timestamp-based checks for now.
    pub fn is_terrible(&self, now: u64) -> bool {
        if self.timestamp > now + 600 {
            return true;
        }
        if now.saturating_sub(self.timestamp) > ADDRMAN_HORIZON {
            return true;
        }
        false
    }

    /// Called on disconnect. Mirrors AddrMan::Connected() in Bitcoin Core, which is called
    /// from FinalizeNode() (net_processing.cpp) when a peer connection is torn down.
    /// Updates nTime at most every 20 minutes.
    pub fn record_connected(&mut self, now: u64) {
        if now.saturating_sub(self.timestamp) > TIMESTAMP_UPDATE_INTERVAL {
            self.timestamp = now;
        }
    }
}

pub struct Addrman {
    pub entries: HashMap<AddressId, AddrmanEntry>,
}

impl Addrman {
    pub fn new() -> Self {
        Addrman {
            entries: HashMap::new(),
        }
    }

    /// Returns a random selection of non-terrible entries for a GETADDR response.
    /// Count is min(1000, addrman_size * 23%) — MAX_PCT_ADDR_TO_SEND = 23.
    pub fn get_addr(&self, now: u64, rng: &mut impl Rng) -> Vec<&AddrmanEntry> {
        let candidates: Vec<_> = self.entries.values().filter(|e| !e.is_terrible(now)).collect();
        let n = (candidates.len() * 23 / 100).min(1000);
        candidates.into_iter().choose_multiple(rng, n)
    }

    /// Add a new address or update an existing one.
    ///
    /// penalty: 0 for own-address self-announcements, 2h for all other received addresses.
    /// Stored value: incoming_timestamp - penalty.
    pub fn add(&mut self, address: AddressId, incoming_timestamp: u64, penalty: u64, now: u64) {
        let update_interval = if now.saturating_sub(incoming_timestamp) < 24 * 3600 {
            3600
        } else {
            86400
        };
        let stored_value = incoming_timestamp.saturating_sub(penalty);

        match self.entries.get_mut(&address) {
            None => {
                self.entries.insert(
                    address,
                    AddrmanEntry {
                        address,
                        timestamp: stored_value,
                    },
                );
            }
            Some(entry) => {
                if stored_value > entry.timestamp + update_interval {
                    entry.timestamp = stored_value;
                }
            }
        }
    }
}

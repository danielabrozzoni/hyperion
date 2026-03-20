# Address Relay Simulator — Specification

## Goal

We want to study how different GETADDR cache timestamp algorithms affect the Bitcoin P2P network. Two specific questions:

1. **Fingerprinting:** can an observer distinguish two nodes by their GETADDR responses? When two nodes return the same (address, timestamp) pairs, they can be identified as the same node across connections. We want to measure how much fingerprinting surface each algorithm exposes.

2. **Staleness:** do departed nodes' addresses persist in addrmans longer than they should? Some algorithms send artificially old timestamps, which slows down the natural aging-out of stale entries via `IsTerrible()`. We want to measure how long departed nodes stay visible in the network.

The simulation models a Bitcoin-like P2P network over days/weeks and measures both metrics under three different cache algorithms.

---

## What We Model (and What We Don't)

### Modeled

- GETADDR / ADDR exchange on outbound connection
- GETADDR response cache (21-27h, per-network scope)
- Three cache timestamp algorithms
- Self-announcements (node announces own address to peers, ~24h interval)
- Relay of self-announcements to 1-2 peers
- Network churn (nodes joining and leaving)
- Addrman with `IsTerrible()` filtering

### Not Modeled

| Mechanism | Reason |
|-----------|--------|
| **Feeler connections** | Feelers do not update `nTime` — the only addrman field that drives our metrics. No effect on fingerprinting or staleness. |
| **Rate limiting / token bucket** | We assume nodes don't spam addresses. The ≤10 batch relay gate is still enforced because it determines which messages propagate gossip. |
| **Block-relay connections** | Completely excluded from address relay in Bitcoin Core. |
| **ADDR_FETCH / MANUAL connections** | Not present in the simulated network topology. |
| **New vs tried addrman tables** | The distinction matters only for feeler target selection, which we don't model. We use a flat addrman map. |
| **Network latency** | Day-scale simulation; sub-second effects are irrelevant. |
| **VERSION/VERACK handshake** | Simplified: connection succeeds if the target node is active, fails if it has departed. |

---

## Bitcoin Core Behavior Reference

All behaviors in this section have been verified against the Bitcoin Core source. Only `OUTBOUND_FULL_RELAY` and `INBOUND` connections are simulated.

### Node Reachability

A node is **reachable on a network** if it listens for inbound connections on that network (`fListen == true`). Reachability is modeled per-network because a dual-stack node may accept inbound connections on its onion address but not its clearnet address (e.g., NAT'd at home but running a Tor hidden service).

In the simulation, reachability is a static property assigned per address at node creation from two CLI inputs: `--reachable-clearnet-pct` and `--reachable-onion-pct`.

Behavioral consequences, evaluated per network:

| Property | Reachable on network N | Unreachable on network N |
|----------|----------------------|------------------------|
| Accepts inbound connections on N | Yes | No |
| Answers GETADDR from N peers | Yes | No (silent drop) |
| Can be selected as outbound target on N | Yes | No |
| Self-announces own address on N | Yes | No |
| Relay count for its own address on N | 2 | 1 or 2 (low bit of SipHash) |

Unreachable nodes are outbound-only on that network: they initiate connections, send GETADDR, receive ADDR responses, and participate in gossip, but they cannot receive inbound connections, will not answer GETADDR, and do not self-announce.

### GETADDR Exchange

**Sending:** Outbound full-relay connections send GETADDR exactly once per connection, immediately on connect.

**Answering:**
- **Unreachable (non-listening) nodes do not answer GETADDR at all.** Only nodes with `fListen == true` respond. This prevents attackers from scraping addrman entries from NAT'd nodes via fake GETADDR requests.
- Each peer gets at most one GETADDR response per connection (`m_getaddr_recvd` flag). Subsequent GETADDR messages from the same peer are ignored.
- Response uses the cache if valid; otherwise rebuilds it.
- Cache is **per network** (`m_network_key`): each network type (IPv4, IPv6, Tor) has its own cache entry with lifetime `21h + rand(0, 6h)`. Requesters on the same network get the same response; requesters on different networks get independent caches.
- Cache contents: addresses with algorithm-modified timestamps, snapshotted at build time and frozen until expiry. Count is `min(1000, addrman_size × 23%)` — `MAX_PCT_ADDR_TO_SEND = 23` caps the response at 23% of addrman regardless of its size.

### Self-Announcement

- First announcement fires on the first `MaybeSendAddr` call where `m_next_local_addr_send == 0`, up to ~30s after connect. Gated on `fListen && !IBD`.
- Subsequently: per-peer timer `m_next_local_addr_send`, exponential distribution, ~24h mean.
- Recipients: all peers (both inbound and outbound full-relay).
- Timestamp: current time.
- **Network-segregated:** a node announces only the local address whose network matches the peer's. If either side is a privacy network (Tor/I2P) and the networks don't match, the address is skipped. Among remaining candidates, the highest-reachability one is picked.
- **Dual-stack consequence:** a dual-stack node announces its clearnet address to clearnet peers and its onion address to onion peers. The two never cross. From the perspective of address propagation, a dual-stack node behaves like two independent single-network nodes.

### Address Relay

When a node receives an address announcement:

1. For each address: update addrman — nTime is stored with a **2-hour penalty** subtracted from the incoming timestamp. Own-address self-announcements are exempt (`penalty = 0` when the sender announces its own address directly; `penalty = 2h` for all relayed addresses). The update fires only when the penalty-adjusted value would improve the stored nTime by at least `update_interval`:
   - Incoming timestamp < 24h old → `update_interval = 1h`
   - Incoming timestamp ≥ 24h old → `update_interval = 24h`

   Full condition: `stored_nTime < incoming_nTime - update_interval - penalty`
   Value stored after update: `incoming_nTime - penalty`

   Practical consequence: a relayed fresh address (timestamp ≈ now) lands in addrman as `now - 2h`. To displace an existing fresh entry, the incoming must be at least 3h newer (1h + 2h penalty). For a stale address (≥ 24h old), the bar is 26h. Direct self-announcements (penalty = 0) need only 1h improvement.
2. Relay decision — **all** conditions must hold:
   - Timestamp within last 10 minutes
   - The message contained ≤ 10 addresses total (GETADDR responses with 1000 addresses are never relayed)
   - Not received in response to our own GETADDR (`!peer.m_getaddr_sent`)
   - Address is routable

**Relay target count:**
- Reachable address → always 2 peers
- Unreachable address → 1 or 2 peers, determined by low bit of the address SipHash (deterministic per address)

**Relay peer selection:** SipHash of (per-node key, peer_id), key rotates every 24 hours. Top-N peers by hash value, source peer excluded.

**Practical consequence:** Only self-announcements (1 address, fresh timestamp) are relay-eligible. GETADDR responses populate addrman only — they never propagate further.

### nTime Update Rules

| Event | `last_try` | `attempts` | `last_success` | `nTime` |
|-------|-----------|-----------|---------------|---------|
| Connection attempt | +now | +1 | — | NO |
| Successful handshake | +now | reset 0 | +now | NO |
| Disconnect (after success) | — | — | — | YES, max every 20 min |
| Receive address announcement | — | — | — | YES, with hysteresis |

`Good()` intentionally does not update `nTime` to avoid leaking which peers we connect to.

### IsTerrible() Criteria

Addresses excluded from GETADDR responses if **any** of these hold:

| Condition | Threshold |
|-----------|-----------|
| Timestamp > 10 min in future | — |
| Timestamp > 30 days old | `ADDRMAN_HORIZON` |
| Never succeeded AND attempts ≥ 3 | `ADDRMAN_RETRIES` |
| attempts ≥ 10 AND last_success > 7 days ago | `ADDRMAN_MAX_FAILURES` + `ADDRMAN_MIN_FAIL` |

**Exception:** never terrible if `last_try` was within the last minute.

**Interaction with cache algorithms:** Sending `5 days ago` as timestamp does not immediately make an address terrible (threshold is 30 days). But it slows down nTime refresh across the network — peers won't update their stored timestamp because the incoming value is too old to pass the hysteresis check — causing departed nodes to linger longer before being filtered.

### Key Constants

```
ADDRMAN_HORIZON              = 30 days
ADDRMAN_RETRIES              = 3
ADDRMAN_MAX_FAILURES         = 10
ADDRMAN_MIN_FAIL             = 7 days
TIMESTAMP_UPDATE_INTERVAL    = 20 min      // Connected() nTime update cap
GETADDR_CACHE_LIFETIME       = 21h + rand(0-6h)
SELF_ANNOUNCE_INTERVAL       = ~24h mean   // exponential, per-peer
MAX_ADDR_IN_RESPONSE         = 1000
MAX_PCT_ADDR_TO_SEND         = 23          // GETADDR response capped at 23% of addrman size
ADDR_RELAY_BATCH_THRESHOLD   = 10
```

---

## Cache Algorithm Design

The three algorithms differ only in what timestamp is written into the cache at build time. Everything else — selection, expiry, per-network scope — is identical across all three.

| Algorithm | CLI | Timestamp in cache | Purpose |
|-----------|-----|-------------------|---------|
| `Current` | `current` | Addrman value, unchanged | Baseline — fingerprinting attack fully present |
| `FixedOffset` | `fixed-offset` | `now - rand_uniform(8, 12) days` per address | Broken baseline — uniform-stale timestamps cause departed nodes to persist |
| `NetworkBased` | `network-based` | Same network as the GETADDR requester → addrman value; cross-network → `now - 5 days` | Candidate fix — eliminates cross-network fingerprint signal |

**Why the cache is the right place to apply the algorithm:** Timestamps are frozen at cache build time and served unchanged to all requesters within the 21-27h window. This is what makes fingerprinting possible under `Current` (repeated queries return identical (address, timestamp) pairs), and what the other algorithms disrupt.

**Adding a new algorithm:** add a variant to the enum, one match arm in `cache_timestamp()`, and one CLI string mapping. Nothing else changes.

---

## Design Decisions

### Resolved

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Time unit | Seconds | Natural for Bitcoin timestamps; day-scale simulation needs no sub-second precision |
| Network latency | Not simulated | Irrelevant at day-scale |
| Initial addrman state | `--warm-start` (pre-populated, default) or `--cold-start` (empty) | Cold start is realistic but needs warm-up time; warm start tests algorithms faster |
| Connection model | Success if node active, failure if departed | Graceful vs timeout doesn't affect our metrics |
| Node departure | Mark address inactive in registry; addrmans age out naturally via `IsTerrible()` | No need to model disconnect mechanics |

### Open

These need answers before the simulation results are meaningful, but they don't block implementation — all are configurable via CLI or post-hoc analysis decisions.

> **TODO — Connection lifecycle model (blocks meaningful results)**
>
> The current model hard-codes a fixed number of outbound connections per node (`--outbounds`) and uses uniform random peer selection. This is not realistic. Real nodes:
> - connect to peers whose addresses they already know (from addrman, DNS seeds, etc.)
> - disconnect and reconnect over time based on peer quality, uptime, etc.
> - have different connection counts depending on node type and version
>
> Before the simulation results are meaningful, this needs to be designed from real-world data:
> - What is the real distribution of outbound connection counts per node type?
> - How long do connections typically last? What drives disconnections?
> - How do nodes select which addrman entries to connect to?
> - How does the churn rate (joins/leaves per day) map to real Bitcoin network turnover?
>
> **Action required:** collect empirical data from the Bitcoin network (e.g. from crawlers, peer monitoring, or published research) and design a connection model that reflects real behavior. The CLI inputs `--outbounds`, `--joins-per-day`, and `--leaves-per-day` are placeholders until this is resolved.

| Question | Considerations |
|----------|---------------|
| Fingerprint similarity threshold | What fraction of matching (address, timestamp) pairs counts as "same fingerprint"? Jaccard similarity? Exact match only? |
| Staleness measurement | Track addresses by age threshold (7 days, 30 days) and/or by actual departure status (address is in addrman but owner node has left)? |
| Network scale | Real Bitcoin: ~15k reachable, ~50k+ unreachable. Smaller scale is faster but less realistic. |
| Warm-up period | How many days before the network reaches steady state and statistics become meaningful? Should warm-up days be excluded from output? |

---

## Expected Output

```
=== Address Relay Simulation ===
Algorithm: network-based
Duration:  30 days
Nodes:     10,000 (1,000 onion-only + 8,000 clearnet-only + 1,000 dual-stack)
Addresses: 11,000 (2,000 onion + 9,000 clearnet)

--- Fingerprinting ---
Day  1:  0 node pairs with matching fingerprints (0.00%)
Day  7:  2 node pairs with matching fingerprints (0.002%)
Day 30:  8 node pairs with matching fingerprints (0.008%)

--- Staleness ---
Day 30:
  >7 days old:                    12,340 (15.4%)
  >30 days old:                    2,100  (2.6%)
  Departed nodes still present:      450 avg per node

--- Health ---
Avg addrman size:    8,234
Avg fresh (<24h):    1,203 (14.6%)
Network coverage:    82%

Results: results.csv
```

# Hyperion Simulator Architecture

This document explains how the Hyperion Bitcoin P2P network simulator works, covering all major structs, methods, and the simulation flow.

## Table of Contents

1. [Overview](#overview)
2. [Project Structure](#project-structure)
3. [Core Structs](#core-structs)
4. [Simulation Flow](#simulation-flow)
5. [Message Types and Protocol](#message-types-and-protocol)
6. [Erlay Implementation](#erlay-implementation)
7. [Configuration Options](#configuration-options)
8. [Extending the Simulator](#extending-the-simulator)

---

## Overview

Hyperion is a **discrete-time event-driven simulator** for Bitcoin transaction propagation. It simulates how a single transaction spreads through a network of nodes using different relay techniques:

- **Traditional Fanout**: INV -> GETDATA -> TX message flow
- **Erlay (BIP-330)**: Set reconciliation-based propagation

The simulator measures propagation time and bandwidth consumption under different configurations.

### Architecture Diagram

```
+----------------------------------------------------------+
|                    HYPERION CLI (main.rs)                |
|  - Parse arguments                                       |
|  - Run simulation loop                                   |
|  - Output statistics                                     |
+----------------------------------------------------------+
                            |
                            v
+----------------------------------------------------------+
|                    HYPER-LIB (Core Engine)               |
|  +----------------+  +----------------+  +--------------+|
|  | Simulator      |  | Network        |  | Statistics   ||
|  | - event queue  |  | - nodes        |  | - messages   ||
|  | - RNG          |  | - links        |  | - bytes      ||
|  +----------------+  +----------------+  +--------------+|
|  +----------------+  +--------------------------------+  |
|  | Node           |  | TxReconciliationState (Erlay) |  |
|  | - peers        |  | - reconciliation sets         |  |
|  | - timers       |  | - sketch computation          |  |
|  +----------------+  +--------------------------------+  |
+----------------------------------------------------------+
```

---

## Project Structure

```
hyperion/
├── Cargo.toml                 # Workspace definition
├── README.md
├── hyper-lib/                 # Core simulation library
│   └── src/
│       ├── lib.rs             # Module exports, SimulationParameters, OutputResult
│       ├── simulator.rs       # Event-driven simulation engine
│       ├── network.rs         # Network topology and message routing
│       ├── node.rs            # Individual node behavior
│       ├── txreconciliation.rs # Erlay set reconciliation
│       ├── statistics.rs      # Metric collection
│       └── graph.rs           # Network visualization (optional)
└── hyperion/                  # CLI application
    └── src/
        ├── main.rs            # Entry point and simulation loop
        ├── lib.rs             # CLI module export
        └── cli.rs             # Command-line argument parsing
```

---

## Core Structs

### Simulator (`simulator.rs`)

The central simulation engine that manages time, events, and network state.

```rust
pub struct Simulator {
    rng: Rc<RefCell<StdRng>>,              // Seeded RNG for reproducibility
    network: Network,                       // The simulated network
    event_queue: BinaryHeap<ScheduledEvent>,// Priority queue (ordered by time)
    cached_node_id: NodeId,                 // Cache for random node selection
}
```

**Key Methods:**

| Method | Purpose |
|--------|---------|
| `new(reachable, unreachable, outbounds, erlay, seed, latency)` | Create simulator with network topology |
| `schedule_set_reconciliation(time)` | Initialize Erlay reconciliation on all nodes |
| `add_event(event)` | Add event to queue (with optional latency) |
| `get_next_event()` | Pop next event from priority queue |
| `get_node(id)` / `get_node_mut(id)` | Access nodes by ID |
| `get_random_nodeid()` | Get random node for transaction source |

### Network (`network.rs`)

Manages network topology and node connections.

```rust
pub struct Network {
    nodes: Vec<Node>,                       // All nodes in the network
    links: HashMap<Link, u64>,              // Bidirectional links with latencies (ns)
    network_latency: bool,                  // Whether to simulate latency
    is_erlay: bool,                         // Global Erlay flag
    reachable_count: usize,                 // Count of reachable (listening) nodes
}

pub struct Link { a: NodeId, b: NodeId }    // Bidirectional network link
```

**Key Methods:**

| Method | Purpose |
|--------|---------|
| `new(reachable, unreachable, outbounds, erlay, rng, latency)` | Create network topology |
| `connect_unreachable(rng)` | Connect unreachable nodes to reachable ones |
| `connect_reachable(rng)` | Connect reachable nodes among themselves |
| `get_latency(link)` | Get latency for a link (sampled from LogNormal) |
| `get_statistics()` | Aggregate statistics across all nodes |

**Network Topology:**
- **Reachable nodes** (IDs: 0..R): Can accept incoming connections
- **Unreachable nodes** (IDs: R..R+U): Behind NAT, outbound-only
- Each node has `outbounds` connections (default: 8)
- Latencies: LogNormal distribution (mean=10ms, variance=20%)

### Node (`node.rs`)

Represents a single Bitcoin node with its peers and state.

```rust
pub struct Node {
    node_id: NodeId,
    is_reachable: bool,                     // Can accept inbound connections
    is_erlay: bool,                         // Erlay-enabled
    in_peers: BTreeMap<NodeId, Peer>,       // Inbound connections
    out_peers: BTreeMap<NodeId, Peer>,      // Outbound connections
    requested_transaction: bool,            // Already requested TX
    delayed_request: Option<NodeId>,        // Pending GETDATA (for inbound prioritization)
    known_transaction: bool,                // Already has the transaction
    inbounds_poisson_timer: PoissonTimer,   // Shared inbound announcement timer
    outbounds_poisson_timer: PoissonTimer,  // Per-peer outbound timers
    node_statistics: NodeStatistics,        // Message/byte counts
}

pub struct Peer {
    tx_announcement: TxAnnouncement,        // INV exchange state
    tx_reconciliation_state: Option<TxReconciliationState>, // Erlay state
}

enum TxAnnouncement { Sent, Received, Scheduled, None }
```

**Key Methods:**

| Method | Purpose |
|--------|---------|
| `broadcast_tx(time)` | Start propagating a transaction |
| `schedule_tx_announcement(time)` | Schedule INV messages to peers |
| `process_scheduled_announcement(peer, time)` | Send scheduled INV |
| `receive_message_from(peer, msg, time)` | Handle incoming messages |
| `add_request(peer)` | Queue GETDATA (prioritize outbounds) |
| `process_delayed_request(peer, time)` | Send delayed GETDATA |
| `process_scheduled_reconciliation(peer, time)` | Send REQRECON |
| `reset()` | Reset state for next simulation run |

### TxReconciliationState (`txreconciliation.rs`)

Manages Erlay set reconciliation state per peer.

```rust
pub struct TxReconciliationState {
    is_initiator: bool,                     // Reconciliation initiator (inbound peers)
    is_reconciling: bool,                   // Currently in reconciliation
    recon_set: bool,                        // Transaction in active reconciliation set
    delayed_set: bool,                      // Transaction pending (not yet available)
}

pub struct Sketch {
    tx_set: bool,                           // Whether peer knows the transaction
    d: usize,                               // Difference count for size calculation
}
```

**Key Methods:**

| Method | Purpose |
|--------|---------|
| `compute_sketch(remote_has_tx)` | Create sketch with local knowledge + difference |
| `compute_sketch_diff(sketch)` | Determine what to request/offer |
| `add_tx()` | Add transaction to delayed set |
| `make_delayed_available()` | Move from delayed to active set |
| `clear()` | Clear reconciliation state |

### Statistics (`statistics.rs`)

Tracks message counts and byte volumes.

```rust
pub struct NodeStatistics {
    inv: Data,          // INV messages
    get_data: Data,     // GETDATA messages
    tx: Data,           // TX messages
    reqrecon: Data,     // REQRECON messages (Erlay)
    sketch: Data,       // SKETCH messages (Erlay)
    reconcildiff: Data, // RECONCILDIFF messages (Erlay)
    bytes: Data,        // Total bytes
}

struct Data {
    from_inbounds: u64,
    from_outbounds: u64,
    to_inbounds: u64,
    to_outbounds: u64,
}
```

---

## Simulation Flow

### Initialization

```
main()
  -> Cli::parse()           // Parse command-line arguments
  -> Simulator::new()       // Create simulator
     -> Network::new()      // Create network topology
        -> Create nodes (reachable + unreachable)
        -> connect_unreachable()  // Connect unreachable -> reachable
        -> connect_reachable()    // Connect reachable nodes
        -> Sample link latencies
```

### Main Loop

For each simulation run (n times):

```
1. Pick random source node
   -> simulator.get_random_nodeid()

2. Initialize Erlay (if enabled)
   -> simulator.schedule_set_reconciliation(t=0)

3. Start transaction broadcast
   -> source_node.broadcast_tx(t=0)
   -> Generates initial events (scheduled announcements)

4. Process events until queue is empty:
   WHILE event_queue not empty:
     -> Pop event with lowest timestamp
     -> Match event type:
        - ReceiveMessageFrom(src, dst, msg)
          -> dst_node.receive_message_from(src, msg, time)
        - ProcessScheduledAnnouncement(src, dst)
          -> src_node.process_scheduled_announcement(dst, time)
        - ProcessDelayedRequest(target, peer)
          -> target_node.process_delayed_request(peer, time)
        - ProcessScheduledReconciliation(src, dst)
          -> src_node.process_scheduled_reconciliation(dst, time)
     -> Add generated events to queue

5. Record propagation times
   -> Time to reach X% of nodes
   -> Time to reach 100% of nodes

6. Reset all nodes
   -> node.reset() for each node
```

### Event Types

```rust
pub enum Event {
    ReceiveMessageFrom(NodeId, NodeId, NetworkMessage),  // src, dst, message
    ProcessScheduledAnnouncement(NodeId, NodeId),        // src, dst
    ProcessDelayedRequest(NodeId, NodeId),               // target, peer
    ProcessScheduledReconciliation(NodeId, NodeId),      // src, dst
}
```

---

## Message Types and Protocol

### NetworkMessage Enum

```rust
pub enum NetworkMessage {
    INV,                    // Transaction inventory announcement
    GETDATA,                // Request for transaction data
    TX,                     // The actual transaction
    REQRECON(bool),         // Reconciliation request (Erlay)
    SKETCH(Sketch),         // Set reconciliation sketch (Erlay)
    RECONCILDIFF(bool),     // Reconciliation difference (Erlay)
}
```

### Message Sizes

| Message | Size | Description |
|---------|------|-------------|
| INV | 36 bytes | 4-byte type + 32-byte hash |
| GETDATA | 36 bytes | Same as INV |
| TX | 192 bytes | Simplified transaction size |
| REQRECON | 0 bytes | Protocol overhead only |
| SKETCH | 4 * d bytes | d = difference count |
| RECONCILDIFF | 4 * m bytes | m = missing count |

### Traditional Fanout Flow

```
Source Node                           Peer Node
    |                                     |
    |-- [knows TX, schedules INV]         |
    |                                     |
    |-- [Poisson timer expires]           |
    |                                     |
    +-- INV -------------------------->   |
                                          |-- [doesn't have TX]
                                          |
    <-------------------------- GETDATA --+
    |                                     |
    |-- [has TX, sends it]                |
    |                                     |
    +-- TX --------------------------->   |
                                          |-- [now knows TX]
                                          |-- [broadcasts to its peers]
```

### Inbound Prioritization

Outbound peers are prioritized over inbound peers for requesting transactions:

```
Node receives INV from outbound peer:
  -> Immediately send GETDATA

Node receives INV from inbound peer:
  -> If already requesting from outbound: delay GETDATA by 2 seconds
  -> Otherwise: send GETDATA
```

This prevents an attacker from delaying transaction propagation by connecting to many nodes.

---

## Erlay Implementation

### Concept

Erlay (BIP-330) reduces bandwidth by:
1. Announcing transactions to only a small subset of peers (fanout)
2. Using set reconciliation to sync remaining peers

### Fanout Selection

```rust
// From node.rs
fn get_fanout_targets(&self) -> Vec<NodeId> {
    // Select OUTBOUND_FANOUT_DESTINATIONS outbound peers (default: 1)
    // Select INBOUND_FANOUT_DESTINATIONS_FRACTION of inbound peers (default: 10%)
}
```

### Erlay Message Flow

```
Source Node                           Peer Node
    |                                     |
    |-- [knows TX]                        |
    |-- [add to delayed_set for non-fanout peers]
    |                                     |
    |-- [8-second timer expires]          |
    |                                     |
    +-- REQRECON(has_tx) ------------->   |
                                          |-- compute_sketch()
                                          |
    <------------------------ SKETCH -----+
    |                                     |
    |-- compute_sketch_diff()             |
    |                                     |
    +-- [INV if they need it]             |
    +-- RECONCILDIFF(wants_tx) ------->   |
                                          |
    <---------------------------- TX -----+  (if we wanted it)
```

### Two-Phase Transaction Availability

To prevent probing attacks:

1. **Delayed Set**: Transaction added but not yet available for reconciliation
2. **Active Set**: Transaction available (moved from delayed after trickle interval)

```rust
// When broadcast_tx() is called:
peer.add_tx_to_reconcile()  // -> adds to delayed_set

// At next trickle interval:
peer.make_delayed_available()  // -> moves to recon_set

// After reconciliation:
peer.clear()  // -> clears both sets
```

### Reconciliation Schedule

- Reconciliation happens every `RECON_REQUEST_INTERVAL / num_peers` seconds
- Default interval: 8 seconds total, spread across all peers
- Connection initiator (outbound peer from remote's perspective) initiates reconciliation

---

## Configuration Options

### CLI Arguments

```bash
hyperion [OPTIONS]

Options:
  -r, --reachable <N>        Number of reachable nodes [default: 10000]
  -u, --unreachable <N>      Number of unreachable nodes [default: 100000]
  -o, --outbounds <N>        Outbound connections per node [default: 8]
  -l, --log-level <LEVEL>    Log verbosity [default: info]
  -p, --percentile <N>       Percentile target for stats [default: 90]
  -e, --erlay                Enable Erlay
  -s, --seed <N>             RNG seed for reproducibility
  --no-latency               Disable network latency simulation
  -n <N>                     Number of simulation runs [default: 1]
  --output-file <PATH>       CSV output file
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `INBOUND_INVENTORY_BROADCAST_INTERVAL` | 5 | Seconds (shared timer for inbounds) |
| `OUTBOUND_INVENTORY_BROADCAST_INTERVAL` | 2 | Seconds (per-peer for outbounds) |
| `OUTBOUND_FANOUT_DESTINATIONS` | 1 | Number of outbound fanout peers |
| `INBOUND_FANOUT_DESTINATIONS_FRACTION` | 0.1 | Fraction of inbound fanout peers |

### Hard-Coded Parameters

| Parameter | Value | Location |
|-----------|-------|----------|
| `NONPREF_PEER_TX_DELAY` | 2 seconds | `node.rs` |
| `RECON_REQUEST_INTERVAL` | 8 seconds | `node.rs` |
| `NET_LATENCY_MEAN` | 10 ms | `network.rs` |
| `NET_LATENCY_VARIANCE` | 20% | `network.rs` |

---

## Extending the Simulator

### Adding a New Message Type

1. Add variant to `NetworkMessage` in `network.rs`:
```rust
pub enum NetworkMessage {
    // ... existing variants
    MyNewMessage(MyData),
}
```

2. Add size calculation in `NetworkMessage::size()`:
```rust
pub fn size(&self) -> u64 {
    match self {
        // ... existing cases
        Self::MyNewMessage(_) => MY_MESSAGE_SIZE,
    }
}
```

3. Handle the message in `Node::receive_message_from()` in `node.rs`:
```rust
pub fn receive_message_from(&mut self, peer_id: NodeId, msg: NetworkMessage, time: u64) -> Vec<Event> {
    match msg {
        // ... existing cases
        NetworkMessage::MyNewMessage(data) => {
            // Handle the message
        }
    }
}
```

4. Add statistics tracking in `statistics.rs` if needed.

### Adding a New Simulation Property

1. Add state to the relevant struct (typically `Node` or `Peer`)
2. Initialize the state in the constructor
3. Update `Node::reset()` to reset the state between runs
4. Generate events in the appropriate methods
5. Handle the events in the main loop (`main.rs`)

### Adding New Statistics

1. Add fields to `NodeStatistics` in `statistics.rs`:
```rust
pub struct NodeStatistics {
    // ... existing fields
    my_metric: Data,
}
```

2. Update `NodeStatistics::update()` to track the metric
3. Update `NetworkStatistics::from()` to aggregate the metric
4. Update `OutputResult` in `lib.rs` to include the metric in output

### Modifying Propagation Behavior

Key locations:
- **Announcement scheduling**: `Node::schedule_tx_announcement()`
- **Fanout selection**: `Node::get_fanout_targets()`
- **Request prioritization**: `Node::add_request()`, `Node::process_delayed_request()`
- **Reconciliation**: `Node::process_scheduled_reconciliation()`

---

## Example Usage

### Basic Simulation

```bash
# Run with default settings (fanout only)
hyperion -r 1000 -u 10000 -n 5

# Run with Erlay enabled
hyperion -r 1000 -u 10000 -n 5 --erlay

# Reproducible run with seed
hyperion -r 1000 -u 10000 -n 5 -s 12345
```

### Comparing Fanout vs Erlay

```bash
# Same seed for fair comparison
hyperion -r 10000 -u 100000 -n 10 -s 42 --output-file fanout.csv
hyperion -r 10000 -u 100000 -n 10 -s 42 --erlay --output-file erlay.csv
```

### Tuning Erlay Parameters

```bash
OUTBOUND_FANOUT_DESTINATIONS=2 \
INBOUND_FANOUT_DESTINATIONS_FRACTION=0.2 \
hyperion -r 10000 -u 100000 --erlay -n 5
```

# Hyperion — Address Relay Simulator

Discrete-event simulator of Bitcoin P2P address relay. Studies fingerprinting
and staleness under three GETADDR cache timestamp algorithms.

This is a **rewrite** of the existing Hyperion transaction propagation
simulator. The existing code in `hyper-lib/` and `hyperion/` is being replaced.

## Key documents

- `doc/ADDRESS_RELAY_SPEC.md` — what we model, Bitcoin Core behavior reference,
  algorithm design. Read this before changing any protocol logic.
- `doc/ADDRESS_RELAY_IMPLEMENTATION_PLAN.md` — authoritative build spec: data
  structures, method signatures, initialization flow, file map.

## Workflow

After completing each step, commit with a message like `addr-relay step N: description`.
Do **not** add `CLAUDE.md` to commits.
Always pass `--no-gpg-sign` to `git commit` — the user does not want commits signed with GPG.

## Build and test

```bash
cargo build
cargo test
cargo run --bin hyperion-addr -- --help
```

## Workspace

```
hyper-lib/   library crate — all simulation logic goes here
hyperion/    binary crate — CLI entry point only
bitcoin/     Bitcoin Core source — reference only, do not modify
```

## What files get rewritten

See the Files table and Step 0 in `doc/ADDRESS_RELAY_IMPLEMENTATION_PLAN.md`.
Short version: `txreconciliation.rs` and `graph.rs` are deleted; everything
else in `hyper-lib/src/` and `hyperion/src/` is rewritten from scratch.

## Constraints

- All Bitcoin Core behavior in the spec is verified against source. Don't
  change protocol semantics without updating the spec first.
- Simulation time is in **seconds** (u64 Unix timestamps).
- `bitcoin/` is read-only reference material — never edit it.
- Documents live in `doc/`. Don't assume they are in the project root.

## Error logging

Whenever a bug or incorrect assumption is found in the plan or spec, add it to
the "Mistakes to avoid" section below. Include what was wrong, what is correct,
and the Bitcoin Core source reference if applicable.

- **Don't add comments explaining the absence of something.** If an event
  variant, match arm, or field doesn't exist, just don't include it. A comment
  like `// No RefreshGetaddrCache — rebuilt lazily` inside a match block is
  noise and confused the user. Either the thing exists or it doesn't.

- **Don't claim existing code is unreusable without reading it.** Before
  recommending a full rewrite, read the files. The old simulator had reusable
  infrastructure (`ScheduledEvent`, event queue, CLI plumbing) that was
  incorrectly declared useless.

- **`NetworkBased` cache algorithm:** `cache_timestamp` must take `cache_network`
  (the network the GETADDR came from) as a parameter and check
  `entry.address.network == cache_network`. Checking `self.addresses` instead is
  wrong — always true for dual-stack nodes so fake timestamps never fire.

- **`AddrMan::Connected()` fires at disconnect, not connect.** Called from
  `FinalizeNode()` (`net_processing.cpp:1737`) when a peer is torn down.
  `on_connect` does not update addrman at all.

- **Silent returns in simulation code should be assertions.** Nodes are
  well-behaved in this simulation. Unexpected protocol states (GETADDR on a
  non-listening network, duplicate GETADDR from same peer) are simulator bugs
  and should `assert!`, not silently return.

- **In-flight messages outlive their sender's connection.** An `AddrAnnounce`
  (or any message) can be scheduled at time T and processed after the sender
  has disconnected from the recipient. Do not `expect("peer not found")` when
  looking up the `from` peer inside message handlers — use `get().map_or(default)`
  instead, since the disconnect is a normal timing artifact, not a bug.

---

## Implementation Status

**Current step: Step 4 DONE — start Step 5**

### Completed

**Step 0 — File cleanup (DONE)**
- Deleted `hyper-lib/src/txreconciliation.rs`
- Deleted `hyper-lib/src/graph.rs`
- Removed `graphrs` and `itertools` from `hyper-lib/Cargo.toml`
- Removed `pub mod graph` and `pub mod txreconciliation` from `lib.rs`

Note: build is expected to be **broken** until Steps 1–10 are done. The existing
`node.rs` and `network.rs` still reference deleted `txreconciliation` items
(`TxReconciliationState`, `Sketch`) — these files are being fully rewritten in
Steps 3 and 6 respectively.

**Step 1 — `address.rs` (DONE)**
- Created `hyper-lib/src/address.rs` with `AddressId`, `NetworkType`, `Address`, `AddressRegistry`
- Added `pub mod address` to `lib.rs`
- No errors in the new file; only pre-existing errors from old node.rs/network.rs

**Step 4 — `statistics.rs` (DONE)**
- Rewrote `statistics.rs`: removed all Erlay/tx tracking (INV, GETDATA, TX, REQRECON, SKETCH, RECONCILDIFF)
- Added `NodeStatistics` (per-node addr relay message counters), `SimulationStatistics`, `FingerprintResult`, `StaleAddressStats`
- Removed placeholder `NodeStatistics` from `node.rs`; node.rs now imports it from `statistics`

### Remaining steps

1. ~~`address.rs`~~ (done)
2. ~~`addrman.rs`~~ (done)
3. ~~`node.rs`~~ (done) — Event/NetworkMessage/AddrPayload defined here to avoid circular deps
4. ~~`statistics.rs`~~ (done) — NodeStatistics expanded; SimulationStatistics/StaleAddressStats/FingerprintResult added
5. `fingerprint.rs` (NEW) — `FingerprintAnalyzer`
6. `network.rs` (REWRITE)
7. `simulator.rs` (PARTIAL REWRITE — keep event queue infrastructure)
8. `lib.rs` (REWRITE)
9. `cli.rs` (REWRITE — keep clap pattern, `--output-file`/`--seed`/`--verbose`)
10. `main.rs` (REWRITE — keep logger setup and CSV output)

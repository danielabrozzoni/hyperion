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

## Constraints

- All Bitcoin Core behavior in the spec is verified against source. Don't
  change protocol semantics without updating the spec first.
- Simulation time is in **seconds** (u64 Unix timestamps).
- `bitcoin/` is read-only reference material — never edit it.
- Documents live in `doc/`. Don't assume they are in the project root.

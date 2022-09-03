# Threshold Signature Scheme over P2P transport
This project implements [`rust-libp2p`](https://github.com/libp2p/rust-libp2p) transport for {t,n}-threshold signature schemes.

The protocol used here is [FROST (Flexible Round-Optimized Schnorr Threshold signatures)](https://datatracker.ietf.org/doc/html/rfc9591), which is a threshold signature scheme supporting both Ed25519 and secp256k1 curves. The implementation uses [`frost-ed25519`](https://crates.io/crates/frost-ed25519) and [`frost-secp256k1`](https://crates.io/crates/frost-secp256k1) libraries.

This codebase aims to follow the modular design of `rust-libp2p`, so could be repurposed to support other elliptic curves and signing schemes, assuming they are intended to be run in the [round-based](https://docs.rs/round-based/latest/round_based/index.html) flow of MPC.

## Acknowledgments

This project is inspired by and builds upon the architecture of [tss-libp2p](https://github.com/nulltea/tss-libp2p), which implements threshold ECDSA using the GG20 protocol. 

**Key differences from the original project:**
- **Protocol**: Uses FROST (Schnorr threshold signatures) instead of GG20 (ECDSA threshold signatures)
- **Curves**: Supports both Ed25519 and secp256k1 curves (original supports secp256k1 ECDSA)
- **Signature Format**: Generates Schnorr signatures with optional ECDSA format conversion
- **Architecture**: Adapts the same P2P networking and coordination framework (room abstraction, echo broadcast, etc.) to work with FROST protocols

The core networking layer, runtime coordination, and RPC interfaces follow similar patterns to the original project, making it easy to understand for those familiar with tss-libp2p.

## Project structure
- `crates/node`: MPC node daemon
  - `network`: libp2p networking stack (broadcast, discovery).
  - `runtime`: engine between network and application layers responsible for orchestrating MPC communication and pre-computation coordination.
  - `rpc`: [`jsonrpc`](https://github.com/paritytech/jsonrpc) server, client, and API trait.
  - `rpc-api`: implements API trait.
- `crates/tss`: application layer implementing FROST MPC protocols
  - `keygen`: FROST distributed key generation (DKG)
  - `keysign`: FROST threshold signing
- `crates/cli`: helpful CLI for deploying node and interacting with it over JsonRPC.

## Design principles

### The "room" abstraction
The underlying networking protocol is structured around the "room" abstraction. Each room is a separate Req-Resp channel over which parties synchronously run MPC (one at a time). To join a room, users must know its name and at least one other user to bootstrap if Kademlia discovery is used.

### Single Proposer; Multiple Joiners
Pre-computation coordination is encoded as session types (see [blog post](https://cathieyun.medium.com/bulletproof-multi-party-computation-in-rust-with-session-types-b3da6e928d5d)) and follows a predefined flow where one party broadcasts a computation proposal and other parties in the room can answer.

Along with the proposal specifying protocol by its id, the Proposer can include an arbitrary challenge serving as means of negotiation (e.g. ask to prove that party has a key share).

After the Proposer sourced enough Joiners, she broadcasts a start message specifying chosen parties and arbitrary arguments relevant for the MPC (e.g. message to be signed).

### Echo broadcast
To ensure reliable broadcast during computation, messages on wire are being echoed, i.e. echo broadcast: once messages from all known parties are received, relayers hash vector containing these messages along with their own ones and send it as an acknowledgment. Assuming relayers sort vectors in the same way (e.g. by party indexes) and all of them received consistent sets of messages, hashes will end up identical and broadcast reliability will be proven.

## Instructions

### Prerequisites
- Rust toolchain (latest stable recommended)
- Multiple terminal windows/sessions for running multiple nodes

### Setup a new node
The following command will setup a new node by generating a new keypair into the path specified by `--path` (pattern `:id` will be replaced with the peer_id) and create a peer config file at `--config-path` with the peer address set as `--multiaddr` and its RPC address as `--rpc-address`:

```bash
cargo run -p mpc-cli setup \
  --config-path ./peer_config0.json \
  --rpc-address 127.0.0.1:8080 \
  --multiaddr /ip4/127.0.0.1/tcp/4000 \
  --path ./data/:id/
```

**Note**: Make sure to populate the `boot_peers` array in each created config file for parties to be able to find each other through peer discovery.

### Run node
The following command will start an MPC node with the specified config and setup path (resolved by default using the `:id` pattern mentioned above). For peer discovery, you can use Kademlia (`--kademlia`), MDNS (`--mdns`), both, or neither:

```bash
cargo run -p mpc-cli node \
  --config-path ./config_peer0.json \
  --kademlia
```

### Run DKG (Distributed Key Generation)
The following command will propose to compute a new shared key with `--parties` (or `-n`) parties in the room `--room` (or `-r`) with threshold parameter `--threshold` (or `-t`) using the node with the specified RPC address `--address` (or `-a`):

```bash
# Using long options
cargo run -p mpc-cli keygen \
  --address ws://127.0.0.1:8090 \
  --room tss/0 \
  --threshold 2 \
  --parties 3

# Or using short options
cargo run -p mpc-cli keygen \
  -a ws://127.0.0.1:8090 \
  -r tss/0 \
  -t 2 \
  -n 3
```

### Run threshold signing
The following command will propose to jointly sign a message with `threshold+1` parties in the room `--room` (or `-r`) on behalf of a node with the specified RPC address `--address` (or `-a`):

```bash
# Using long options
cargo run -p mpc-cli sign \
  --address ws://127.0.0.1:8080 \
  --room tss/0 \
  --threshold 2 \
  --messages "hello"

# Or using short options
cargo run -p mpc-cli sign \
  -a ws://127.0.0.1:8080 \
  -r tss/0 \
  --threshold 2 \
  --messages "hello"
```

**Note**: The `--threshold` parameter specifies the minimum number of parties needed to sign. The actual signing will require `threshold + 1` parties to participate.

## Features
- **FROST threshold signatures**: Supports both Ed25519 and secp256k1 curves
- **Distributed Key Generation (DKG)**: Generate threshold keys without a trusted dealer
- **Threshold Signing**: Sign messages with threshold number of parties
- **P2P Networking**: Built on libp2p for decentralized communication
- **ECDSA Format Support**: Converts FROST Schnorr signatures to ECDSA format for compatibility with existing systems
- **Multiple Discovery Methods**: Supports Kademlia DHT and mDNS for peer discovery
- **Modular Architecture**: Easy to extend with additional curves and signing schemes

## Limitations (future features)
- Rooms need to be known in advance when deploying node.
  - The future goal is to have them activated inside the node dynamically.
- All rooms source from a single Kad-DHT, which isn't practical.
  - Either DHT-per-room or internal separation needed to be implemented.
- No key refresh/rotation protocol support available yet.

## Warning
**Do not use this in production.** Code here hasn't been audited and is likely not stable.  
It is no more that a prototype for learning and having fun doing it.

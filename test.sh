#!/bin/bash
echo "=== Stopping all nodes ==="
pkill -f "mpc-cli node"

echo "=== Building mpc-cli ==="
cargo build -p mpc-cli
echo "=== Starting 3 nodes ==="
./target/debug/mpc-cli node --config-path ./config_peer0.json --kademlia > /tmp/node0.log 2>&1 &
./target/debug/mpc-cli node --config-path ./config_peer1.json --kademlia > /tmp/node1.log 2>&1 &
./target/debug/mpc-cli node --config-path ./config_peer2.json --kademlia > /tmp/node2.log 2>&1 &
sleep 3

echo "=== Waiting 30 seconds for nodes to connect ==="
sleep 30

echo "=== Running keygen ==="
./target/debug/mpc-cli keygen --address ws://127.0.0.1:8090 --room tss/0 --threshold 2 --parties 3
echo "=== Keygen done ==="

echo "=== Running keysign ==="
./target/debug/mpc-cli sign --address ws://127.0.0.1:8090 --room tss/0 --threshold 2 --messages "hello"
echo "=== Keysign done ==="
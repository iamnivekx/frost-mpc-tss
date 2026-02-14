#!/bin/bash
echo "=== Stopping all nodes ==="
pkill -f "mpc-cli node"

echo "=== Building mpc-cli ==="
cargo build -p mpc-cli
echo "=== Starting 3 nodes ==="
RUST_LOG=sub-libp2p=info ./target/debug/mpc-cli node --config-path ./config_peer0.json --kademlia > /tmp/node0.log 2>&1 &
RUST_LOG=sub-libp2p=info ./target/debug/mpc-cli node --config-path ./config_peer1.json --kademlia > /tmp/node1.log 2>&1 &
RUST_LOG=sub-libp2p=info ./target/debug/mpc-cli node --config-path ./config_peer2.json --kademlia > /tmp/node2.log 2>&1 &
sleep 3

echo "=== Waiting for nodes to connect (condition-based, max 30s) ==="
deadline=$((SECONDS + 30))
ready=0
while [ $SECONDS -lt $deadline ]; do
  c0=$(grep -c "Libp2p => Connected" /tmp/node0.log 2>/dev/null || true)
  c1=$(grep -c "Libp2p => Connected" /tmp/node1.log 2>/dev/null || true)
  c2=$(grep -c "Libp2p => Connected" /tmp/node2.log 2>/dev/null || true)

  if [ "$c0" -ge 2 ] && [ "$c1" -ge 2 ] && [ "$c2" -ge 2 ]; then
    ready=1
    break
  fi
  sleep 1
done

# Number of keygen rounds and keysign rounds to run
NUM_KEYGEN=${NUM_KEYGEN:-3}
NUM_KEYSIGN_PER_WALLET=${NUM_KEYSIGN_PER_WALLET:-2}

# Millisecond timer (works on Linux and macOS)
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

WALLET_IDS=()
KEYGEN_TIMES=()
KEYSIGN_TIMES=()

echo "=== Running $NUM_KEYGEN keygen(s) ==="
for i in $(seq 1 "$NUM_KEYGEN"); do
  echo "--- Keygen $i/$NUM_KEYGEN ---"
  start=$(now_ms)
  keygen_output=$(./target/debug/mpc-cli keygen --address ws://127.0.0.1:8090 --threshold 2 --parties 3)
  end=$(now_ms)
  elapsed=$((end - start))
  KEYGEN_TIMES+=("$elapsed")
  echo "$keygen_output"
  WALLET_ID=$(echo "$keygen_output" | sed -n 's/^wallet_id: //p' | head -n 1)
  if [ -z "$WALLET_ID" ]; then
    echo "=== Failed to parse wallet_id from keygen $i ==="
    exit 1
  fi
  WALLET_IDS+=("$WALLET_ID")
  echo "Keygen $i done in ${elapsed}ms, wallet_id: $WALLET_ID"
done
echo "=== All keygen done ==="

echo "=== Running keysign ($NUM_KEYSIGN_PER_WALLET per wallet, ${#WALLET_IDS[@]} wallets) ==="
for wid in "${WALLET_IDS[@]}"; do
  for j in $(seq 1 "$NUM_KEYSIGN_PER_WALLET"); do
    echo "--- Keysign wallet $wid ($j/$NUM_KEYSIGN_PER_WALLET) ---"
    start=$(now_ms)
    ./target/debug/mpc-cli sign --address ws://127.0.0.1:8090 --wallet-id "$wid" --threshold 2 --messages "hello"
    end=$(now_ms)
    elapsed=$((end - start))
    KEYSIGN_TIMES+=("$elapsed")
    echo "Keysign done for wallet $wid round $j in ${elapsed}ms"
  done
done
echo "=== All keysign done ==="

# Summary: keygen
echo ""
echo "=== Keygen time ==="
total_keygen=0
for i in $(seq 0 $((${#KEYGEN_TIMES[@]} - 1))); do
  t=${KEYGEN_TIMES[$i]}
  echo "  Keygen $((i+1)): ${t}ms"
  total_keygen=$((total_keygen + t))
done
if [ ${#KEYGEN_TIMES[@]} -gt 0 ]; then
  echo "  Keygen total: ${total_keygen}ms, avg: $((total_keygen / ${#KEYGEN_TIMES[@]}))ms (${#KEYGEN_TIMES[@]} runs)"
fi

# Summary: keysign
echo ""
echo "=== Keysign time ==="
total_keysign=0
for i in $(seq 0 $((${#KEYSIGN_TIMES[@]} - 1))); do
  t=${KEYSIGN_TIMES[$i]}
  echo "  Keysign $((i+1)): ${t}ms"
  total_keysign=$((total_keysign + t))
done
if [ ${#KEYSIGN_TIMES[@]} -gt 0 ]; then
  echo "  Keysign total: ${total_keysign}ms, avg: $((total_keysign / ${#KEYSIGN_TIMES[@]}))ms (${#KEYSIGN_TIMES[@]} runs)"
fi
echo ""
echo "=== All done ==="

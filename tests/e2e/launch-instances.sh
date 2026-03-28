#!/usr/bin/env bash
# Multi-instance Jasmine test harness
# Launches N Jasmine instances with isolated app data, keystore, and discovery namespaces.
# Usage: ./launch-instances.sh [num_instances] [binary_path]
#   num_instances: default 2
#   binary_path:   default ../../src-tauri/target/release/jasmine

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NUM_INSTANCES="${1:-2}"
BINARY="${2:-$SCRIPT_DIR/../../src-tauri/target/release/jasmine}"
RUN_ID="run-$(date +%s)"
BASE_DIR="/tmp/jasmine-e2e/$RUN_ID"

if [[ ! -x "$BINARY" ]]; then
  echo "ERROR: Binary not found or not executable: $BINARY"
  echo "Run 'cargo build --release' in src-tauri/ first."
  exit 1
fi

echo "=== Jasmine Multi-Instance Test Harness ==="
echo "Run ID:     $RUN_ID"
echo "Instances:  $NUM_INSTANCES"
echo "Binary:     $BINARY"
echo "Base dir:   $BASE_DIR"
echo ""

INSTANCE_NAMES=("alpha" "beta" "gamma" "delta" "epsilon")
WEBDRIVER_BASE_PORT=4445
PIDS=()

cleanup() {
  echo ""
  echo "=== Shutting down all instances ==="
  for pid in "${PIDS[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      echo "Killing PID $pid"
      kill "$pid" 2>/dev/null || true
    fi
  done
  # Wait briefly then force kill
  sleep 2
  for pid in "${PIDS[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      echo "Force killing PID $pid"
      kill -9 "$pid" 2>/dev/null || true
    fi
  done
  echo "All instances stopped."
}

trap cleanup EXIT INT TERM

for i in $(seq 0 $((NUM_INSTANCES - 1))); do
  NAME="${INSTANCE_NAMES[$i]:-instance-$i}"
  INST_DIR="$BASE_DIR/$NAME"
  APP_DATA="$INST_DIR/app-data"
  DOWNLOADS="$INST_DIR/downloads"
  KEYSTORE="$INST_DIR/keystore"
  LOG_FILE="$INST_DIR/jasmine.log"

  mkdir -p "$APP_DATA" "$DOWNLOADS" "$KEYSTORE"

  CONFIG_JSON=$(cat <<EOF
{
  "instanceId": "$NAME",
  "appDataDir": "$APP_DATA",
  "downloadDir": "$DOWNLOADS",
  "wsBindAddr": "0.0.0.0:0",
  "discovery": {
    "mode": "mdns-only",
    "namespace": "$RUN_ID"
  },
  "keystore": {
    "mode": "file",
    "root": "$KEYSTORE"
  }
}
EOF
)

  WEBDRIVER_PORT=$((WEBDRIVER_BASE_PORT + i))

  echo "Starting instance '$NAME'..."
  echo "  App data:        $APP_DATA"
  echo "  WebDriver port:  $WEBDRIVER_PORT"
  echo "  Config:          $CONFIG_JSON"

  JASMINE_TEST_CONFIG="$CONFIG_JSON" TAURI_WEBDRIVER_PORT="$WEBDRIVER_PORT" "$BINARY" > "$LOG_FILE" 2>&1 &
  PID=$!
  PIDS+=("$PID")
  echo "  PID: $PID"
  echo "  Log: $LOG_FILE"
  echo ""

  # Small delay between launches to avoid race conditions
  sleep 1
done

echo "=== All $NUM_INSTANCES instances launched ==="
echo "PIDs: ${PIDS[*]}"
echo ""
echo "Instance logs:"
for i in $(seq 0 $((NUM_INSTANCES - 1))); do
  NAME="${INSTANCE_NAMES[$i]:-instance-$i}"
  echo "  $NAME: $BASE_DIR/$NAME/jasmine.log"
done
echo ""
echo "Press Ctrl+C to stop all instances."
echo "Run ID for Playwright: $RUN_ID"
echo ""

# Write instance info to a JSON file for Playwright to consume
INFO_FILE="$BASE_DIR/instances.json"
echo "[" > "$INFO_FILE"
for i in $(seq 0 $((NUM_INSTANCES - 1))); do
  NAME="${INSTANCE_NAMES[$i]:-instance-$i}"
  PID="${PIDS[$i]}"
  WD_PORT=$((WEBDRIVER_BASE_PORT + i))
  COMMA=""
  if [[ $i -lt $((NUM_INSTANCES - 1)) ]]; then
    COMMA=","
  fi
  cat >> "$INFO_FILE" <<EOF
  {
    "name": "$NAME",
    "pid": $PID,
    "webdriverPort": $WD_PORT,
    "appDataDir": "$BASE_DIR/$NAME/app-data",
    "logFile": "$BASE_DIR/$NAME/jasmine.log"
  }$COMMA
EOF
done
echo "]" >> "$INFO_FILE"
echo "Instance info written to: $INFO_FILE"

# Wait for all processes — if any exits unexpectedly, report it
wait_for_all() {
  while true; do
    for i in "${!PIDS[@]}"; do
      pid="${PIDS[$i]}"
      if ! kill -0 "$pid" 2>/dev/null; then
        NAME="${INSTANCE_NAMES[$i]:-instance-$i}"
        wait "$pid" 2>/dev/null
        EXIT_CODE=$?
        if [[ $EXIT_CODE -ne 0 ]]; then
          echo "WARNING: Instance '$NAME' (PID $pid) exited with code $EXIT_CODE"
          echo "Check log: $BASE_DIR/$NAME/jasmine.log"
        fi
      fi
    done
    sleep 2
  done
}

wait_for_all

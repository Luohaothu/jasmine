#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/../.."

echo "Building Jasmine with e2e-test feature..."
(cd "$PROJECT_ROOT/src-tauri" && cargo build --release --features e2e-test)
BINARY="$PROJECT_ROOT/src-tauri/target/release/jasmine"

RUN_ID="run-$(date +%s)"
BASE_DIR="/tmp/jasmine-e2e/$RUN_ID"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  sleep 1
  for pid in "${PIDS[@]}"; do
    kill -9 "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

for NAME in alpha beta gamma; do
  mkdir -p "$BASE_DIR/$NAME/app-data" "$BASE_DIR/$NAME/downloads" "$BASE_DIR/$NAME/keystore"
done

INSTANCES_FILE="$BASE_DIR/instances.json"

launch_instance() {
  local name=$1
  local port=$2
  JASMINE_TEST_CONFIG="{\"instanceId\":\"$name\",\"appDataDir\":\"$BASE_DIR/$name/app-data\",\"downloadDir\":\"$BASE_DIR/$name/downloads\",\"wsBindAddr\":\"0.0.0.0:0\",\"discovery\":{\"mode\":\"mdns-only\",\"namespace\":\"$RUN_ID\"},\"keystore\":{\"mode\":\"file\",\"root\":\"$BASE_DIR/$name/keystore\"}}" \
  TAURI_WEBDRIVER_PORT="$port" "$BINARY" > "$BASE_DIR/$name/jasmine.log" 2>&1 &
  PIDS+=($!)
}

launch_instance alpha 4445
sleep 2
launch_instance beta 4446
sleep 2
launch_instance gamma 4447
sleep 5

cat > "$INSTANCES_FILE" <<EOF
[
  {
    "name": "alpha",
    "pid": ${PIDS[0]},
    "webdriverPort": 4445,
    "appDataDir": "$BASE_DIR/alpha/app-data",
    "downloadsDir": "$BASE_DIR/alpha/downloads",
    "keystoreDir": "$BASE_DIR/alpha/keystore",
    "logFile": "$BASE_DIR/alpha/jasmine.log"
  },
  {
    "name": "beta",
    "pid": ${PIDS[1]},
    "webdriverPort": 4446,
    "appDataDir": "$BASE_DIR/beta/app-data",
    "downloadsDir": "$BASE_DIR/beta/downloads",
    "keystoreDir": "$BASE_DIR/beta/keystore",
    "logFile": "$BASE_DIR/beta/jasmine.log"
  },
  {
    "name": "gamma",
    "pid": ${PIDS[2]},
    "webdriverPort": 4447,
    "appDataDir": "$BASE_DIR/gamma/app-data",
    "downloadsDir": "$BASE_DIR/gamma/downloads",
    "keystoreDir": "$BASE_DIR/gamma/keystore",
    "logFile": "$BASE_DIR/gamma/jasmine.log"
  }
]
EOF

node "$SCRIPT_DIR/e2e-tests.mjs" 4445 4446 4447 "$BASE_DIR" "$BINARY"

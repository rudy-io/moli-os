#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
export E2E_DIR="$PWD/.run"
if [[ -z ${MOLI_BIN:-} ]]; then
  MOLI_BIN=$(cargo metadata --manifest-path ../../Cargo.toml --locked --no-deps --format-version 1 |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"] + "/debug/moli-os")')
fi
export MOLI_BIN
if [[ ${1:-} != --inside ]]; then
  test -x "$MOLI_BIN"
  test -d node_modules || { echo 'Run npm ci --ignore-scripts in e2e first' >&2; exit 1; }
  exec unshare --user --map-root-user --net "$PWD/run.sh" --inside
fi
# Fail closed: never launch a Matter process if a physical network is visible.
[[ $(ip -o link show | wc -l) == 1 ]]
[[ $(ip -o link show) == *' lo:'* ]]
ip link set lo up
ip link add e2e0 type veth peer name e2e1
ip link set e2e0 up
ip link set e2e1 up
ip addr add 192.0.2.1/24 dev e2e0
ip -6 addr add fd00:ee::1/64 dev e2e0 nodad
ip -6 addr add fe80::1/64 dev e2e0 nodad
ip -6 route add ff00::/8 dev e2e0
export E2E_ISOLATED=yes
umask 077
rm -rf "$E2E_DIR"
mkdir "$E2E_DIR"
node accessory.mjs >"$E2E_DIR/accessory.log" 2>&1 &
accessory=$!
trap 'kill "$accessory" 2>/dev/null || true; wait "$accessory" 2>/dev/null || true' EXIT
python3 validate.py

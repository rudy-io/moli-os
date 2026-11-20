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
  # One owner per disposable directory, including across separate network namespaces.
  lock_id=$(printf '%s' "$PWD" | sha256sum | cut -c1-16)
  exec 9>"${TMPDIR:-/tmp}/moli-matter-e2e-${lock_id}.lock"
  flock -n 9 || { echo 'Another harness owns this .run directory' >&2; exit 1; }
  [[ ${E2E_FIXTURE_ONLY:-0} == 1 ]] || test -x "$MOLI_BIN"
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
cleanup() {
  kill "$accessory" 2>/dev/null || true
  for ((i=0; i<30; i++)); do
    kill -0 "$accessory" 2>/dev/null || break
    sleep .1
  done
  kill -KILL "$accessory" 2>/dev/null || true
  wait "$accessory" 2>/dev/null || true
}
trap cleanup EXIT
if [[ ${E2E_FIXTURE_ONLY:-0} == 1 ]]; then
  for ((i=0; i<60; i++)); do
    if [[ -f "$E2E_DIR/accessory-ready" ]]; then
      echo 'PASS: isolated accessory startup only (no Moli assertions)'
      exit 0
    fi
    kill -0 "$accessory" || { cat "$E2E_DIR/accessory.log" >&2; exit 1; }
    sleep .5
  done
  echo 'FAIL: accessory startup timeout' >&2
  exit 1
fi
python3 validate.py

#!/bin/sh
# The one public vendor-fixture exception must never become a directory bypass.
set -eu
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
root=$tmp/tree
fixture=vendor/matter-rust/crates/matter-commissioning/src/thread_dataset.rs
mkdir -p "$root/scripts" "$(dirname "$root/$fixture")"
cp scripts/leak-check.sh "$root/scripts/leak-check.sh"
# Split the public test identifier so this test does not itself need an exception.
public_id='78:96:21:7f:'
public_id=$public_id'78:7f'
write_fixture() {
    awk -v line="$1" -v value="$2" 'BEGIN { for (i=1; i<line; i++) print ""; print "// " value }' >"$root/$fixture"
}
check() {
    expected=$1
    status=0
    MOLI_DENYLIST="$tmp/deny" MOLI_ALLOWLIST="$tmp/no-allow" \
        sh "$root/scripts/leak-check.sh" >"$tmp/output" 2>&1 || status=$?
    if [ "$status" -ne "$expected" ]; then
        cat "$tmp/output" >&2
        echo "vendor leak guard: expected $expected, got $status" >&2
        exit 1
    fi
}
write_fixture 225 "$public_id"
check 0
write_fixture 226 "$public_id"
check 1
write_fixture 225 "$public_id"
mv "$root/$fixture" "$root/other.rs"
check 1
mv "$root/other.rs" "$root/$fixture"
other_id='78:96:21:7f:'
other_id=$other_id'78:80'
write_fixture 225 "$other_id"
check 1
write_fixture 225 "$public_id"
printf '%s\n' "$public_id" >"$tmp/deny"
check 1
echo 'vendor leak guard: 5 checks passed (private denylist remains blocking)'

#!/bin/sh
# Cross-check the Lua plugin against the real CLI.
#
# The plugin builds a command line from string literals; nothing in Lua can
# catch a flag being renamed in crates/tohdr-cli/src/cli.rs. This asserts that
# every flag and every enum value the plugin can emit is one the binary
# actually accepts, by asking the binary.
#
# Usage: lightroom/tests/test_cli_contract.sh [path-to-tohdr]
set -u

BIN="${1:-./target/release/tohdr}"
fail=0
checks=0

if [ ! -x "$BIN" ]; then
    echo "SKIP: no tohdr binary at $BIN (build with: cargo build --release -p tohdr-cli)"
    exit 0
fi

HELP="$("$BIN" convert --help 2>&1)"

want_flag() {
    checks=$((checks + 1))
    if ! printf '%s' "$HELP" | grep -q -- "$1"; then
        echo "  FAIL: 'tohdr convert --help' does not mention $1"
        fail=$((fail + 1))
    fi
}

# Every flag TohdrCli.buildConvertArgs can emit.
for f in --output --flavor --engine --max-size --quality --min-quality \
         --tone-map --gain-subsample --headroom --json; do
    want_flag "$f"
done

# Every enum value the dialog offers must parse. A bad value makes clap exit
# non-zero with "unknown flavor"/"unknown engine", so we check the exit status
# of a parse that fails for a *different*, expected reason (missing input file
# is fine -- we only care that argument parsing got past the enum).
try_value() {
    checks=$((checks + 1))
    out="$("$BIN" convert /nonexistent-source.tiff --output /dev/null "$1" "$2" 2>&1)"
    if printf '%s' "$out" | grep -qi "unknown $3\|invalid value\|unexpected argument"; then
        echo "  FAIL: $1 $2 rejected by the CLI: $(printf '%s' "$out" | head -1)"
        fail=$((fail + 1))
    fi
}

for v in apple iso both; do try_value --flavor "$v" flavor; done
for v in apple portable; do try_value --engine "$v" engine; done
for v in clip reinhard; do try_value --tone-map "$v" tone-map; done

# The two size spellings the dialog can produce.
for v in 4MB 4MiB; do
    checks=$((checks + 1))
    out="$("$BIN" convert /nonexistent-source.tiff --output /dev/null --max-size "$v" 2>&1)"
    if printf '%s' "$out" | grep -qi "invalid value\|not a number"; then
        echo "  FAIL: --max-size $v rejected: $(printf '%s' "$out" | head -1)"
        fail=$((fail + 1))
    fi
done

echo "$checks checks, $fail failures"
[ "$fail" -eq 0 ] || exit 1

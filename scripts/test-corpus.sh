#!/usr/bin/env bash
set -uo pipefail

corpus=${1:-/home/max/siox-tests}
root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

passed=0
failed=0
for source in "$corpus"/*.siox; do
    name=$(basename "${source%.siox}")
    binary="$tmp/$name"
    if grep -q '#\[test\]' "$source"; then
        command=(cargo run -q --manifest-path "$root/Cargo.toml" --bin sioxc --
            --std "$root/std" --test "$source" -o "$binary")
    else
        command=(cargo run -q --manifest-path "$root/Cargo.toml" --bin sioxc --
            "$source" --std "$root/std" --emit metadata)
    fi
    if "${command[@]}" && { [[ ! -e "$binary" ]] || "$binary"; }; then
        passed=$((passed + 1))
    else
        echo "FAILED: $source" >&2
        failed=$((failed + 1))
    fi
done

echo "$passed passed; $failed failed"
test "$failed" -eq 0

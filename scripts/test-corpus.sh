#!/usr/bin/env bash
set -uo pipefail

corpus=${1:-/home/max/siox-tests}
root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

sioxc=(cargo run -q --manifest-path "$root/Cargo.toml" --bin sioxc --)

# `--emit source` must produce source that parses back to the same program,
# and printing must be idempotent (spec Stage 2). Compiling and running a
# corpus file never exercises the printer, so three files silently printed
# broken string literals until this was checked.
roundtrip() {
    local source=$1 name=$2
    local one="$tmp/$name.print1.siox" two="$tmp/$name.print2.siox"
    "${sioxc[@]}" --std "$root/std" --emit source "$source" >"$one" 2>/dev/null || return 0
    "${sioxc[@]}" --std "$root/std" --emit source "$one" >"$two" || {
        echo "  printed source does not re-parse" >&2
        return 1
    }
    cmp -s "$one" "$two" || {
        echo "  printing is not idempotent" >&2
        return 1
    }
}

passed=0
failed=0
for source in "$corpus"/*.siox; do
    name=$(basename "${source%.siox}")
    binary="$tmp/$name"
    vcd="$tmp/$name.vcd"
    if grep -q '#\[test\]' "$source"; then
        command=(cargo run -q --manifest-path "$root/Cargo.toml" --bin sioxc --
            --std "$root/std" --test "$source" -o "$binary")
    else
        command=(cargo run -q --manifest-path "$root/Cargo.toml" --bin sioxc --
            "$source" --std "$root/std" --emit metadata)
    fi
    if "${command[@]}" \
        && { [[ ! -e "$binary" ]] || "$binary" --vcd "$vcd"; } \
        && { [[ ! -e "$binary" ]] || python3 "$root/scripts/check-vcd.py" "$vcd" --profile "$name"; } \
        && roundtrip "$source" "$name"
    then
        passed=$((passed + 1))
    else
        echo "FAILED: $source" >&2
        failed=$((failed + 1))
    fi
done

echo "$passed passed; $failed failed"
test "$failed" -eq 0

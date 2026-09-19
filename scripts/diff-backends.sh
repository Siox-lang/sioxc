#!/usr/bin/env bash
# Run every corpus test through both native backends and compare what the
# executables actually print.
#
# Step 7 of the unified process pipeline requires "identical test results,
# diagnostics, time progression, resolved values" across the compatibility
# generated-C backend and direct Process IR lowering. A pass/fail matrix does
# not show that: two backends can both exit zero while printing different
# things, which is how `print_test` (a missing `finish at <time> fs` line) and
# `warn_test` (an extra `(source N:M)` suffix) diverged unnoticed.
#
# The generated-C backend is the oracle. Divergence is reported against it.
#
#   ./diff-backends.sh [corpus] [--write-baseline]
#
# With `scripts/diff-backends.baseline` present, the script fails when a case
# that previously agreed stops agreeing. Without it, it only reports — the
# direct backend is still incomplete, so most non-agreement is a known gap
# rather than a regression, and only the baseline can tell those apart.
#
# `--write-baseline` refuses to run on a dirty tree. A baseline is a claim
# about a commit, and one recorded over uncommitted changes describes a tree
# nobody can check out: the first such baseline recorded `xz_compare_test` as
# agreeing because another agent's unstaged fix was present, which made the
# gate report green on a case HEAD was failing.
set -uo pipefail

corpus=${1:-/home/max/siox-tests}
root=$(cd "$(dirname "$0")/.." && pwd)
baseline=$root/scripts/diff-backends.baseline
write_baseline=0
[[ ${2:-} == --write-baseline || ${1:-} == --write-baseline ]] && write_baseline=1
[[ ${1:-} == --write-baseline ]] && corpus=/home/max/siox-tests

# Checked before the run, not after it: the whole point is to not spend twenty
# minutes measuring a tree the baseline must not describe.
if [[ $write_baseline -eq 1 ]] \
    && ! git -C "$root" diff --quiet HEAD -- . ':!scripts/diff-backends.baseline'; then
    echo "refusing to write a baseline from a dirty tree:" >&2
    git -C "$root" status --short -- . ':!scripts/diff-backends.baseline' >&2
    echo "a baseline describes a commit; commit or stash first" >&2
    exit 2
fi

# A direct-backend gap must not hang the whole run. Simulated time is
# deterministic, so a wall-clock limit only catches a scheduler that fails to
# terminate.
limit=${SIOX_DIFF_TIMEOUT:-30}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

features=${SIOXC_FEATURES:-}
cargo_args=(-q --manifest-path "$root/Cargo.toml" --bin sioxc)
[[ -n "$features" ]] && cargo_args+=(--features "$features")
sioxc=(cargo run "${cargo_args[@]}" --)

# Build and run one case, leaving its combined output in $2 and returning the
# executable's exit status. A build failure is distinguished from a run failure
# because only the second is a semantic disagreement.
run_backend() {
    local source=$1 out=$2 binary=$3
    if ! "${sioxc[@]}" --std "$root/std" --test "$source" -o "$binary" >/dev/null 2>&1; then
        printf '<build failed>\n' >"$out"
        return 200
    fi
    timeout "$limit" "$binary" >"$out" 2>&1
}

# A VCD records every signal at every timestamp, so it is a far stronger
# observable than stdout for the many corpus tests that print nothing. It is
# compared only when BOTH backends emit one; the direct runtime does not accept
# `-o` yet, so today this is inert and will start contributing by itself.
compare_waves() {
    local binary=$1 out=$2
    "$binary" -o "$out" >/dev/null 2>&1 || return 1
    [[ -s $out ]] || return 1
    # `$date`/`$version` are writer metadata rather than design behaviour.
    sed -e '/^\$date/,/\$end/d' -e '/^\$version/d' "$out" >"$out.norm"
}

declare -A state
agree=0 diverge=0 gap=0 oracle=0 skipped=0 boilerplate=0 waved=0
for source in "$corpus"/*.siox; do
    name=$(basename "${source%.siox}")
    grep -q '#\[test\]' "$source" || { skipped=$((skipped + 1)); continue; }

    run_backend "$source" "$tmp/$name.c" "$tmp/$name.c.bin"
    c_status=$?
    SIOX_DIRECT_PROCESS_RUNTIME=1 run_backend "$source" "$tmp/$name.direct" "$tmp/$name.direct.bin"
    direct_status=$?

    if [[ $c_status -ne 0 ]]; then
        # The oracle is expected to pass everything; if it does not, comparing
        # against it says nothing. Note some corpus files read sibling data at
        # compile time, so this also fires when the corpus path is wrong.
        state[$name]=ORACLE-FAIL
        oracle=$((oracle + 1))
        if [[ $c_status -eq 200 ]]; then
            echo "ORACLE-FAIL  $name (generated-C did not build)" >&2
        else
            echo "ORACLE-FAIL  $name (generated-C exit $c_status)" >&2
        fi
    elif [[ $direct_status -ne 0 ]]; then
        # Split the way the migration tracks it: a fail-closed coverage gap is
        # expected progress, a semantic failure is a behaviour difference the
        # oracle did not have, and a build failure is neither.
        if [[ $direct_status -eq 200 ]]; then
            state[$name]=DIRECT-BUILD-FAIL
        elif [[ $direct_status -eq 124 ]]; then
            state[$name]=DIRECT-TIMEOUT
        elif grep -q 'lowering is incomplete' "$tmp/$name.direct"; then
            state[$name]=DIRECT-UNSUPPORTED
        else
            state[$name]=DIRECT-SEMANTIC
        fi
        gap=$((gap + 1))
    elif ! cmp -s "$tmp/$name.c" "$tmp/$name.direct"; then
        state[$name]=DIVERGE
        diverge=$((diverge + 1))
        echo "DIVERGE      $name (stdout)" >&2
        diff "$tmp/$name.c" "$tmp/$name.direct" | sed 's/^/    /' >&2
    elif compare_waves "$tmp/$name.c.bin" "$tmp/$name.c.vcd" \
        && compare_waves "$tmp/$name.direct.bin" "$tmp/$name.direct.vcd" \
        && ! cmp -s "$tmp/$name.c.vcd.norm" "$tmp/$name.direct.vcd.norm"; then
        state[$name]=DIVERGE
        diverge=$((diverge + 1))
        echo "DIVERGE      $name (waveform)" >&2
        diff "$tmp/$name.c.vcd.norm" "$tmp/$name.direct.vcd.norm" | head -20 | sed 's/^/    /' >&2
    else
        state[$name]=AGREE
        agree=$((agree + 1))
        [[ -s $tmp/$name.direct.vcd ]] && waved=$((waved + 1))
        # Record how little some agreements prove: a test that prints nothing
        # contributes only the harness banner, so "agree" there means both
        # backends exited zero and said so in the same words.
        grep -qvE '^$|^running |^test .* \.\.\. |^test result:' "$tmp/$name.direct" \
            || boilerplate=$((boilerplate + 1))
    fi
done

echo
echo "$agree agree; $diverge diverge; $gap direct-only gaps; $oracle oracle failures; $skipped without tests"
if [[ $agree -gt 0 ]]; then
    echo "  of those agreements, $boilerplate compared harness output only" \
         "and $waved also compared a waveform"
fi
for kind in DIRECT-UNSUPPORTED DIRECT-SEMANTIC DIRECT-TIMEOUT DIRECT-BUILD-FAIL; do
    count=0
    for name in "${!state[@]}"; do [[ ${state[$name]} == "$kind" ]] && count=$((count + 1)); done
    [[ $count -gt 0 ]] && echo "  $kind: $count"
done

if [[ $write_baseline -eq 1 ]]; then
    for name in "${!state[@]}"; do printf '%s %s\n' "$name" "${state[$name]}"; done \
        | sort >"$baseline"
    echo "wrote $baseline"
    exit 0
fi

[[ -f $baseline ]] || {
    echo "no baseline; rerun with --write-baseline to record this state"
    exit 0
}

# Only a loss of agreement is a regression. Gaps closing, or a gap changing
# shape, is ordinary migration progress.
regressed=0
while read -r name was; do
    [[ $was == AGREE ]] || continue
    now=${state[$name]:-MISSING}
    [[ $now == AGREE ]] && continue
    echo "REGRESSED    $name: agreed at baseline, now $now" >&2
    regressed=$((regressed + 1))
done <"$baseline"

test "$regressed" -eq 0

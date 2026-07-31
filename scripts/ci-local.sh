#!/usr/bin/env bash
# Run what CI runs, in CI's order, and report by exit code.
#
# Checking `cargo clippy | grep '^error'` does not work: clippy colours its
# output, so the escape sequence sits before `error` and the grep silently
# matches nothing. CI was red for four commits behind a check that looked
# clean locally for exactly that reason. Trust exit codes, not greps.
#
# Lints differ between compiler releases, so running them on a *newer*
# toolchain than CI is not the check CI runs: `only_used_in_recursion` fires
# on the pinned 1.90 and not on 1.96, so a clean local gate went red upstream.
# Take the pin straight from the workflow so the two cannot drift.
set -uo pipefail
corpus="${1:-$(dirname "$0")/../../siox-tests}"
fail=0
pin=$(grep -oE 'rust-toolchain@[0-9]+\.[0-9]+(\.[0-9]+)?' \
    "$(dirname "$0")/../.github/workflows/ci.yml" | head -1 | cut -d@ -f2)
if [ -n "$pin" ] && rustup toolchain list 2>/dev/null | grep -q "^${pin}-"; then
    cargo=(cargo "+$pin")
else
    cargo=(cargo)
    echo "warning: CI pins Rust ${pin:-<unknown>}; this run uses the default" \
         "toolchain, whose lints differ. rustup toolchain install ${pin:-1.90.0}"
fi
step() {
    local name="$1"; shift
    printf '%-44s ' "$name"
    if "$@" >/tmp/ci-local.log 2>&1; then
        echo "ok"
    else
        echo "FAIL"
        tail -20 /tmp/ci-local.log | sed 's/^/    /'
        fail=1
    fi
}
step "fmt"                       "${cargo[@]}" fmt --all --check
step "check (frontend only)"     "${cargo[@]}" check --locked --no-default-features --lib
step "clippy (frontend only)"    "${cargo[@]}" clippy --locked --no-default-features --lib -- -D warnings
step "build"                     "${cargo[@]}" build --locked
step "test"                      "${cargo[@]}" test --locked
step "test (bitpack)"            "${cargo[@]}" test --locked --features bitpack
step "clippy (all targets)"      "${cargo[@]}" clippy --locked --all-targets --all-features -- -D warnings
step "corpus"                    bash "$(dirname "$0")/test-corpus.sh" "$corpus"
SIOXC_FEATURES=bitpack \
step "corpus (bitpack)"          bash "$(dirname "$0")/test-corpus.sh" "$corpus"
exit $fail

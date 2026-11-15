#!/usr/bin/env bash
# Argument-only checks: mock the manager; never start services or workloads.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$UID}
unset SIOX_JOB_MEMORY SIOX_JOB_HIGH
systemctl() { [[ $1 == --user && $2 == show-environment ]]; }
systemd-run() { printf 'arg:%s\n' "$@"; }
export -f systemctl systemd-run

expect_usage() {
    if bash "$root/scripts/guarded-run.sh" "$@" >/dev/null 2>&1; then
        echo 'missing command unexpectedly accepted' >&2
        return 1
    else
        test "$?" = 2
    fi
}
expect_usage
expect_usage --wait
for mode in default queued; do
    options=()
    if [[ $mode == queued ]]; then options=(--wait); fi
    output=$(bash "$root/scripts/guarded-run.sh" "${options[@]}" /usr/bin/printf '%s' 'literal $HOME ; spaces')
    for argument in '/usr/bin/flock' "$XDG_RUNTIME_DIR/siox-verification.lock" \
        '/usr/bin/printf' '%s' 'literal $HOME ; spaces' \
        '--property=MemoryMax=2G' '--property=MemoryHigh=1536M' '--property=MemorySwapMax=0'; do
        rg -F -x -- "arg:$argument" <<<"$output" >/dev/null
    done
    if [[ $mode == default ]]; then
        rg -F -x -- 'arg:--nonblock' <<<"$output" >/dev/null
        rg -F -x -- 'arg:--conflict-exit-code=75' <<<"$output" >/dev/null
    else
        ! rg -F -x -- 'arg:--nonblock' <<<"$output" >/dev/null
        ! rg -F -x -- 'arg:--conflict-exit-code=75' <<<"$output" >/dev/null
        ! rg -F -x -- 'arg:--wait' <<<"$output" >/dev/null
    fi
done
systemctl() { return 1; }
export -f systemctl
if bash "$root/scripts/guarded-run.sh" --wait /usr/bin/true >/dev/null 2>&1; then
    echo 'missing user manager unexpectedly accepted' >&2
    exit 1
else
    test "$?" = 2
fi
printf 'guarded-run arguments: defaults, queued lock, limits, literal arguments and missing-manager rejection passed\n'

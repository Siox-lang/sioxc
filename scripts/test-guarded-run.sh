#!/usr/bin/env bash
# Linux-only integration check; the intentional OOM is confined to 64 MiB.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
units=()
cleanup() {
    for unit in "${units[@]}"; do
        systemctl --user stop "$unit" >/dev/null 2>&1 || true
        systemctl --user reset-failed "$unit" >/dev/null 2>&1 || true
    done
}
trap cleanup EXIT

start() {
    local output
    output=$(bash "$root/scripts/guarded-run.sh" "$@")
    unit=$(sed -n 's/^unit=//p' <<<"$output")
    log=$(sed -n 's/^log=//p' <<<"$output")
    test -n "$unit"
    units+=("$unit")
}
property() { systemctl --user show "$unit" --property="$1" --value; }
wait_done() {
    for ((n=0; n<200; n++)); do
        case $(property SubState) in exited|failed|dead) return ;; esac
        sleep 0.1
    done
    echo "timed out: $unit" >&2
    exit 1
}

scope=$(awk -F: '$1 == "0" {print $3}' /proc/self/cgroup)
before=$(awk '$1 == "oom_kill" {print $2}' "/sys/fs/cgroup$scope/memory.events")
start /usr/bin/bash -c 'printf "%s\n" "$1" "$DBUS_SESSION_BUS_ADDRESS" "$TMPDIR"; test -d "$TMPDIR"; printf "%s %s\n" "$CARGO_BUILD_JOBS" "$RUST_TEST_THREADS"; cat /proc/self/cgroup' guard 'literal $HOME ; spaces'
wait_done
test "$(property ExecMainStatus)" = 0
rg -F -x 'literal $HOME ; spaces' "$log" >/dev/null
rg -F -x 'unix:path=/dev/null' "$log" >/dev/null
rg -F -x "${XDG_CACHE_HOME:-$HOME/.cache}/siox-jobs/tmp" "$log" >/dev/null
rg -F -x '2 2' "$log" >/dev/null
rg -F "$unit" "$log" >/dev/null
! rg -F "$scope" "$log" >/dev/null
test "$(property MemoryMax)" = 2147483648
test "$(property OOMPolicy)" = kill

start /usr/bin/sh -c 'exit 7'
wait_done
test "$(property ExecMainStatus)" = 7
test "$(property Result)" = exit-code

# Keep one lock holder alive while checking that a competing job never runs.
start /usr/bin/sh -c 'echo ready; sleep 3'
holder=$unit
for ((n=0; n<100; n++)); do
    if [[ -s $log ]]; then break; fi
    sleep 0.05
done
test -s "$log"
start /usr/bin/sh -c 'echo must-not-run'
wait_done
test "$(property ExecMainStatus)" = 75
! rg -F 'must-not-run' "$log" >/dev/null
unit=$holder
wait_done

# Exercise the hard limit without spending the timeout in high-limit reclaim.
export SIOX_JOB_MEMORY=64M SIOX_JOB_HIGH=64M
start /usr/bin/python3 -c 'blocks=[]
while True: blocks.append(bytearray(8 * 1024 * 1024))'
wait_done
test "$(property Result)" = oom-kill
test "$(property ExecMainCode)" = 2 # CLD_KILLED, not a swallowed success
after=$(awk '$1 == "oom_kill" {print $2}' "/sys/fs/cgroup$scope/memory.events")
test "$before" = "$after"
printf 'guarded-run: success, literal arguments, failure status, serialization and isolated OOM passed\n'

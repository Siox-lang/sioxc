#!/usr/bin/env bash
# Start one bounded verification job outside the calling agent's terminal scope.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: scripts/guarded-run.sh COMMAND [ARGUMENT ...]" >&2
    exit 2
fi
if [[ -z ${XDG_RUNTIME_DIR:-} ]] || ! systemctl --user show-environment >/dev/null; then
    echo "guarded-run: a systemd user manager is required; refusing an unguarded fallback" >&2
    exit 2
fi

umask 077
logs=${XDG_CACHE_HOME:-$HOME/.cache}/siox-jobs
mkdir -p "$logs/tmp"
unit="siox-job-$UID-$(date +%s%N)-$$"
log="$logs/$unit.log"
# Headless editor clients must not use the session bus to move themselves into
# fresh, unlimited app scopes outside this service's memory limit.
environment=(--setenv=CARGO_BUILD_JOBS=2 --setenv=RUST_TEST_THREADS=2
    --setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/dev/null
    "--setenv=TMPDIR=$logs/tmp")
for name in PATH HOME CARGO_HOME RUSTUP_HOME LLVM_SYS_221_PREFIX LLVM_SYS_231_PREFIX \
    SIOXC_FEATURES SIOX_STD SIOX_LSP_BIN SIOX_CI_LOG_DIR \
    LD_LIBRARY_PATH FONTCONFIG_FILE; do
    if [[ -v $name ]]; then environment+=("--setenv=$name"); fi
done

# A service is a sibling cgroup, not --scope/--slice-inherit: an OOM in the
# workload must not reach the terminal's OOMPolicy=stop and kill the agent.
# flock is shared by all checkouts for this user; a busy job exits 75 rather
# than accumulating parallel compilers. The manager owns logs, not a TUI pipe.
systemd-run --user --quiet --unit="$unit" --description="siox guarded verification" \
    --service-type=exec --expand-environment=no --working-directory="$PWD" \
    --property=MemoryMax="${SIOX_JOB_MEMORY:-2G}" \
    --property=MemoryHigh="${SIOX_JOB_HIGH:-1536M}" \
    --property=MemorySwapMax=0 \
    --property=OOMPolicy=kill --property=OOMScoreAdjust=500 \
    --property=LimitCORE=0 --property=UMask=0077 \
    --property=RuntimeMaxSec="${SIOX_JOB_TIMEOUT:-1h}" --property=RemainAfterExit=yes \
    --property="StandardOutput=append:$log" --property=StandardError=inherit \
    "${environment[@]}" \
    /usr/bin/flock --nonblock --conflict-exit-code=75 \
    "$XDG_RUNTIME_DIR/siox-verification.lock" "$@"
printf 'unit=%s.service\nlog=%s\n' "$unit" "$log"

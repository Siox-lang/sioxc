#!/usr/bin/env bash
# Native debugging smoke test, not a promise of source-level DWARF support.
# On the shared host: bash scripts/guarded-run.sh bash scripts/test-gdb.sh
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
corpus=${1:-/home/max/siox-tests}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
command -v gdb >/dev/null
cargo build --locked --manifest-path "$root/Cargo.toml" --bin sioxc
compiler="$root/target/debug/sioxc"

if "$compiler" --std "$root/std" --test --debug "$corpus/fifo_test.siox" -o "$work/debug" >"$work/debug.log" 2>&1; then
    echo 'unsupported source-debug build unexpectedly succeeded' >&2
    exit 1
fi
rg -q 'native Process IR debug metadata is not implemented' "$work/debug.log"
test ! -e "$work/debug"

read -r -d '' check <<'PYTHON' || true
import gdb, os

design = os.environ['SIOX_GDB_DESIGN']
wait = gdb.Breakpoint('sx_runtime_suspend_time')
wait.ignore_count = 2 if design == 'fifo' else 0
gdb.execute('run')
assert gdb.selected_frame().name() == 'sx_runtime_suspend_time'
trace = gdb.execute('backtrace', to_string=True)
assert 'sx_runtime_run_test' in trace and 'sx.process' in trace, trace
print(trace)

def value(expression):
    return int(gdb.parse_and_eval(expression))

time = value('(unsigned long long)sx_runtime_now()')
assert time == (11000000 if design == 'fifo' else 0), time
count = value('*(unsigned int*)&sx_wave_signal_count')
signals = {}
for index in range(count):
    name = gdb.parse_and_eval(f'((char**)&sx_wave_signal_names)[{index}]').string()
    signal = value(f'((unsigned int*)&sx_wave_signal_ids)[{index}]')
    width = value(f'((unsigned int*)&sx_wave_signal_widths)[{index}]')
    signals.setdefault(name, []).append((signal, width))
for name, expected in ({'count': 1, 'dout': 11} if design == 'fifo' else {'y': 1 << 64}).items():
    assert name in signals, name
    for signal, width in signals[name]:
        words = [value(f'(unsigned long long)sx_read_word({signal}, {word})')
                 for word in range((width + 63) // 64)]
        actual = sum(word << (64 * index) for index, word in enumerate(words))
        assert actual == expected, (name, width, words, expected)
        print(f'{design}: {name}, {width} bits, words={words}, time={time} fs')

# No Siox variables or line breakpoints are advertised without DWARF.
assert gdb.lookup_symbol('head')[0] is None
try:
    gdb.execute('break ' + os.environ['SIOX_GDB_SOURCE'] + ':24', to_string=True)
except gdb.error as error:
    print('Expected unsupported source breakpoint:', error)
else:
    raise AssertionError('source breakpoint unexpectedly available; update this smoke test')

# Use the actual entry table rather than guessed process symbol spellings.
process = value('*(unsigned int*)&sx_current_process')
entry = value(f'((void**)&sx_process_entries)[{process}]')
wait.delete()
process_break = gdb.Breakpoint(f'*{entry}')
gdb.execute('continue')
assert value('$pc') == entry
gdb.execute('x/i $pc')
gdb.execute('stepi')
assert value('$pc') != entry
process_break.delete()
gdb.execute('continue')
assert value('$_exitcode') == 0
PYTHON

for design in fifo wide; do
    source="$corpus/${design}_test.siox"
    binary="$work/$design"
    "$compiler" --std "$root/std" --test "$source" -o "$binary"
    "$binary" -o "$work/$design.baseline.vcd"
    SIOX_GDB_DESIGN="$design" SIOX_GDB_SOURCE="$source" \
        gdb -nx -q --batch "$binary" \
        -ex 'set disable-randomization off' -ex 'set breakpoint pending off' \
        -ex "set args -o $work/$design.debugger.vcd" -ex "python $check"
    cmp "$work/$design.baseline.vcd" "$work/$design.debugger.vcd"
    echo "$design: GDB breakpoints/inspection/instruction stepping and VCD parity passed"
done

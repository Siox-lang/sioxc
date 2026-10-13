#!/usr/bin/env python3
"""Measure what the compiler produces, so an optimization is judged by numbers.

For each build mode (default, `bitpack`, host-SIMD `simd`) this builds a
release `sioxc`, compiles each testbench, and reports one TSV row:

    mode  program  compile_s  compile_mb  binary_kb  run_s

`compile_mb` is the compiler's peak resident memory; `run_s` the fastest of
`--runs` executions of the test binary, which is the simulation throughput on
a fixed workload. The binary links the fixed runtime, so `binary_kb` differs
between programs and modes only by the design object.

    scripts/bench.py                         # all modes, the default programs
    scripts/bench.py --modes default a.siox  # one mode, chosen programs
    scripts/bench.py > before.tsv; ...; scripts/bench.py > after.tsv
    scripts/bench.py --compare before.tsv after.tsv

Run it detached under a memory cap like any other compile (see CLAUDE.md).
Timings are noisy: compare runs from the same idle machine, and treat a few
percent as noise.
"""
import argparse
import os
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MODES = {"default": "", "bitpack": "bitpack", "simd": "simd"}
# The corpus's compile-time and compile-memory outliers (float formatting,
# hardware locals/places/aggregate writes, wide vectors), plus a long-running
# workload: corpus tests finish in milliseconds and say nothing about
# simulation speed.
PROGRAMS = [
    "float_sqrt_test",
    "hardware_local_values_test",
    "hardware_procedure_places_test",
    "hardware_aggregate_writes_test",
    "wide_test",
    "aggregate_operator_values_test",
]
WORKLOADS = [os.path.join(ROOT, "scripts", "bench", "throughput_test.siox")]


def measure(command):
    """Wall seconds, peak RSS in MB and exit code of one child process."""
    start = time.perf_counter()
    child = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    _, status, usage = os.wait4(child.pid, 0)
    elapsed = time.perf_counter() - start
    error = child.stderr.read().decode(errors="replace")
    return elapsed, usage.ru_maxrss // 1024, os.waitstatus_to_exitcode(status), error


def build(mode):
    target = os.path.join(ROOT, "target", "bench", mode)
    command = ["cargo", "build", "-q", "--release", "--bin", "sioxc",
               "--manifest-path", os.path.join(ROOT, "Cargo.toml"), "--target-dir", target]
    if MODES[mode]:
        command += ["--features", MODES[mode]]
    subprocess.run(command, check=True)
    return os.path.join(target, "release", "sioxc")


def bench(args):
    sources = args.programs or [os.path.join(args.corpus, f"{p}.siox") for p in PROGRAMS] + WORKLOADS
    print("mode\tprogram\tcompile_s\tcompile_mb\tbinary_kb\trun_s", flush=True)
    with tempfile.TemporaryDirectory() as scratch:
        for mode in args.modes.split(","):
            sioxc = build(mode)
            for source in sources:
                name = os.path.basename(source).removesuffix(".siox")
                binary = os.path.join(scratch, name)
                compile_s, compile_mb, code, error = measure(
                    [sioxc, "--std", os.path.join(ROOT, "std"), "--test", source, "-o", binary])
                if code:
                    sys.exit(f"{mode} {name}: compile failed ({code})\n{error}")
                runs = []
                for _ in range(args.runs):
                    run_s, _, code, error = measure([binary])
                    if code:
                        sys.exit(f"{mode} {name}: test failed ({code})\n{error}")
                    runs.append(run_s)
                size_kb = os.path.getsize(binary) // 1024
                print(f"{mode}\t{name}\t{compile_s:.2f}\t{compile_mb}\t{size_kb}\t{min(runs):.3f}",
                      flush=True)


def compare(before_path, after_path):
    def load(path):
        with open(path) as f:
            rows = [line.rstrip("\n").split("\t") for line in f if line.strip()]
        header, body = rows[0], rows[1:]
        return header, {(r[0], r[1]): r for r in body}

    header, before = load(before_path)
    _, after = load(after_path)
    print("\t".join(header[:2] + [f"{h} (change)" for h in header[2:]]))
    for key, new in after.items():
        old = before.get(key)
        if old is None:
            continue
        cells = []
        for a, b in zip(old[2:], new[2:]):
            a, b = float(a), float(b)
            change = (b - a) / a * 100 if a else 0.0
            cells.append(f"{b:g} ({change:+.1f}%)")
        print("\t".join(list(key) + cells))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("programs", nargs="*", help="testbench sources (default: a fixed corpus set)")
    parser.add_argument("--modes", default=",".join(MODES), help="comma-separated: default,bitpack,simd")
    parser.add_argument("--runs", type=int, default=3, help="executions per binary; the fastest counts")
    parser.add_argument("--corpus", default=os.path.join(ROOT, "..", "siox-tests"))
    parser.add_argument("--compare", nargs=2, metavar=("BEFORE", "AFTER"),
                        help="print the change between two earlier outputs")
    args = parser.parse_args()
    if args.compare:
        compare(*args.compare)
    else:
        bench(args)


if __name__ == "__main__":
    main()

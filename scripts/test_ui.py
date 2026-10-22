#!/usr/bin/env python3
"""Compare compiler diagnostics and exit status with annotated corpus snapshots."""

import argparse
from collections import Counter
import difflib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
ANNOTATION = re.compile(r"//~(\^*)\s+(ERROR|WARNING)(?:\s+([EW]-P\d{3}))?\s*$")
HEADER = re.compile(r"^(error|warning)(?:\[([EW]-P\d{3})\])?: ")
LOCATION = re.compile(r"^  --> (.*):(\d+):(\d+)$")
PROGRESS = re.compile(r"^== stage [2-6]: .* == .*$")
CODE = re.compile(r'\s*pub const \w+: &str = "([EW]-P\d{3})";')


def active_codes(catalogue):
    codes = set()
    documentation = []
    for line in catalogue.read_text(encoding="utf-8").splitlines():
        if line.lstrip().startswith("///"):
            documentation.append(line)
            continue
        if match := CODE.fullmatch(line):
            if not any("Retired:" in text for text in documentation):
                codes.add(match[1])
        documentation.clear()
    if not codes:
        raise ValueError(f"no diagnostic codes found in {catalogue}")
    return codes


def annotations(source):
    expected = Counter()
    for line, text in enumerate(source.read_text(encoding="utf-8").splitlines(), 1):
        if "//~" not in text:
            continue
        match = ANNOTATION.search(text)
        if not match:
            raise ValueError(f"{source}:{line}: malformed //~ annotation")
        above, severity, code = match.groups()
        target = line - len(above)
        if target < 1:
            raise ValueError(f"{source}:{line}: annotation points before the file")
        expected[(severity.lower(), code, source.name, target)] += 1
    return expected


def diagnostics(stderr):
    """Only primary spans count; labelled secondary spans stay in the snapshot."""
    actual = Counter()
    pending = None
    for text in stderr.splitlines():
        header = HEADER.match(text)
        if header:
            if pending:
                actual[(*pending, None, None)] += 1
            pending = header.groups()
        elif pending and (location := LOCATION.match(text)):
            file, line, _ = location.groups()
            actual[(*pending, file, int(line))] += 1
            pending = None
    if pending:
        actual[(*pending, None, None)] += 1
    return actual


def normalize(stderr, source, std, output):
    for path, replacement in (
        (str(source), source.name),
        (str(output), "$OUT"),
        (str(std), "$STD"),
        (str(source.parent), "$UI"),
        (str(ROOT), "$COMPILER"),
    ):
        stderr = stderr.replace(path, replacement)
    return "".join(
        line + "\n" for line in stderr.splitlines() if not PROGRESS.fullmatch(line)
    )


def check_annotations(expected, actual):
    missing = expected - actual
    if missing:
        raise ValueError(f"missing diagnostic(s): {dict(missing)}")
    unexpected = actual - expected
    if unexpected:
        raise ValueError(f"unannotated diagnostic(s): {dict(unexpected)}")


def run_case(source, command, std, bless):
    source = source.resolve()
    expected = annotations(source)
    with tempfile.TemporaryDirectory(prefix="siox-ui-") as temporary:
        output = Path(temporary) / "test"
        args = [*command, "--std", str(std), str(source)]
        if "// test" in source.read_text(encoding="utf-8").splitlines():
            args += ["--test", "--out", str(output)]
        else:
            args += ["--emit", "ir"]
        result = subprocess.run(
            args, cwd=source.parent, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, text=True, encoding="utf-8", timeout=30,
        )
        stderr = normalize(result.stderr, source, std, output)
    actual = diagnostics(stderr)
    try:
        check_annotations(expected, actual)
    except ValueError as error:
        raise ValueError(f"{error}\n{stderr}") from error
    wanted_status = int(any(key[0] == "error" for key in expected))
    if result.returncode != wanted_status:
        raise ValueError(
            f"exit status {result.returncode}, expected {wanted_status}; "
            "refusing to bless a crash or unexpected success/failure\n" + stderr
        )
    snapshot = stderr + f"exit-status: {result.returncode}\n"
    destination = source.with_suffix(".stderr")
    if bless:
        destination.write_text(snapshot, encoding="utf-8")
    else:
        if not destination.exists():
            raise ValueError("missing .stderr snapshot; review annotations then use --bless")
        saved = destination.read_text(encoding="utf-8")
        if saved != snapshot:
            raise ValueError("".join(difflib.unified_diff(
                saved.splitlines(keepends=True), snapshot.splitlines(keepends=True),
                fromfile=str(destination), tofile="actual diagnostics",
            )))
    return {key[1] for key in actual if key[1]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", nargs="?", type=Path, default=ROOT.parent / "siox-tests")
    parser.add_argument("--bless", action="store_true", help="update validated snapshots")
    parser.add_argument("--compiler", type=Path, help="use an already built sioxc")
    options = parser.parse_args()
    cases = sorted((options.corpus / "ui").glob("*.siox"))
    if not cases:
        parser.error(f"no UI cases in {options.corpus / 'ui'}")
    if options.compiler:
        command = [str(options.compiler.resolve())]
    else:
        cargo_args = ["--quiet", "--locked", "--manifest-path", str(ROOT / "Cargo.toml"),
                      "--bin", "sioxc"]
        if features := os.environ.get("SIOXC_FEATURES"):
            cargo_args += ["--features", features]
        try:
            result = subprocess.run(
                ["cargo", "build", *cargo_args, "--message-format=json-render-diagnostics"],
                cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True, encoding="utf-8",
            )
            artifacts = (json.loads(line) for line in result.stdout.splitlines())
            executable = next((
                artifact["executable"] for artifact in artifacts
                if artifact.get("reason") == "compiler-artifact"
                and artifact.get("target", {}).get("name") == "sioxc"
                and artifact.get("executable")
            ), None)
            if executable is None:
                raise ValueError("Cargo did not report the sioxc executable")
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            print(f"cannot build compiler: {error}", file=sys.stderr)
            return 1
        command = [executable]
    failures = 0
    covered = set()
    for case in cases:
        try:
            covered.update(run_case(case, command, ROOT / "std", options.bless))
            print(f"ok: {case.name}")
        except (ValueError, OSError, subprocess.TimeoutExpired) as error:
            failures += 1
            print(f"FAILED: {case}\n{error}", file=sys.stderr)
    registered = active_codes(ROOT / "src/diag.rs")
    missing = registered - covered
    if missing:
        print("Missing UI coverage: " + ", ".join(sorted(missing)), file=sys.stderr)
    print(f"{len(cases) - failures} UI cases passed; {failures} failed; "
          f"{len(registered & covered)}/{len(registered)} diagnostic codes covered")
    return int(bool(failures or missing))


if __name__ == "__main__":
    sys.exit(main())

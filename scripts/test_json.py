#!/usr/bin/env python3
"""Validate real JSON diagnostics, source coordinates and atomic migration fixes."""

import argparse
from collections import Counter
import json
from pathlib import Path
import subprocess
import sys
import tempfile

import test_ui as ui


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key {key}")
        result[key] = value
    return result


def validate_span(span):
    require(isinstance(span, dict), "a span must be an object")
    for key in ("file_id", "byte_start", "byte_end"):
        require(type(span.get(key)) is int and span[key] >= 0, f"invalid {key}")
    require("file_name" in span and (span["file_name"] is None or isinstance(span["file_name"], str)),
            "invalid span filename")
    keys = ("line_start", "column_start", "line_end", "column_end")
    require(all(key in span for key in keys), "missing span coordinates")
    coordinates = [span[key] for key in keys]
    if coordinates == [None] * 4:
        return
    require(all(type(value) is int and value >= 1 for value in coordinates), "invalid coordinates")
    require(span["file_name"] is not None and span["byte_start"] <= span["byte_end"], "invalid location")
    path = Path(span["file_name"])
    if path.is_file():
        source = path.read_bytes()
        require(span["byte_end"] <= len(source), "span outside its source")
        for suffix in ("start", "end"):
            prefix = source[:span[f"byte_{suffix}"]].decode("utf-8")
            lines = prefix.split("\n")
            require((len(lines), len(lines[-1]) + 1) ==
                    (span[f"line_{suffix}"], span[f"column_{suffix}"]), "wrong Unicode coordinates")


def validate_diagnostic(value):
    require(isinstance(value, dict) and value.get("$message_type") == "diagnostic", "not a diagnostic")
    require("code" in value and (value["code"] is None or isinstance(value["code"], str)), "invalid code")
    require(value.get("severity") in ("error", "warning", "note", "help"), "invalid severity")
    require(isinstance(value.get("message"), str), "invalid message")
    require("primary" in value, "missing primary")
    if value["primary"] is not None:
        validate_span(value["primary"])
    for key in ("labels", "notes", "suggestions"):
        require(isinstance(value.get(key), list), f"invalid {key}")
    for label in value["labels"]:
        require(isinstance(label, dict) and isinstance(label.get("message"), str), "invalid label")
        validate_span(label.get("span"))
    require(all(isinstance(note, str) for note in value["notes"]), "invalid notes")
    require("help" in value and (value["help"] is None or isinstance(value["help"], str)), "invalid help")
    for suggestion in value["suggestions"]:
        require(isinstance(suggestion, dict) and suggestion.get("applicability") in
                ("MachineApplicable", "MaybeIncorrect", "HasPlaceholders", "Unspecified"), "invalid applicability")
        require(isinstance(suggestion.get("edits"), list) and suggestion["edits"], "empty or missing edit group")
        for edit in suggestion["edits"]:
            require(isinstance(edit, dict) and isinstance(edit.get("replacement"), str), "invalid replacement")
            validate_span(edit.get("span"))


def decode(stderr):
    if not stderr:
        return []
    require(stderr.endswith("\n"), "JSON diagnostics must end in a newline")
    records = []
    # Split only on LF: other Unicode separators are valid JSON string content.
    for line in stderr.split("\n")[:-1]:
        require(line, "blank line in JSON stream")
        record = json.loads(line, object_pairs_hook=unique_object)
        validate_diagnostic(record)
        require(json.loads(json.dumps(record, ensure_ascii=False)) == record, "JSON does not round-trip")
        records.append(record)
    return records


def run(command, arguments, status):
    output = subprocess.run([*command, "--error-format=json", *arguments],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, encoding="utf-8", timeout=30)
    require(output.returncode == status, f"exit {output.returncode}, expected {status}: {output.stderr}")
    return output, decode(output.stderr)


def apply_edits(source, records):
    edits = []
    for record in records:
        for suggestion in record["suggestions"]:
            require(suggestion["applicability"] == "MachineApplicable", "migration needs review")
            edits.extend(suggestion["edits"])
    require(edits, "migration provided no fix")
    for index, edit in enumerate(edits):
        span = edit["span"]
        require(span["line_start"] is not None, "edit has no valid location")
        require(Path(span["file_name"]).resolve() == source.resolve(), "edit targets a different file")
        for previous in edits[:index]:
            other = previous["span"]
            require(span["byte_end"] <= other["byte_start"] or other["byte_end"] <= span["byte_start"],
                    "overlapping edits")
    fixed = source.read_bytes()
    for _, edit in sorted(enumerate(edits), key=lambda item: (item[1]["span"]["byte_start"], item[0]), reverse=True):
        span = edit["span"]
        fixed = fixed[:span["byte_start"]] + edit["replacement"].encode("utf-8") + fixed[span["byte_end"]:]
    return fixed


def check_case(source, command):
    source = source.resolve()
    expected = ui.annotations(source)
    with tempfile.TemporaryDirectory(prefix="siox-json-") as directory:
        directory = Path(directory)
        arguments = ["--std", str(ui.ROOT / "std"), str(source)]
        native = "// test" in source.read_text(encoding="utf-8").splitlines()
        arguments += ["--test", "--out", str(directory / "test")] if native else ["--emit", "ir"]
        status = int(any(key[0] == "error" for key in expected))
        _, records = run(command, arguments, status)
        actual = Counter()
        for record in records:
            primary = record["primary"]
            actual[(record["severity"], record["code"],
                    Path(primary["file_name"]).name if primary and primary["file_name"] is not None else None,
                    primary["line_start"] if primary else None)] += 1
        ui.check_annotations(expected, actual)
        if native and status == 0:
            require((directory / "test").is_file(), "JSON mode lost the native artifact")
        fixed = "// json-fix" in source.read_text(encoding="utf-8").splitlines()
        if fixed:
            target = directory / source.name
            target.write_bytes(apply_edits(source, records))
            _, after = run(command, ["--std", str(ui.ROOT / "std"), str(target), "--emit", "ir"], 0)
            require(not any(record["severity"] == "error" for record in after), "fix did not compile")
        return {record["code"] for record in records if record["code"]}, fixed


def check_cli(command, corpus):
    with tempfile.TemporaryDirectory(prefix="siox-json-cli-") as directory:
        directory = Path(directory)
        missing = directory / 'missing "λ\\\n.siox'
        source = corpus / "ui" / "test_mode_pass.siox"
        for arguments in ([str(missing)], ["--explain", "E-P000"],
                          [str(missing), "--test", "--emit", "ir"],
                          [str(source), "--std", str(ui.ROOT / "std"), "--top", "__missing__"]):
            output, records = run(command, arguments, 1)
            require(not output.stdout and records, "failure is not structured")
            require(any(record["severity"] == "error" and record["primary"] is None for record in records),
                    "missing unspanned CLI/input/selection error")
        bad_output = directory / "directory-not-executable"
        bad_output.mkdir()
        _, records = run(command, [str(source), "--std", str(ui.ROOT / "std"),
                                  "--test", "--out", str(bad_output)], 1)
        require(any(record["severity"] == "error" and record["primary"] is None for record in records),
                "backend failure is not structured")
        for emit in ("source", "tokens"):
            output, records = run(command, [str(source), "--emit", emit, "--verbose", "-A", "warnings",
                                          "--std", str(ui.ROOT / "std")], 0)
            require(output.stdout and not records, f"{emit} artifact/JSON stream changed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", nargs="?", type=Path, default=ui.ROOT.parent / "siox-tests")
    parser.add_argument("--compiler", type=Path, help="use an already built sioxc")
    options = parser.parse_args()
    cases = sorted((options.corpus / "ui").glob("*.siox"))
    if not cases:
        parser.error("no diagnostic UI cases")
    try:
        command = ui.compiler_command(options.compiler)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"cannot build compiler: {error}", file=sys.stderr)
        return 1
    failures, covered, fixed = 0, set(), set()
    for source in cases:
        try:
            codes, applied = check_case(source, command)
            covered.update(codes)
            if applied:
                fixed.add(source.name)
            print(f"ok: JSON {source.name}")
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            failures += 1
            print(f"FAILED: {source}: {error}", file=sys.stderr)
    try:
        check_cli(command, options.corpus.resolve())
        require({"process_label_migration.siox", "metadata_migration.siox"} <= fixed, "missing migration round-trip")
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        failures += 1
        print(f"FAILED: JSON CLI/fixes: {error}", file=sys.stderr)
    registered = ui.active_codes(ui.ROOT / "src/diag.rs")
    missing = registered - covered
    if missing:
        print("Missing JSON coverage: " + ", ".join(sorted(missing)), file=sys.stderr)
    print(f"{len(cases)} JSON cases; {failures} failure(s); {len(covered & registered)}/{len(registered)} codes; "
          f"{len(fixed)} apply/recompile migrations")
    return int(bool(failures or missing))


if __name__ == "__main__":
    sys.exit(main())

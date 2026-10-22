import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import test_ui as ui


class UiTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.source = Path(self.temporary.name) / "case.siox"
        self.std = Path(self.temporary.name) / "std"

    def write(self, text):
        self.source.write_text(text, encoding="utf-8")
        return ui.annotations(self.source)

    def rendered(self, line=2, code="E-P003", severity="error", message="wrong type"):
        return (
            f"== stage 2: parse == 1 item(s) in 1 module(s)\n"
            f"{severity}[{code}]: {message}\n"
            f"  --> {self.source}:{line}:5\n"
            "   |\n 2 | bad();\n   |     ^\n"
            "   = first declared here (at 1:1)\n"
            "   = note: extra context\n   = help: change the type\n"
        )

    def run_case(self, stderr=None, status=1, bless=True):
        completed = subprocess.CompletedProcess([], status, stderr=stderr or self.rendered())
        with patch.object(ui.subprocess, "run", return_value=completed) as run:
            codes = ui.run_case(self.source, ["sioxc"], self.std, bless)
        return codes, run.call_args

    def test_annotation_carets_and_repeated_diagnostics(self):
        expected = self.write("bad(); //~ ERROR E-P003\n//~^ ERROR E-P003\n")
        self.assertEqual(expected[("error", "E-P003", "case.siox", 1)], 2)

    def test_malformed_annotations_fail(self):
        for text in ("//~ ERROR E-P3", "//~ error E-P003", "//~^ ERROR E-P003"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.write(text)

    def test_snapshot_preserves_context_and_normalizes_paths(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        codes, call = self.run_case()
        snapshot = self.source.with_suffix(".stderr").read_text()
        self.assertEqual(codes, {"E-P003"})
        self.assertIn("--> case.siox:2:5", snapshot)
        self.assertIn("= note: extra context", snapshot)
        self.assertIn("= help: change the type", snapshot)
        self.assertIn("first declared here (at 1:1)", snapshot)
        self.assertNotIn("== stage", snapshot)
        self.assertTrue(snapshot.endswith("exit-status: 1\n"))
        self.assertEqual(call.args[0][-2:], ["--emit", "ir"])
        self.run_case(bless=False)

    def test_message_drift_fails_until_blessed(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        self.run_case()
        with self.assertRaisesRegex(ValueError, "actual diagnostics"):
            self.run_case(self.rendered(message="changed wording"), bless=False)

    def test_unannotated_duplicate_cannot_be_blessed(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        with self.assertRaisesRegex(ValueError, "unannotated"):
            self.run_case(self.rendered() * 2)
        self.assertFalse(self.source.with_suffix(".stderr").exists())

    def test_wrong_code_line_or_file_cannot_be_blessed(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        for stderr in (
            self.rendered(code="E-P001"), self.rendered(line=1),
            self.rendered().replace(str(self.source), "other.siox"),
        ):
            with self.subTest(stderr=stderr), self.assertRaisesRegex(ValueError, "missing"):
                self.run_case(stderr)

    def test_codeless_annotation_rejects_a_coded_diagnostic(self):
        self.write("module case;\nbad(); //~ ERROR\n")
        with self.assertRaisesRegex(ValueError, "missing"):
            self.run_case()

    def test_codeless_parser_error(self):
        self.write("module case;\nbad(); //~ ERROR\n")
        self.run_case(self.rendered().replace("error[E-P003]", "error"))

    def test_diagnostic_without_primary_span_fails(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        with self.assertRaisesRegex(ValueError, "missing"):
            self.run_case("error[E-P003]: wrong type\n")

    def test_crash_and_wrong_exit_status_cannot_be_blessed(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        for status in (0, 101, -9):
            with self.subTest(status=status), self.assertRaisesRegex(ValueError, "exit status"):
                self.run_case(status=status)

    def test_warning_expects_success(self):
        self.write("module case;\nbad(); //~ WARNING W-P003\n")
        self.run_case(self.rendered(code="W-P003", severity="warning"), status=0)

    def test_missing_snapshot_fails_without_bless(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        with self.assertRaisesRegex(ValueError, "missing .stderr"):
            self.run_case(bless=False)

    def test_test_directive_uses_native_compile_in_temporary_directory(self):
        self.write("// test\nbad(); //~ ERROR E-P003\n")
        _, call = self.run_case()
        args = call.args[0]
        self.assertIn("--test", args)
        self.assertNotIn("--emit", args)
        self.assertNotEqual(Path(args[-1]).parent, self.source.parent)

    def test_empty_corpus_is_not_a_pass(self):
        with patch.object(ui.sys, "argv", ["test-ui", self.temporary.name]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                ui.main()
        self.assertEqual(error.exception.code, 2)

    def test_retired_codes_are_excluded_from_coverage(self):
        self.source.write_text(
            '    /// A live error.\n    pub const LIVE: &str = "E-P003";\n'
            '    /// Retired: reserved for the old scheduler.\n'
            '    /// Never reuse this code.\n    pub const OLD: &str = "E-P028";\n'
            '    pub const WARNING: &str = "W-P003";\n'
        )
        self.assertEqual(ui.active_codes(self.source), {"E-P003", "W-P003"})

    def test_empty_code_catalogue_is_not_a_pass(self):
        self.source.write_text("")
        with self.assertRaisesRegex(ValueError, "no diagnostic codes"):
            ui.active_codes(self.source)

    def test_compiler_timeout_does_not_bless_a_snapshot(self):
        self.write("module case;\nbad(); //~ ERROR E-P003\n")
        with patch.object(ui.subprocess, "run", side_effect=subprocess.TimeoutExpired("sioxc", 30)):
            with self.assertRaises(subprocess.TimeoutExpired):
                ui.run_case(self.source, ["sioxc"], self.std, True)
        self.assertFalse(self.source.with_suffix(".stderr").exists())

    def test_cli_builds_selected_features_before_running_cases(self):
        directory = Path(self.temporary.name) / "ui"
        directory.mkdir()
        (directory / "case.siox").write_text("module case;")
        with patch.object(sys, "argv", ["test-ui", self.temporary.name]):
            with patch.dict(ui.os.environ, {"SIOXC_FEATURES": "bitpack"}):
                with patch.object(ui.subprocess, "run") as build:
                    build.return_value.stdout = json.dumps({
                        "reason": "compiler-artifact", "target": {"name": "sioxc"},
                        "executable": "/custom-target/debug/sioxc",
                    }) + "\n"
                    with patch.object(ui, "run_case", return_value={"E-P003"}) as case:
                        with patch.object(ui, "active_codes", return_value={"E-P003"}):
                            with contextlib.redirect_stdout(io.StringIO()):
                                self.assertEqual(ui.main(), 0)
        self.assertEqual(build.call_args.args[0][:2], ["cargo", "build"])
        self.assertEqual(build.call_args.args[0][-3:-1], ["--features", "bitpack"])
        self.assertEqual(case.call_args.args[1], ["/custom-target/debug/sioxc"])

    def test_failed_or_unreported_build_never_runs_a_stale_compiler(self):
        directory = Path(self.temporary.name) / "ui"
        directory.mkdir()
        (directory / "case.siox").write_text("module case;")
        for stdout in ("", "not JSON", '{"reason":"build-finished","success":true}\n'):
            with self.subTest(stdout=stdout):
                with patch.object(sys, "argv", ["test-ui", self.temporary.name]):
                    with patch.object(ui.subprocess, "run") as build:
                        build.return_value.stdout = stdout
                        with patch.object(ui, "run_case") as case:
                            with contextlib.redirect_stderr(io.StringIO()):
                                self.assertEqual(ui.main(), 1)
                        case.assert_not_called()
        with patch.object(sys, "argv", ["test-ui", self.temporary.name]):
            with patch.object(ui.subprocess, "run", side_effect=subprocess.CalledProcessError(101, "cargo")):
                with patch.object(ui, "run_case") as case:
                    with contextlib.redirect_stderr(io.StringIO()):
                        self.assertEqual(ui.main(), 1)
                case.assert_not_called()

    def test_missing_code_coverage_fails_the_cli(self):
        directory = Path(self.temporary.name) / "ui"
        directory.mkdir()
        (directory / "case.siox").write_text("module case;")
        with patch.object(sys, "argv", ["test-ui", self.temporary.name, "--compiler", "sioxc"]):
            with patch.object(ui, "run_case", return_value={"E-P003"}):
                with patch.object(ui, "active_codes", return_value={"E-P003", "W-P003"}):
                    with contextlib.redirect_stdout(io.StringIO()):
                        with contextlib.redirect_stderr(io.StringIO()) as stderr:
                            self.assertEqual(ui.main(), 1)
        self.assertIn("W-P003", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()

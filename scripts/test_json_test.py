import json
from pathlib import Path
import tempfile
import unittest

import test_json as gate


class JsonGateTests(unittest.TestCase):
    def diagnostic(self):
        return {"$message_type": "diagnostic", "code": None, "severity": "error",
                "message": 'quote " slash \\ newline\nλ\u2028🙂', "primary": None,
                "labels": [], "notes": ["context"], "help": None, "suggestions": []}

    def test_unicode_newlines_and_forward_compatible_fields(self):
        record = self.diagnostic()
        record["future_field"] = 17
        encoded = json.dumps(record, ensure_ascii=False) + "\n"
        self.assertEqual(gate.decode(encoded), [record])
        self.assertEqual(gate.decode(""), [])

    def test_progress_duplicate_keys_and_partial_lines_are_rejected(self):
        for text in ('== stage 2: parse ==\n', '{"severity":"error","severity":"warning"}\n',
                     json.dumps(self.diagnostic()), '\n', 'null\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                gate.decode(text)

    def test_missing_and_mistyped_fields_are_rejected(self):
        for key, value in (("primary", False), ("notes", "not an array"), ("labels", [None]),
                           ("severity", "unknown"), ("suggestions", [{"applicability": "MachineApplicable", "edits": []}])):
            record = self.diagnostic()
            record[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                gate.decode(json.dumps(record) + "\n")
        record = self.diagnostic()
        del record["help"]
        with self.assertRaises(ValueError):
            gate.decode(json.dumps(record) + "\n")


    def test_atomic_application_keeps_original_offsets_and_rejects_overlap(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "case.siox"
            source.write_bytes("aλb".encode())
            def edit(start, end, replacement):
                return {"span": {"file_name": str(source), "byte_start": start,
                                 "byte_end": end, "line_start": 1}, "replacement": replacement}
            def records(edits):
                return [{"suggestions": [{"applicability": "MachineApplicable", "edits": edits}]}]
            self.assertEqual(gate.apply_edits(source, records([
                edit(1, 3, "λ"), edit(3, 3, "first"), edit(3, 3, "second")
            ])).decode(), "aλfirstsecondb")
            for edits in ([edit(0, 3, "x"), edit(1, 1, "insert")],
                          [edit(0, 3, "x"), edit(1, 3, "overlap")]):
                with self.subTest(edits=edits), self.assertRaisesRegex(ValueError, "overlapping"):
                    gate.apply_edits(source, records(edits))


if __name__ == "__main__":
    unittest.main()

"""The alias profiles must inspect each elaborated instance's waveform."""

from pathlib import Path
import runpy
import unittest


check_profile = runpy.run_path(str(Path(__file__).with_name("check-vcd.py")))["check_profile"]


class EntityAliasWaveforms(unittest.TestCase):
    def wave(self, root, expected):
        return {
            f"{root}.{instance}.value": [(0, f"{value:064b}")]
            for instance, value in zip(("direct", "group[0]", "group[1]"), expected)
        }

    def test_static_aliases(self):
        check_profile("entity_aliases_test", self.wave("AliasTest", (7, 7, 7)))

    def test_ordered_connections(self):
        check_profile("entity_alias_connections_test", self.wave("AliasOrder", (13, 13, 17)))

    def test_wrong_element_value_is_rejected(self):
        with self.assertRaises(AssertionError):
            check_profile("entity_alias_connections_test", self.wave("AliasOrder", (13, 13, 13)))

    def test_missing_child_signal_is_rejected(self):
        changes = self.wave("AliasTest", (7, 7, 7))
        del changes["AliasTest.group[1].value"]
        with self.assertRaises(AssertionError):
            check_profile("entity_aliases_test", changes)


if __name__ == "__main__":
    unittest.main()

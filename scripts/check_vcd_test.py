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


class GenericAliasWaveforms(unittest.TestCase):
    def wave(self):
        data = {
            f"GenericAliases.{instance}.{field}": [(0, f"{value:064b}")]
            for instance, field, value in (
                ("direct", "count", 9), ("group[0]", "count", 10),
                ("group[1]", "count", 10), ("selected", "count", 11),
                ("group[0]", "value", 11), ("group[1]", "value", 22),
                ("selected", "value", 31),
            )
        }
        data["GenericAliases.direct.value"] = [(0, "2.5")]
        return data

    def test_generic_entity_parameters_and_values(self):
        check_profile("generic_entity_aliases_test", self.wave())

    def test_wrong_parameter_value_is_rejected(self):
        data = self.wave()
        data["GenericAliases.direct.count"] = [(0, f"{103:064b}")]
        with self.assertRaises(AssertionError):
            check_profile("generic_entity_aliases_test", data)

    def test_wrong_real_value_is_rejected(self):
        data = self.wave()
        data["GenericAliases.direct.value"] = [(0, "3.5")]
        with self.assertRaises(AssertionError):
            check_profile("generic_entity_aliases_test", data)

    def test_generic_data_alias(self):
        check_profile("generic_data_aliases_test", {"AliasData.wire.value": [(0, f"{6:09b}")], "AliasData.wide.value": [(0, f"{11:010b}")]})

    def test_wrong_wider_alias_value_is_rejected(self):
        with self.assertRaises(AssertionError):
            check_profile("generic_data_aliases_test", {"AliasData.wire.value": [(0, f"{6:09b}")], "AliasData.wide.value": [(0, f"{6:010b}")]})

    def test_wrong_generic_width_is_rejected(self):
        with self.assertRaises(AssertionError):
            check_profile("generic_data_aliases_test", {"AliasData.wire.value": [(0, f"{6:08b}")]})


class EnumConstantWaveforms(unittest.TestCase):
    def wave(self):
        return {f"EnumConstantValues.dut.{name}": events for name, events in {
            "a": [(0, "1"), (1_000_000, "x"), (2_000_000, "z")],
            "b": [(0, "0"), (1_000_000, "1")],
            "both": [(0, "0"), (1_000_000, "x")],
            "either": [(0, "1"), (1_000_000, "x"), (1_000_000, "1")],
            "inverted": [(0, "0"), (1_000_000, "x")],
            "shared": [(0, "x"), (2_000_000, "1")],
        }.items()}

    def test_source_encoding_is_reactive(self):
        check_profile("enum_constant_values_test", self.wave())

    def test_wrong_symbol_time_or_driver_resolution_is_rejected(self):
        for signal, events in [("a", [(0, "1"), (1_000_000, "x"), (2_000_000, "x")]),
                               ("both", [(0, "0"), (2_000_000, "x")]),
                               ("shared", [(0, "0"), (2_000_000, "1")]),
                               ("either", [(0, "1"), (1_000_000, "1")])]:
            data = self.wave()
            data[f"EnumConstantValues.dut.{signal}"] = events
            with self.assertRaises(AssertionError):
                check_profile("enum_constant_values_test", data)


class EnumPhaseWaveforms(unittest.TestCase):
    def wave(self):
        return {f"EnumPhaseValues.dut.{name}": events for name, events in {
            "value": [(0, "low"), (1_000_000, "high")],
            "seen": [(0, "low"), (1_000_000, "high")],
            "inherited": [(0, "high")],
            "normalized": [(0, "next")],
            "code": [(0, f"{9:064b}"), (1_000_000, f"{10:064b}")],
        }.items()}

    def test_shared_numbers_defaults_and_symbols(self):
        check_profile("enum_phase_values_test", self.wave())

    def test_wrong_number_symbol_or_time_is_rejected(self):
        for name, events in [("code", [(0, f"{0:064b}"), (1_000_000, f"{1:064b}")]),
                             ("inherited", [(0, "low")]),
                             ("normalized", [(0, "first")]),
                             ("seen", [(0, "low"), (2_000_000, "high")])]:
            data = self.wave()
            data[f"EnumPhaseValues.dut.{name}"] = events
            with self.assertRaises(AssertionError):
                check_profile("enum_phase_values_test", data)


class GenericLayoutWaveforms(unittest.TestCase):
    def test_named_function_width_sizes_the_reactive_port(self):
        check_profile("generic_layout_values_test", {
            "GenericLayoutValues.dut.value": [(0, "101"), (1_000_000, "011")],
        })

    def test_wrong_width_value_or_time_is_rejected(self):
        for initial, when, updated in [("0101", 1_000_000, "0011"),
                                       ("101", 1_000_000, "101"),
                                       ("101", 2_000_000, "011")]:
            with self.assertRaises(AssertionError):
                check_profile("generic_layout_values_test", {
                    "GenericLayoutValues.dut.value": [(0, initial), (when, updated)],
                })


class TraitValueWaveforms(unittest.TestCase):
    def test_typed_value_contract_remains_reactive(self):
        check_profile("trait_value_arguments_test", {
            "TraitValueArgumentsTest.dut.result": [(0, f"{42:064b}"), (1_000_000, f"{47:064b}")],
        })

    def test_wrong_value_or_time_is_rejected(self):
        for when, value in [(1_000_000, 42), (2_000_000, 47)]:
            with self.assertRaises(AssertionError):
                check_profile("trait_value_arguments_test", {
                    "TraitValueArgumentsTest.dut.result": [(0, f"{42:064b}"), (when, f"{value:064b}")],
                })


if __name__ == "__main__":
    unittest.main()

"""Semantic waveform profiles must reject incorrect values and declarations."""

from pathlib import Path
import runpy
import tempfile
import unittest


checker = runpy.run_path(str(Path(__file__).with_name("check-vcd.py")))
check_profile = checker["check_profile"]
read_vcd = checker["read_vcd"]


class AggregateConstantWaveforms(unittest.TestCase):
    def wave(self):
        return "\n".join([
            "$timescale 1fs $end",
            "$scope module AggregateConstantContexts $end",
            "$scope module dut $end",
            "$var wire 3 v0 source $end",
            "$var wire 3 v1 result $end",
            "$var wire 64 v2 lanes[0] $end",
            "$var wire 64 v3 lanes[1] $end",
            "$upscope $end", "$upscope $end", "$enddefinitions $end",
            "#0", "b101 v0", "b101 v1",
            f"b{10:064b} v2", f"b{11:064b} v3",
            "#1000000", "b011 v0", "b011 v1", "#2000000",
        ])

    def verify(self, text):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.vcd"
            path.write_text(text)
            declarations = {}
            changes, timestamps = read_vcd(path, declarations)
            check_profile("aggregate_constant_contexts_test", changes, declarations)
            self.assertEqual(read_vcd(path), (changes, timestamps))
            return timestamps

    def test_source_required_timeline_and_reader_compatibility(self):
        self.assertEqual(self.verify(self.wave()), [0, 1_000_000, 2_000_000])

    def test_every_required_declaration_is_checked(self):
        for declaration in [line for line in self.wave().splitlines() if line.startswith("$var ")]:
            with self.subTest(declaration=declaration):
                with self.assertRaises(AssertionError):
                    self.verify(self.wave().replace(declaration, ""))

    def test_wider_declarations_are_not_hidden_by_short_values(self):
        for old, new in [("wire 3 v0", "wire 4 v0"),
                         ("wire 3 v1", "wire 8 v1"),
                         ("wire 64 v2", "wire 65 v2"),
                         ("wire 64 v3", "wire 128 v3")]:
            with self.subTest(declaration=new):
                with self.assertRaises(AssertionError):
                    self.verify(self.wave().replace(old, new))

    def test_wrong_values_missing_transitions_and_times_are_rejected(self):
        for old, new in [("b101 v0", "b100 v0"),
                         ("b011 v1", ""),
                         ("b011 v0", "b010 v0"),
                         (f"b{10:064b} v2", f"b{11:064b} v2"),
                         (f"b{11:064b} v3", f"b{12:064b} v3"),
                         ("#1000000", "#2000000")]:
            with self.subTest(mutation=(old, new)):
                with self.assertRaises(AssertionError):
                    self.verify(self.wave().replace(old, new))

    def test_wrong_signal_kind_is_rejected(self):
        with self.assertRaises(AssertionError):
            self.verify(self.wave().replace("$var wire 3 v1", "$var string 3 v1"))

    def test_missing_declaration_metadata_is_rejected(self):
        with self.assertRaisesRegex(AssertionError, "declaration metadata"):
            check_profile("aggregate_constant_contexts_test", {})


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


class EnumOwnerAliasWaveforms(unittest.TestCase):
    def wave(self):
        data = {f"EnumOwnerAliasTest.dut.{name}": [(0, f"{first:064b}"),
                                                (1_000_000, f"{second:064b}")]
                for name, first, second in (("left_code", 9, 17), ("right_code", 3, 5),
                                            ("matched_code", 9, 17), ("forwarded_code", 9, 17))}
        data["EnumOwnerAliasTest.dut.zero_code"] = [(0, f"{31:064b}")]
        return data

    def test_aliases_keep_distinct_domains_and_explicit_constructors(self):
        check_profile("enum_owner_alias_test", self.wave())

    def test_wrong_owner_default_value_or_time_is_rejected(self):
        for name, events in (("right_code", [(0, f"{9:064b}"), (1_000_000, f"{17:064b}")]),
                             ("left_code", [(0, f"{9:064b}")]),
                             ("zero_code", [(0, f"{23:064b}")]),
                             ("forwarded_code", [(0, f"{9:064b}"), (2_000_000, f"{17:064b}")])):
            wave = self.wave()
            wave[f"EnumOwnerAliasTest.dut.{name}"] = events
            with self.assertRaises(AssertionError):
                check_profile("enum_owner_alias_test", wave)


class ComputedWidthWaveforms(unittest.TestCase):
    def wave(self):
        return {"LengthExprTest.dut.source": [(0, "1001")],
                "LengthExprTest.dut.value": [(0, "001")]}

    def test_runtime_truncation_to_computed_width(self):
        check_profile("length_expr_test", self.wave())

    def test_wrong_width_value_or_time_is_rejected(self):
        for events in [[(0, "1001")], [(0, "000")], [(1_000_000, "001")]]:
            wave = self.wave()
            wave["LengthExprTest.dut.value"] = events
            with self.assertRaises(AssertionError):
                check_profile("length_expr_test", wave)


class PackedLogicIndexWaveforms(unittest.TestCase):
    def wave(self):
        return {f"PackedLogicIndexTest.probe.{name}": events for name, events in {
            "index": [(time * 1_000_000, f"{value:064b}")
                      for time, value in enumerate((6, 5, 4, 3, 0, 8, 7, 6))],
            "seen": [(0, "x"), (7_000_000, "z")],
            "nested": [(0, "x"), (7_000_000, "z")],
            "wide": [(0, "x")],
            "dynamic": [(0, "x"), (1_000_000, "z"), (2_000_000, "x"),
                        (3_000_000, "x"), (4_000_000, "x"), (5_000_000, "0"),
                        (6_000_000, "1"), (7_000_000, "x"), (7_000_000, "z")],
            "value.data": [(0, "01xzxx01x"), (7_000_000, "01zxxx01x")],
            "value.wide": [(0, "01xzxx01x" * 9)],
        }.items()}

    def test_checked_indices_and_metadata_only_updates(self):
        check_profile("packed_logic_index_test", self.wave())

    def test_missing_signals_are_rejected(self):
        for signal in self.wave():
            wave = self.wave()
            del wave[signal]
            with self.assertRaises(AssertionError):
                check_profile("packed_logic_index_test", wave)

    def test_wrong_values_widths_and_times_are_rejected(self):
        for name, events in [("index", [(0, "110")]),
                             ("seen", [(0, "0"), (7_000_000, "z")]),
                             ("nested", [(0, "x"), (8_000_000, "z")]),
                             ("wide", [(0, "0")]),
                             ("value.data", [(0, "01xzxx01x")]),
                             ("value.wide", [(0, "01xzxx01x" * 8)])]:
            wave = self.wave()
            wave[f"PackedLogicIndexTest.probe.{name}"] = events
            with self.assertRaises(AssertionError):
                check_profile("packed_logic_index_test", wave)

    def test_dynamic_read_timeline_and_delta_updates_are_checked(self):
        original = self.wave()["PackedLogicIndexTest.probe.dynamic"]
        for events in [original[:-1], original[:-2] + original[-1:],
                       original[:1] + [(2_000_000, "z")] + original[2:],
                       original[:-2] + list(reversed(original[-2:]))]:
            wave = self.wave()
            wave["PackedLogicIndexTest.probe.dynamic"] = events
            with self.assertRaises(AssertionError):
                check_profile("packed_logic_index_test", wave)


class ConstantContextWaveforms(unittest.TestCase):
    def wave(self):
        wave = {f"ConstantContexts.dut.{name}": [(0, f"{value:064b}")]
                for name, value in {"constant_choice": 13, "runtime_choice": 13,
                                    "generated_choice": 14}.items()}
        wave["ConstantContexts.dut.source"] = [(0, "low"), (1_000_000, "above")]
        wave["ConstantContexts.dut.runtime_choice"].append((1_000_000, f"{21:064b}"))
        wave["ConstantContexts.dut.packed_choice"] = [(0, f"{8:08b}")]
        wave["ConstantContexts.dut.wrapped_choice"] = [(0, f"{144:08b}")]
        return wave

    def test_constant_and_runtime_source_order_agree(self):
        check_profile("consteval_contexts_test", self.wave())

    def test_wrong_constant_runtime_result_or_time_is_rejected(self):
        for name, time, value in [("constant_choice", 0, 21),
                                  ("runtime_choice", 0, 21),
                                  ("generated_choice", 0, 99),
                                  ("constant_choice", 1_000_000, 13)]:
            wave = self.wave()
            wave[f"ConstantContexts.dut.{name}"] = [(time, f"{value:064b}")]
            with self.assertRaises(AssertionError):
                check_profile("consteval_contexts_test", wave)

    def test_runtime_transition_value_and_time_are_checked(self):
        for events in [[(0, f"{13:064b}")],
                       [(0, f"{13:064b}"), (1_000_000, f"{13:064b}")],
                       [(0, f"{13:064b}"), (2_000_000, f"{21:064b}")]]:
            wave = self.wave()
            wave["ConstantContexts.dut.runtime_choice"] = events
            with self.assertRaises(AssertionError):
                check_profile("consteval_contexts_test", wave)

    def test_packed_initializer_width_value_and_time_are_checked(self):
        for name, events in [("packed_choice", [(0, f"{8:064b}")]),
                             ("wrapped_choice", [(0, f"{400:09b}")]),
                             ("wrapped_choice", [(1_000_000, f"{144:08b}")])]:
            wave = self.wave()
            wave[f"ConstantContexts.dut.{name}"] = events
            with self.assertRaises(AssertionError):
                check_profile("consteval_contexts_test", wave)


class EnumPhaseWaveforms(unittest.TestCase):
    def wave(self):
        return {f"EnumPhaseValues.dut.{name}": events for name, events in {
            "value": [(0, "low"), (1_000_000, "high")],
            "seen": [(0, "low"), (1_000_000, "high")],
            "inherited": [(0, "high")],
            "normalized": [(0, "next")],
            "code": [(0, f"{9:064b}"), (1_000_000, f"{10:064b}")],
            "macro_value": [(0, "next")],
            "macro_code": [(0, f"{22:064b}")],
        }.items()}

    def test_shared_numbers_defaults_and_symbols(self):
        check_profile("enum_phase_values_test", self.wave())

    def test_wrong_number_symbol_or_time_is_rejected(self):
        for name, events in [("code", [(0, f"{0:064b}"), (1_000_000, f"{1:064b}")]),
                             ("inherited", [(0, "low")]),
                             ("normalized", [(0, "first")]),
                             ("seen", [(0, "low"), (2_000_000, "high")]),
                             ("macro_value", [(0, "first")]),
                             ("macro_code", [(0, f"{0:064b}")])]:
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

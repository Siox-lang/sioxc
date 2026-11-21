from pathlib import Path
import unittest

import check_registries as guard


class RegistryGuardTests(unittest.TestCase):
    def fields(self, source):
        return {finding.key for finding in guard.findings(source, "src/types/example.rs")}

    def test_wrapped_named_and_tuple_state_tables(self):
        self.assertEqual(self.fields("""
            struct Checker<'a> {
                declarations: std::cell::RefCell<HashMap<String, &'a FnDecl>>,
                names: HashSet<(String, u32)>,
                indexed: BTreeMap<&'a str, FnDecl>,
                by_id: HashMap<DefId, FnDecl>,
                members: HashMap<(DefId, String), FnDecl>,
                nested_members: HashMap<DefId, HashMap<String, FnDecl>>,
                labels: HashMap<Span, String>,
            }
            struct Registry(pub HashMap<String, FnDecl>);
            enum Cache { Empty, Full { by_name: HashMap<String, FnDecl> } }
        """), {
            "src/types/example.rs:Checker.declarations", "src/types/example.rs:Checker.names",
            "src/types/example.rs:Checker.indexed", "src/types/example.rs:Registry.0",
            "src/types/example.rs:Cache::Full.by_name",
        })

    def test_comments_literals_lifetimes_and_test_modules_are_not_state(self):
        self.assertEqual(self.fields('''
            // struct Wrong { values: HashMap<String, Wrong> }
            /* outer /* nested */ struct Wrong { values: HashMap<String, Wrong> } */
            const TEXT: &str = r###"struct Wrong { values: HashMap<String, Wrong> }"###;
            const ESCAPED: &str = "\\\"struct Wrong { values: HashMap<String, Wrong> }";
            const CHARACTER: char = '\\u{1f600}';
            struct Right<'a> { values: HashMap<DefId, &'a str> }
            #[cfg(test)] mod tests {
                fn compare() { if 1 < 2 { assert!(1 << 2 > 0); } }
                struct Fixture { values: HashMap<String, Fixture> }
            }
        '''), set())

    def test_variant_attributes_and_nested_function_types_keep_field_identity(self):
        self.assertEqual(self.fields("""
            enum Registry {
                #[cfg(feature = "llvm")]
                Named { #[allow(dead_code)] values: HashMap<String, fn(u32) -> bool> },
                #[cfg(feature = "llvm")]
                Tuple(Box<BTreeSet<String>>),
            }
        """), {"src/types/example.rs:Registry::Named.values",
               "src/types/example.rs:Registry::Tuple.0"})

    def test_forward_generic_aliases_and_alias_keys_are_classified(self):
        self.assertEqual(self.fields("""
            struct Checker { values: Wrapped<FnDecl>, scoped: Table<DefId, FnDecl> }
            type Wrapped<V> = Box<Table<Key, V>>;
            type Key = String;
            type Table<K, V> = HashMap<K, Vec<V>>;
        """), {"src/types/example.rs:Checker.values"})

    def test_removed_and_unclassified_tables_fail_the_ratchet(self):
        expected = {"old": ("migration", "borrow contracts by DefId")}
        self.assertEqual(len(guard.audit({"new": object()}, expected)), 2)
        self.assertEqual(guard.audit({"old": object()}, expected), [])
        self.assertEqual(len(guard.audit({"old": object()}, expected, strict=True)), 1)

    def test_equal_rust_owner_and_alias_names_remain_module_scoped(self):
        self.assertEqual(self.fields("""
            mod left { type Table = HashMap<String, FnDecl>; struct Checker { values: Table } }
            mod right { type Table = HashMap<DefId, FnDecl>; struct Checker { values: Table } }
        """), {"src/types/example.rs:left::Checker.values"})

    def test_symbols_and_walk_state_do_not_become_migration_debt(self):
        actual = {name: object() for name in ["operator", "local", "wire"]}
        expected = {"operator": ("symbol", "operator text is a language key"),
                    "local": ("state", "scoped runtime binding"),
                    "wire": ("output", "serialized signal path")}
        self.assertEqual(guard.audit(actual, expected, strict=True), [])

    def test_repository_inventory_has_no_new_or_stale_entries(self):
        root = Path(__file__).resolve().parents[1]
        self.assertEqual(guard.audit(guard.inventory(root),
                                    guard.read_inventory(root / "scripts/registry_inventory.txt")), [])


if __name__ == "__main__":
    unittest.main()

# Phase 1 exit audit

Audit date: 2026-10-06. Status: **not complete**.

This is an evidence ledger, not a replacement specification. The requirements
remain [TODO's exit criteria](../TODO.md#phase-1-exit-criteria), the eleven
[language deliverables](language.md#6-phase-1-final-deliverable), and the twelve
[named examples](language.md#5-phase-1-example-suite). A passing fixture or
presence check does not establish a broader requirement.

## Pipeline invariants

| Requirement | Current evidence | Assessment |
| --- | --- | --- |
| Explicit and implicit hardware/test behavior lowers once into canonical Process IR | `src/compiler.rs` invokes `lower_processes`; hardware lowering binds arena roots before normalization and derives scheduler views. Initialization CFGs and selected-only procedural/initializer host effects have focused and full native coverage. | Source implementation verified for the current suite; API ingress below still contradicts canonical authority. |
| Native objects and tests share IR-to-LLVM lowering | `emit_object` and `emit_object_with_sources` use the same module builder; source object/native integration tests pass in the current default/bitpack Rust gates. | Shared route verified; alternative executable ingress still contradicts the stronger invariant below. |
| Fixed runtime owns scheduling, time, delta cycles and host services without per-design generated C | `src/driver/build.rs` embeds fixed runtime sources/precompiled objects; design functions/data are emitted by LLVM. The initializer ABI migration uses the same scheduler for suspension. | Full default/bitpack initializer gates pass on the recorded inputs. |
| Default and bitpack execution preserves results, diagnostics, time, resolution and VCD/FST | Full Rust gates and both 224-program corpora pass. All 47 direct-native cases per mode pass assertion/profile checks, independent baseline comparisons and default/bitpack parity; observable FST is decoded and compared semantically with VCD. | Suite verified; the new inactive-hardware-effect probe proves missing semantic coverage, so this is not blanket completion. |
| AST adapter removed; no second production executable expression representation | Source imports of legacy expressions are confined to `#[cfg(test)]`. However public object emission accepts legacy scheduler trees without updating canonical CFGs. | **Contradicted by an API probe.** Production LLVM ingress and derived-view validation remain required. |

The legacy probe compiles a source design whose canonical driver writes `1`,
replaces only `design.drivers[0].expr` with `Expr::Const(7)`, then calls
`Design::validate` and public `llvm::emit_object`. Validation returns no issues
and an object is emitted. Accepting this independent executable tree is not
consistent with canonical Process IR owning behavior. Rejecting unsupported
source constructs is not a substitute for closing this API path.

Guarded hardware foreign calls are **contradicted by a native probe** too.
An entity assigns `value = if enabled { putchar(81) } else { 7 }` while its
test keeps `enabled = false`. The executable passes the `value == 7` assertion
but prints `Q`, proving an inactive arm performed a foreign effect. Selected
effects must execute only on the selected path; guarded index diagnostics
alone do not establish that property.

## Language deliverables

The current full default/bitpack Rust gates are evidence for the listed test
areas, not blanket proof of every syntax combination. Final sign-off also
requires the pipeline/API gaps above to be closed and fresh broad gates.

| Language item | Evidence to inspect before sign-off |
| --- | --- |
| 1. Parse Phase 1 syntax | Syntax unit tests, source corpus parse/build results, explicit Phase 2 rejection tests. |
| 2. Resolve modules, names, attributes and paths | Resolver tests and multi-module/std corpus results; failed name resolution must retain useful source diagnostics. |
| 3. Type-check digital entities, structs, enums, traits and impls | Type/visibility/view/operator tests plus native bus/trait, aggregate and enum examples. |
| 4. Elaborate parameterized entities | Elaboration tests and specialized hierarchy/port metadata, with native generic/instance connection assertions. |
| 5. Lower digital simulation IR | Canonical source-value/CFG tests and public API validation; legacy executable ingress currently blocks this invariant. |
| 6. Simulate combinational/sequential behavior | Native mux/register/counter/FSM, procedure/value CFG, initializer and event tests; check values and time, not just exit status. |
| 7. Event/old for all digital/discrete values | Scalar, enum, real, nested struct/directed array and 128-bit X/Z snapshot tests and waveforms. `packet_struct_event` is the named aggregate/history example. |
| 8. Run test entities | Native descriptors, list/filter tests and all-root initialization under a qualified test filter; unselected ordinary stimulus must not run. |
| 9. Assertions | Passing/failing severity/native tests, exact source failure locations and failure-before-later-effects coverage. |
| 10. Waveforms | Native VCD profiles and independent FST decoding/semantic parity in both storage modes; validate empty-FST rejection separately. |
| 11. Useful diagnostics | Frontend diagnostic tests and native range/index/file/UTF-8/assertion failure tests, with spans and no subsequent observable effects. |

## Named artifacts

`scripts/check-phase1-examples.py` reads the authoritative list in the language
document and currently confirms all twelve files exist in the sibling corpus.
It is a presence check only.

- Direct-native/profile suite: `basic_mux`, `register`, `fsm`,
  `enum_event_monitor`, `packet_struct_event`, `stream_bus`,
  `producer_consumer`, `attribute_usage`.
- `counter.siox`: hardware object/elaboration artifact; execution proof also
  comes from the counter native ABI test and `counter_test.siox` assertions.
- `counter_test.siox`, `fsm_test.siox`: full corpus compiles/runs these named
  assertion-bearing tests; inspect both current storage-mode corpus results.
- `external_entity_stub.siox`: metadata/tree-only foreign black box, as the
  specification explicitly states. Foreign HDL execution remains Phase 3.

Do not delete an example or substitute a nearby test to make this list pass.

## Current verification identity

Full initializer r6 session `46824` is terminal with exit 0. Full CI, both
224-program corpora and all 47 native cases per mode pass on these inputs:

- Tracked compiler/runtime/std/core/tests/scripts/language-spec and actual
  libfst sources:
  `192685c81af94a5ec03e8a4cd72ff12689ddb2517900b3ee271258b40c96ba91`.
- Sibling Siox sources:
  `281e66ee22807d77dc837a09ba105ce87f83c2230a60dca93d3b6b1de787d1ee`.
- Default compiler:
  `76bb8787e4019e1bcc81620508c8f020093b17d4a16cf40c47a2882cfb19c0b6`.
- Bitpack compiler:
  `c6db422d272248ad4453b4fa2f6edaf00125fde5222a7e32c7e09be350401392`.

The native comparison permits exactly one documented baseline correction:
the unselected packet-history root's index reset sample at 3,000,001 fs becomes
its declared `3`, rather than stale `0`. Its full four-sample trace is asserted;
every other waveform and output is compared without that exception. The old
failed run and immutable baseline artifacts remain preserved.

Future exit claims must identify terminal handles, current input/artifact
hashes, both corpus totals and native/waveform evidence. If code changes after
a gate, that gate is historical evidence, not verification of the new tree.

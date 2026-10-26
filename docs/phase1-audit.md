# Phase 1 exit audit

Audit date: 2026-10-06. Status: **complete**.

This is an evidence ledger, not a replacement specification. The requirements
remain [TODO's exit criteria](https://github.com/Siox-lang/sioxc/blob/main/TODO.md#phase-1-exit-criteria), the eleven
[language deliverables](language.md#6-phase-1-final-deliverable), and the twelve
[named examples](language.md#5-phase-1-example-suite). A passing fixture or
presence check does not establish a broader requirement.

## Pipeline invariants

| Requirement | Current evidence | Assessment |
| --- | --- | --- |
| Explicit and implicit hardware/test behavior lowers once into canonical Process IR | `src/compiler.rs` calls `ir::lower_in` then `lower_processes`. Hardware roots bind before normalization; `source_processes.rs` retains that arena and CFGs while attaching procedural state. Initialization, explicit/implicit stimulus, clocks and hardware entries are source-lowered, not imported from scheduler views. | Verified source routes and full default/bitpack regressions on final inputs. |
| Native objects and tests share IR-to-LLVM lowering | Both public object entry points use `build_module_with_sources`; `src/driver/build.rs` calls `emit_object_with_sources` before linking fixed objects. The object ABI regression checks shared/overwritten foreign effects independently of the executable. | All three object tests and native suites pass in both modes. |
| Fixed runtime owns scheduling, time, delta cycles and host services without per-design generated C | `src/driver/build.rs` embeds fixed runtime sources/precompiled objects; only design functions/data are emitted by LLVM. Runtime tests verify simultaneous resumes, reactive quiescence, delayed transactions and source-ordered suspending initialization. | Verified direct linking without generated design C and independently suspending foreground processes. |
| Default and bitpack execution preserves results, diagnostics, time, resolution and VCD/FST | Final effects-r3 passed both 225-program corpora, both full Rust suites and 48 direct-native cases per mode. Independent profiles, historical comparisons, decoded FST parity, failure-location and guarded-effect regressions check semantics. | Verified on unchanged final execution inputs. |
| AST adapter removed; no second production executable expression representation | The former `test_ir` module is absent. Source fragment imports and legacy LLVM tree emission are `#[cfg(test)]` only. Production `Design::validate` compares derived counts, canonical roots/read sets, targets, guards, contexts, spans and event writes with CFGs. | Seven independent public-API mutations fail validation/object emission without an artifact in both full suites. |

The legacy probe compiles a source design whose canonical driver writes `1`,
replaces only `design.drivers[0].expr` with `Expr::Const(7)`, then calls
`Design::validate` and public `llvm::emit_object`. Validation returns no issues
and an object was emitted before the fix. The regression now verifies rejection
of that tree, a different valid arena root, a different target/guard, removed
drivers and changed event condition/value. Both validation and public object
emission fail without an output artifact.

Guarded hardware foreign calls were **contradicted by a native probe** too.
An entity assigns `value = if enabled { putchar(81) } else { 7 }` while its
test keeps `enabled = false`. The executable passes the `value == 7` assertion
but printed `Q`, proving an inactive arm performed a foreign effect. The native
regression now verifies inactive if/match/event effects are absent, and selected
effects retain their values. A separate object-ABI regression checks one shared
foreign call across five struct leaves and bounded helpers, and both effects of
overwritten/retained event writes. This consumer previously duplicated shared
calls and omitted overwritten effects. Internal per-phase captures and selected
discarded-value evaluation fix these paths. Both full default/bitpack suites
and both native/output parity gates pass with those regressions enabled.

## Language deliverables

The requirement audit combines inspected source routes, assertions inside the
tests, terminal suite results and emitted artifacts. It does not infer semantics
from fixture presence or a green exit alone. The final gate repeated verification
with the reconciled language document included in its frozen input identity.

| Language item | Acceptance evidence inspected |
| --- | --- |
| 1. Parse Phase 1 syntax | Syntax unit tests, source corpus parse/build results, explicit Phase 2 rejection tests. |
| 2. Resolve modules, names, attributes and paths | Resolver tests and multi-module/std corpus results; failed name resolution must retain useful source diagnostics. |
| 3. Type-check digital entities, structs, enums, traits and impls | Type/visibility/view/operator tests plus native bus/trait, aggregate and enum examples. |
| 4. Elaborate parameterized entities | Elaboration tests and specialized hierarchy/port metadata, with native generic/instance connection assertions. |
| 5. Lower digital simulation IR | Canonical source-value/CFG tests and the seven-case public API rejection regression pass in both modes; frontend hardware roots bind before normalization, procedural lowering retains the same arena, and derived forms are validated against CFGs. |
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
- `counter.siox`: generic hardware/elaboration input checked with metadata;
  instantiated execution is proved separately by the counter native ABI test
  and `counter_test.siox` assertions, not an object with an unbound width.
- `counter_test.siox`, `fsm_test.siox`: full corpus compiles/runs these named
  assertion-bearing tests; inspect both current storage-mode corpus results.
- `external_entity_stub.siox`: metadata/tree-only foreign black box, as the
  specification explicitly states. Foreign HDL execution remains Phase 3.

Do not delete an example or substitute a nearby test to make this list pass.

## Historical stage acceptance criteria

The historical plan remains an acceptance source, not the active work queue.
Its obsolete four-state logic/operator-shim and one-foreground sketches are
reconciled with current source and native evidence, not used to excuse
missing advertised behavior.

| Stage | Acceptance evidence inspected |
| --- | --- |
| 1. Frozen syntax and examples | `docs/language.md` is the grammar/design authority; the twelve named sibling artifacts cover counter, reset register, mux, FSM, stream producer/consumer, enum history, assertions, foreign metadata and attributes. |
| 2. Parser and pretty-printer | Parser recovery tests retain ports/items after malformed input, report undeclared operators once and retain spans. Both corpus gates parse printed source and compare the second print byte-for-byte. |
| 3. Names and modules | Resolver tests check unknown/duplicate/ambiguous imports, private/qualified access, exact module identity, re-exports and undeclared attributes. Native module-specific enum/struct/function tests verify resolved identities survive codegen. |
| 4. Types and kinds | Type tests reject mismatched widths, input/view writes, unavailable/private methods and wrong attribute targets/values; IR diagnostics cover detectable undriven reads. Native cast/range/index failures check the execution contract separately. |
| 5. Elaboration | Generic-width substitution tests assert concrete port types and connections; hierarchy tests assert root/child/tree paths; directional bus tests check leaf permissions; external stub remains explicitly metadata-only. |
| 6. Digital IR | Canonical source/CFG/API tests inspect dependencies, direct current/old/event reads, staged versus immediate assignments, source contexts and lowered calls. Production accepts only validated scheduler projections. |
| 7. Simulation | Native/profile mux/register/counter/FSM/stream/producer-consumer/enum/packet examples assert values, event/history, handshake timing and exact changes. Default/bitpack parity and independently decoded FST are checked. |
| 8. Tests and stimulus | Runtime tests assert success/failure reports, source snippets, independent roots/filtering, time advancement and clocks. `concurrent_process_test.siox` asserts two foreground continuations are not serialized; its exact profile requires b at 1 ns and a at 2 ns in both full corpus/native modes. |
| 9. Waves | Counter integration checks clk/count transitions and equal VCD/FST timestamps; FSM profile checks symbolic states, packet profile checks nested paths and wide X/Z; zero-time/empty selection and multi-test timeline tests inspect files, not only exit codes. |
| 10. Diagnostics | `DiagnosticSink` and `diag::codes` own coded compiler reports/spans/help; parser/type/IR tests assert semantic codes and native failure-location tests assert the expression/assignment line, snippet and caret. |
| 11. Std | Counter/FSM/stream/test artifacts compile with ordinary imports; current core/std own Bool, Bit, nine-state ULogic/Logic, arithmetic/operator traits, encoding/resolution tables, metadata and time/frequency suffixes. No per-design C std implementation is restored. |
| 12. CLI | Both immutable r2 compilers directly pass counter metadata, named native-test compilation/execution and qualified filtering; source/AST/IR/tree dumps are nonempty. An unknown type exits 1 with E-P001/span/help; an emitted failing assertion exits 1 with message, source line and caret. Native object ABI behavior is verified separately by `tests/aot_object.rs`. |

The new concurrency regression is a real runtime test, not a stub discovery
test: the first-declared process suspends until 2 ns; the second resumes at 1 ns
and observes the first is still suspended. DUT outputs and a later continuation
verify committed state. No worker threads or second scheduler were added.

## Direct CLI and artifact evidence

The named Stage 12 commands were executed against both immutable r2 compilers,
not substituted with API-only calls:

```bash
sioxc --std /home/max/siox/std /home/max/siox-tests/counter.siox --emit metadata
sioxc --std /home/max/siox/std --test /home/max/siox-tests/counter_test.siox -o counter-tests
./counter-tests examples::counter_test::CounterTest -o counter.vcd
```

The waveforms contain `CounterTest.dut.{clk,rst,en,count}`. Reset changes from
1 to 0 at 10 ns; enable stays 1; count reaches binary `00001010` at 105 ns,
at the final rising clock edge. Both complete traces compare byte-for-byte.
The external stub's tree independently contains `ExternalCounter<W=16>`.
Logs/artifacts are `/tmp/siox-cli-acceptance-{default,bitpack}-*`.

The error probes compile source from stdin, without adding an intentionally
failing program to the passing corpus. They check compiler exit **1** for an
unknown type and native exit **1** for `assert!(false, "phase1 acceptance
failure")`, with the qualified test filter and source/caret output. This also
distinguishes compiler failure from loader/linker failures.

Runtime recursion, non-packed conversions, general runtime-sized arrays and
computed file paths remain explicitly unsupported extensions in TODO, not
alternate execution routes. Native source DWARF, worker threads, project tooling,
analogue and foreign HDL/synthesis are separately documented later work. This
audit does not claim those are implemented or change their scope.

## Current verification identity

Final effects-r3 session `16773` is terminal with exit 0; its scope is
inactive/dead. Formatting, example presence, frontend checks/lints, both full
Rust suites, all-target/all-feature linting, both **225 passed / 0 failed**
corpora and **48 direct-native/profile/FST cases per mode** pass. No execution
input changed during the run.

- Cargo/compiler/runtime/core/std/tests/scripts and reconciled language spec:
  `9815c88573c11a279c511c175ff6cfb568f5e4613f547941b0fa4bbb1a727572`.
- Actual libfst sources: `e422b98891be317559212f119d6c7adedfb13c6ea1bb59b12c08da598e7da720`.
- All sibling Siox sources: `d17a7c34dbdd72dfe24a7b33f446976013f92cf88497a2402ab891102c1b3de4`.
- Default compiler: `ca5afad8150b99575839e68795385b73bb632343405efeb8d64cf0e91bc84611`.
- Bitpack compiler: `68cb78debcb8430ba9a32d5103138e27cb9992bc551fb73edac215aa2058307b`.

Logs are `/tmp/siox-effects-r3-ci/`, `/tmp/siox-effects-r3-ci-summary.log` and
`/tmp/siox-effects-r3-native-{default,bitpack}.log`. The direct CLI probes above
use r2 compiler artifacts whose hashes are identical to r3 in each mode.
Sign-off documentation/TODO changes after the gate do not change its execution
inputs or language requirements. This audit closes the Phase 1 baseline, not
the explicitly listed later capabilities.

The libfst identity hashes the actual tracked submodule files, not only the
parent repository's gitlink. From the compiler root it is reproducible with:

```bash
git ls-files -z --recurse-submodules third_party/libfst | xargs -0 sha256sum | sha256sum
```

### Earlier gates

Effects-r2 session `46869` is terminal with exit 0 and inactive/dead scope.
Full CI, both 225-program corpora and all 48 native/profile/FST cases per mode
pass. Its execution inputs were unchanged from start to finish:

- Cargo/compiler/runtime/core/std/tests/scripts and the language document:
  `44bdadc0fa82f6a1cc0b10809200991a531070f34c463676e4b07d516b72c3e6`.
- Actual libfst sources: `e422b98891be317559212f119d6c7adedfb13c6ea1bb59b12c08da598e7da720`.
- All sibling Siox files, including the new concurrency example:
  `d17a7c34dbdd72dfe24a7b33f446976013f92cf88497a2402ab891102c1b3de4`.
- Default compiler: `ca5afad8150b99575839e68795385b73bb632343405efeb8d64cf0e91bc84611`.
- Bitpack compiler: `68cb78debcb8430ba9a32d5103138e27cb9992bc551fb73edac215aa2058307b`.

Logs are `/tmp/siox-effects-r2-ci/`, `/tmp/siox-effects-r2-ci-summary.log` and
`/tmp/siox-effects-r2-native-{default,bitpack}.log`. The newer r3 gate above
includes subsequent prose-only language corrections; compiler/runtime/test
sources and emitted compiler hashes are unchanged.

API/activity/object-capture gate `95354` is terminal with exit 0. Both full
Rust suites, frontend/all-target lint checks, both 224-program corpora and all
47 direct-native/profile/FST cases per mode pass. This includes the strengthened
source-object inactive/order regression. Verification inputs are:

- Tracked Cargo/compiler/runtime/core/std/tests/scripts and language spec:
  `7cb1ef31f100e5f3b39c8d656b7baf1956131d8cd4777748e31775c9c9eeaae3`.
- Actual libfst sources: `e422b98891be317559212f119d6c7adedfb13c6ea1bb59b12c08da598e7da720`.
- Sibling Siox sources: `281e66ee22807d77dc837a09ba105ce87f83c2230a60dca93d3b6b1de787d1ee`.
- Default compiler: `46b210fa84b36826f1d308cec9a69bf6e5e604fc15638afbc03f106c4dd73adc`.
- Bitpack compiler: `fee6572bf6fcf8578e2f9cff30cbe729d0d9c9dd2b4e108256ab28f3a11f3ee8`.

Logs: `/tmp/siox-effects-r1-ci/`, `/tmp/siox-effects-r1-ci-summary.log`,
and `/tmp/siox-effects-r1-native-{default,bitpack}.log`. The scope is inactive;
the final execution input hash matches its starting value. Historical initializer evidence
below is retained for comparison, not substituted for this newer gate.

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

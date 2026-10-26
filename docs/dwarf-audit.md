# Native DWARF acceptance

Status: **accepted**, 2026-10-12. This checks the native source
debugging contract documented in [architecture](architecture.md) and
[testing](testing.md#native-debugger-smoke-test); it does not add a debugger,
DAP server, or editor integration.

## Required behavior and permanent checks

Checks below are in `siox/tests/debug_info.rs` unless qualified otherwise.
They compile and execute native artifacts, rather than only searching LLVM text.

| Requirement | Evidence required |
| --- | --- |
| Direct LLVM source metadata in objects and native tests; ordinary builds omit it | `direct_dwarf_supports_source_steps_and_persistent_values`; `scripts/test-gdb.sh` |
| Source breakpoints, stepping, instance names, persistent state, lexical shadowing and suspended locals | `direct_dwarf_supports_source_steps_and_persistent_values`; FIFO/wide smoke script |
| Distinct repeated/nested/selected source calls in procedural and hardware execution | `expression_inline_calls_keep_source_frames_and_value_bindings`; `expression_branches_preserve_shadowing_and_selected_source_stops`; `hardware_expression_calls_keep_source_frames` |
| Captured formal parameters and live aliases, receivers and companions across waits | `repeated_inline_calls_keep_locals_and_source_frames_across_await` |
| Declared scalar/enum, non-byte-aligned wide, recursive struct/array and string inspection types | Persistent-value, wide-return, packed-binding and string-return checks |
| Consumed value and metadata planes remain faithful; absent/pruned/narrowed data stays unavailable | `packed_expression_bindings_expose_logic_companions`; `complete_source_returns_do_not_invent_missing_logic_planes`; `narrow_source_consumers_do_not_fabricate_binding_bits` |
| Scalar, mixed register, memory, wide, odd-sized and string returns preserve values and caller payloads | `source_real_returns_match_the_debugger_finish_abi`; `mixed_aggregate_returns_match_the_debugger_finish_abi`; `large_aggregate_returns_match_the_debugger_finish_abi`; wide/odd-sized/string return checks; LLVM boundary units |
| Partial and independently selected return fragments do not force discarded fields or reorder checks/effects | Partial/joint/independently-selected/mixed-partial return checks; `checked_and_effectful_aggregates_preserve_demand_and_order`; demand-analysis units |
| Declared operator/conversion signatures and explicit target formats survive sharing | `conversion_and_non_enum_resolve_keep_source_signatures`; `numeric_family_conversions_keep_source_signatures`; hardware-expression check |
| Debugging preserves executable demand, shared bodies, cached effects and pure SSA DAGs | `effectful_source_frames_share_actual_cached_results`; `pure_loop_muxes_keep_shared_debug_ssa`; LLVM shared-body/cache units |
| Both packing modes retain execution, stdout/VCD parity and valid DWARF | All 23 debugger checks in default/bitpack suites; four ordinary/debug corpus runs; `llvm-dwarfdump --verify`; native GDB smoke checks |

Tool-dependent probes may skip when GDB or llvm-dwarfdump is absent. Acceptance
requires both tools installed, not just a green test count. Actual host checks
find `/usr/bin/gdb` and `/usr/bin/llvm-dwarfdump`.

The transport review follows the actual backend variants in
`src/llvm/process/debug/inline_values.rs`: `SourceReturn::Direct`, `Register`
and `Memory`, with packed, literal-string and runtime-string memory views.
Scalar/real checks cover direct returns; mixed/sub-byte checks cover register
packing; large-aggregate and string checks cover every memory-view variant.
Wide/odd-sized and partial-frame checks additionally exercise cases where
source inspection deliberately cannot advertise a whole native return.
Type inspection covers the backend's scalar/enum, packed, struct and bounded
array layouts; unsized `Char[]` uses the separate borrowed-string view.

## Current verification

Compiler commit: `a76a029` (`Preserve target formats in source conversion returns`).
The frozen source/test/core SHA-256 is
`5b29df4680466b53b223725e9282daea0cb86710997a5effd71cec66d7a693fd`.
Guarded service `siox-job-3000-1791828977283331750-116642.service` completed
with `SubState=exited`, `Result=success`, `ExecMainStatus=0` and an unchanged
source hash. Peak memory was 2,607,013,888 bytes under the bounded 3 GiB cap.
Retained stage logs are in
`/home/max/.cache/siox-jobs/tmp/dwarf-target-format-acceptance.mnj0PE`.

- `cargo fmt --all --check` and every stage of `scripts/ci-local.sh` passed:
  artifact checks, frontend checks/lints, build, both full test suites and
  strict all-target/all-feature Clippy on the pinned Rust 1.90 toolchain.
- `test.log` and `test__bitpack_.log` each record all 23 debugger checks passed,
  with zero failures or ignored/filtered tests. Installed GDB and dwarfdump
  execute the tool-dependent probes, including actual source stops, values,
  returns, DWARF verification and ordinary/debug parity.
- `corpus.log`, `corpus__bitpack_.log`, `debug_corpus.log` and
  `debug_bitpack_corpus.log` each record **239 passed; 0 failed**.
- `gdb_smoke.log` records FIFO and wide native breakpoint/inspection/stepping
  and VCD parity passed in both ordinary and debug builds.
- `graph_update.log` records the AST refresh and reclustering completed:
  5,523 nodes and 242 communities. Existing libfst extraction warnings concern
  the graph parser, not compiler/DWARF acceptance.

Every requirement above has current native or metadata evidence from these
checks; no known implementation work remains for this documented contract.
The limits below stay explicit rather than being hidden by fabricated values.

### LLVM 23 bindings alignment, 2026-10-13

Compiler commit: `74cd66c` (`Align LLVM bindings and CI on LLVM 23.1`).
The earlier acceptance used LLVM 22 bindings against the local LLVM 23 library.
The bindings now explicitly select LLVM 23 through pinned Inkwell revision
`c8234a0ee4171e946f94f6b3b5da0ea8d6ef5f3b` and llvm-sys 231.0.0. Repository
and CI builds enable strict LLVM 23.1 version checking; CI installs Clang/LLVM 23.
Guarded job 406157 completed exited/success/0 with peak 2,621,702,144 bytes:
both full suites (23 DWARF checks each), strict Clippy, all four 239/0 corpora,
FIFO/wide GDB source checks and VCD parity passed. Rebuilt sioxc links
`libLLVM.so.23.1`. Logs are retained under
`/home/max/.cache/siox-jobs/tmp/llvm23-alignment.F68ZKc`.
The accepted source/config/build/README hash, including `.cargo` configuration,
is `3d4928781cbb008bc12a1db112b6bbd03aa37b71840331a4e89ebe6845d4ff72`.
This verifies the local CI-equivalent gate; the changed GitHub workflow still
requires a push/PR run. No Rust compiler APIs or language behavior changed.

## Supported limits, not fabricated values

- Inspection buffers and `$return`/`$logic` snapshots are read-only views of
  already-emitted data. They are not executable storage or extra evaluation roots.
- Unconsumed arguments, discarded fields, missing metadata and truncated bits
  remain unavailable. No debugger-only operand request fills them in.
- GDB's AMD64 arbitrary-width return classifier is not a compiler ABI oracle.
  Wide/odd-sized and partial results use truthful inline frames and snapshots
  where native `finish` decoding is unsupported; tests verify caller payloads.
- Runtime-sized arrays currently supported by the compiler are `Char[]` handles.
  General dynamic arrays, native recursion and unsupported frontend constructor
  forms are separate language extensions, not DWARF deliverables.
- Expression syntax uses the C++ debugger grammar until Siox debugger-language
  integration exists. Code generation still uses direct LLVM, not generated C.

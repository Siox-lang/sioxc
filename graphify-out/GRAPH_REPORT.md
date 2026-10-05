# Graph Report - siox  (2026-10-05)

## Corpus Check
- 193 files · ~588,454 words
- Verdict: corpus is large enough that graph structure adds value.
- Unclassified: 48 file(s) not represented in the graph (top: .siox 37, (none) 7, .build 3)

## Summary
- 5072 nodes · 13531 edges · 235 communities (179 shown, 56 thin omitted)
- Extraction: 90% EXTRACTED · 10% INFERRED · 0% AMBIGUOUS · INFERRED: 1299 edges (avg confidence: 0.85)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `e6608be3`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- Parser<'a>
- 2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs
- DiagnosticSink
- lz4.c
- Agent chat
- `sext(x) < 0` was false for every negative x
- tests/calls.rs
- pretty.rs
- TokenKind
- ast.rs
- process.rs
- Log
- process.c
- check_modules
- behavior.rs
- Codegen<'ctx, 'd>
- fstapi.c
- lower_src
- Resolver<'a>
- diag.rs
- diag_codes
- Compilation
- syntax/macros.rs
- FunctionIndex
- resolve.rs
- FunctionIndex<'a>
- process_value
- Lowering<'a>
- ProcessValueId
- ProcessBinaryOp
- .load
- ProcessValueKind
- SignalId
- build_binary.rs
- elab.rs
- Ty
- emit_state_helpers
- places.rs
- What does not yet make sense
- Vendor-neutral RTL interchange
- process_value_layout
- fit
- fstWriterContext
- Lowering<'a>
- BinOp
- Lowering<'a>
- SourceValues
- lints.rs
- Val
- .infer_type_of
- command
- source_processes.rs
- passes.rs
- check_src
- 3. Phase 1 hard rules
- Lowering<'a>
- expr_path
- ProcessDisplayKind
- SourceLayout
- gather_generate
- tests/operators.rs
- Checker<'a>
- fstReaderIterBlocks2
- driver/build.rs
- process_packed_meta_uncached
- Expander
- query.rs
- Pattern
- 2026-07-29 — hardened random bounds and the native timeline
- fstWriterFlushContextPrivate
- tests/diagnostics.rs
- expr_span
- value_ref_with_type_inner
- check
- Expr
- lower/hardware.rs
- Expr
- .lower_stmt_at
- process_entry
- lexer.rs
- matches.rs
- driver.rs
- Lowering<'a>
- SourceMap
- DefId
- runtime_failure_location.rs
- Lowering<'a>
- testbench.rs
- aot.rs
- Emit
- Elaborator<'a>
- Checker<'a>
- generate_index_bounds.rs
- process/mod.rs
- Span
- .emit
- ProcessHostValueOp
- docs/README.md
- [Unreleased]
- process/tests.rs
- FileId
- wave.c
- helpers.rs
- .project_place
- Lowering<'a>
- BinOp
- .tokenize
- Checker<'a>
- tests/writes.rs
- Compiler foundations borrowed from rustc
- The siox standard library
- metavalue_operand_sharing.rs
- Lowering<'a>
- compiler.rs
- ProcessBlockId
- ir/tests.rs
- check-vcd.py
- type_head_name
- expr.rs
- DefKind
- llvm/mod.rs
- hardware_source_values.rs
- Macros
- Checker
- .lower_shaped_source
- derive.rs
- Checker<'a>
- Architecture
- siox Phase 1 — Digital Language Specification
- Public methods on entities
- Pipelined functions: `#[latched]` and `#[latch]`
- substitute.rs
- Checker<'a>
- Q: Trace scalar Logic waveform metadata and both waveform writers
- conversion_without_from.rs
- Historical Stage 8 — Test entities, assertions, and stimulus
- Historical Stage 11 — Minimal digital standard library
- `core` and `std`
- House rules
- emit_wave_metadata
- Checker<'a>
- import_forms.rs
- operator_no_impl.rs
- .resolve_connections
- siox
- CachedIntOp
- FailureKind
- Checker<'a>
- TODO
- Historical Stage 7 — Event-driven simulator core
- fastlz.c
- lower
- core_library.rs
- generic_argument_operator.rs
- sx_fail
- struct_let_initializer.rs
- words_const
- format.rs
- late_diagnostic_spans.rs
- Historical Stage 2 — Lexer and parser
- Historical Stage 3 — Name resolution and module system
- Historical Stage 4 — Type system and kind checking
- Historical Stage 5 — Entity specialization and elaboration
- Historical Stage 6 — Digital IR generation
- Historical Stage 9 — Waveform and tracing output
- Standard-library build-out
- Simulation
- driver_override.rs
- compiler_api.rs
- module_files.rs
- Interoperability
- Historical Stage 1 — Syntax freeze and examples
- Historical Stage 10 — Diagnostics and lint rules
- Historical Stage 12 — CLI and project workflow
- At a glance
- siox documentation
- siox roadmap
- Testing
- Q: Which paths control canonical cache lifetime and compact packed write width?
- Diagnostic
- packed_partial_write.rs
- libfst
- 2026-07-31 (cont.) — a probe that used the wrong syntax found a real bug
- Q: optimize these first
- Artifact
- 2026-07-31 (cont.) — writing wrong programs on purpose
- An index checked only when it was written as a literal
- Generic arguments: a hint that names the spelling that works
- ci-local.sh
- test-corpus.sh
- main
- 2026-07-31 — closing the loop
- 2026-07-31 (cont.) — a "bug" that was the spec, and a task that was already done
- 2026-07-31 (cont.) — asking the corpus what it never says
- 2026-07-31 (cont.) — automating the differential
- 2026-07-31 (cont.) — closing the logged gap
- 2026-07-31 (cont.) — reading the spec as a checklist
- 2026-07-31 (cont.) — the bottom of the coverage list paid twice
- 2026-07-31 (cont.) — the same audit, one layer down
- 2026-08-01 — generic implementations bind their parameters, as Rust does
- 2026-08-02 (cont.) — a false alarm, then the same constant one layer down
- 2026-08-02 (cont.) — a `let` in a function body, and a check that does not check
- 2026-08-02 (cont.) — a struct as a call argument
- 2026-08-02 (cont.) — predicting the next table instead of finding it
- 2026-08-02 (cont.) — the other two statement shapes
- 2026-08-02 (cont.) — where an entity may be instantiated (E-P020)
- 2026-08-02 — the rename had a second stage to reach
- A conversion with nowhere to go, found by writing a UART
- A function that returns a struct returned nothing
- A rotation rotated everything to the same value
- `abs(n)` returned `n` in hardware and 5 in the testbench
- nvc differential sweep: vector metavalue semantics (2026-08-22)
- An operator that matches nothing produces nothing
- The distribution was narrower than the lowering it mirrors
- The same bug, in the function next to the one I fixed
- Registering a bus: the expansion existed on one path only
- The two engines disagreed about the conversion I had just fixed
- siox
- CachedCast
- .key
- sx_runtime_run_test
- Checker<'a>
- types/mod.rs
- main
- fstHandle
- Q: optimize these first
- deterministic_output.rs
- positional_struct.rs
- tests/control.rs

## God Nodes (most connected - your core abstractions)
1. `Span` - 239 edges
2. `ProcessValueId` - 190 edges
3. `Ty` - 155 edges
4. `FileId` - 152 edges
5. `2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs` - 142 edges
6. `SignalId` - 139 edges
7. `Agent chat` - 134 edges
8. `Parser<'a>` - 117 edges
9. `SourceLayout` - 101 edges
10. `LoweringContext` - 100 edges

## Surprising Connections (you probably didn't know these)
- `2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs` --references--> `sx_runtime_run_test()`  [INFERRED]
  chat.md → runtime/process.c
- `2026-09-13 — Claude — independent check of the direct matrix, and two regressions` --references--> `sx_runtime_schedule()`  [INFERRED]
  chat.md → runtime/process.c
- `2026-09-13 — Claude — independent check of the direct matrix, and two regressions` --references--> `sx_runtime_suspend_time()`  [INFERRED]
  chat.md → runtime/process.c
- `2026-09-13 — Claude — independent check of the direct matrix, and two regressions` --references--> `sx_runtime_settle()`  [INFERRED]
  chat.md → runtime/process.c
- `sx_wave_close_fst()` --calls--> `fstWriterClose()`  [INFERRED]
  runtime/wave.c → third_party/libfst/src/fstapi.c

## Import Cycles
- None detected.

## Communities (235 total, 56 thin omitted)

### Community 0 - "Parser<'a>"
Cohesion: 0.06
Nodes (61): a_process_name_is_reported_as_a_label(), a_stray_token_keeps_the_ports_around_it(), a_stray_token_run_is_quoted_by_character(), a_stray_token_run_reports_once_in_any_list(), an_undeclared_operator_is_named(), assignments_take_labels(), attr_decl_application_and_extern_entity(), attr_items_are_declarations_or_bindings() (+53 more)

### Community 1 - "2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs"
Cohesion: 0.01
Nodes (139): 2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs, 2026-09-12 — Codex — constant Siox calls in Process IR, 2026-09-12 — Codex — foreground write settle boundaries, 2026-09-12 — Codex — normalized constant shift widths, 2026-09-12 — Codex — Process IR module constants, 2026-09-13 — Codex — explicit Process raw resize, 2026-09-13 — Codex — runtime-valued Siox calls, 2026-09-13 — Codex — typed Process defaults (+131 more)

### Community 2 - "DiagnosticSink"
Cohesion: 0.05
Nodes (68): DiagnosticSink, applied(), attach(), attached(), attr_name(), base_span(), bind_module(), binding_and_read_errors() (+60 more)

### Community 3 - "lz4.c"
Cohesion: 0.05
Nodes (81): LZ4_attach_dictionary(), LZ4_clearHash(), LZ4_compress(), LZ4_compress_continue(), LZ4_compress_default(), LZ4_compress_destSize(), LZ4_compress_destSize_extState(), LZ4_compress_destSize_extState_internal() (+73 more)

### Community 4 - "Agent chat"
Cohesion: 0.02
Nodes (100): 2026-07-29 — defined oversized shifts and complete harness width scanning, 2026-07-29 — hardened arithmetic edge cases, 2026-07-29 — made extreme layouts diagnostic-safe, 2026-07-30 (cont.) — a length computed, used, and thrown away, 2026-07-30 (cont.) — a ROM that reads as zeros, 2026-07-30 (cont.) — a signed expression that was only signed once named, 2026-07-30 (cont.) — a typo through a bus port was never checked, 2026-07-30 (cont.) — asking the same question one stage up (+92 more)

### Community 5 - "`sext(x) < 0` was false for every negative x"
Cohesion: 0.02
Nodes (96): 2026-08-06 — Claude — a named struct literal is not a sub-instance, 2026-08-06 — Claude — a sweep that found no bug, and a coverage hole it did find, 2026-08-06 — Claude — partial writes compose across event blocks too, 2026-08-06 — Claude — partial writes to a packed vector compose, 2026-08-06 — Claude — slice writes in the testbench; a correction, 2026-08-06 — Codex — associated-function call contracts, 2026-08-06 — Codex — chained alias range/struct checks, 2026-08-06 — Codex — clean generated match conditions (+88 more)

### Community 6 - "tests/calls.rs"
Cohesion: 0.09
Nodes (9): an_undeclared_call_is_reported(), associated_and_instance_method_call_forms_are_distinct(), calls_without_a_declaration_are_not_all_mistakes(), local_function_arguments_use_their_declared_types(), method_calls_check_argument_count_and_types(), return_outside_a_function_is_reported(), return_values_match_the_function_signature(), value_returning_functions_return_on_every_path() (+1 more)

### Community 7 - "pretty.rs"
Cohesion: 0.05
Nodes (49): attr(), bin_op(), bin_prec(), explicit_process_roundtrips(), expr(), expr_inner(), expr_prec(), expr_string() (+41 more)

### Community 8 - "TokenKind"
Cohesion: 0.03
Nodes (77): TokenKind, Amp, AmpEq, Arrow, Attr, Bang, BangEq, CharacterLit (+69 more)

### Community 9 - "ast.rs"
Cohesion: 0.06
Nodes (55): Lowering, Attr, AttrBinding, AttrDecl, ConstDecl, EntityDecl, EnumDecl, EnumVariant (+47 more)

### Community 10 - "process.rs"
Cohesion: 0.04
Nodes (72): assignment_drives_design_value(), process_place_assignment(), arena_constant_integer(), integer_words_width(), process_assignment_snapshot(), process_instruction_values(), process_place_classes(), process_terminator_targets() (+64 more)

### Community 11 - "Log"
Cohesion: 0.03
Nodes (64): 2026-07-17 — Claude — bus-mode hardening + generics, 2026-07-17 — Claude — bus modes landed, 2026-07-17 — Claude — full generics, 2026-07-17 — Claude — kickoff + recent landings, 2026-07-18 — Claude — match expressions + or-patterns, 2026-07-18 — Claude — range patterns + compound assignment, 2026-07-19 — Claude — array literals `[..]`, 2026-07-19 — Claude — three struct-style connection forms (+56 more)

### Community 12 - "process.c"
Cohesion: 0.11
Nodes (26): sx_append_error_location(), sx_format_reserve(), sx_runtime_assert(), sx_runtime_format_begin(), sx_runtime_format_char(), sx_runtime_format_integer(), sx_runtime_format_real(), sx_runtime_format_signed() (+18 more)

### Community 13 - "check_modules"
Cohesion: 0.16
Nodes (22): check_modules(), a_standard_symbol_is_not_a_custom_operator(), a_foreign_view_cannot_publish_private_backing_fields(), a_private_trait_keeps_its_implementation_methods_private(), a_split_impl_in_the_types_module_keeps_private_access(), a_view_does_not_publish_backing_struct_methods(), an_applied_view_is_an_explicit_structural_interface(), compiler_hook_traits_are_selected_by_declaration_not_leaf() (+14 more)

### Community 14 - "behavior.rs"
Cohesion: 0.10
Nodes (18): composite_and_enum_signals_flatten_with_widths(), concurrent_resolved_slices_lower_without_expression_explosion(), enum_width_covers_explicit_discriminants(), if_expression_lowers_to_select(), independent_resolved_targets_keep_separate_root_contexts(), lowers_nested_instances_with_connections(), lowers_signals_driver_and_event_block(), newtype_enum_takes_its_base_width() (+10 more)

### Community 16 - "fstapi.c"
Cohesion: 0.08
Nodes (44): main(), fstExtractRvatDataFromFrame(), fstReaderClose(), fstReaderClrFacProcessMask(), fstReaderClrFacProcessMaskAll(), fstReaderDeallocateRvatData(), fstReaderDeallocateScopeData(), fstReaderGetAliasCount() (+36 more)

### Community 17 - "lower_src"
Cohesion: 0.12
Nodes (30): lower_src(), a_call_result_carries_its_return_type(), a_char_literal_is_its_code_point_in_any_value_position(), an_inlined_parameter_keeps_its_argument_width(), bit_string_decodes_nine_value(), bit_string_initializer_sets_init(), chained_aliases_retain_terminal_signal_representation(), clean_clocked_override_clears_metavalue_companion_in_order() (+22 more)

### Community 19 - "diag.rs"
Cohesion: 0.04
Nodes (50): ATTR_WITHOUT_VALUE, COMBINATIONAL_LOOP, COMPILE_TIME_IO, CONFLICTING_DRIVERS, CONST_ENTITY_INSTANCE, DEAD_ASSIGNMENT, DUPLICATE_ATTR_BINDING, DUPLICATE_ITEM (+42 more)

### Community 20 - "diag_codes"
Cohesion: 0.11
Nodes (14): attributes_with_no_effect_are_flagged(), a_struct_containing_itself_is_rejected(), struct_literal_field_names_are_checked(), diag_codes(), suspicious_logic_compare_warns_on_integer_literal(), a_statement_with_no_effect_is_reported(), an_entity_cannot_be_instantiated_outside_a_generate(), data_array_index_is_bound_checked() (+6 more)

### Community 21 - "Compilation"
Cohesion: 0.19
Nodes (9): Text, backend_unavailable(), Compilation, CompileFailure, Compiler, default_object_root_is_structural_not_vendor_metadata(), explicit_top_requires_qualification_when_entity_leaves_collide(), select_top() (+1 more)

### Community 22 - "syntax/macros.rs"
Cohesion: 0.11
Nodes (27): FragmentKind, Expr, Ident, Item, Path, Stmt, Tokens, Type (+19 more)

### Community 23 - "FunctionIndex"
Cohesion: 0.10
Nodes (29): FunctionIndex, bind_generic_values(), eval_const(), eval_const_fns(), eval_const_stmts(), eval_logic_expr(), eval_logic_function(), eval_logic_stmts() (+21 more)

### Community 24 - "resolve.rs"
Cohesion: 0.08
Nodes (39): a_generate_label_shares_the_member_namespace(), a_generic_impl_must_bind_and_apply_its_parameters(), a_parameter_used_as_a_value_counts_as_used(), applied_view_owners_include_the_backing_type(), assignment_labels(), assignment_labels_are_unique_within_a_process(), assignment_labels_in_if(), COMPILER_TRAITS (+31 more)

### Community 25 - "FunctionIndex<'a>"
Cohesion: 0.09
Nodes (7): Elaborator, comparison_symbol(), FunctionIndex<'a>, is_blanket_array_impl(), FnDecl, FnParam, ImplDecl

### Community 26 - "process_value"
Cohesion: 0.11
Nodes (28): IndexSite, aggregate_signal_value(), dynamic_index_position(), dynamic_index_region(), packed_index_discriminant(), process_binary(), process_value_packed_width(), emit_format_character() (+20 more)

### Community 28 - "ProcessValueId"
Cohesion: 0.16
Nodes (54): await_is_time(), builtin_callee_is(), inherit_receiver_layout(), inline_pattern_condition(), inline_process_array_binary_operator(), inline_process_array_unary_operator(), inline_process_binary_operator(), inline_process_call() (+46 more)

### Community 29 - "ProcessBinaryOp"
Cohesion: 0.05
Nodes (37): process_binary_is_signed(), ProcessBinaryOp, Add, And, ArithmeticShr, Custom, Div, Eq (+29 more)

### Community 31 - "ProcessValueKind"
Cohesion: 0.05
Nodes (38): ProcessUnaryOp, IntegerToReal, Neg, Not, RealToInteger, ProcessValueKind, Array, Attribute (+30 more)

### Community 32 - "SignalId"
Cohesion: 0.10
Nodes (38): SignalId, accepts_arbitrarily_many_abi_words(), bounds_and_reuses_combinational_helpers(), build_module(), build_module_with_sources(), canonical_combinational_values_are_invalidated_after_state_writes(), canonical_staged_updates_share_values_before_state_commit(), Codegen (+30 more)

### Community 33 - "build_binary.rs"
Cohesion: 0.06
Nodes (5): decode_fst(), native_fst_keeps_multiple_tests_on_one_monotonic_timeline(), native_vcd_preserves_logic_metavalues_and_enum_symbols(), test_no_run_builds_a_runnable_binary(), waveform_times()

### Community 34 - "elab.rs"
Cohesion: 0.10
Nodes (35): a_labelled_generate_names_its_instances_through_its_scope(), an_entity_cannot_be_instantiated_in_a_process(), builds_instance_tree_with_params_and_connections(), cell_names(), check_analyses_an_entity_nothing_instantiates(), check_src(), connection_width_mismatch_is_reported(), elaborate() (+27 more)

### Community 35 - "Ty"
Cohesion: 0.14
Nodes (19): bool_type(), process_attribute_type(), process_index_type(), process_kind_type(), process_type_is_signed(), process_type_is_string(), process_value_type(), process_value_type_for_storage() (+11 more)

### Community 36 - "emit_state_helpers"
Cohesion: 0.15
Nodes (29): ProcessId, ProcessLocalId, emit_state_helpers(), storage_changed_ptr(), storage_changed_ptr_at(), storage_dirty_ptr(), storage_flag_ptr_at(), emit_array_loop() (+21 more)

### Community 37 - "places.rs"
Cohesion: 0.15
Nodes (32): assignment_metadata(), assignment_place_supported(), assignment_value(), delayed_place(), dynamic_place(), DynamicPlace, DynamicPlaceIndex, packed_bit_place_layout() (+24 more)

### Community 38 - "What does not yet make sense"
Cohesion: 0.05
Nodes (37): 10. Arrays and nominal array newtypes use one representation model, 10. The type-construction surface is overloaded, 11. Entity functions have a sharp associated/receiver cliff, 11. Sequential and combinational assignment rules are explicit, 12. Clock inference is elegant but under-specified for tooling, 12. Resolved and unresolved logic are distinct types, 13. `sioxc` is a compiler, not a project manager, 13. `using` and future library discovery need one vocabulary (+29 more)

### Community 39 - "Vendor-neutral RTL interchange"
Cohesion: 0.05
Nodes (37): Arithmetic operations, Backend independence, Bottom line, CIRCT backend, Clock domains, Combinational operations, Compiler architecture, Decision (+29 more)

### Community 40 - "process_value_layout"
Cohesion: 0.12
Nodes (45): ProcessStorageId, aggregate_metadata_projection(), MetadataEmitter, MetadataEmitter<'_, 'ctx, '_>, process_aggregate_projection_meta(), process_aggregate_projection_meta_supported(), process_value_meta_in_layout(), process_value_meta_supported() (+37 more)

### Community 41 - "fit"
Cohesion: 0.18
Nodes (31): ProcessSignalState, Current, Event, Old, storage_meta_name(), storage_meta_old_name(), storage_old_name(), storage_state_name() (+23 more)

### Community 42 - "fstWriterContext"
Cohesion: 0.09
Nodes (34): sx_wave_open_fst(), sx_wave_register_scope(), fstCopyVarint64ToRight(), fstRealpath(), fstUtilityBinToEsc(), fstUtilityBinToEscConvertedLen(), fstWriterCreateEnumTable(), fstWriterCreateVar2() (+26 more)

### Community 44 - "BinOp"
Cohesion: 0.06
Nodes (35): BinOp, Add, And, AShr, Div, Eq, FAdd, FDiv (+27 more)

### Community 45 - "Lowering<'a>"
Cohesion: 0.18
Nodes (5): Lowering<'a>, BlockLocal, eq(), index_label(), array_of()

### Community 46 - "SourceValues"
Cohesion: 0.15
Nodes (8): canonical_projection_compacts_without_expression_roundtrips(), captured_literal_planes_survive_rewrite_and_compaction(), concrete_local_formats_survive_rewrites_and_reachability_compaction(), HardwareDraft, Lowering<'_>, SourceValues, table_recognition_preserves_a_typed_stride_boundary(), visit_references()

### Community 47 - "lints.rs"
Cohesion: 0.11
Nodes (23): default_level_warns_and_names_the_lint_once(), directive(), forbid_cannot_be_lowered(), innermost_directive_wins(), is_known(), Level, Allow, Deny (+15 more)

### Community 48 - "Val"
Cohesion: 0.14
Nodes (8): Lowering<'a>, bind_format_attrs(), select_val(), Lowering<'_>, OperatorOperand, Val, Fields, Scalar

### Community 49 - ".infer_type_of"
Cohesion: 0.19
Nodes (5): suffix_scale(), Checker<'a>, is_comparison(), is_liftable_array_key(), Checker<'a>

### Community 50 - "command"
Cohesion: 0.07
Nodes (6): HARNESS, FIXTURE, imported_custom_operators_are_known_during_api_parse(), imported_custom_operators_run_in_native_harness(), imported_operator_fixture(), FIXTURE

### Community 51 - "source_processes.rs"
Cohesion: 0.09
Nodes (59): InstanceId, LayoutDirection, In, InOut, Out, aggregate_only_values_retain_recursive_layouts(), apply_process_declared_ranges(), assignment_base() (+51 more)

### Community 52 - "passes.rs"
Cohesion: 0.24
Nodes (21): and_expr(), any_unknown(), bit(), call_fn_key(), companion_memoization_distinguishes_activity_domains(), companion_read(), logic_disc_in(), logic_element_disc() (+13 more)

### Community 53 - "check_src"
Cohesion: 0.10
Nodes (21): a_bound_value_must_match_the_declared_type(), accepts_digital_sysattrs(), attribute_on_right_target_is_fine(), attribute_on_wrong_target_is_rejected(), attribute_value_type_is_checked(), rejects_phase2_ddt(), unknown_system_attribute_is_reported(), check_src() (+13 more)

### Community 54 - "3. Phase 1 hard rules"
Cohesion: 0.07
Nodes (30): 3.10 Clock helpers are `ClockLike` methods over `'event` and `'old`, 3.11 Processes are concurrent; their bodies are sequential, 3.12 Assignment uses one operator, 3.13 Sequential assignments use next-state semantics, 3.14 Assignments use source-order override inside one process, 3.15 Reset is normal logic, 3.16 Digital conditions, 3.17 No implicit broad conversions (+22 more)

### Community 55 - "Lowering<'a>"
Cohesion: 0.12
Nodes (11): Driver, EventBlock, NextUpdate, AccessStep, Field, Index, DynamicWriteTarget, PackedBit (+3 more)

### Community 56 - "expr_path"
Cohesion: 0.24
Nodes (3): Lowering<'a>, expr_path(), sunk_sysattr()

### Community 57 - "ProcessDisplayKind"
Cohesion: 0.27
Nodes (10): process_display_kind(), process_display_kind_for_type(), process_display_kind_from_value(), ProcessDisplayKind, Character, Enum, Real, Signed (+2 more)

### Community 58 - "SourceLayout"
Cohesion: 0.09
Nodes (19): LayoutField, LayoutKind, Array, Opaque, Packed, Scalar, Struct, LayoutRange (+11 more)

### Community 59 - "gather_generate"
Cohesion: 0.11
Nodes (11): Lowering<'a>, connection_value_is_static(), Lowering<'a>, gather_generate(), instance_let_parts(), literal_leaves(), blanket_requirement(), file_integer_words() (+3 more)

### Community 60 - "tests/operators.rs"
Cohesion: 0.07
Nodes (11): bare_logic_condition_is_rejected(), compared_logic_and_bit_conditions_are_fine(), decimal_literals_require_real_context_or_explicit_conversion(), integer_and_logic_literals_are_polymorphic(), intrinsic_arithmetic_requires_numeric_operands(), nominal_array_newtype_does_not_forward_unsatisfied_array_operator(), nominal_array_newtype_forwards_matching_blanket_array_operator(), numeric_separators_and_based_type_indices_are_checked_at_full_width() (+3 more)

### Community 61 - "Checker<'a>"
Cohesion: 0.11
Nodes (3): Checker<'a>, is_type_kind(), MemberVisibility

### Community 63 - "fstReaderIterBlocks2"
Cohesion: 0.17
Nodes (29): fastlz_decompress(), chk_report_abort(), fstDetermineBreakSize(), fstFread(), fstGetSVarint64(), fstGetVarint32(), fstGetVarint32NoSkip(), fstGetVarint64() (+21 more)

### Community 64 - "driver/build.rs"
Cohesion: 0.09
Nodes (22): build(), BuildRequest, LIBFST_API_C, LIBFST_API_H, LIBFST_API_O, LIBFST_FASTLZ_C, LIBFST_FASTLZ_H, LIBFST_FASTLZ_O (+14 more)

### Community 65 - "process_packed_meta_uncached"
Cohesion: 0.15
Nodes (20): Design, LogicEncoding, Signal, adapt_binding_value(), compact_discriminant(), convert_logic_scalar(), logic_binary_discriminant(), logic_unary_discriminant() (+12 more)

### Community 66 - "Expander"
Cohesion: 0.12
Nodes (16): MacroArgs, MacroDelim, Brace, Bracket, Paren, Expander, Expansion, Expr (+8 more)

### Community 67 - "query.rs"
Cohesion: 0.11
Nodes (14): bin_sym(), check_expr(), dedup(), Design, paren(), Process, ProcessKind, Comb (+6 more)

### Community 68 - "Pattern"
Cohesion: 0.11
Nodes (13): pattern_has_wildcard(), collect_named_variants(), Pattern, BitPattern, CharLit, Or, Path, Range (+5 more)

### Community 69 - "2026-07-29 — hardened random bounds and the native timeline"
Cohesion: 0.08
Nodes (24): 2026-07-29 — Codex — call declared C functions from native testbenches, 2026-07-29 — Codex — complete native local-string operations, 2026-07-29 — Codex — enforce native composite assignment shapes, 2026-07-29 — Codex — enforce ranged integers before truncation, 2026-07-29 — Codex — fit and sign foreign integer calls, 2026-07-29 — Codex — format strings as values, 2026-07-29 — Codex — make numeric literal spelling consistent across passes, 2026-07-29 — Codex — materialize native array locals element-wise (+16 more)

### Community 70 - "fstWriterFlushContextPrivate"
Cohesion: 0.15
Nodes (25): fstCopyVarint32ToLeft(), fstDestroyMmaps(), fstFtruncate(), fstFwrite(), fstGetUint32(), fstGetVarint32Length(), fstMmap2(), fstWriterClose() (+17 more)

### Community 71 - "tests/diagnostics.rs"
Cohesion: 0.08
Nodes (33): a_bad_assignment_target_says_which_kind_it_is(), chained_runtime_indices_lower_to_muxes_and_gated_writes(), runtime_index_then_struct_field_reaches_the_scalar_leaf(), testbench_value_connections_become_canonical_drivers(), applied_view_flattens_its_backing_struct_fields(), combinational_loop_lint(), concat_assignment_target_width_must_match(), conflicting_assignments_are_named_by_their_labels() (+25 more)

### Community 72 - "expr_span"
Cohesion: 0.18
Nodes (3): expr_span(), Checker<'a>, ty_name()

### Community 73 - "value_ref_with_type_inner"
Cohesion: 0.11
Nodes (30): callee_name(), character_number(), checked_process_index(), ConstantSuffix, contextual_string_bits(), definition_number(), eval_real_suffix_block(), eval_real_suffix_expr() (+22 more)

### Community 74 - "check"
Cohesion: 0.16
Nodes (23): duplicate_literal_field_and_named_type_rendering(), typed_records_expression_types(), a_generic_call_argument_keeps_its_commas(), body_errors_name_the_macro(), builtin_macros_are_core_declarations(), builtin_macros_still_work(), check(), compile() (+15 more)

### Community 75 - "Expr"
Cohesion: 0.11
Nodes (18): Expr, Binary, Canonical, CCall, CheckedIndex, Const, Current, Event (+10 more)

### Community 76 - "lower/hardware.rs"
Cohesion: 0.24
Nodes (16): a_fraction_shift_keeps_the_full_raw_product(), append_digital_assignment(), constant_helper_keeps_its_context_owner_without_signal_reads(), event_process_values_are_one_bit(), hardware_process_location(), hierarchy_locations(), InstanceLocation, internal_hardware_process_inherits_read_owner() (+8 more)

### Community 77 - "Expr"
Cohesion: 0.08
Nodes (24): Expr, Array, Binary, BitStrLit, Call, CharLit, Concat, Construct (+16 more)

### Community 78 - ".lower_stmt_at"
Cohesion: 0.14
Nodes (11): Lowering<'a>, and(), not(), loop_range(), array_elements(), ArrayOperator, Binary, Unary (+3 more)

### Community 79 - "process_entry"
Cohesion: 0.17
Nodes (9): emit_condition_suspend(), emit_settle_suspend(), emit_timed_suspend(), emit_scheduled_apply(), process_entry(), process_entry_table(), scheduled_value_from_words(), ScheduleLane (+1 more)

### Community 80 - "lexer.rs"
Cohesion: 0.10
Nodes (7): assignment_line_lexes_cleanly(), error_recovery_reports_and_continues(), is_ident_start(), keyword_kind(), kinds(), lex(), Lexer

### Community 81 - "matches.rs"
Cohesion: 0.14
Nodes (5): a_character_pattern_needs_a_character_valued_enum(), a_numeric_match_covering_its_domain_is_quiet(), a_numeric_match_reports_the_range_it_leaves_out(), an_unevaluable_prefix_is_a_diagnostic_in_pattern_position_too(), match_expression_checks_every_arm_type()

### Community 82 - "driver.rs"
Cohesion: 0.13
Nodes (16): Cli, CliEmit, Ast, Expanded, Ir, LlvmIr, Metadata, Object (+8 more)

### Community 83 - "Lowering<'a>"
Cohesion: 0.20
Nodes (6): Argument, Binding, Place, Value, Lowering<'a>, SourceCallFrame

### Community 85 - "SourceMap"
Cohesion: 0.28
Nodes (5): a_snippet_puts_the_caret_under_its_column(), a_tab_indent_is_carried_into_the_caret_row(), an_unknown_file_has_no_snippet(), SourceFile, SourceMap

### Community 86 - "DefId"
Cohesion: 0.14
Nodes (7): DefId, DefInfo, ImplOwner, ImportSite, is_library_module(), Resolver, type_head_path()

### Community 87 - "runtime_failure_location.rs"
Cohesion: 0.18
Nodes (21): a_combinational_range_violation_names_its_assignment(), a_conditional_assignment_before_a_default_is_still_checked(), a_failing_assertion_in_a_macro_names_the_invocation(), a_failing_assertion_names_its_line(), a_failing_file_read_names_the_declaration(), a_failure_shows_the_source_line_with_a_caret(), a_hardware_index_violation_names_the_access_and_declared_direction(), a_hardware_packed_read_checks_wide_and_negative_indices_before_narrowing() (+13 more)

### Community 88 - "Lowering<'a>"
Cohesion: 0.20
Nodes (3): 2026-09-17 — Claude — private-item documentation, and four rustdoc warnings that came back, subst_type_params(), Lowering<'a>

### Community 89 - "testbench.rs"
Cohesion: 0.21
Nodes (13): discover(), DiscoveredTest, discovery_uses_the_builtin_directive_not_a_namesake_attribute(), elaborate(), implementation_items(), is_clock_process(), is_clock_statement(), modules() (+5 more)

### Community 90 - "aot.rs"
Cohesion: 0.19
Nodes (14): eight_word_object_links_and_carries(), emit_object(), emit_object_module(), emit_object_with_sources(), fixed_process_runtime_schedules_reactive_delta(), host_target_machine(), object_links_and_runs(), process_aggregate_storage_executes_through_flattened_bindings() (+6 more)

### Community 91 - "Emit"
Cohesion: 0.11
Nodes (15): CompileRequest, Emit, Ast, Expanded, Ir, LlvmIr, Metadata, Object (+7 more)

### Community 92 - "Elaborator<'a>"
Cohesion: 0.14
Nodes (12): collect_field_assign_ports(), Elaborator<'a>, eval_params(), GenPath, InstanceSpec, loop_range(), param_env(), ParamValue (+4 more)

### Community 94 - "generate_index_bounds.rs"
Cohesion: 0.22
Nodes (19): a_bit_index_past_a_packed_vector_is_reported_as_a_bit(), a_clocked_block_inside_a_generate_loop_is_checked_too(), a_descending_range_into_a_negative_index_is_reported(), a_folded_negative_slice_bound_is_reported_with_its_source(), a_folded_slice_bound_past_the_top_is_reported(), a_generate_loop_that_runs_past_the_end_is_reported(), a_parameter_substituted_index_is_reported(), a_read_past_the_end_is_reported_not_clamped() (+11 more)

### Community 95 - "process/mod.rs"
Cohesion: 0.11
Nodes (7): PROCESS_ABI_VERSION, PROCESS_COMPLETED, PROCESS_FINISHED, PROCESS_SETTLING, PROCESS_STOPPED, PROCESS_SUSPENDED, PROCESS_UNSUPPORTED

### Community 96 - "Span"
Cohesion: 0.06
Nodes (31): Span, Lowering<'a>, UnelaboratedInstanceUse, impl_member(), Block, ElseBranch, Block, If (+23 more)

### Community 98 - "ProcessHostValueOp"
Cohesion: 0.18
Nodes (11): ProcessHostValueOp, FileExists, Random, RandomRange, ReadBinary, ReadUtf8, ReadUtf8Fixed, StringEqualsUtf8 (+3 more)

### Community 100 - "[Unreleased]"
Cohesion: 0.11
Nodes (18): [0.1.0] - 2026-07-12, Added, Added, Added, Added, Changed, Changed, Changed (+10 more)

### Community 101 - "process/tests.rs"
Cohesion: 0.14
Nodes (12): derived_hardware_keeps_shared_values_when_widening_arithmetic(), fixed_and_empty_strings_are_executable_process_values(), metavalue_facts_handle_a_deeply_shared_dag_without_recursion(), metavalue_facts_preserve_unknown_inputs_and_explicit_integer_conversion(), oversized_recursive_metadata_frames_fail_before_llvm_type_construction(), recursive_metadata_fixture(), recursive_metadata_preflight_rejects_an_unsupported_leaf(), recursive_metadata_regions_use_value_offsets_and_exact_widths() (+4 more)

### Community 102 - "FileId"
Cohesion: 0.16
Nodes (28): FileId, design_validator_rejects_invalid_process_ownership_and_entry(), a_public_method_on_a_private_type_has_only_module_visibility(), a_public_signature_cannot_expose_a_private_type(), a_public_view_cannot_project_a_private_field_type(), a_public_view_may_project_a_private_field_of_public_type(), a_qualified_attribute_uses_the_exact_module(), a_renamed_import_binds_its_local_name() (+20 more)

### Community 103 - "wave.c"
Cohesion: 0.26
Nodes (15): sx_wave_add_words(), sx_wave_bit(), sx_wave_changed(), sx_wave_close(), sx_wave_close_fst(), sx_wave_discriminant(), sx_wave_open_vcd(), sx_wave_prepare() (+7 more)

### Community 104 - "helpers.rs"
Cohesion: 0.13
Nodes (11): Checker<'a>, declared_bounds_of(), explicit_range_len(), is_self_value(), path_string(), read_call_type(), signed_lit(), strlit_help() (+3 more)

### Community 105 - ".project_place"
Cohesion: 0.26
Nodes (6): Access, Lowering<'_>, Place, Storage, Local, Signal

### Community 106 - "Lowering<'a>"
Cohesion: 0.16
Nodes (3): write_guard(), is_test_entity(), Lowering<'a>

### Community 107 - "BinOp"
Cohesion: 0.12
Nodes (16): BinOp, Add, And, Custom, Div, Eq, Ge, Gt (+8 more)

### Community 110 - "tests/writes.rs"
Cohesion: 0.12
Nodes (13): a_view_method_cannot_drive_an_input_leaf(), an_impl_function_cannot_assign_to_a_const(), an_impl_function_checks_ranged_assignments(), an_impl_function_checks_widths_of_its_own_parameters(), assigning_bool_to_a_bit_port_is_rejected(), bad_initializer_type_is_rejected(), chained_integer_aliases_still_enforce_value_ranges(), enum_assignment_uses_the_enum_type() (+5 more)

### Community 111 - "Compiler foundations borrowed from rustc"
Cohesion: 0.12
Nodes (12): 1. UI tests for diagnostics, 2. Diagnostics: `--explain`, JSON, structured suggestions, 3. One constant evaluator, 4. A resolved tree between `types` and `elab`, 5. Lang items: std marks what the compiler hooks, Compiler foundations borrowed from rustc, Open questions, Order (+4 more)

### Community 112 - "The siox standard library"
Cohesion: 0.12
Nodes (16): `core::macros`, `core::ops` (re-exported by `std::ops`), Current boundaries, Module map, `std::attrs` and `core::attrs`, `std::bits`, `std::fixed`, `std::float` (+8 more)

### Community 113 - "metavalue_operand_sharing.rs"
Cohesion: 0.42
Nodes (6): lower_nested(), lower_resolved(), nested_metavalue_operands_are_hoisted_not_duplicated(), nodes(), resolved_multi_driver_contributions_are_hoisted_not_duplicated(), std_logic_tables_finish_as_interned_lookups()

### Community 115 - "compiler.rs"
Cohesion: 0.13
Nodes (18): absolute_use_path(), CompilationStats, CORE, core_source(), DependencySource, discover_dependencies(), discover_import_modules(), lexical_dependency_discovery_matches_both_import_spellings() (+10 more)

### Community 116 - "ProcessBlockId"
Cohesion: 0.13
Nodes (19): lower_for(), lower_if(), lower_match(), lower_process(), lower_statement(), lower_statements(), ProcessActivation, Reactive (+11 more)

### Community 117 - "ir/tests.rs"
Cohesion: 0.25
Nodes (5): a_non_constant_initializer_is_reported(), an_exhaustive_match_is_not_an_inferred_latch(), CLK_PRELUDE, COUNTER, expand_fixture_expressions()

### Community 118 - "check-vcd.py"
Cohesion: 0.18
Nodes (8): LIBFST, main(), precompile_runtime(), RUNTIME_SOURCES, check_profile(), main(), read_vcd(), values()

### Community 119 - "type_head_name"
Cohesion: 0.20
Nodes (5): is_blanket_array_impl(), self_ty(), type_head_name(), type_head_span(), Checker<'a>

### Community 120 - "expr.rs"
Cohesion: 0.23
Nodes (11): DEFAULT_LOGIC_TYPE, LookupTable, LookupTableId, UnOp, IntToReal, Neg, Not, RealToInt (+3 more)

### Community 121 - "DefKind"
Cohesion: 0.14
Nodes (14): DefKind, Attr, Builtin, Const, Entity, Enum, EnumVariant, Fn (+6 more)

### Community 123 - "hardware_source_values.rs"
Cohesion: 0.16
Nodes (3): alias_source(), long_source_alias_chains_are_linear_and_do_not_recurse_per_statement(), typed_aliases_do_not_recompute_arithmetic_at_each_consumers_width()

### Community 124 - "Macros"
Cohesion: 0.15
Nodes (13): Built-in macros, Declaring a macro, Diagnostics, Expansion, Fragment kinds, Hygiene, Invoking a macro, Later slices (+5 more)

### Community 125 - "Checker"
Cohesion: 0.18
Nodes (7): Direction, In, Inout, Out, dir_str(), Checker, PortInfo

### Community 126 - ".lower_shaped_source"
Cohesion: 0.31
Nodes (9): aggregate_leaf_pairs(), array(), Lowering<'_>, nested_reindexing_keeps_written_position_order_and_canonical_ids(), reindex_aggregate_value(), reindexing_does_not_truncate_a_mismatched_shape_or_missing_leaf(), scalar(), span() (+1 more)

### Community 127 - "derive.rs"
Cohesion: 0.26
Nodes (14): append_assignments(), derive_scheduler_forms(), DerivedAssignment, digital_binary(), digital_expr(), digital_node(), event_design(), event_scheduler_view_is_rebuilt_from_the_declared_process_region() (+6 more)

### Community 129 - "Architecture"
Cohesion: 0.18
Nodes (11): Architecture, Compiler API and CLI, Cross-cutting conventions, Current Process IR ingress boundary, Data that flows between stages today, Modules, rustc-shaped compiler boundary, Signal widths (+3 more)

### Community 130 - "siox Phase 1 — Digital Language Specification"
Cohesion: 0.18
Nodes (11): 1. Phase 1 goal, 2. Core principle, 3.28 Nominal type derivation, 3.29 Uninitialized values (`new`), 3.30 Macros, 4. Historical Phase 1 implementation plan, 5. Phase 1 example suite, 6. Phase 1 final deliverable (+3 more)

### Community 131 - "Public methods on entities"
Cohesion: 0.18
Nodes (10): Elaboration, Interaction with existing rules, Motivation, Open questions, Precedent, Public methods on entities, Restriction for a first version, Scope (+2 more)

### Community 132 - "Pipelined functions: `#[latched]` and `#[latch]`"
Cohesion: 0.18
Nodes (9): Decision, Lowering, Not proposed, Open questions, Pipelined functions: `#[latched]` and `#[latch]`, References, Stage names and references, Stalls (+1 more)

### Community 133 - "substitute.rs"
Cohesion: 0.32
Nodes (11): expr_to_type(), fold_const(), int_literal(), subst_block_paths(), subst_expr(), subst_expr_paths(), subst_if(), subst_if_paths() (+3 more)

### Community 136 - "Q: Trace scalar Logic waveform metadata and both waveform writers"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: Trace scalar Logic waveform metadata and both waveform writers, Source Nodes

### Community 137 - "conversion_without_from.rs"
Cohesion: 0.33
Nodes (10): a_bit_taken_out_of_a_vector_is_reported_the_same_way(), a_derivation_chain_still_converts_both_ways(), a_vector_conversion_is_untouched(), an_explicit_from_impl_still_converts(), diagnostics(), narrowing_logic_to_bit_is_reported(), native_test_lowering_refuses_the_same_conversions(), testbench_diagnostics() (+2 more)

### Community 138 - "Historical Stage 8 — Test entities, assertions, and stimulus"
Cohesion: 0.20
Nodes (10): Acceptance criteria, `after`, `await`, and background clocks, Endgoal, Generic functions and trait bounds, Goal, Historical Stage 8 — Test entities, assertions, and stimulus, Macros vs. functions, No exceptions (+2 more)

### Community 139 - "Historical Stage 11 — Minimal digital standard library"
Cohesion: 0.20
Nodes (10): Acceptance criteria, Endgoal, Goal, Historical Stage 11 — Minimal digital standard library, Modules, `std::attrs`, `std::bits`, `std::logic` (+2 more)

### Community 140 - "`core` and `std`"
Cohesion: 0.20
Nodes (10): Base metadata in `std::attrs`, `core`, `core` and `std`, Migration, Open questions, Prelude, Slice 1, Slice 2: directives are built in, and `core` follows rustc's layout (+2 more)

### Community 141 - "House rules"
Cohesion: 0.20
Nodes (10): 1. The design principle, 2. Pipeline layering, 3. Diagnostics, 4. Testing gate, 5. Surface-syntax changes are breaking, 6. Language vocabulary, 7. Working alongside other agents, 8. Commits (+2 more)

### Community 142 - "emit_wave_metadata"
Cohesion: 0.18
Nodes (17): collect_wave_scopes(), emit_metadata(), emit_source_locations(), emit_wave_metadata(), emit_wave_scope_header(), wave_logic_symbols(), wave_logic_symbols_for_type(), WaveScope (+9 more)

### Community 144 - "import_forms.rs"
Cohesion: 0.36
Nodes (9): a_format_struct_is_its_family_over_the_range(), a_name_from_two_globs_is_ambiguous_only_where_used(), a_variant_import_loads_its_module_and_checks_the_variant(), an_import_cannot_pass_through_a_type_alias(), compile(), every_import_form_resolves(), explicit_names_conflict_and_locals_beat_globs(), self_names_a_module_or_an_enum() (+1 more)

### Community 145 - "operator_no_impl.rs"
Cohesion: 0.33
Nodes (8): a_nominal_array_newtype_falls_back_to_builtin_arithmetic(), a_self_typed_right_operand_still_dispatches(), a_std_nominal_array_family_is_not_reported(), an_unmatched_right_operand_on_a_struct_is_reported(), diagnostics(), plain_vector_arithmetic_is_untouched(), PRELUDE, the_declared_right_operand_type_still_dispatches()

### Community 146 - ".resolve_connections"
Cohesion: 0.16
Nodes (14): concrete_ty(), Connection, EType, Array, Named, Other, eval(), index_width() (+6 more)

### Community 147 - "siox"
Cohesion: 0.22
Nodes (9): Editor support, Get the compiler, Learn more, License, Run it, See the waveforms, siox, The commands you'll use (+1 more)

### Community 148 - "CachedIntOp"
Cohesion: 0.25
Nodes (7): CachedIntOp, Add, And, Mul, Or, Sub, Xor

### Community 149 - "FailureKind"
Cohesion: 0.33
Nodes (5): FailureKind, Backend, Input, Selection, Validation

### Community 152 - "TODO"
Cohesion: 0.22
Nodes (9): API, AST, IR, LLVM, Out of scope for the current compiler, Output, Phase 1 exit criteria, std (+1 more)

### Community 153 - "Historical Stage 7 — Event-driven simulator core"
Cohesion: 0.25
Nodes (8): Acceptance criteria, Basic simulation loop, Endgoal, `'event` rule, Goal, Historical Stage 7 — Event-driven simulator core, `'old` rule, Required simulator concepts

### Community 154 - "fastlz.c"
Cohesion: 0.29
Nodes (3): fastlz_compress(), FASTLZ_COMPRESSOR(), FASTLZ_DECOMPRESSOR()

### Community 155 - "lower"
Cohesion: 0.17
Nodes (17): a_labelled_generate_scopes_its_signal_names(), applied_view_methods_dispatch_on_view_and_backing_identity(), custom_logic_encoding_trait_cannot_create_backend_metadata(), custom_traits_named_like_hooks_keep_their_module_identity(), entity_associated_functions_keep_resolved_owner_identity(), equal_entity_leaves_lower_the_resolved_bodies(), equal_enum_leaves_keep_variants_widths_and_symbols_distinct(), equal_free_function_leaves_lower_the_resolved_bodies() (+9 more)

### Community 156 - "core_library.rs"
Cohesion: 0.43
Nodes (7): a_hook_name_alone_grants_nothing(), a_variant_named_like_a_type_does_not_capture_it(), compile(), core_and_std_paths_name_the_same_hooks(), core_needs_no_standard_library(), STD, user_modules_cannot_bind_lang_items()

### Community 157 - "generic_argument_operator.rs"
Cohesion: 0.46
Nodes (6): a_bound_closing_on_a_shift_token_still_parses(), an_unparenthesised_operator_points_at_the_parentheses(), in_generic(), nested_generic_type_arguments_parse_without_aliases(), parse_errors(), the_spellings_that_work_are_left_alone()

### Community 158 - "sx_fail"
Cohesion: 0.23
Nodes (17): 2026-09-13 — Claude — independent check of the direct matrix, and two regressions, sx_allocate_event(), sx_bit(), sx_clear_lane(), sx_event_const_masks(), sx_event_has_value(), sx_event_masks(), sx_fail() (+9 more)

### Community 159 - "struct_let_initializer.rs"
Cohesion: 0.46
Nodes (7): a_body_that_cannot_be_folded_is_reported(), a_copy_from_another_struct_is_reported(), a_default_construction_stays_silent(), a_foldable_call_is_not_reported(), a_struct_literal_initializer_stays_silent(), diagnostics(), the_scalar_rule_it_mirrors_still_holds()

### Community 160 - "words_const"
Cohesion: 0.29
Nodes (4): words_const(), Lowering<'a>, logic_binary_table_result(), packed_logic_tables_are_interned_as_compact_lookups()

### Community 161 - "format.rs"
Cohesion: 0.48
Nodes (6): arity(), escaped_braces_do_not_consume_arguments(), FormatPart, Placeholder, Text, parts()

### Community 162 - "late_diagnostic_spans.rs"
Cohesion: 0.62
Nodes (5): a_compile_time_file_error_renders_at_the_let_declaration(), a_diagnostic_shows_its_source_line_with_a_caret(), an_ir_lint_renders_at_the_port_declaration(), compile(), rendered()

### Community 163 - "Historical Stage 2 — Lexer and parser"
Cohesion: 0.33
Nodes (6): Acceptance criteria, AST should represent, Endgoal, Goal, Historical Stage 2 — Lexer and parser, Work items

### Community 164 - "Historical Stage 3 — Name resolution and module system"
Cohesion: 0.33
Nodes (6): Acceptance criteria, Endgoal, Goal, Historical Stage 3 — Name resolution and module system, Name-resolution rules, Work items

### Community 165 - "Historical Stage 4 — Type system and kind checking"
Cohesion: 0.33
Nodes (6): Acceptance criteria, Digital type rules, Endgoal, Goal, Historical Stage 4 — Type system and kind checking, Work items

### Community 166 - "Historical Stage 5 — Entity specialization and elaboration"
Cohesion: 0.33
Nodes (6): Acceptance criteria, Elaboration example, Endgoal, Goal, Historical Stage 5 — Entity specialization and elaboration, Work items

### Community 167 - "Historical Stage 6 — Digital IR generation"
Cohesion: 0.33
Nodes (6): Acceptance criteria, Endgoal, Goal, Historical Stage 6 — Digital IR generation, Important IR distinction, Work items

### Community 168 - "Historical Stage 9 — Waveform and tracing output"
Cohesion: 0.33
Nodes (6): Acceptance criteria, Endgoal, Example CLI, Goal, Historical Stage 9 — Waveform and tracing output, Work items

### Community 169 - "Standard-library build-out"
Cohesion: 0.33
Nodes (6): Boundary, Build order, Deliberate exclusions, Existing modules, Standard-library build-out, What std is

### Community 170 - "Simulation"
Cohesion: 0.33
Nodes (6): Native execution, Simulation, Simulation time and the event wheel, The model: delta-cycle, event-driven, Waveforms, X/Z propagation through vectors

### Community 171 - "driver_override.rs"
Cohesion: 0.53
Nodes (4): a_clocked_default_holds_when_its_override_does_not_fire(), a_combinational_default_holds_when_its_override_does_not_fire(), a_replaced_driver_leaves_no_trace_in_the_value(), run()

### Community 172 - "compiler_api.rs"
Cohesion: 0.60
Nodes (5): compiler(), in_memory_analysis_retains_every_completed_phase(), input_failures_are_separate_and_typed(), language_errors_are_structured_results_not_host_failures(), textual_artifacts_are_returned_in_memory()

### Community 173 - "module_files.rs"
Cohesion: 0.73
Nodes (5): a_file_declaring_its_own_path_loads(), a_file_declaring_the_wrong_module_is_reported_at_its_declaration(), a_missing_module_file_names_the_path_it_looked_for(), compile(), project()

### Community 174 - "Interoperability"
Cohesion: 0.40
Nodes (5): Compiler embedding API, Editor support (`siox-lsp`), File I/O, Foreign functions (`extern "C"`), Interoperability

### Community 175 - "Historical Stage 1 — Syntax freeze and examples"
Cohesion: 0.40
Nodes (5): Acceptance criteria, Endgoal, Goal, Historical Stage 1 — Syntax freeze and examples, Work items

### Community 176 - "Historical Stage 10 — Diagnostics and lint rules"
Cohesion: 0.40
Nodes (5): Acceptance criteria, Endgoal, Goal, Historical Stage 10 — Diagnostics and lint rules, Required diagnostics

### Community 177 - "Historical Stage 12 — CLI and project workflow"
Cohesion: 0.40
Nodes (5): Acceptance criteria, Commands, Endgoal, Goal, Historical Stage 12 — CLI and project workflow

### Community 178 - "At a glance"
Cohesion: 0.40
Nodes (5): At a glance, Diagnostics, Logic, Structure, Types and generics

### Community 179 - "siox documentation"
Cohesion: 0.40
Nodes (5): Build and run, Current status (summary), siox documentation, The compiler pipeline, Where to start

### Community 180 - "siox roadmap"
Cohesion: 0.40
Nodes (5): Non-goals, Phase 1 — digital simulation, Phase 2 — analogue and mixed signal, Phase 3 — design, foreign HDL, and synthesis, siox roadmap

### Community 181 - "Testing"
Cohesion: 0.40
Nodes (5): How the compiler is tested, Reporting, Running, `#[test]` entities are testbenches, Testing

### Community 182 - "Q: Which paths control canonical cache lifetime and compact packed write width?"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: Which paths control canonical cache lifetime and compact packed write width?, Source Nodes

### Community 183 - "Diagnostic"
Cohesion: 0.14
Nodes (7): Diagnostic, Label, Severity, Error, Help, Note, Warning

### Community 185 - "packed_partial_write.rs"
Cohesion: 0.60
Nodes (5): a_conditional_bit_write_still_warns(), a_conditional_runtime_bit_write_still_warns(), a_constant_bit_write_is_not_an_inferred_latch(), an_unconditional_checked_runtime_bit_write_is_not_an_inferred_latch(), diagnostics()

### Community 186 - "libfst"
Cohesion: 0.40
Nodes (4): A consequence worth knowing, Fetching it, libfst, Third-party sources

### Community 187 - "2026-07-31 (cont.) — a probe that used the wrong syntax found a real bug"
Cohesion: 0.50
Nodes (4): 2026-07-31 (cont.) — a probe that used the wrong syntax found a real bug, Enum widths, measured rather than assumed, The differential caught what the implementation hid, The same bug had a second spelling

### Community 188 - "Q: optimize these first"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: optimize these first, Source Nodes

### Community 189 - "Artifact"
Cohesion: 0.40
Nodes (5): Artifact, File, FileArtifact, Object, TestExecutable

### Community 190 - "2026-07-31 (cont.) — writing wrong programs on purpose"
Cohesion: 0.67
Nodes (3): 2026-07-31 (cont.) — writing wrong programs on purpose, The array index was the real one, Where I stopped short

### Community 191 - "An index checked only when it was written as a literal"
Cohesion: 0.67
Nodes (3): A negative index folded to nothing at all, An index checked only when it was written as a literal, Found, not fixed: an instance array over-runs its declared size

### Community 192 - "Generic arguments: a hint that names the spelling that works"
Cohesion: 0.67
Nodes (3): Found, not fixed: a generic argument cannot itself be generic, Generic arguments: a hint that names the spelling that works, The measurement was broken for three commands and I read it anyway

### Community 224 - "CachedCast"
Cohesion: 0.40
Nodes (5): CachedCast, SignExtend, Truncate, ZeroExtend, CastKey

### Community 226 - "sx_runtime_run_test"
Cohesion: 0.21
Nodes (12): sx_apply_due_events(), sx_clear_error(), sx_clear_events(), sx_clear_strings(), sx_design_failed(), sx_fail_id(), sx_fail_process_block(), sx_has_changed_sensitivity() (+4 more)

### Community 228 - "types/mod.rs"
Cohesion: 0.20
Nodes (8): AttrValueTy, Bool, Integer, Other, Str, check(), PHASE2_ATTRS, SYS_ATTRS

### Community 229 - "main"
Cohesion: 0.27
Nodes (4): main(), sx_is_vcd(), sx_runtime_error(), sx_runtime_warning_count()

### Community 230 - "fstHandle"
Cohesion: 0.39
Nodes (8): fstWriterEmitValueChange(), fstWriterEmitValueChange32(), fstWriterEmitValueChange64(), fstWriterEmitValueChangeVec32(), fstWriterEmitValueChangeVec64(), fstWriterEmitVariableLengthValueChange(), fstWriterUint32WithVarint32(), fstWriterUint32WithVarint32AndLength()

### Community 231 - "Q: optimize these first"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: optimize these first, Source Nodes

### Community 232 - "deterministic_output.rs"
Cohesion: 0.67
Nodes (3): a_design_compiles_to_identical_output_every_time(), compile_once(), DESIGN

## Knowledge Gaps
- **1361 isolated node(s):** `siox`, `LIBFST`, `RUNTIME_SOURCES`, `Path`, `Memory` (+1356 more)
  These have ≤1 connection - possible missing edges or undocumented components. (Counts symbols only; 1773 node(s) total have ≤1 connection when file, concept and rationale nodes are included.)
- **56 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Span` connect `Span` to `Parser<'a>`, `DiagnosticSink`, `substitute.rs`, `pretty.rs`, `ast.rs`, `process.rs`, `Checker<'a>`, `.resolve_connections`, `diag.rs`, `Resolver<'a>`, `.settle`, `FunctionIndex`, `syntax/macros.rs`, `FunctionIndex<'a>`, `process_value`, `Lowering<'a>`, `ProcessValueId`, `Checker<'a>`, `ProcessValueKind`, `SignalId`, `elab.rs`, `Ty`, `places.rs`, `process_value_layout`, `fit`, `Lowering<'a>`, `Lowering<'a>`, `SourceValues`, `lints.rs`, `source_processes.rs`, `passes.rs`, `Lowering<'a>`, `Diagnostic`, `expr_path`, `SourceLayout`, `Checker<'a>`, `process_packed_meta_uncached`, `Expander`, `Pattern`, `expr_span`, `value_ref_with_type_inner`, `Expr`, `lower/hardware.rs`, `Expr`, `.lower_stmt_at`, `process_entry`, `SourceMap`, `DefId`, `Lowering<'a>`, `testbench.rs`, `Elaborator<'a>`, `Checker<'a>`, `FileId`, `.project_place`, `Lowering<'a>`, `.tokenize`, `Checker<'a>`, `compiler.rs`, `ProcessBlockId`, `type_head_name`, `Checker`, `derive.rs`?**
  _High betweenness centrality (0.295) - this node is a cross-community bridge._
- **Are the 5 inferred relationships involving `ProcessValueId` (e.g. with `nested_reindexing_keeps_written_position_order_and_canonical_ids()` and `process_storage_identity_is_validated()`) actually correct?**
  _`ProcessValueId` has 5 INFERRED edges - model-reasoned connections that need verification._
- **What connects `siox`, `LIBFST`, `RUNTIME_SOURCES` to the rest of the system?**
  _1361 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Parser<'a>` be split into smaller, more focused modules?**
  _Cohesion score 0.05573734729493892 - nodes in this community are weakly interconnected._
- **Why does `2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs` connect `2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs` to `Lowering<'a>`, `sx_runtime_run_test`, `Agent chat`, `sx_fail`?**
  _High betweenness centrality (0.272) - this node is a cross-community bridge._
- **Are the 95 inferred relationships involving `FileId` (e.g. with `span()` and `check_src()`) actually correct?**
  _`FileId` has 95 INFERRED edges - model-reasoned connections that need verification._
- **Should `2026-08-31 Claude -> Codex: discussion on the metavalue hoisting fix in src/ir.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.014388489208633094 - nodes in this community are weakly interconnected._
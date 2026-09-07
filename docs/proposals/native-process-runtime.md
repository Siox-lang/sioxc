# The native process runtime

Status: **migration ABI**. Runtime services and executable instruction/value
coverage remain to be implemented. The LLVM object exports source-independent
test/process descriptors and one callable resume entry per process. It
specifies the fixed ABI that direct LLVM process lowering will call, so step 6 of the
[unified process pipeline](testbench-software-ir.md) has a target to build
against rather than discovering one while porting.

The plan already states the principle: *"A small linked runtime should own
services rather than compiler transformations."* This says which services, with
what signatures, and which of today's generated C is **not** a service and must
become emitted code instead.

## Today's boundary

`src/driver/build.rs` emits a self-contained C program that links against the
LLVM object. The split is currently lopsided — the object owns design state and
settling, and the C owns everything else.

**Provided by the LLVM object, called from C** (the whole of today's design ABI):

```c
void     sx_reset(void);
void     sx_settle(void);
uint64_t sx_read_word(uint32_t signal, uint32_t word);
void     sx_set_word(uint32_t signal, uint32_t word, uint64_t value);
uint32_t sx_range_error(void);   int64_t sx_range_value(void);
uint32_t sx_range_site(void);
uint32_t sx_index_error(void);   int64_t sx_index_value(void);
```

The object also exports immutable process discovery metadata. The
compatibility harness already consumes `sx_test_count` and `sx_test_names` for
filtering and reporting; the remaining generated test-body dispatcher accepts
only the descriptor index. The reusable runtime will consume the same tables:

```c
extern const uint32_t sx_process_abi_version;
extern const uint32_t sx_test_count;
extern const char *const sx_test_names[];
extern const uint32_t sx_test_roots[];
extern const uint32_t sx_test_process_offsets[];
extern const uint32_t sx_test_process_ids[];

extern const uint32_t sx_process_count;
extern const uint32_t sx_process_roots[];
extern const uint32_t sx_process_owners[];
typedef uint8_t (*sx_process_entry)(uint32_t resume_block);
extern sx_process_entry const sx_process_entries[];
extern const uint32_t sx_process_initial_blocks[];
extern const uint8_t  sx_process_activations[];
extern const uint32_t sx_process_sensitivity_offsets[];
extern const uint8_t  sx_process_sensitivity_kinds[];
extern const uint32_t sx_process_sensitivity_ids[];
```

Offsets use the usual half-open flattened-table representation. Activation is
`0 = time zero`, `1 = reactive`; sensitivity is `0 = signal`, `1 = persistent
storage`. Counts make the one ABI-safe sentinel in each logically empty table
unobservable. Changing any table or encoding increments
`sx_process_abi_version`.

The runtime starts a process by calling `sx_process_entries[id]` with
`sx_process_initial_blocks[id]`. A suspended process is called again with the
resume block recorded by the suspension service. Entry status is `0 =
completed`, `1 = suspended` (reserved until suspension lowering lands), `2 =
stopped`, `3 = finish simulation`, and `255 = unsupported migration node`.
The last status is a temporary fail-closed guard: current entries execute
control-only CFGs and must not pretend that an instruction omitted by the
incremental emitter completed successfully.

**Provided by the generated C**: 46 embedded runtime functions plus the test
`main`, the waveform writers, and the AST-to-C translation of every process
body. 7,513 of build.rs's 8,833 function lines touch `ast::`; the rest is the
runtime inventoried below.

## Inventory, and what each becomes

### Becomes runtime — services with state or host contact

| service | today | notes |
| ------- | ----- | ----- |
| scheduler and time wheel | `sx_run_settle`, `sx_step_clock`, `sx_next_edge` | owns the ready queue, delta cycles, and the earliest-next-edge search |
| failure record | `sx_check_ranges`, `sx_checked_index`, `sx_io_fail` | first-failure wins, with value, declared range and source location |
| file services | `sx_read_file`, `sx_read_text`, `sx_read_values`, `sx_io_alloc`, `sx_io_reset` | buffers and lifetime |
| UTF-8 | `sx_utf8`, `sx_utf8_next` | decode/encode across the string boundary |
| deterministic random | `sx_rand`, `sx_randint`, `sx_random_value`, `sx_uniform` | seed state must be reproducible across backends |
| dynamic arrays | `sx_dyn_get`, `sx_dyn_get_checked`, `sx_dyn_equal_values` | heap-backed values read at run time |
| formatting | `sx_decimal`, `sx_chars` | arbitrary-width decimal and char-vector rendering |
| waveforms | `sx_vcd_*`, `sx_fst_*`, `sx_is_vcd`, `sx_wave_begin_test` | writer lifetime, per-test files, libfst linkage |
| descriptors and accounting | generated `main`, `sx_dbg_*` | count/name discovery is already object-owned; move result counting, stable output, and debug lookup into the runtime |

### Does *not* become runtime — emit it instead

These exist only because C lacks arbitrary-width integers and the generator
needed helpers. LLVM has the operations natively and must emit them inline:

`sx_mask`, `sx_set`, `sx_shl`, `sx_shr`, `sx_udiv`, `sx_idiv`, `sx_ishr`,
`sx_i64`, `sx_f64`, `sx_b64`, `sx_logic_element`

Keeping them as runtime calls would be slower than the code LLVM already emits
for `Expr::Binary`, and would put value semantics in two places — the exact
"two engines, one meaning" hazard the architecture warns about.

## The inversion this implies

`sx_settle` today is *provided by* the object and *called by* the C scheduler.
After the migration the runtime owns the scheduler and calls into
LLVM-emitted process entry points instead. The design ABI keeps `sx_reset`,
`sx_read_word`, `sx_set_word` and the failure latches; `sx_settle` stops being
an ABI entry and becomes an internal consequence of running the ready queue.

That inversion is the substantive change. Everything else is relocation.

## ABI decisions that depend on Process IR

Flagged rather than guessed, because Codex owns these and the answers determine
signatures:

1. **Process entry and resume — decided and emitted.** Each process has one
   entry pointer taking a `ProcessBlockId`. Initial and resumed activation use
   the same function; the initial block lives in its own descriptor table.
   This bounds symbol/function growth while preserving explicit CFG resume
   identity. Instruction/value and suspension-service lowering remains.
2. **Storage allocation.** Whether `ProcessStorage` is runtime-allocated and
   addressed by `ProcessStorageId`, or emitted as LLVM globals. Initializers,
   recursive layouts, flattened DUT bindings, and endpoint directions are now
   complete, so this is an ABI/allocation choice rather than an IR blocker.
3. **Staged writes.** Whether the runtime commits end-of-step signal writes or
   LLVM-emitted code does. Timing is now explicit: locals and test storage are
   immediate, hardware signals are staged, and an immediate storage write
   stages propagation to its bound DUT inputs. Ownership of that commit loop is
   still the open ABI choice.
4. **Activation — decided and emitted.** Exact
   `ProcessActivation::Reactive` signal/storage sensitivity lists are immutable
   offset tables in the design object. The runtime reads them while resetting a
   selected test and gives reactive processes their initial time-zero
   activation; no generated registration calls are required.
5. **Test descriptors — decided and emitted.** `ProcessTest` is an immutable
   name/root/process-list table in the design object. Each descriptor lists
   stimulus, clocks, and all nested DUT hardware processes under its root.

## Non-goals

- Changing `src/driver/build.rs`, `build.rs`, the Cargo files, or the LLVM
  backend. This document is specification only.
- Replacing clang as the linker driver. It stops being a source-language
  translator; it may remain a linker.
- Designing the runtime's internal data structures. Only the boundary is fixed
  here.

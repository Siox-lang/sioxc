# The native process runtime

Status: **proposal**, and an inventory. Nothing here is implemented and no
code is changed by it. It specifies the fixed ABI that direct LLVM process
lowering will call, so step 6 of the
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
| descriptors and accounting | generated `main`, `sx_dbg_*` | test table, name filtering, result counting, stable output |

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

1. **Process entry and resume.** How a suspended process is re-entered —
   one entry point taking a resume block id, or one function per resume point.
   Depends on how `ProcessTerminator::Suspend` and `ProcessBlockId` are meant to
   survive lowering.
2. **Storage allocation.** Whether `ProcessStorage` is runtime-allocated and
   addressed by `ProcessStorageId`, or emitted as LLVM globals. Blocked on
   storage initializers and bindings, which are unimplemented.
3. **Staged writes.** Whether the runtime commits end-of-step signal writes or
   LLVM-emitted code does. Depends on `ProcessAssignment` semantics, and note
   that a storage write currently reports `StagedSignal` only because it is not
   a process local.
4. **Activation.** How `ProcessActivation::Reactive`'s sensitivity list reaches
   the ready queue — a static table, or registration at reset.
5. **Test descriptors.** Whether `ProcessTest` becomes a static table the
   runtime walks, or generated registration calls.

## Non-goals

- Changing `src/driver/build.rs`, `build.rs`, the Cargo files, or the LLVM
  backend. This document is specification only.
- Replacing clang as the linker driver. It stops being a source-language
  translator; it may remain a linker.
- Designing the runtime's internal data structures. Only the boundary is fixed
  here.

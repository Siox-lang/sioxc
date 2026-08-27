# Native testbenches without generated C

Status: **proposal**. The current generated-C harness remains the supported
backend until this replacement reaches feature parity.

## Motivation

Siox currently has two native lowering paths:

```text
entity behavior  -> digital IR -> Inkwell/LLVM -> object
#[test] behavior -> generated C -> Clang       -> object
```

The split was useful for bringing up executable testbenches quickly, but it now
duplicates expression semantics, aggregate layout, bounds checks, conversions,
foreign calls, and diagnostics. Adding a language rule requires auditing both
the LLVM emitter and a growing Rust-to-C string generator. The C source is also
an implementation artifact that users did not write and that Siox must debug.

Rustc does not translate tests to C. It synthesizes a test harness in its own
compiler representation, lowers that through the normal backend, and links a
runtime library. Siox should use the same boundary while retaining the
different execution models of hardware and procedural test code.

## Attribute and tool boundary

Freeze discovery semantics before introducing another backend. `test` and
`top` serve different owners and must not share root-selection machinery:

- `test` is the canonical standard-library attribute. `sioxc` resolves that
  declaration by identity, registers each enabled `#[test] entity` in the
  native test map, and compiles the map into one filterable executable.
  A same-named user attribute is ordinary metadata and must not register a
  test. `#[test]` is shorthand for `#[test = true]`; `#[test = false]` is not
  registered.
- `top` is neither a standard-library declaration nor a compiler hook. It is
  ordinary declared metadata that the compiler preserves for an RTL exporter,
  Vivado/Quartus project integration, or a Cocotb flow. Those consumers may use
  it to choose a design-under-test or implementation root. `sioxc` itself must
  not discover native tests, choose elaboration roots, or change entity
  semantics from the textual name `top`.

The frontend therefore needs one resolved `TestPlan` (qualified test identity,
enabled value, hierarchy root, entry body, source span, and shared layout
metadata) that is consumed first by the compatibility C backend and later by
software IR lowering. Discovery must not be reimplemented inside either
emitter. Other declared attributes, including `top`, remain attached to their
declarations for future output backends without entering `TestPlan`.

The current implementation does not yet satisfy this boundary: the resolver
seeds both names, `std::attrs` declares both, and type checking, elaboration,
digital IR, and the C harness inspect leaf spelling or mere presence. The
migration must remove that coupling first. This intentionally avoids building
new LLVM test lowering on top of a root model that would immediately need to be
replaced.

## Proposed pipeline

```mermaid
flowchart LR
    SOURCE["resolved + typed Siox"]
    SOURCE --> TESTPLAN["TestPlan<br/>canonical std tests"]
    SOURCE --> META["preserved metadata<br/>top + vendor attributes"]
    TESTPLAN --> ELAB["test hierarchies<br/>shared layouts"]
    ELAB --> DESIGN["digital Design IR<br/>signals + processes"]
    ELAB --> TESTIR["software Test IR<br/>procedural test functions"]
    DESIGN --> LLVM["Inkwell module"]
    TESTIR --> LLVM
    RUNTIME["Rust test runtime<br/>scheduler · files · reports · waves"] --> LINK["native linker"]
    LLVM --> OBJECT["native object"] --> LINK
    LINK --> EXE["test executable"]
    META -.-> OUTPUT["future RTL/tool output"]
```

This is not one undifferentiated IR. Digital design behavior remains normalized
around signals, drivers, event blocks, delta cycles, and synthesizable values.
Testbench behavior needs a small software control-flow IR with locals, mutable
storage, calls, branches, loops, early returns, and runtime-owned arrays. Both
IRs share type/layout metadata and the native value ABI, then contribute
functions and globals to one LLVM module.

## Software test IR

The minimum representation should include:

- typed scalar, aggregate, and runtime-array values;
- local allocation, load, store, field/index projection, and checked indexing;
- basic blocks with branch, conditional branch, return, and loop lowering;
- calls to generated Siox functions, `extern "C"`, and runtime intrinsics;
- source spans on instructions that can fail;
- test descriptors containing the qualified name and entry function;
- explicit scheduler operations for settle, time advance, and `await`.

The IR should reuse `Design::source_layouts` or a shared extracted layout type.
It must not rediscover field order, widths, ranges, or enum encodings from AST
spelling. Checked index and constrained-range operations should use one failure
ABI in both IRs so hardware and testbench reports cannot diverge again.

## Rust runtime boundary

A small Rust crate, compiled as an ordinary static library, should own services
that are not compiler transformations:

- test filtering, result accounting, and stable output;
- assertion/warning formatting and source-location reports;
- file existence and `read<T>` buffers, including UTF-8 decoding;
- deterministic random state;
- VCD/FST writers and test-to-test waveform lifetime;
- scheduler entry points used by `await` and generated clock processes;
- the first-failure record for bounds, constrained values, I/O, and assertions.

The compiler declares a versioned C-compatible ABI for these runtime calls.
`extern "C"` remains user interoperability and does not become the internal
representation of Siox expressions.

## Harness synthesis

For every enabled entity in the resolved `TestPlan`, the compiler emits a
zero-argument test function and a static descriptor. It then synthesizes one
entry function that:

1. parses the name filter and waveform paths;
2. initializes the shared runtime and design state;
3. invokes each selected descriptor;
4. prints results and returns a process status.

This mirrors rustc/libtest structurally. A future Cargo-like Siox tool may run
the executable and choose filters, but `sioxc` remains a compiler that only
builds it.

## Migration plan

1. Normalize attribute ownership and test discovery before changing codegen:
   remove `top` from `std`, stop seeding or consuming it as a compiler hook,
   preserve it as ordinary resolved output metadata, resolve `test` to its
   canonical std declaration, honor its Boolean value, and build one shared
   `TestPlan`. Migrate the compatibility C harness to consume that plan.
2. Extract and document the existing generated harness/runtime ABI and add
   differential tests for its supported constructs. Include same-name custom
   attributes, disabled tests, qualified duplicate test leaves, and preserved
   vendor `top` metadata so the boundary cannot regress.
3. Introduce `test_ir` with rendering and validation, initially covering
   scalar locals, assertions, and settle/await.
4. Emit test functions through Inkwell and link the Rust runtime, while keeping
   generated C selectable internally for differential testing.
5. Add aggregates, checked indexing, strings/file I/O, formatting, foreign
   calls, clocks, wide values, and waveform output one capability at a time.
6. Run every corpus test through both backends and require identical pass/fail,
   messages, time progression, and waveform values.
7. Make software IR the only backend, remove Clang-as-harness-compiler and the
   Rust-to-C translator, then update the architecture documents.

The compatibility backend should not receive unrelated refactors during this
migration. Bug fixes needed for current correctness still land there and gain a
differential test that the replacement must inherit.

## Non-goals

- Merging procedural test code into synthesizable digital IR.
- Making `sioxc` execute tests or manage projects.
- Replacing LLVM or the digital scheduler.
- Removing user `extern "C"` support.
- Interpreting vendor/tool attributes such as `top` inside native test
  discovery or software IR lowering.
- Designing DAP/debugger integration; stable spans and LLVM debug locations are
  prerequisites, but debugger transport is separate.

## Acceptance criteria

Generated C can be removed only when:

- the full default and bit-packed corpus pass through software IR;
- native file I/O, UTF-8 strings, arbitrary-width values, extern calls,
  assertions, filtering, multiple clocks, and VCD/FST have focused coverage;
- hardware and testbench bounds/range failures share messages and source spans;
- native discovery recognizes only enabled uses of the canonical std `test`
  declaration, while same-named custom attributes remain inert;
- `top` and other vendor metadata survive through the output boundary without
  affecting sioxc test discovery, elaboration roots, or entity semantics;
- `cargo check --no-default-features --lib` remains LLVM/runtime independent;
- `sioxc --test` still produces an executable without running it;
- no C compiler is needed solely to translate the harness.

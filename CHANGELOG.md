# Changelog

All notable changes to `siox` are recorded here. The project is pre-release
(**Phase 1: simulation-first**), so everything lives under *Unreleased*; the
format loosely follows [Keep a Changelog](https://keepachangelog.com).

The core pipeline — lexer, parser, name resolution, type/kind checking,
elaboration, digital IR, and a delta-cycle simulator with `#[test]` discovery,
assertions, and VCD export — predates this changelog. See
[`docs/implementation.md`](docs/implementation.md) for per-stage status.

## [Unreleased]

### Added
- **Strict assignment widths.** Assigning a signal to a target of a different
  width is now an error (`E-P003`) — including widths that only become concrete
  after elaboration substitutes a parameter (`uint[W]`), which the type checker
  can't see. Only direct references (name/field/element/slice/concat) are
  checked; arithmetic is exempt (results are not auto-widened — overflow wraps,
  and a different width is an explicit `resize`), so `sum = a + b` still works.
- **Composite `inout` ports.** A struct- or array-typed `inout` port aliases
  each flattened leaf (`bus.hi`, `pin[0]`) onto the matching leaf of the shared
  net, so every leaf's parallel drivers fold through `Resolve` independently —
  the bidirectional model that already worked for scalar/vector inout, now
  per-leaf for composites.
- **Method calls.** `recv.method(args)` (spec 3.20) now works in hardware: the
  impl method's body inlines during IR lowering — `self` binds to the receiver,
  parameters to the arguments, and the receiver type propagates so operators in
  the body dispatch on the concrete type — so the interpreter, JIT, and native
  engines all see the same primitive tree. Resolves inherent (`impl T`) and
  trait (`impl Tr for T`) methods in two forms: value-returning methods
  (`a.cmp(b)`, `p.sum()`, branching on `self`) inline to a value and are typed
  as the method's declared return type, and statement methods (`s.send(v)`
  whose body drives `self.valid`/`self.data`) inline as drivers on the
  receiver's fields. Method calls inside testbench stimulus are the remaining
  follow-up.
- **Fix-it help when a string literal is used as a value.** A double-quoted
  `"…"` is a `string` (a `Char` array), so using it where a single value is
  expected (`let s: Logic = "0"`) or to build a logic/enum array (`"01"`) is a
  type error. The message now carries targeted guidance: a scalar target points
  at the character literal (`'0'`, an enum variant), a vector target at the
  bit-string literal (`b"…"`), and an array target at element values
  (`{'0', '1', …}`). Assigning a string to a `Char` array stays correct.
- **Instance arrays and sub-instance port access.** An instance's ports are now
  readable as `<inst>.<port>` (`b = s.y`), so an output may be left open at
  construction and read directly — only `in` ports must be wired. Building on
  that, `let stage: Sub[N]` declares an array of instances, constructed
  element-wise (`stage[i] = Sub { .. }`, typically in a generate loop) and
  indexable from anywhere (`stage[i].port`). Works on all three engines.
- **Combinational-loop lint (`W-P010`).** A combinational signal whose value
  depends on itself with no register in the path (a zero-delay cycle with no
  settled value) is flagged at compile time — the simulators otherwise stop it
  at an arbitrary point.
- **Symbolic enum values in VCD waveforms.** A named enum (an FSM `State`,
  `Bool`) dumps as a VCD `string` variable, so a waveform viewer shows `Idle`/
  `Run`/`false`/`true` instead of a raw discriminant. Logic scalars keep their
  native `0/1/z/x`.
- **`sioxc test <dir>`** runs every `.siox` file in a directory (sorted), each
  under its own header, then prints an aggregate line; the exit code is nonzero
  if any file failed. A file with no `#[test]` entity reports zero tests rather
  than failing.

### Fixed
- **Possible-latch lint no longer flags an if/else mux.** A combinational
  signal assigned in both the `if` and the `else` branch is fully covered and
  is no longer reported as an inferred latch (`W-P002`); a signal assigned only
  under an `if` with no covering `else` still is.
- **Module `const`s now resolve in testbench expressions.** A bare `const`
  reference (`HIGH`, a user const) evaluated to `0` in the testbench evaluator —
  correct only for consts that happened to be zero (`LOW`). Now collected to a
  fixpoint (literals, logic chars, enum variants, other consts, const-fn
  arithmetic) and resolved on the interpreter, JIT, and native binary.
- **File reads are now source-relative.** `read`/`read_to_string`/`exists`
  resolve a relative path against the `.siox` file's own directory (like
  `include_bytes!`), not the process working directory, so a program that bakes
  in a data file works no matter where it is compiled from. Absolute paths are
  unchanged. Applies to compile-time bakes, the interpreter/JIT runtime, and the
  native binary's `exists`.

### Changed
- The runnable `.siox` example/conformance corpus moved out of `examples/` into
  a sibling repo, [Siox-lang/siox-tests](https://github.com/Siox-lang/siox-tests).
  CI checks it out and runs it against the freshly-built compiler, so a
  regression still fails the build. The Rust unit/integration tests and the
  differential harness stay in-tree (they exercise crate internals).

## [0.1.0] - 2026-07-12

First tagged release. The full Phase-1 pipeline works — lexer, parser, name
resolution, type/kind checking, elaboration, digital IR, and a delta-cycle
simulator with `#[test]` discovery, `await`/clock timing, assertions, and VCD
output — behind two engines (an LLVM JIT/AOT backend and a delta-cycle
interpreter that doubles as a differential oracle).

### Added
- **Safety lints** — **possible latch** (`W-P002`: a combinational signal only
  ever assigned under a condition holds its old value otherwise) and **unused
  import** (`W-P005`: a `using` name never referenced in its file; std excluded).
  `sioxc check` now elaborates and lowers, so structural diagnostics — these
  lints and the existing unresolved-multiple-drivers error — surface at check
  time instead of only under `test`/`sim`.
- **Struct/array-typed ports across instances** — a bundle port (`in s: Stream`,
  `in v: uint[8][3]`) now wires leaf-by-leaf across an instance boundary
  (`.s = link` connects `s.valid`<->`link.valid`, `s.data`<->`link.data`).
  Unblocks ready/valid handshakes and buses between blocks — the shape real
  multi-block designs are built from.
- **`inout` bidirectional ports / tristate buses** — an `inout` port aliases the
  net it connects to (Verilog's model): the body's `pin = expr` drives the shared
  net and reads of `pin` read the resolved value, so parallel pads fold through
  `impl Resolve for Logic` — a driven '0'/'1' beats 'Z', contention is 'X'.
- **Symbolic enum/`Logic` printing** — `print!("{}", sig)` shows a variant symbol
  (`'X'`, `Idle`) instead of the raw discriminant, on the interpreter, JIT, and
  native binary. Signals carry their enum type and the design exports a
  discriminant→symbol map (`enum_syms`) spanning std.
- **Generate loops** — `for i in lo..hi { let inst = Sub { .. } }` unrolls to
  one sub-instance per iteration, with the loop index substituted into instance
  names, type arguments, and indexed connections (`.x = wires[i]`,
  `.y = wires[i+1]`, folded to concrete element signals). Unrolled identically
  in the elaborator (hierarchy/`siox tree`) and the IR lowerer (signals and
  connection drivers), so the interpreter, JIT, and native binary all see the
  same instance graph (differential-tested, both loop directions).
- **`await` / `clock` timing primitives** — `await 10ns` (advance time),
  `await clk::rising` (edge; also `::falling`/`::event`), `await cond`
  (condition), and `clock(clk, period)` for a free-running background clock.
  Runs identically on the interpreter, the JIT, and the native test binary.
- **Rust-style operator traits** — operator overloading now uses named traits
  with Rust's `std::ops` names (`impl Add for Complex { fn add(...) }`,
  `BitAnd`/`bitand` for `and`, `Not` for unary `not`); one `impl Ord`
  (`fn cmp -> Ordering`) derives all six comparisons (replaces `impl "<=>"`).
  Mixed operands use the trait's type argument, exactly Rust's spelling:
  `impl Add<integer> for Complex`. Quoted operator-trait names
  (`impl "+" for T`) are removed — a targeted parse error points at the
  Rust-style name. Future custom operators are reserved as
  `impl ops::custom<"sym", Rhs> for T`.
- **Type-targeted attributes** — `pub attr external_clock: Bool for Pll;`
  declares an attribute valid only on that type; applied per instance
  (`#[external_clock = true] let p = Pll { .. };`), validated (E-P006 on the
  wrong target, unknown names reported), and preserved through elaboration
  (shown in `sioxc tree`) for future export to external tools.
- **Explicit conversions: `T(x)` and `resize(x, n)`** — `uint[16](a)`
  zero-extends, `int[16](s)` sign-extends from an int source, `uint[4](a)`
  truncates, `integer(s)` crosses to the kernel; `resize` keeps the family
  and takes its width as a const-evaluable value argument. Typed as their
  target (the E-P003 friction now has its answer); lowered in hardware
  (sign-extension included), masked in testbench evaluation and the native
  binary.
- **Signed division and arithmetic shift-right for `int`** — now std source
  (`std/bits.siox`), built on `resize` and `self::width`: magnitude divide +
  sign restore; top-bit mask fill. `-20 / 3 = -6`, `-20 >> 1 = -10`, verified
  on interp, JIT, and native.
- **`std::prelude`, auto-loaded** — `Bit`/`Logic`/`Bool`/`Clock`,
  `uint`/`int`, `Boolean`/`Ordering`, `string`, `Time`/`Freq` are in scope in
  every file with no `using` (like VHDL's implicit `std.standard`). Ends the
  silent kernel-fallback: a bare file now gets signed `int` comparison and
  `10ns` out of the box. A std root without `prelude.siox` skips it silently.
- **Signedness fully erased from the compiler's types** — the vestigial
  `signed` field on `Ty::Vector` / `EType::Vector` is gone (both are now just
  `{ width }`), `vector_families` is a membership set (not name->signed), the
  dead sign-extension path in lowering is removed, and the struct-attribute
  machinery (added only for `#[vector]`/`#[signed]`) is reverted — no struct
  carries attributes. `int` and `uint` are now the *same* compiler type
  (a bit vector of width N), distinguished purely by their operator impls;
  errors show both as `uint[N]`. All engines + differential green.
- **No signedness marker at all — it lives in the operator impls** — the
  `Signed` trait is gone too. `int` is signed purely because its `Ord`/`Shr`/
  `Div` impls are (signed compare, arithmetic shift, signed divide),
  dispatched by type at lowering; `uint` uses the kernel's unsigned ops. The
  compiler tracks no signedness. Sign-extension on widening — the one thing
  that isn't an operator — becomes the library `std::bits::sext`:
  `int[16](sext(x))` for signed widening, bare `int[16](x)` is a raw resize.
  All engines green.
- **`#[signed]` removed — signedness is the `Signed` capability trait** —
  unlike `#[vector]` (structural, redundant with shape), signedness is a
  *capability* (it changes comparison, shift, division, widening), so it
  belongs as a trait like int's other signed behaviours, not metadata.
  `impl Signed for int {}` (std::ops) replaces `#[signed]`; the compiler reads
  it only to sign-extend on widening (compare/shift/div are already int's own
  impls). A user `struct MyWord : Logic[];` is unsigned; `impl Signed for
  MyWord {}` makes it signed (sign-extends on widen — verified). No struct
  carries a compiler attribute anymore.
- **`#[vector]` removed — bit vectors are recognized by shape** — an array
  of bits *is* a vector, so a bodyless struct deriving from `Logic[]`/`Bit[]`
  (`struct uint : Logic[]`) is a packed bit vector with no annotation needed;
  the shape is the definition. Only `#[signed]` remains, for the one thing the
  shape can't say (uint and int are both `Logic[]`). All three engines'
  recognizers (types/elab/ir) switched from reading the attribute to the shape.
- **Boolean operators are boolean-per-bit** — `and`/`or`/`xor`/`not` are one
  family (no bitwise-vs-logical pair): plain boolean on `Bool`, and per-bit
  on bit-derived types, returning the same bit array (VHDL logic-vector
  style, `uint[32] and uint[32] -> uint[32]`). Intrinsic to bit types — no
  per-type impl (the redundant uint impls were removed; Logic keeps its
  4-value truth table). Rejected on non-bit types (`real`/`Char`). Fixes the
  earlier logical-vs-bitwise confusion: the engines were logical on
  multi-bit words (`12 and 10` gave 1); now correctly per-bit (8) on all
  three engines.
- **Generic functions, trait bounds, and `where`** — `fn maxi<T: Ord>(a, b)`
  (or the `where T: Ord` spelling, exact sugar). Fns inline, so a call is a
  monomorphization: the body dispatches operators on the caller's concrete
  type, and the bound is enforced at the call site (a named type needs an
  explicit `impl Tr`; kernel scalars/vectors satisfy built-ins). Verified on
  interp/JIT/native (examples/generic_test.siox, where_test.siox). The
  `where`-clause proposal note is now implemented and removed.
- **Built-in uint/int fully removed** — the compiler no longer has `UInt(w)`
  / `Int(w)` types or any `uint`/`int` name-check. Its only numeric-vector
  notion is a generic `Ty::Vector { width, signed }` / `EType::Vector` — a
  packed bit vector, the irreducible hardware fact. All recognition
  (array-vs-vector, conversion syntax, port width, signedness) flows through
  the `#[vector]` family set. `uint`/`int` are purely std names; the strings
  survive in the compiler only as conventional display/trait-key output for
  an unsigned/signed vector. Suite (default + interp) + corpus green on all
  three engines.
- **Representation attributes `#[vector]` / `#[signed]`** — the numeric-vector
  layout is now *declared by std*, not inferred by the compiler from the
  `: Logic[]` shape. `std/bits.siox` marks `#[vector] struct uint` and
  `#[vector] #[signed] struct int`; the compiler recognizes families by the
  attribute and drops the last name-recognition residuals (`is_int_type` is
  now `integer`-only). Attributes attach to struct declarations, validated
  against a `struct` target. Replaces the short-lived `Signed` marker trait.
- **uint/int dropped as compiler-magic names** — they are now ordinary
  `struct uint : Logic[]` / `struct int : Logic[]` declarations in
  `std/bits.siox`, no longer seeded builtins. The compiler recognizes any
  array-derived Logic family (`struct F : Logic[]`) as a numeric vector and
  reads `impl Signed` (std::ops) for the interpretation — so a user
  `struct Word : Logic[]` behaves exactly like uint, and fixed-point families
  follow the same path. Resolve seed + the `path_ty` name-mapping removed;
  the efficient `UInt(w)/Int(w)` encoding is now derived from the
  declaration. Whole suite (126 tests, incl. the JIT-vs-interp differential
  harness) + corpus green across all three engines.
- **`Clock : Bit` + inherited enum-variant paths** — `Clock` now derives
  from `Bit` (was a duplicate declaration), so `Clock(b)`/`Bit(clk)` convert
  for free. `Child::InheritedVariant` paths (`Extended::A` from a base enum)
  now resolve. Tests across syntax/types/ir plus an end-to-end derivation
  showcase (examples/derive_chain_test.siox).
- **Nominal type derivation** — `enum B : A` / `struct B : A` (with
  optional `{ … }` to extend) create distinct types reusing a base's
  representation. std's logic scalars became the chain `Bit → ULogic : Bit →
  Logic : ULogic`, retiring `std/logic/unresolved.siox` — the
  resolved/unresolved split is now a real derivation (Logic gains `Resolve`,
  ULogic doesn't). Total conversions are auto-synthesized for `T(x)`
  (parent-struct projection, enum variant-subset — representation-identity),
  so the explicit crossing impls are gone; non-total directions still need
  `impl From`. Never implicit, no `as`. Array-base-with-fields and duplicate
  inherited members are errors. All engines.
- **`warn!` + the no-exceptions decision** — siox has no `throw`/`catch`
  (incompatible with synthesizable hardware and pure inlined functions);
  errors are signals, panics, or probe-and-branch. `warn!(cond, "msg")` is
  the non-fatal sibling of `assert!`: reports to stderr and counts in the
  test summary (`ok ... 1 passed; 0 failed; 2 warnings`) without failing.
  All three engines.
- **`std::fs` primitives** — Rust-shaped file IO: `read(path)` fills a
  declared array with the file's bytes, `read_to_string(path)` gives a
  string its length, `exists(path)` probes. No handles, no modes, no `with`
  — nothing to close. In initializer position the *compiler* reads the file
  (include_bytes! style): contents bake into `Signal.init`, so the native
  binary carries its ROM with zero runtime IO (verified: binary passes after
  the source file is deleted). Format loaders (hex images, CSV) are parsing
  libraries, deliberately NOT std.
- **S5 `std::rand` + S7 text encodings** — `rand()`/`randint(lo, hi)`/
  `uniform()`/`seed(n)` as ordinary runtime functions: one deterministic
  xorshift64* shared by the runner and the native harness, so every engine
  reproduces the same sequence from the same seed. `std::text`
  gains the encoding tables (`unicode`/`ascii`/`char_of`) over the new
  `Char(n)` conversion; character literals write natively as code points.
- **S3: `print!`, `stop()`, `finish()`** — the bang marks a *macro*
  (compile-time expansion / source capture: `print!`, `assert!`); everything
  else is an ordinary function — the language does not classify functions by
  purity. `print!` format-expands at compile time (`{}` renders per argument
  kind, reals as floats); `stop()`/`finish()` end a test cleanly with the
  time. All three paths (native compiles print! to printf). `clock()`
  removed — the `after`-form is the one generator.
- **Dynamic range asserts + real initial values** — a ranged numeric
  (`integer<1..10>`) is checked after every settle on all three paths;
  leaving the domain fails the simulation with the signal, value, and time
  (`n = 11 left its range 1..10 at 95000000 fs`). Enabler: `let v: T = 1;`
  initial values now actually apply (VHDL-style Signal.init in both engines'
  reset) instead of everything starting at zero.
- **Conversion fit checking** — a constant conversion argument must be
  representable in the target: `uint[4](300)` / `int[4](-9)` are
  compile-time errors (signed domains respected; simple const expressions
  fold). Dynamic range checks deferred to the simulation-reporting design.
- **The `From` conversion trait** — `T(x)` on a named type dispatches to
  `impl From<Source> for T` (struct-valued results included): `Complex(10)`,
  and the `Logic(u)`/`ULogic(l)` resolved/unresolved crossing, all std
  source. Char literals now type into user enums with matching character
  variants. Nothing converts implicitly (spec 3.17 unchanged).
- **`extern "C"` blocks** — foreign C functions callable from siox
  (`real` = double, integer types = 64-bit words), in hardware and
  testbenches; std::math is now literally an extern block over libm, and any
  C symbol works (`labs` from libc verified in hardware on JIT + native).
- **Module-level functions** — `fn` is an item: inlined in hardware,
  const-evaluated in width positions (`uint[clog2(DEPTH)]`), callable in
  testbenches. std gains `abs`/`min`/`max`, `clog2`.
- **`std::math` real functions** — `sqrt`/`sin`/`cos`/`exp`/`log`/`pow`/
  `floor`/`ceil`/`round` as kernel externs (libm / LLVM intrinsics), plus
  `PI`/`E` real constants.
- **Parallel-driver resolution (`Resolve`)** — a signal driven from several
  contexts (impl blocks / connections) folds through its type's
  `impl Resolve` (`Logic` gets the std_logic table: tristate buses work,
  contention is 'X'); types without one error — the VHDL unresolved-type
  safety rule. `std::logic::unresolved::ULogic` is the checked mirror.
- **uint/int operators moved to std** — only the kernel types (`integer`,
  `real`) keep built-in operators; `uint[N]`/`int[N]` arithmetic and shifts
  are now `impl Add for uint` etc. in `std/bits.siox`, and `int` gains
  **signed comparison** via a sign-aware `impl Ord for int` (library source,
  not compiler code; `self::width` is available inside operator impls).
  Overload selection tightened: exact rhs match, then integer-literal
  coercion — a sole candidate is never taken on a known mismatch.
- **Python-style array iteration + testbench locals** — `for x in xs`
  iterates any array (`for i in 0..n` now binds `i` too); `xs::len` joins the
  `::` metadata attributes; testbench `let`s run in statement order and
  unconnected names are real locals. All three paths (interp, JIT, native
  binary — arrays via generated id tables).
- **Rust-style `if` expressions** — `y = if sel { a } else { b };` with
  required `else` and `else if` chains; lowers to a select everywhere
  (hardware drivers, operator-impl bodies via the inliner, testbench
  evaluation, and the native binary as a C conditional). There is still no
  `?:` — the spec freezes if/else as the only conditional form.
- **VHDL-style delayed assignment** — `clk = not clk after 5ns;` is the
  canonical clock generator (self-toggle registers on the event wheel), and
  `rst = '0' after 12ns;` is a one-shot delayed write (value evaluated at
  schedule time). `after` is positional, not a reserved word; testbench-only
  (hardware rejects it); works on interp, JIT, and the native binary (clock
  idiom; one-shots error cleanly there for now).
- **Hierarchical simulation** — an entity may instantiate sub-entities; each
  instance lowers into its own signal namespace (`Add2.s1.a`) and every port
  connection becomes a driver. Multiple instances of one entity take
  per-instance parameters (`Reg<8>` and `Reg<4>` in one parent size correctly).
- **Bare-file compile / `sioxc <file>`** — compiles the `#[top]` design to a
  native object (rustc-shaped); `--top` picks the top entity.
- **`sioxc test --no-run`** — links a standalone native test binary that runs
  every `#[test]` with libtest-style output and a name filter.
- **Compiled backend** (`siox-llvm`, inkwell): a **JIT** (`sioxc test`) and
  **AOT** native objects, both driving the shared test runner via an `Engine`
  trait.
- **Differential harness** — the JIT is verified bit-for-bit against the
  interpreter oracle across the expression surface (`--features interp`).
- Examples: `hierarchy_test`, `multiclock_test`, `instances_test` (two
  instances of one entity on different clocks), `await_test`, `top_counter`.

### Changed
- **Loop ranges are now inclusive and directional.** A numeric `for i in
  lo..hi` range includes both ends and follows its written direction (`0..2` →
  `0,1,2`; `2..0` → `2,1,0`), matching bit slices, array types, and `range`
  constants — `..` now means the same thing everywhere (it is *not* Python's
  half-open `range`). Loops were previously half-open and ascending-only; an
  index loop over an `N`-element array now runs `0..N-1`. Applied uniformly in
  the elaborator, the IR lowerer, the testbench interpreter, and the C/native
  emitter.
- **LLVM is the default execution engine.** `sioxc test` JIT-runs and
  `sioxc sim --wave` JIT-traces; the default build needs an LLVM toolchain
  (`--no-default-features` for an LLVM-free build).
- **Interpreter feature-gated off** (`interp`, off by default). It stays in-tree
  as the differential oracle and the >64-bit fallback; the engine-generic test
  runner (`Testbench`, `await`/`clock`, `assert!`) is always compiled and the
  JIT drives it.
- **Simulation time moved to the runner/kernel.** The `Engine` trait is now
  purely combinational (`set`/`read`/`settle`); the runner owns `time_fs` and
  the event wheel — deliberately the factoring digital events, Phase-2 analogue
  timesteps, and cocotb will all share.
- **Split the simulation kernel from the interpreter.** The test runner —
  `Engine` trait, `#[test]` discovery, stimulus, `await`/`clock` scheduler,
  time, waveform recording — moved to a new **`siox-run`** crate (engine-agnostic,
  always compiled). `siox-sim` is now *only* the delta-cycle interpreter (one
  `Engine`), pulled in via `--features interp` as the differential oracle — the
  rustc/Miri split at the crate level.
- **Compiler renamed `siox` → `sioxc`** (crate + binary) — the rustc side of the
  planned rustc/cargo split (the cargo-like `pcb`/`circuit` is a future repo).
- `test` reports in **libtest style** (`running N tests` … `test result: …`).

- **Tops-only lowering.** Only `#[top]`/`#[test]` roots lower; sub-entities and
  a testbench's DUTs lower recursively per-instance (`CounterTest.dut.*`), so
  two instances of one entity in a testbench no longer share state.
- **The native test binary got a real event wheel** — generated C tracks
  simulation time and per-clock next-edge state, so multiple clocks of
  different periods interleave correctly (previously all clocks toggled in
  lockstep) and `await <duration>` advances real time.

- **`wait` and `tick()` removed — `await` is the one timing primitive.**
  `wait` is a parse error (recovering as `await`); `tick()` fails with a
  pointer to the replacements: a manual pulse is plain code
  (`clk = '1'; await 5ns; clk = '0';`) and edge-driven tests use a generator
  (`clk = not clk after 5ns;`) with `await clk::rising`. `tick()` returns
  later as a std function. All examples converted.

### Fixed
- **JIT-traced VCD timestamps** were frozen at `#0` (the JIT engine reported
  time 0). The runner now owns time, so waveforms carry real timestamps and
  multiple clocks interleave correctly on one event wheel.
- **Hierarchical designs** with submodules wired up wrong (lowering was flat,
  per entity type); now per-instance with connection drivers.
- Divide-by-zero yields `0` consistently on both engines; the IR validator
  rejects malformed IR before codegen.

### Deferred / by design
- **Signedness is not compiler-hardcoded.** `int[N]`/`uint[N]` operators — and
  signed compare/divide/arithmetic-shift — will live in `std` as operator-trait
  impls, deleting the last numeric shim (#37). The compiler already inlines such
  impls (`Complex` in `std/math.siox` is the proof).
- **cocotb** integration (VPI/GPI) is a later, separate layer (#36).

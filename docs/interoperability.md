# Interoperability

How siox talks to the outside world: foreign C functions, foreign HDLs, file
I/O, editors, and compiler embedding.

## Foreign functions (`extern "C"`)

A design or testbench may call C functions declared `extern "C"`:

```siox
extern "C" { fn sin(x: real) -> real; }
```

Only the `"C"` ABI is supported. Its current contract is deliberately scalar:

- `real` maps to C `double`.
- `integer`, including aliases and constrained forms, maps to a signed 64-bit
  ABI word.
- A packed numeric type from 1 through 64 bits maps to an unsigned 64-bit ABI
  word; the result is converted to its declared storage width.
- A `std::ffi` type is passed at its C width: `c_int` as a 32-bit `int`,
  `c_uchar` as an `unsigned char`, `c_float` as a `float`, `c_long` at the
  build host's `long` width. Its result is extended back to the 64-bit word,
  sign-extended for the signed types. Declare a C function with these types
  whenever its prototype has them, since a narrower result read as a 64-bit
  word has undefined upper bits:

  ```siox
  use std::ffi::{c_int, c_double};
  extern "C" { fn ilogb(x: c_double) -> c_int; }
  ```

A declaration with a scalar return value is usable in combinational
expressions, clocked logic, and testbenches. A declaration with no return
type is a C `void` function, called for its effect:

```siox
extern "C" { fn srand(seed: integer); }
```

Its call is a statement in a testbench process, or in a function a testbench
calls, and runs in statement order. Hardware has no place to order an effect,
so a call from hardware is rejected: directly (`E-P008`), and through any
chain of function calls that reaches one. Using a void call as a value is a
type error. The same rule covers the runtime's own testbench-only intrinsics
(`rand`, `read`), which are not C functions.

A **struct or fixed array** parameter crosses by pointer, to a copy laid out
as C lays it out (natural alignment, zeroed padding), so a C function taking
`int *`, `double *` or `struct timeval *` can be declared directly:

```siox
use std::ffi::{c_int, c_double};
struct TimeVal { pub sec: integer, pub usec: integer, }
extern "C" {
    fn frexp(x: c_double, exponent: c_int[1]) -> c_double;
    fn gettimeofday(tv: TimeVal, zone: natural) -> c_int;
}
```

Its leaves must be C scalars: `integer`, `natural`, `real` (a `double`),
ranged integers (the smallest C integer holding the range, so a `c_int` field
is an `int`), `c_float` (a `float`), and packed values (up to 64 bits a C
integer, wider ones little-endian 64-bit words). When the argument is a
process-local variable it receives what C writes through the pointer, as an
out-parameter: after `frexp(8.0, e)`, `e[0]` is 4. Other arguments are passed
read-only. Such a function is testbench-only, since hardware holds an
aggregate as separate signals. An aggregate **result** follows the build
host's struct-return rules: on x86-64 SysV, up to 16 bytes come back in
registers by eightbyte class (INTEGER or SSE) and larger ones through a hidden
`sret` buffer; on AArch64, a homogeneous aggregate of up to four doubles or
floats comes back in vector registers, up to 16 bytes in x0/x1, and larger
ones through `sret`. Another host reports that it has no rule. An
`unsigned[128]` result is a C `unsigned __int128`.

Generic declarations and characters are rejected during type checking. Wrap
such an API in a C adapter that accepts and returns the types above.

In the **native binary**, symbols resolve at link time. `std::math` uses no
foreign symbols — its `real` functions are compiler built-ins — but the math
library is linked by default, so a C declaration of `sin`, `labs` and the
like resolves out of the box. A test executable links further objects and
libraries named with `--link FILE` (repeatable; `CompileRequest::with_link` in
the API), which is how a testbench reaches its own C models:

```sh
clang -c model.c -o model.o
sioxc --test model_test.siox --link model.o -o model-tests
```

Other symbols may be resolved by a consumer linking a compiler-emitted object.
Library search paths and `-l` discovery belong to the project tool.

## Foreign HDLs

Phase 1 meets other HDLs only at an `extern entity`: a black box whose
interface siox elaborates and checks, with `library`/`name` metadata naming the
foreign implementation (`siox-tests/external_entity_stub.siox`). Nothing
simulates or emits a foreign design yet; that is Phase 3 ([roadmap](roadmap.md)).
This section fixes what a siox value and a siox operation *mean* at such a
boundary, so a future co-simulation bridge, foreign-interface import, or
RTL/HDL exporter preserves siox semantics instead of inheriting the other
language's.

It covers VHDL, Verilog, SystemVerilog, SystemC, and Chisel/FIRRTL. Chisel
reaches other tools through the SystemVerilog `firtool` emits, so a Chisel
design meets siox there; its own rules matter only when one is written against
a siox interface.

### What crosses unchanged

| siox | VHDL | Verilog | SystemVerilog | SystemC | Chisel |
|---|---|---|---|---|---|
| `entity` + `impl` | entity + architecture | `module` | `module` | `SC_MODULE` | `Module` |
| `<W: integer>` | generic | `parameter` | `parameter` | template/constructor argument | Scala parameter |
| `<T>` type parameter | VHDL-2008 type generic | none | `parameter type` | template | Scala type parameter |
| `in` / `out` / `inout` | same | `input`/`output`/`inout` | same | `sc_in`/`sc_out`/`sc_inout` | `Input`/`Output`/`Analog` |
| `.x = y` | named association | `.x(y)` | `.x(y)` | port binding | `<>` / `:=` |
| `for`/`if` generate | `for`/`if generate` | `generate` | `generate` | constructor loop | Scala loop |
| `if clk.rising()` register | `rising_edge(clk)` | `always @(posedge clk)` | `always_ff` | `SC_METHOD` on `clk.pos()` | `Reg` (implicit clock) |
| impl-body statement | concurrent assignment | `assign` | `assign`/`always_comb` | `SC_METHOD` | `:=` |
| delta cycles, `after` (inertial), `transport` | identical | nonblocking update, `#` delay | same | `sc_signal` update phase | none (cycle-based) |
| `real` | `real` | `real` | `real` | `double` | none in hardware |
| `enum` | enumeration type | none | `enum` | C++ `enum` | `ChiselEnum` |

Traits, macros, `match`, inlining and generics are resolved before any boundary
and have nothing to map.

### Boundary rules

A bridge or exporter follows these. "4-state" means Verilog/SystemVerilog
`logic`/`wire` and SystemC `sc_logic`; "2-state" means SystemVerilog `bit`/`int`,
SystemC `bool`/`sc_uint`, and Chisel.

1. **Logic values.** A 4-state port is typed `Logic` (or `Logic[N]`) on the
   siox side; the nine values are a superset of the four, so the boundary
   translates and the user writes nothing.

   - **Into siox** nothing changes: `0`, `1`, `x`, `z` are `'0'`, `'1'`,
     `'X'`, `'Z'`.
   - **Out of siox** each element is collapsed by IEEE 1164 `To_X01Z`, as
     mixed-language simulators do: `'L'`→`0`, `'H'`→`1`,
     `'U'`/`'W'`/`'-'`→`x`, and the other four pass through. This is the
     defined meaning of a siox value in Verilog, not a loss, and is not
     reported.
   - **`inout`**: the net stays a resolved siox `Logic`. The foreign driver
     joins it as one more `Logic` driver, and the foreign side reads the
     resolved value through `To_X01Z`. For strong drivers `Logic`'s
     resolution agrees with a Verilog `wire` (`0` against `1` is `x`, `z`
     yields to anything, `x` wins).
   - **Strength**, where a Verilog net carries it: `pull`/`weak` `0`, `1`, `x`
     enter as `'L'`, `'H'`, `'W'`, and `'L'`/`'H'` leave as `weak0`/`weak1`,
     so a Verilog `pullup` resolves against a siox driver as in Verilog. A
     bridge without strength sees a pull-up as a strong `1`.
   - **2-state** (`Bit`, `unsigned`, `signed`, and 2-state foreign types):
     `'1'` and `'H'` are `1` and everything else is `0`, as SystemVerilog's
     4-to-2 cast. An `x` or `z` reaching a 2-state siox port is the one real
     loss, and a bridge warns on the first one per port. Ports that face a
     4-state design should therefore be `Logic`.

   VHDL needs none of this: `Logic` is `std_logic` and `Bit` is `bit`, value
   for value.
2. **Widths.** A siox operator produces its operands' width and wraps, and an
   assignment must match its target's width exactly, as VHDL `numeric_std`.
   Chisel's `+` (`+%`) agrees; its `+&` widens. Verilog and SystemVerilog size
   an expression to its context, so the carry of a wider target survives: an
   exporter sizes every arithmetic subexpression explicitly (an SV
   `W'(a + b)` cast or a slice). SystemC's `sc_uint<W>` wraps only on
   assignment, so intermediate results are cast the same way.
3. **Signedness and shifts.** Signedness belongs to the type. siox promotes
   `signed` + `unsigned` to a signed format that holds both ([language §3.17](language.md)), which
   `numeric_std` and Chisel reject and Verilog/SystemVerilog compute *unsigned*:
   one unsigned operand makes the whole expression unsigned. An exporter emits
   the extension and the sign explicitly (`$signed({1'b0, u})`, `asSInt`,
   `signed(resize(u, n))`). `>>` on `signed` is arithmetic, as VHDL
   `shift_right` and Chisel; Verilog/SystemVerilog need `>>>` on a signed
   operand.
4. **Comparisons with metavalues.** siox follows `numeric_std`: `==`, `<`,
   `<=`, `>`, `>=` with an unknown operand are false and `!=` is true (checked
   against `nvc`, `siox-tests/xz_compare_test.siox`). Verilog/SystemVerilog
   answer `x`, which an `if` treats as false, so `!=` takes the other branch.
   SystemC's `sc_lv` compares symbols exactly (`"0X" == "0X"` is true). An
   exporter to 4-state code writes each relation `r` as `(r) === 1'b1` and
   `a != b` as `!((a == b) === 1'b1)`. 2-state targets have no unknowns. Bit
   patterns (`Match`, `'-'` don't-care) are SystemVerilog's `==?` and VHDL's
   `?=`/`std_match`.
5. **Integers.** The kernel `integer` is 64-bit signed at run time. VHDL
   guarantees 32 bits before VHDL-2019, Verilog `integer` and SystemVerilog
   `int` are 32-bit, and Chisel has no hardware integer. At a port an
   `integer` is a 64-bit signed vector (SV `longint`, VHDL `signed(63 downto
   0)`); a ranged `integer<lo..hi>` that fits 32 bits may map to VHDL
   `integer range lo to hi`.
6. **Initial values.** A siox signal starts at its type's default ([language §3.29](language.md)): `Bit`
   `'0'`, scalar `Logic` `'U'`, an enum's first variant, and a numeric vector
   `0`. VHDL agrees except for vectors (`unsigned` starts all `'U'`); Verilog
   and 4-state SystemVerilog variables start `x`, 2-state ones `0`; SystemC
   `sc_logic` starts `'X'`. The values are a simulation power-on state. A
   co-simulated or exported design that must agree at time zero resets
   explicitly.
7. **Drivers.** Two drivers of an unresolved siox type (`Bit`, a struct
   without `Resolve`) are a compile error, as VHDL's unresolved types,
   SystemVerilog variables under `always_ff`/`always_comb`, and SystemC's
   default single-writer `sc_signal`. `Logic` resolves through
   `std_logic_1164`'s table, as VHDL `std_logic`, Verilog `wire`/`tri` and
   SystemC `sc_signal_resolved`. Chisel's last-connect rule (a later `:=`
   wins, at module level too) has no siox counterpart: in siox only a
   process's sequential statements override each other, and two
   impl-body statements are two drivers.
8. **Composite ports.** A struct port is a VHDL record and a SystemVerilog
   packed struct. A view ([language §3.19](language.md)) is a VHDL-2019 mode view, a SystemVerilog
   `interface` with one `modport` per view, and a Chisel `Bundle` with
   `Flipped` leaves; an array of views is an array of those. Verilog-2001 and
   SystemC have no directional bundles, so the port flattens to one scalar
   port per leaf, joined with `_` in declaration order (`bus.awvalid` →
   `bus_awvalid`, `down[2].awvalid` → `down_2_awvalid`); an exporter reports a
   flattened name that collides with another port. Tool support for VHDL-2019
   views is still thin, so a VHDL exporter may fall back to records split by
   direction.
9. **Fixed and float.** `ufixed<W, F>`/`sfixed<W, F>` and `float<W, M>` are
   VHDL-2008 `fixed_pkg`/`float_pkg` formats, ranges included (`ufixed<8, 4>`
   is `3 downto -4`). SystemC's `sc_fixed<wl, iwl>` takes the integer width:
   `iwl = W - F`. Verilog, SystemVerilog and Chisel have no fixed or float
   types, so these cross as plain vectors whose binary point the interface
   documents. Rounding and overflow styles ([std.md](std.md), `std::fixed`) are written out,
   never left to the other side's defaults.
10. **Time.** siox resolves time in femtoseconds, as VHDL's default. Verilog and
    SystemVerilog modules carry their own `timescale`/`timeprecision`, SystemC
    its `sc_time` resolution (1 ps by default): a bridge converts at the
    boundary and rejects a delay the coarser side cannot represent.
11. **Names.** siox identifiers are case-sensitive, like Verilog, SystemVerilog,
    SystemC and Chisel; VHDL's are not, so `Data` and `data` collide there. An
    identifier that is a keyword of the target (`next`, `range`, `type`,
    `select`, `wait`, `signal`, `buffer` in VHDL; `reg`, `wire`, `input`,
    `output`, `logic`, `bit` in Verilog/SystemVerilog) is escaped (VHDL
    `\next\`, Verilog `\reg `), never renamed silently.
12. **Simulation-only constructs.** `Char`, strings, file I/O, `print!`,
    testbench `await`, and hardware loops with non-constant bounds (W-P019)
    have no synthesizable counterpart in any of these languages. They stay on
    the siox side of a co-simulation boundary, and an exporter rejects them
    with the construct's span.

### Working with nvc

nvc is the reference simulator for siox's logic semantics, and the free
simulator most likely to sit on the other side of a boundary. This records
what nvc 1.23 offers (from its source, `src/elab.c`, `src/vhpi/`,
`src/jit/jit-ffi.c` and `lib/nvc/verilog*.vhd`) and how siox can use it.

nvc is GPL-3 and has no embedding API, so it is never linked into `sioxc` or
the siox runtime. Every arrangement below keeps nvc a separate program: siox
either runs as a VHPI plugin that nvc loads (as cocotb does) or produces VHDL
that nvc compiles. Mapping tables are taken from IEEE 1164 and 1364, not from
nvc's code. Both simulators count time in 64-bit femtoseconds (nvc signed,
siox unsigned) and share VHDL's delta-cycle model, so the two can step in
lock-step without translating time.

**nvc's own Verilog boundary.** For a VHDL instance of a Verilog module,
nvc's elaborator inserts conversion functions from its `NVC.VERILOG` package
into the port map, which is the arrangement rule 1 describes. It differs from
rule 1 in two places, and siox keeps its own choice in both:

| | nvc | siox (rule 1) |
|---|---|---|
| Verilog `x` enters as | `'U'` | `'X'`: `'U'` means never assigned, which `x` cannot say |
| `pull0`/`pull1` enter as | `'0'`/`'1'` (only weak, medium and small become `'L'`/`'H'`) | `'L'`/`'H'`, so a `pullup` resolves against a siox driver |

The outward direction agrees (`'U'`/`'X'`/`'W'`/`'-'`→`x`, `'L'`/`'H'`→weak
`0`/`1`). nvc accepts only `input` and `output` across the language boundary
and rejects `inout`, so it is no reference for rule 1's `inout` case.

**What nvc's VHPI supports.** Value-change, end-of-processes, next-time-step
and end-of-elaboration callbacks, object-model iteration over ports and
generics, and `vhpi_put_value` with force, deposit and their propagating
forms. Not supported: foreign architectures (`vhpi_register_foreignf` accepts
only `vhpiFuncF`/`vhpiProcF`) and `vhpi_schedule_transaction` (missing), so a
plugin cannot add a driver to a VHDL signal.

The integrations, cheapest first:

1. **Importing VHDL interfaces.** A VHPI plugin walks the elaborated design's
   ports and generics at end of elaboration and prints the matching
   `extern entity` declarations. siox gets VHDL interfaces without a VHDL
   parser and without reading nvc's private library format. The VHDL top must
   elaborate on its own, so its generics need defaults or `-g` values.
2. **Co-simulation: siox inside nvc.** `nvc --load=libsiox_vhpi.so` runs the
   siox runtime beside nvc's. Because nvc has no foreign architectures, a
   siox entity appears in VHDL as an empty shell entity with its ports; the
   plugin watches the shell's inputs, runs siox's delta cycles, and deposits
   its outputs with propagation. Deposits are not drivers, so an `inout`
   cannot resolve across this boundary, and siox outputs arrive one delta
   cycle after the inputs that caused them, which is invisible to clocked
   interfaces.
3. **Exporting siox to VHDL.** The boundary rules above are what an exporter
   must preserve. It is the largest piece, and the one that lets any siox
   design run in nvc and turns today's hand-checked nvc comparisons (the
   logic tables, the X/Z corpus tests) into an automatic differential test.
4. **VHPI in the siox runtime.** cocotb drives nvc through VHPI; a siox
   runtime that implements the same subset gets cocotb unchanged.
   `vhpi-model.c` documents the subset that cocotb needs.

nvc can also call C from VHDL (`VHPIDIRECT`, GHDL's convention: integer, enum
and real scalars and arrays, no signal parameters). siox functions could be
called that way once siox can export a C symbol, which it cannot today
(`extern "C"` only imports); there is no plan for it.

## File I/O

Testbenches can read fixtures from disk with `read<T>` and `exists` (in
`std::fs`). Paths resolve **relative to the source file**, so a test
and its data travel together:

```siox
let rom: unsigned[16][256] = read<unsigned[16]>("rom.bin");
let raw: integer = read<integer>("word.bin");
let banner: string = read<string>("banner.txt");
```

In a native `--test` binary these reads happen when the generated executable
runs. `read<string>` decodes UTF-8. `read<integer>` reads raw little-endian
binary, and `read<T>` for a packed numeric type applies the ordinary
`T(integer)` construction to each value. Each numeric value consumes
`ceil(T'length / 8)` bytes. A fixed target keeps its declared length (short
files zero-fill; long files fail), while an unconstrained `string` owns a
dynamic Unicode code-point buffer. Missing files, invalid UTF-8, and capacity
failures fail the test with the operation and path. Hardware/top initializers
remain different by design: the same call is a compile-time ROM image baked
into the object.

A testbench path need not be a literal. Any string works: a string local, an
element of a string array, or a string read from another fixture. It is
resolved against the source file's directory when the test runs, exactly as a
literal is when the test is compiled:

```siox
let names: Char[8][2] = ["word.bin", "wore.bin"];
let total: integer = 0;
for fixture in names { total = total + read<integer>(fixture); }
let listed: string = read<string>("list.txt");   // holds another fixture's name
let value: integer = read<integer>(listed);
```

A hardware/top read happens at compile time, so its path stays a literal.

A testbench can also read a file of unknown length into a **runtime-sized
array**, `T[]`, for `integer` or a packed numeric of any width:

```siox
let samples: unsigned[16][] = read<unsigned[16][]>("stimulus.bin");
for sample in samples { din = sample; await 10ns; }
assert!(samples'length > 0, "stimulus present");
```

Each element consumes `ceil(T'length / 8)` bytes, little-endian. A trailing
partial element fails the test, as does an index past `'length`. Elements are
written in place (`samples[i] = v`, `sx_runtime_array_store`). Assigning the
array copies it (`sx_runtime_array_copy`), while a `T[]` parameter writes
through to the caller's array as any parameter does. `samples.push(v)` appends
(`sx_runtime_array_push`, doubling its allocation); an unset `T[]` is the empty
array, handle zero. A character enum element (`Logic`, `Bit`) comes from text
(`sx_runtime_read_symbols`): the runtime stores each symbol's position in the
enum's symbol list, and the compiler maps positions to the enum's values. It
lives in the runtime's
string table, so a `string` and a `T[]` are the same kind of handle.

## Editor support (`siox-lsp`)

The separate [`siox-lsp`](https://github.com/Siox-lang/siox-lsp) repository
speaks LSP over stdin/stdout and references the compiler core through Cargo Git:

```bash
git clone git@github.com:Siox-lang/siox-lsp.git
cargo build --manifest-path siox-lsp/Cargo.toml
siox-lsp/target/debug/siox-lsp --stdio --std /path/to/sioxc/std
```

Point your editor at that command for the `siox` language; `--std <dir>` locates
the standard library (default `./std`). It provides:

- Live lexer / parser / name-resolution / type-check diagnostics.
- Definition and type-definition navigation, references, highlights, safe rename.
- Hover, contextual completion, signature help, parameter hints.
- Semantic tokens, document/workspace symbols, folding and selection ranges.
- Quick fixes (suggested names, removable unused imports) and std import links.
- Canonical whole-document formatting for comment-free source.

**Limitations:** formatting returns no edit when comments are present (the
canonical printer does not yet retain comment trivia, so it declines rather than
delete them); cross-file user-module analysis follows the compiler's current
single-entry-file limitation (std modules load transitively).

### Compiler embedding API

Editors and future project tools should compile through `siox::compiler`, not
call the individual passes themselves. A frontend-only consumer can avoid the
LLVM dependency:

```toml
[dependencies]
siox = { git = "https://github.com/Siox-lang/sioxc", default-features = false }
```

Then compile a disk file or an unsaved editor buffer through the same request
boundary:

```rust
use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

let compiler = Compiler::new("/path/to/sioxc/std");
let request = CompileRequest::new(
    SourceInput::memory("/workspace/design.siox", current_editor_text),
    Emit::Metadata,
);
let compilation = compiler.compile(request);

for diagnostic in compilation.diagnostics() {
    // Map diagnostic.primary through compilation.sources into an LSP range.
}
```

`SourceInput::Path` reads a disk file; `SourceInput::Memory` preserves the
document path while using unsaved text. A `Compilation` retains the source map,
entry tokens and modules, and each completed `Resolved`, `Typed`, `Hierarchy`,
and `Design` product. Language errors are structured diagnostics and do not
discard earlier products. Host failures such as an unreadable file, ambiguous
top selection, invalid codegen IR, or unavailable backend are represented by
`CompileFailure`. The API writes no terminal output and never executes an
artifact, so a host remains responsible for presentation, caching, process
execution, and project discovery.

`Compilation::macro_expansions` lists every macro call the compilation
expanded, in expansion order, for editor views that tell generated code from
handwritten source. Each `MacroExpansion` names the macro (`demo::twice`), the
span of the invoked form's declaration, the span of the whole `name!(..)` call,
and the expansion as source text, before any macro call it contains is itself
expanded. A call written inside a macro body is expanded too: its `parent` is
the index of the expansion it came from, and `depth` counts the expansions
around it (0 for a call in handwritten source), so a view can walk a nested
expansion back to the call the user wrote. An argument a body substitutes twice
is expanded once per copy.

`Emit::Metadata`, `Source`, `Tokens`, `Ast`, `Tree`, and `Ir` work in the
frontend-only build. `Emit::LlvmIr`, `Object`, and `TestExecutable` return a
backend-unavailable `CompileFailure` unless the `llvm` feature is enabled.
File artifacts require an output path when the host wants to control their
location; `CompileRequest::with_output` sets it. The compiler writes the
artifact but still never launches it.

`Emit::Source` returns comment-preserving, whitespace-formatted entry source
as `Artifact::Text`. Retained lexer spans anchor the original token text;
comments, literals, macro tokens and trailing commas are not rewritten or
discarded. Formatting is idempotent. This is separate from expanded AST
pretty-printing, and does not by itself change the LSP's comment-free-only
formatting policy described above.

`CompileRequest::with_lint_levels` takes the same levels as `sioxc -A/-W/-D/-F`,
in order (`vec![(Level::Deny, "warnings".into())]` is `-D warnings`). Lint
levels are applied as the diagnostics are produced: an allowed lint never
appears in `compilation.diagnostics()`, and a denied one arrives as an error.
Each diagnostic now also carries `notes` (rendered as `= note:` lines), which
is where a lint says what set its level.

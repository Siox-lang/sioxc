# Interoperability

How siox talks to the outside world: foreign C functions, file I/O, editors, and
the planned cocotb integration.

## Foreign functions (`extern "C"`)

A design or testbench may call C functions declared `extern "C"`:

```siox
extern "C" fn sin(x: real) -> real;
```

Only the `"C"` ABI is supported. Its current contract is deliberately scalar:

- `real` maps to C `double`.
- `integer`, including aliases and constrained forms, maps to a signed 64-bit
  ABI word.
- A packed numeric type from 1 through 64 bits maps to an unsigned 64-bit ABI
  word; the result is converted to its declared storage width.

Every declaration must have a scalar return value. Generic declarations,
statement-only/void calls, structs, arrays, characters, and packed values wider
than 64 bits are rejected during type checking because the backends do not yet
have C layout or multiword-call rules for them. Wrap such an API in a C adapter
that accepts and returns the scalar types above. Calls are usable in
combinational expressions, clocked logic, and testbenches.

In the **native binary**, symbols resolve at link time. The math library is
linked by default, so the `std::math` surface — `sin`, `sqrt`, … — works out of
the box. Other symbols may be resolved by a consumer linking a compiler-emitted
object; the generated native test executable does not yet accept custom library
search paths or `-l` options.

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

## cocotb (planned)

Because `await`'s trigger model *is* cocotb's trigger model, the runtime's
existing scheduler is the surface a cocotb driver needs — but it isn't yet
exposed as a foreign, callback-driven ABI. Driving a compiled siox design from
cocotb (Python, over a VPI/GPI-shaped ABI) is designed but unimplemented; it
would be its own layer (`siox-vpi`), not core-compiler work. The ABI design —
name→handle lookup, get/put/force/release, and the five GPI callback kinds
mapped onto the event wheel — is in
[proposals/timing-and-await.md](proposals/timing-and-await.md).

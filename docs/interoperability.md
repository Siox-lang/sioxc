# Interoperability

How siox talks to the outside world: foreign C functions, file I/O, editors, and
the planned cocotb integration.

## Foreign functions (`extern "C"`)

A design or testbench may call C functions declared `extern "C"`:

```siox
extern "C" fn sin(x: real) -> real;
```

Only the `"C"` ABI is supported. Type mapping: `real` is `double`; `integer` and
the word-sized numeric types are 64-bit words. Calls are usable in hardware
expressions and in testbenches.

- On the **JIT**, symbols resolve from the running process.
- In the **native binary**, they resolve at link time (the math library is
  linked by default, so the `std::math` surface — `sin`, `sqrt`, … — works out
  of the box).

## File I/O

Testbenches can read fixtures from disk with `read`, `read_to_string`, and
`exists` (in `std::fs`). Paths resolve **relative to the source file**, so a test
and its data travel together:

```siox
let rom: uint[8][256] = read("rom.bin");     // sized by the file
let banner: string = read_to_string("banner.txt");
```

In the native `--no-run` binary these reads currently happen at **build time**
and are baked into the binary (fine for stable fixtures); a genuine runtime
`fopen`/`fread` is a possible follow-up.

## Editor support (`siox-lsp`)

siox ships a language server, `siox-lsp`, speaking LSP over stdin/stdout, so any
LSP-capable editor can use it:

```bash
cargo build --bin siox-lsp
target/debug/siox-lsp --stdio --std ./std
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

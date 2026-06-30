# Architecture

The siox compiler is a Cargo workspace arranged as **one strict top-to-bottom
pipeline**. Each crate consumes the output of the crate above it and produces
the input to the crate below. The only crate everything may depend on is
`siox-diag`.

```mermaid
flowchart LR
    subgraph pipeline [compiler pipeline]
        direction LR
        SY[siox-syntax] --> RE[siox-resolve] --> TY[siox-types] --> EL[siox-elab] --> IR[siox-ir] --> SI[siox-sim] --> WA[siox-wave]
    end
    DIAG[siox-diag] -. used by all .-> pipeline
    CLI[siox-cli] == drives ==> pipeline
```

**Layering rule:** a crate may depend only on the crates above it in this list
(plus `siox-diag`). Do not introduce upward or sideways dependencies.

## Crates

| Crate | Spec stage(s) | Role |
| ----- | ------------- | ---- |
| `siox-diag`    | 10   | Foundation: `Span`, `SourceMap`, `Diagnostic`, `DiagnosticSink`, and the stable error/warning code catalogue (`codes`). |
| `siox-syntax`  | 1–2  | Lexer, tokens, AST, recursive-descent + Pratt parser, pretty-printer. `parse_module` is the entry point. |
| `siox-resolve` | 3    | Name resolution: top-level definitions and `DefId`s, `using` imports/aliases, `::` paths, enum-associated items, attribute names. Produces `Resolved` (definition table + use-site → `DefId` map). |
| `siox-types`   | 4    | Type and kind checking; a light type-inference core (annotation → `Ty`, per-impl symbol table, `type_of`); rejects Phase-2 syntax (`::ddt`). Produces `Typed`. |
| `siox-elab`    | 5    | Elaboration: const-evaluate parameters, build the instance hierarchy from `#[top]`/`#[test]` roots, resolve port connections, expand bus modes. Produces `Hierarchy`. |
| `siox-ir`      | 6    | Lowers to digital simulation IR: combinational `Driver`s vs. sequential `EventBlock`s; `::event`/`::old` become first-class IR ops. Produces `Design`. |
| `siox-sim`     | 7–8  | Event-driven delta-cycle `Simulator`; `#[test]` discovery, stimulus, assertions. |
| `siox-wave`    | 9    | `Trace` recording + VCD export (FST later). |
| `siox-cli`     | 12   | The `siox` binary; runs the pipeline up to the stage each subcommand needs and renders diagnostics. |

Each crate's `lib.rs` opens with a doc-comment summarising its responsibility
and the spec acceptance criteria — read it first when entering a crate.

## Data that flows between stages

```mermaid
flowchart TD
    A["&str (source)"] -->|siox-syntax| B["ast::Module"]
    B -->|siox-resolve| C["Resolved<br/>defs + use-site → DefId"]
    C -->|siox-types| D["Typed<br/>expression / signal types"]
    D -->|siox-elab| E["Hierarchy<br/>instances + connections"]
    E -->|siox-ir| F["Design<br/>signals, drivers, event blocks"]
    F -->|siox-sim| G["values / TestResults"]
    G -->|siox-wave| H["VCD"]
```

`siox-diag::Span` (a byte range plus `FileId`) is attached to AST nodes and most
later-stage data, and is used both for diagnostics and as the key that links a
name-use site to the declaration it resolves to.

## Cross-cutting conventions

- **Spans everywhere.** Every AST node — and most later-stage data — carries a
  `siox_diag::Span`. New node/data types should too; diagnostics depend on it.

- **Diagnostics flow through `DiagnosticSink`.** Stages take `&mut
  DiagnosticSink`, `emit` into it, and the CLI renders/counts at the end. Use
  the stable codes in `siox_diag::codes` (e.g. `WRITE_TO_INPUT_PORT`); add new
  codes to that catalogue rather than scattering string literals.

- **Best-effort, keep going.** A stage returns a usable result even on error
  (e.g. `parse_module` returns a partial AST, the parser guarantees forward
  progress, resolve/types never bail on the first error) so later stages still
  run and surface more diagnostics in one pass.

- **No false positives over completeness.** Where a stage cannot yet decide
  something soundly (e.g. value identifiers before full scoping, or widths
  before elaboration), it stays silent rather than emitting a wrong error. The
  strict checks are the ones that are correct today.

- **The IR distinction is central.** Combinational `Driver(target, cond, expr)`
  and sequential `OnEvent(cond): next(target) = expr` are kept separate; e.g.
  `clk::rising` lowers to `Event(clk) && Old(clk)=='0' && Current(clk)=='1'`.
  Preserve this split when working in `siox-ir`/`siox-sim`.

- **Reject Phase-2 syntax, don't implement it.** Analogue constructs (`domain`,
  `across`/`through`, `::ddt`, layout attrs) must produce errors
  (`codes::PHASE2_SYNTAX`), not silent acceptance.

## Builtins while `std/` is empty

The standard library (`std/`) is a placeholder. Until it is filled in, the
primitive types (`Bit`, `Logic`, `Bool`, `Clock`, `uint`, `int`, ...) and the
`std::attrs` attributes (`top`, `test`, `keep`, ...) are **seeded as builtins**
inside `siox-resolve` and `siox-types`, so the example programs resolve and
type-check without the real library. When `std/` lands (Stage 11), these move
out of the compiler.

## The CLI as the pipeline driver

`siox-cli` is where the stages are composed. It loads a file into a
`SourceMap`, runs the stages a subcommand needs on a shared `DiagnosticSink`,
narrates each stage to stderr (more with `-v`), prints the requested artifact to
stdout, and exits non-zero if any errors were reported. This makes the CLI the
practical place to watch data move through the compiler — see the `tokens`,
`parse -v`, `check`, and `tree` commands.

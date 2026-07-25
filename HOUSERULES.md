# House rules

Conventions for anyone working on this repo — human or agent. These are the
rules that have actually cost us time when broken; the reasoning behind each is
one line, so you can tell when an exception is justified.

Companion files: [`chat.md`](chat.md) is the *chronological* coordination log
(who is touching what, hand-offs). This file is the *durable* rules.
[`docs/README.md`](docs/README.md) is the documentation set;
[`docs/architecture.md`](docs/architecture.md) has the pipeline and crate
layout. Agent-specific setup files (e.g. `CLAUDE.md`) are gitignored and local.

---

## 1. The design principle

**The compiler names types; std owns values.**

Anything expressible in the language belongs in `std/*.siox` as siox source, not
baked into Rust. The compiler may reference a type *name* (`"ULogic"`,
`"Ordering"`, `"Bool"`) but must not hardcode its *values*, discriminants, or
truth tables — read those from the enum/impl tables at elaboration.

Why: it keeps the language self-describing, lets the runtime and external tools
(VCD writers, waveform viewers) stay compiler-independent, and means a user type
gets the same capabilities as a std one.

Only these stay in the compiler: static analysis, elaboration-time metadata,
codegen, and primitives with no source-level expression yet (radix expansion,
`'event`/`'old`, scheduler hooks). If you find yourself writing a `match` over
logic values or a `0/1/2` discriminant literal in Rust, stop — source it from
std instead.

## 2. Pipeline layering

A module may use only the modules **above** it, plus `diag` which everything
uses:

`diag` → `syntax` → `resolve` → `types` → `elab` → `ir` → `run` → `wave`

Inside the `siox` crate this is a convention; across crates it is enforced
(`siox-llvm` → `siox`, never the reverse). No upward or sideways `use`.

## 3. Diagnostics

- **Spans everywhere.** Every AST node and most later-stage data carries a
  `diag::Span`. New node types should too — diagnostics depend on it.
- **Through `DiagnosticSink`.** Stages take `&mut DiagnosticSink`, `emit` into
  it; the CLI renders and counts at the end.
- **Stable codes.** Use `diag::codes` (`E-P003`, `W-P013`, …) and add new codes
  to that catalogue — never scatter bare string literals.
- **Best-effort, keep going.** A stage returns a usable result even on error
  (`parse_module` returns a partial AST) so later stages surface more problems
  in one pass. Don't bail on the first error.
- **No false positives over completeness.** If a stage can't decide something
  soundly yet, stay silent rather than emit a wrong error.
- **Suppress cascades.** After reporting a bad construct, return `Ty::Error` (or
  equivalent) so one mistake yields one diagnostic, not three.

## 4. Testing gate

Before you commit, all three must be green:

```bash
cargo test --workspace                       # unit + integration
cargo run -q -p sioxc -- test /home/max/siox-tests    # the .siox corpus
```

Anything touching lowering or codegen must be checked on **both** engines — the
JIT and the native AOT path (`sioxc test <file> --no-run --out <bin>`, then run
the binary). They diverge; a JIT-only check has missed real bugs.

**The corpus lives in a sibling repo** (`Siox-lang/siox-tests`, checked out at
`/home/max/siox-tests`). Two consequences:

- New `.siox` example programs go **there**, not in this repo.
- **Use that checkout — don't clone a fresh copy** into a scratch dir. Other
  agents leave uncommitted corpus work there, so a clone silently tests stale
  files and invents phantom regressions.

## 5. Surface-syntax changes are breaking

A syntax change isn't done when the parser accepts it. In the same change, also
migrate:

1. `std/*.siox` (the library is written in the language),
2. the corpus in `/home/max/siox-tests`,
3. embedded siox snippets in Rust tests (they're string literals — grep, the
   compiler won't find them for you),
4. `docs/language.md` — the authority for syntax — and any other affected doc.

Distinguish **surface syntax** from **internal encoding**: e.g. attributes are
written `x'length`, but the IR's inlining environment keys are still
`"self::length"` strings built from the AST. Internal keys are an implementation
detail — don't "migrate" them.

## 6. Language vocabulary

- **Three accessors, one job each.** `.` values (fields, methods) · `::` types
  and modules (paths, enum variants, associated items, views) · `'` attributes
  (`sig'event`, `x'length`). Don't overload one to do another's job.
- **One operator trait.** Every operator is
  `impl Operator<"sym", Input, Output>` with a single `apply`. Standard symbols
  carry built-in precedence; any other symbol is a user operator and must
  declare `#[precedence = N]`.
- **The grammar's own symbols are reserved** (`=`, `::`, `.`, `..`, `->`, `=>`,
  brackets, the six comparisons) — they cannot be overloaded.
- **Phase 2 is rejected, not implemented.** Analogue constructs (`domain`,
  `across`/`through`, `'ddt`, layout attrs) must produce
  `codes::PHASE2_SYNTAX`, never silent acceptance.

## 7. Working alongside other agents

- **Announce shared-file edits in `chat.md`** before starting (parsers, `std/*`,
  `docs/language.md`, the IR lowerer). Append; never edit another agent's entry.
- **Don't clobber uncommitted work.** Check `git status` before large edits —
  another agent's feature may be in flight in the same file.
- **When two agents' changes are intermingled** and can't be split per-file, the
  owner's call is a **single combined, co-authored commit**.
- **Never sweep another agent's work under your commit message.** If it happens
  anyway, say so in `chat.md` and fix the message while the commit is local.

## 8. Commits

- The message describes what the commit **actually contains** — verify with
  `git show --stat` before pushing, especially after a combined commit.
- **Amend freely while local**, never after pushing.
- **Push is a separate, explicit decision.** Don't push another agent's
  unreviewed work.
- Commit trailers: co-author and session link, per the owner's setup.

## 9. Dependencies

Third-party crates are kept minimal on purpose. Add one only when a stage
genuinely needs it, and say why in the commit message. Toolchain: edition 2021,
`rust-version = 1.90`.

# Compiler foundations borrowed from rustc

Status: **proposal**. Nothing here is implemented. Five independent changes
that bring sioxc's internals closer to rustc where rustc has already solved a
problem sioxc keeps meeting. Each part lands on its own and leaves the
compiler working; the order below is the recommended one.

siox is deliberately aligned with Rust at the language level. This proposal
applies the same habit to the compiler: when sioxc has a structural problem,
check how rustc is built before inventing a new shape. It stops where hardware
starts. Elaboration, the instance hierarchy and the simulation runtime have no
rustc counterpart and are not touched here.

Not proposed: a query system, incremental compilation, or splitting the crate.
rustc needs those at its scale; sioxc compiles whole programs in milliseconds
and its layering rule (each module uses only those above it) already does the
job crate boundaries do for rustc.

## 1. UI tests for diagnostics

**Problem.** The corpus checks only that a program builds and its tests pass.
Diagnostics are covered by Rust unit tests asserting on message fragments, one
hand-written test per case. A change that rewords, re-spans or duplicates an
error passes every corpus run. Two such bugs were fixed in one recent session
alone: a failed attribute read that cascaded into an unrelated "unknown system
attribute" error, and a module-name mismatch that stopped the build as a
parse failure.

**rustc.** `tests/ui/` holds a `.rs` file next to the `.stderr` it must
produce. `compiletest` compiles it, normalizes paths, and diffs; `--bless`
rewrites the snapshot after an intended change. Inline `//~ ERROR` annotations
pin the important lines so a snapshot cannot drift silently.

**Proposal.**

- A `ui/` directory in `siox-tests`. Each `name.siox` is compiled with
  `sioxc --emit ir` (or `--test` when it names `// test`), and its rendered
  diagnostics are compared to `name.stderr`. Paths are normalized to the file
  name; the exit status is part of the snapshot.
- Inline annotations, rustc's spelling: `//~ ERROR E-P014` on the line a
  diagnostic must point at (`//~^` for the line above). A diagnostic with no
  annotation, or an annotation with no diagnostic, fails the test even if the
  snapshot matches.
- `scripts/test-ui.sh <corpus>` runs them; `--bless` rewrites snapshots. CI and
  `ci-local.sh` run it after the corpus step.
- Seed it with the negative probes this year's bug hunts produced, which today
  live only in commit messages and scratch directories.

**Acceptance.** Every error and warning code has at least one UI test; a
deliberately reworded message fails CI until blessed.

## 2. Diagnostics: `--explain`, JSON, structured suggestions

**Problem.** Codes are stable but explain nothing beyond the one-line message.
Output is human text only, so the language server and editors re-parse it.
Many `help:` lines are an exact replacement (``write `update: process { … }` ``,
``inside the impl, write `attr precedence = 40;` ``), but only a human can apply
them.

**rustc.** `rustc --explain E0453` prints a long-form page from a catalogue
compiled into the binary. `--error-format=json` emits one JSON object per
diagnostic with spans, children and suggestions. A suggestion carries a span, a
replacement and an *applicability* (`MachineApplicable`, `MaybeIncorrect`, …),
which `cargo fix` applies.

**Proposal.**

- `docs/diagnostics/E-P014.md` etc., one page per code: what it means, a
  failing example, the fix. `sioxc --explain E-P014` prints it (the pages are
  `include_str!`-ed, so the binary is self-contained). A test fails when a code
  in `diag::codes` has no page.
- `--error-format=json`: one object per diagnostic with `code`, `severity`,
  `message`, primary and labelled spans (file, byte range, line/column),
  `notes`, `help`, and suggestions.
- `Diagnostic::suggest(span, replacement, Applicability)` beside `.help()`.
  Migration diagnostics are the first users, since their help text is already
  a replacement.
- Later, `sioxc --fix file.siox` applies the machine-applicable suggestions in
  place. That turns every future syntax migration into one command, where
  today it is `--emit source` and hoping the printer loses nothing.

**Acceptance.** Every code has an explanation; JSON output round-trips through
a schema test; the label and attribute migrations offer machine-applicable
fixes.

## 3. One constant evaluator

**Problem.** Constant folding is written nine times:

| where | function |
| --- | --- |
| `types/calls.rs` | `const_fold` |
| `types/indexing.rs` | `const_literal` |
| `elab.rs` | `eval` |
| `ir/lower_helpers/builders.rs` | `eval_const`, `eval_const_fns`, `eval_const_stmts` |
| `ir/lower_helpers/substitute.rs` | `fold_const` |
| `ir/lower/collect.rs` | `fold_const`, `eval_const` |

They have different reach. Only `eval_const_fns` calls const functions; the
two in `types` see no generic or loop bindings at all, while `elab`'s `eval`
and the IR's `eval_const` take an environment. So the same expression can be a
constant in one stage and not in the next, and each fix lands in one copy.

**rustc.** One evaluator, the MIR interpreter (the core of Miri), answers every
constant question: array lengths, const generics, `const` items, and
`const fn` calls.

**Proposal.**

- A `consteval` module in `types` (the highest stage all three users can
  reach), with one entry point: evaluate an expression in an environment of
  generic and loop bindings, with access to const functions and declared
  constants, producing a typed value or a reason it is not constant.
- It owns siox's rules once: inclusive directional ranges, the wrapping width
  semantics, `'length`/`'high`/… on statically sized values, const-function
  calls with a recursion bound.
- Migrate callers one at a time. Before deleting each old folder, run both
  over the corpus and the Rust test sources and require identical answers
  wherever the old one answered.

**Acceptance.** The nine folders are gone; `E-P021` ("not a constant") and
range errors come from one place and agree across stages.

## 4. A resolved tree between `types` and `elab`

**Problem.** Resolution produces `DefId`s, but every later stage walks the
original AST and re-derives what resolution already decided, often by leaf
name. The type checker's registries are keyed by `String` (`entities`,
`attr_targets`, `attr_value_kinds`, `trait_impls`, `trait_impls_by_type`, …),
so two attributes declared with one name in different modules share a single
entry. Elaboration and IR lowering each re-find declarations, instance names
and generate structure; that is how they came to name generated instances
differently until both were moved onto one function.

**rustc.** The AST is lowered once into **HIR**: every path is replaced by the
`DefId` or local it resolved to, desugaring is done, and nothing after that
point resolves a name again. Typeck writes its results to side tables keyed by
`HirId`.

**Proposal.** A full HIR rewrite is not needed. Instead:

- **No name lookups after `resolve`.** Every registry in `types`, `elab` and
  `ir` is keyed by `DefId` (or a `(DefId, member)` pair), not by a leaf
  string. `Resolved::uses` (span → `DefId`) already holds the answer; later
  stages read it instead of matching text.
- **One typed side table.** `Typed` records each expression's type by span, so
  `elab` and `ir` stop recomputing operand types.
- **One shared walk for generated structure.** `elab::GenPath` (from the
  labels work) is the model: one function decides a generated name, and both
  stages call it.
- An `xtask`-style check, or a clippy lint, flags `HashMap<String, …>`
  registries outside `resolve` so the rule stays enforced.

**Acceptance.** No stage after `resolve` looks a declaration up by its leaf
name; equal leaf names in different modules are covered by a test per
registry.

## 5. Lang items: std marks what the compiler hooks

**Problem.** The compiler depends on std declarations it finds by path and
name. `COMPILER_TRAITS` lists `Operator`, `Prefix`, `Suffix`, …, and
`is_compiler_trait` checks that the definition sits in `std::ops` (or
`std::logic` for `LogicEncoding`). `#[test]` is recognised by checking for a
`test` declared in `std::attrs`. Builtin fallbacks are seeded by the same
names. The built-in macros are worse: `assert!`, `print!` and `warn!` are
matched by string in `types/calls.rs` and again in
`ir/lower/source_processes.rs`, and declared nowhere.

**rustc.** `#[lang = "add"]` on `core::ops::Add` tells the compiler "this is
the addition hook"; the compiler looks up lang items, never paths. Built-in
macros are declared in `core` with `#[rustc_builtin_macro]`, so `assert!` is
an ordinary importable item whose expansion the compiler supplies.

**Proposal.**

- A std-only metadata attribute, `attr lang: string for trait, enum, struct,
  attr = "";`, bound in std: `attr lang for Operator = "operator";`,
  `attr lang for test = "test";`. Resolution builds a `lang → DefId` table,
  and the compiler asks for `lang("operator")` instead of matching
  `std::ops::Operator`. A user module cannot bind `lang` (it is reserved to
  std), and two items claiming one lang name is an error.
- Builtin fallbacks for std-less compilations seed the same table, so there is
  one lookup either way.
- The built-in macros follow once [macros.md](macros.md) lands:
  `pub macro assert(...) { builtin # assert(...) }` in std, which that
  proposal already sketches. Until then they stay as they are.

This needs module-level bindings to name traits, enums and attributes, not
only entities. That is a small extension of the binding pass.

**Acceptance.** No compiler code names a std path or a std declaration's
spelling; renaming `std::ops::Operator` in std and its `lang` binding together
compiles and passes.

## Order

1. **UI tests** first. They are the safety net for everything after, and they
   need no compiler change.
2. **Diagnostics** (`--explain`, JSON). Small and independent. Suggestions
   come with the next syntax migration that needs them.
3. **One constant evaluator.** Medium-sized, fully covered by UI tests and the
   corpus once (1) exists.
4. **The resolved tree.** The largest part, done registry by registry; it
   shares files with Codex's IR work, so it should follow the IR inversion.
5. **Lang items.** Small once (4) has removed path matching, and it is the
   natural home for the macro builtins later.

## Open questions

- Should UI snapshots live in `siox-tests` (with the corpus) or in this repo
  (with the compiler that produces them)? rustc keeps them with the compiler.
  sioxc keeps programs in `siox-tests`, but diagnostics are compiler output,
  not program behaviour.
- Does `--fix` belong in `sioxc`, or in the future project tool, the way
  `cargo fix` wraps `rustc`?
- Is `lang` a metadata attribute, or a directive? It changes what the compiler
  does, which argues for a directive (`#[lang = "operator"]`). But
  `#[...]` takes no `=` values and std-only directives are new.

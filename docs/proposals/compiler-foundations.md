# Compiler foundations borrowed from rustc

Status: **partially implemented**. Core loading, lang-item registration and
library-declared built-in macros have landed. Diagnostic UI snapshots now have
a runner and corpus fixtures; `--explain CODE` has embedded reference pages.
See sections 1–2 for verification and usage.
Structured diagnostics, shared constant evaluation, declaration-ID registries
and residual name-based hook cleanup remain. Each part lands independently.

Local integration acceptance (2026-10-22): both complete Rust suites, all
23 DWARF tests per mode, strict frontend/all-target Clippy, both 255-program
corpora and both 59-case UI suites pass. Fourteen ordinary/debug native VCD
comparisons match. Full retained evidence is recorded in [chat.md](../../chat.md).

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

**Implementation (2026-10-22).**

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
- The initial 59 cases cover all 54 active error/warning codes, plus codeless
  parser errors, warnings promoted to errors, primary/related spans, notes,
  help and native test compilation. Auxiliary modules live in `ui/auxiliary/`
  and are imported by cases rather than treated as independent tests.

The Bash entry point delegates to a dependency-free Python runner, following
the existing artifact/VCD checkers. It builds the selected compiler before
running cases and honors `SIOXC_FEATURES=bitpack`. Cases run the executable
reported by Cargo, including custom target directories; Cargo is not rerun
from fixture directories. `--compiler /path/to/sioxc`
selects an already-built binary explicitly; the caller owns its freshness.
Only stage-progress lines and checkout/temporary paths are normalized; messages,
severity, line/column, snippets, labels, notes, help and exit status are retained.

An annotation may omit its code for a codeless parser error. Multiple carets in
`//~^^ ERROR E-P003` point two lines upward. Use an additional annotation line
when one source line produces several diagnostics. Every error/warning must
match an annotation exactly once. `--bless` validates annotations and the
expected exit status first; a crash, timeout, extra diagnostic or wrong primary
line cannot be blessed. A missing snapshot or empty suite fails normally.

```bash
bash scripts/test-ui.sh /path/to/siox-tests
SIOXC_FEATURES=bitpack bash scripts/test-ui.sh /path/to/siox-tests
bash scripts/test-ui.sh /path/to/siox-tests --bless
python3 -m unittest discover -s scripts -p test_ui_test.py -v
```

On the shared development host, run builds and compiler tests through
`bash scripts/guarded-run.sh COMMAND ...` as required by HOUSERULES.md. Review
snapshot diffs after blessing; passing annotations do not approve a wording
change automatically. UI tests complement runnable simulations, not replace them.

**Coverage gate.** The runner reads `diag::codes` and requires every active
error/warning code to appear in a passing UI case. Declarations documented as
`Retired:` (currently the reserved scheduler code) are excluded, not silently
reused. A new code therefore needs a new fixture. A deliberately reworded
message fails comparison until blessed; a runner self-test verifies this.

## 2. Diagnostics: `--explain`, JSON, structured suggestions

**Explanations implemented (2026-10-22); JSON and suggestions remain.**
`sioxc --explain CODE` prints an embedded page without a source file, std tree
or network. All declared codes, including the reserved retired scheduler code,
have [reference pages](../diagnostics/README.md). Their lookup and the public
`diag::codes::ALL` list come from the existing code declarations; a missing page
fails compilation. Library tests check page identity and required sections, and
CLI tests check every page, unknown codes, and source/explanation exclusivity.
This does not change diagnostic rendering or language semantics.

The catalogue follows rustc's documented
[error-code registration model](https://rustc-dev-guide.rust-lang.org/diagnostics/error-codes.html):
one registered identity requires its Markdown explanation. Siox retains its
existing named constants and generates the lookup from those declarations,
instead of maintaining a second list that can drift.

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

- Implemented: `docs/diagnostics/E-P014.md` etc., one embedded page per code:
  what it means, an example and the fix. The binary is self-contained.
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

## 5. Remove residual name-based hook lookups

The lang-item mechanism is implemented: `Resolved::lang` maps roles to `DefId`,
`core` is embedded by the compiler, and `core`/`std` declare hook roles through
`attr lang`. Built-in macros live in `core::macros` over `builtin #`.
The current library surface is in [std.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/std.md); macro semantics are in
[language.md](https://github.com/Siox-lang/siox-paper/blob/main/docs/language.md), §3.30. These are no longer proposals.

**Remaining problem.** Type classification still has leaf-name dispatch such
as `types/keys.rs::ty_from_head`, and frontend-only fixtures retain fallback
identities. Do not confuse a registered hook table with complete removal of
name matching.

**Proposal.** Audit classification and hook consumers, replacing library-type
spelling checks with resolved identities, lang roles or source-owned layout/
encoding metadata as appropriate. Keep the grammar's kernel types distinct
from ordinary library declarations. Remove fixture fallbacks only when their
callers load core or explicitly supply equivalent identities.

`#[test]` and lint directives are compiler built-ins, not importable declarations;
do not recreate `std::attrs::test` or make test discovery depend on metadata.

**Acceptance.** Renaming a library hook and retaining its lang role preserves
behavior; unrelated same-leaf-name user types cannot acquire its compiler role.

## Order

1. **UI tests** first: implemented runner, full active-code fixtures and CI/local
   wiring. Preserve the coverage gate as diagnostics evolve.
2. **Diagnostics**: `--explain` is implemented; JSON and structured suggestions
   remain. Suggestions come with the next syntax migration that needs them.
3. **One constant evaluator.** Medium-sized, fully covered by UI tests and the
   corpus once (1) exists.
4. **The resolved tree.** The largest part, done registry by registry; it
   shares files with Codex's IR work, so it should follow the IR inversion.
5. **Residual hook cleanup.** Use the existing lang table and resolved identities;
   do not reimplement core loading or macro declarations.

## Decisions and open questions

- UI programs and snapshots live in `siox-tests/ui/`, consistent with the house
  rule for source programs. The runner and its self-tests live with the compiler.
- Does `--fix` belong in `sioxc`, or in the future project tool, the way
  `cargo fix` wraps `rustc`?

# Compiler foundations borrowed from rustc

Status: **partially implemented**. Core loading, lang-item registration and
library-declared built-in macros have landed. Diagnostic UI snapshots now have
a runner and corpus fixtures; `--explain CODE` has embedded reference pages.
See sections 1–2 for verification and usage. JSON diagnostics and atomic
migration suggestions are implemented. Scalar traversal and the first resolved
declaration registry are shared across stages; fully typed constant evaluation,
the remaining declaration-ID registries and residual name-based hook cleanup
remain. Each part lands independently.

Local acceptance (2026-10-23, declaration-index baseline): both complete Rust
suites, all 23 DWARF tests per mode, 541 frontend-only tests, strict Clippy and
both 257-program corpora
pass. Both modes pass 61 human UI and 61 JSON/schema cases, cover all 54 active
codes and apply/recompile both migration fixtures. The earlier compiler-fix
integration was followed by 20 ordinary/debug native runs and VCD checks. Retained
evidence and the fixture-only correction are recorded in [chat.md](../../chat.md).

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

**Explanations, JSON and initial structured suggestions implemented (2026-10-22).**
`sioxc --explain CODE` prints an embedded page without a source file, std tree
or network. All declared codes, including the reserved retired scheduler code,
have [reference pages](../diagnostics/README.md). Their lookup and the public
`diag::codes::ALL` list come from the existing code declarations; a missing page
fails compilation. Library tests check page identity and required sections, and
CLI tests check every page, unknown codes, and source/explanation exclusivity.
This does not change diagnostic rendering or language semantics.

`--error-format=json` now exposes the existing sink through newline-delimited
JSON on stderr, including labels, notes/help, source byte ranges and Unicode
scalar columns. It suppresses plain progress/status messages without changing
the pipeline or artifact production. Input/selection/backend failures and
early compiler-option errors are structured too; Clap usage/help remain text.
See the [wire format and consumer contract](../diagnostics/JSON.md).

`Diagnostic::suggest` and `suggest_edits` carry applicability-tagged, atomic
fixes. Label and metadata migrations are the first producers; moving metadata
inside an impl is one multipart group, not independently applicable deletion
and insertion suggestions. Human help is unchanged. Dedicated corpus fixtures
apply the emitted fixes and recompile; recovered/comment-bearing metadata is
not automatically applicable. JSON/schema tests exercise every active code.

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
- Implemented: `--error-format=json`, one object per diagnostic with `code`, `severity`,
  `message`, primary and labelled spans (file, byte range, line/column),
  `notes`, `help`, and suggestions.
- Implemented: `Diagnostic::suggest(span, replacement, Applicability)` beside `.help()`.
  Migration diagnostics are the first users, since their help text is already
  a replacement.
- Later, `sioxc --fix file.siox` applies the machine-applicable suggestions in
  place. That turns every future syntax migration into one command, where
  today it is `--emit source` and hoping the printer loses nothing.

**Acceptance.** Every code has an explanation; JSON output round-trips through
a schema test; the label and attribute migrations offer machine-applicable
fixes.

## 3. One constant evaluator

**Problem.** Constant folding has been duplicated across stages:

| where | function | migration status |
| --- | --- | --- |
| `types/calls.rs` | `const_fold` | removed; uses shared integer analysis |
| `types/indexing.rs` | `const_literal` | shared-evaluator adapter |
| `types/patterns.rs` | `const_int_at` | removed; `const_int` supplies resolver-selected constants |
| `elab.rs` | `eval` | shared-interpreter parameter adapter |
| `ir/lower_helpers/builders.rs` | `eval_const`, `eval_const_fns`, `eval_const_stmts` | shared-interpreter binding/function adapters |
| `ir/lower_helpers/substitute.rs` | `fold_const` | already an IR evaluator adapter |
| `ir/lower/collect.rs` | `fold_const`, `eval_const` | aggregate collection and an IR evaluator adapter |

The traversal and a borrowed declaration index are now shared. Free/foreign
function contracts, constants and transparent aliases are selected by `DefId`.
Checker analysis and elaboration can evaluate integer-signature free functions;
elaboration also supplies parameter/loop values, and IR supplies bound
attributes and legacy scalar values. Typed enum/packed/real values and uniform
attribute context remain necessary so constant status does not depend on stage.

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

**Implemented first migration.** `types::consteval::integer` owns checked
integer arithmetic and declaration traversal, returning an `i128` analysis
value or a spanned reason evaluation failed. Its three checker callers share
this evaluator; narrowing to `i64` happens at the consumer boundary. This
retains the pattern checker's wider domain and avoids division/remainder
panics at the signed-word minimum. Resolver-owned lookup stays outside the
evaluator, so qualified constants and parameter shadowing keep their identity.
No new runtime or packed-value width limit is introduced.

Before removing the three old folders, a temporary side-by-side oracle passed
the full Rust suite and actual 255-program corpus, requiring agreement for
every previously returned value. New tests cover failure spans, zero divisors,
overflow, shifts, named constants, shadowing and cycles. Literal parsing is
still the existing parser's domain; this is not yet full typed const evaluation.

**Implemented cross-stage traversal.** Elaboration and IR now use the same
interpreter as checker analysis. It owns arithmetic, expression traversal and
pure `return`/`if`/`else` function control flow. Contexts supply declarations,
parameters and attributes without upward imports from `types` to IR. Analysis
keeps its wider integer intermediates; the existing native-word interfaces
retain checked arithmetic and kernel shift truncation, including the checked
signed-minimum remainder boundary. These domains do not limit packed values.

Calls borrow the root environment and parameter names, allocating only their
local binding/value frames rather than cloning every module constant. Failed
selected branches propagate a reason instead of being mistaken for empty
branches and falling through to a later return. `else if` chains are walked
without cloning their ASTs or consuming function-recursion depth. Tests live
beside the interpreter in `types/consteval/tests.rs`.

Before deleting the old elaboration/expression/function walkers, temporary
side-by-side checks passed the complete Rust suite and actual 256-program
corpus with agreement for every old successful result. The added
`consteval_calls_test.siox` exercises module constants versus same-named
parameters, restored nested frames and recursive widths in hardware/processes.

**Remaining.** Raw scalar logical/bitwise hooks are explicitly transitional
(`shortcut:` comments), not a second definition of operators. Their disagreement
cannot be resolved from untyped integer bits: replace them with typed,
source-owned operator and condition evaluation. Unify function/attribute
availability through the shared resolved registry; then include enum and
packed values, directional ranges, all statically sized attributes, additional
pure-function statements. Metadata validation now shares scalar evaluation as
described below; arbitrary typed attribute values remain open.

**Implemented integer metadata.** Raw bindings/defaults resolve first; the
resolve-layer attribute pass then attaches bindings and substitutes reads,
retaining the original binding/default expression spans
and resolved constants/functions. Instance `let` metadata now goes through
resolution too. This prevents a reader's same-named constants/functions from
capturing imported metadata. The shared interpreter validates integer defaults
and bindings in the native integer domain, and an owned `Typed` side table
records the concrete values for a post-check expansion pass. Applied attrs,
source bindings/defaults and their copied reads all receive those values before
elaboration; rejecting arithmetic or carrying unevaluated calls to tools is no
longer the default behavior. Negative results keep an explicit unary AST node.

The minimum signed literal is checked after negating its magnitude. A native
boundary regression also caught contextual widening sign-extending a negative
literal's positive magnitude before completing the negation. Source IR now
materializes a checked negative kernel literal as one integer value before its
consumer widens it. General context-width arithmetic and packed conversions
retain their existing semantics; packed widths are not limited by this fix.

Core precedence remains literal-only and uses its canonical spelling during
bootstrap discovery: operator grouping is discovered before normal
parsing/resolution, and accepting an expression only in the later checker would
silently ignore its value. Bool/string/enum metadata and pure-function statement
coverage still need the typed-value work; no new mini evaluator or truth table
was added. Tests cover invalid/nonconstant arithmetic, domain boundaries,
normalization, both module orders and hardware/native imported metadata reads.

An additional integer folder was found in `resolve::check_generic_arity`.
Move that width analysis to the owning later stage (or share the appropriately
earlier primitive layer), never introduce a `resolve -> types` import. The
source-owned enum operator-table evaluator also remains separate until values
carry their enum identity. Do not mark the proposal complete merely because
the scalar traversal is shared.

**Acceptance.** Duplicate expression interpreters are gone (thin adapters may
remain); `E-P021` ("not a constant") and range errors agree across stages.

## 4. A resolved tree between `types` and `elab`

**Problem at proposal start.** Resolution produces `DefId`s, but every later stage walks the
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

**Implemented first registry.** `types::declarations::DeclarationIndex` borrows
module constants, free/foreign functions and non-generic transparent aliases,
keyed by `DefId`. The checker reads contracts/initializers directly instead of
keeping separate `generic_bodies`, `const_types`, `const_values`, `fn_arity`,
`fn_param_types` and `fn_return_types` maps. IR's separate free-function map is
gone; its existing API delegates to this index. The same item collector builds
the index in each immutable phase, after source rewrites rather than retaining
references across a mutable AST pass. `Typed` therefore keeps its owned side
tables without acquiring a source lifetime.

Elaboration now resolves module constants and free integer functions in
parameters, widths, generate conditions and rendered indices. Source constants
cannot be captured by same-named caller parameters. Integer analysis follows
transparent integer aliases, but leaves real/packed/generic value conversions
unknown: an integer-only interpreter must not invent their semantics or reject
valid explicit packed narrowing. Associated methods and trait/type
registries and fully typed values still need migration.

Tests cover equal function/constant leaves, imports/renames, constant aliases,
borrowed contracts, foreign prototypes and rebuilding after rewrites. Tree
and native tests verify known parameters before IR and a width mismatch
rejected during elaboration, plus the packed-result narrowing boundary.

**Implemented metadata registries.** `resolve::attributes` runs after raw name
resolution, not in `syntax`: binding names/defaults/values retain their original
IDs without violating stage layering. Declaration schemas, target/value kinds,
entity binding fallbacks and duplicate checks are keyed by resolved `DefId`.
Concrete named targets compare their resolved type heads rather than equal leaf
names; this does not yet normalize transparent type aliases. Type attributes
conflict only with metadata visible in their declaration scope.

Imports and renames retain identity: two aliases of one attribute are duplicate
bindings, while two same-leaf attributes from different modules coexist. System
tick names remain reserved even through imports. The four existing standard
hooks register `metadata_keep`, `metadata_library`, `metadata_name` and
`metadata_precedence` lang roles in core/std. A user's same-leaf declaration
does not acquire a hook, warning or literal-only restriction. The test directive
remains separate from ordinary metadata named `test`.

Elaborated `Instance::attrs` keeps its existing string-pair API. User metadata
exports canonical module-qualified declaration names, independent of the alias
used at the binding; registered standard hooks keep their short output names.
Frontend regressions cover module order, conflicting types/defaults, entity
fallbacks, target identity, import aliases and private lookalike hooks. API/native
tests verify unambiguous hierarchy output, diagnostics and test discovery.
The pre-resolution operator bootstrap still requires literal
`attr precedence = N;` spelling; resolving aliases and expressions during that
discovery is separate work, not implied by the metadata-ID migration.

**Implemented entity declarations.** The shared `DeclarationIndex` now borrows
entity declarations too. Elaboration, structural-root collection and IR use it
instead of constructing separate entity declaration maps. IR's existing
`FunctionIndex` facade delegates entity lookup to that index rather than owning
another table. The checker owns only computed port facts, keyed by `DefId`;
test membership and private entity member owners also use resolver IDs. Valid
generic parameters retain their own identities, without reconstructing a
module-qualified entity name during these checks.

This removes duplicate indexing and string-key reconstruction; it is not a
claim that every prior qualified-string registry had a leaf collision. Tests
cover borrowed source pointers, both module orders, equal entity names with
different port types, import renames, private members, generic wrappers and
native parameter binding. One stale elaboration fixture used an unbound
`impl Buf<T>` and now uses the already-required `impl<T> Buf<T>` syntax.
No new implicit binder or grammar rule is introduced. Transparent entity
`type` aliases still require normalization at a shared typed boundary; import
renames and type aliases are not interchangeable implementation mechanisms.

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
2. **Diagnostics**: explanations, JSON and atomic suggestions for the existing
   label/attribute migrations are implemented. Add suggestions with future
   migration diagnostics; automatic source rewriting remains deferred.
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

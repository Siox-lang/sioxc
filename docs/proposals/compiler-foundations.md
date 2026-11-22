# Compiler foundations borrowed from rustc

Status: **partially implemented**. Core loading, lang-item registration and
library-declared built-in macros have landed. Diagnostic UI snapshots now have
a runner and corpus fixtures; `--explain CODE` has embedded reference pages.
See sections 1–2 for verification and usage. JSON diagnostics and atomic
migration suggestions are implemented. Scalar traversal and resolved
declaration contracts are shared across stages; fully typed constant evaluation,
the remaining declaration-ID registries and residual name-based hook cleanup
remain. Each part lands independently.

Latest local acceptance (2026-11-22): full combined service
1795380680496268099-2179693 completed exited/success/0 at22:57:52 CET
(started21:51:20), peak1,612,677,120 bytes under unchanged normal limits.
Independent read-only review confirms all823 frontend/57 evaluator/14 imports,
880 default/859 bitpack library cases and125 integration targets per mode,
ALL48 GDB/DWARF cases and the original four FIFO/wide CI debugger smoke checks.
Both ACTUAL274-program corpora,71 UI/71 JSON/56 codes/two apply-recompile
migrations per mode, strict Clippy,80 script tests/36 waveform tests/12 specified
artifacts,72 direct native/debug programs and four aggregate timeline/VCD/parity
runs pass. Exact new frontend/native test names are present; no zero-test result
is used as new-feature coverage.

This accepts shared applied field contracts and nested static field metadata:
the checker and evaluator borrow the same original inherited field owner/span;
only selected field types are instantiated by resolved type/value arguments.
Pure path/field shapes need no runtime signal contents; calls/index effects,
private fields and static-function scope remain protected. Privacy-only queries
avoid unused type instantiation. It also accepts deletion of Lowering.trait_decls:
trait IDs borrow original default bodies from the shared declaration index.
Named applied receiver keys explicitly remain debt.

The ratchet classifies82 fields; strict mode correctly rejects37 actual remaining
declaration migrations. Only the deleted trait table is removed from inventory.
Focused field2140153/source93cbc481 and trait2140427/source9b425771 checks
preceded this complete combined proof, which also covers the allocation review
fix. Earlier2031802/2034604 test-compilation failures remain rejected evidence.

Sourcef9436f78a581b46a1816be03e0398170f653c58d13a6b35e9270520426f8a41f
and corpus04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf
match before/after and independent current review. Code-only excluding Markdown:
6fc21fbf99dbd3216c6e8b7537130a0911fcd33c8685022368078e5bf22cea29.
Only acceptance Markdown afterward; the pinned dependency, Rust1.90/jobs1/
threads2/host debug1 and generated debug checks remain unchanged. Normal2GiB
hard/1.5GiB soft/no-swap/no-core limits are verified from the actual service.

Defaults and applied-type regression prototypes are NOT included or accepted.
All FIVE sections stay required at about86% of foundations, not the compiler:
fully applied generic/associated contexts, remaining typed values/defaults/
static attributes, integer/fractional suffix migration, remaining registries
and residual hooks remain open. No time/integer semantic changes or peer/main/
corpus edits. No push, PR or wiki publication.

Previous local acceptance (reviewed 2026-11-22): full combined service
1795237211316578391-2005774 completed exited/success/0 on November21,
06:00:11–07:03:30 CET, peak1,612,320,768 bytes under unchanged normal caps.
All819 frontend/56 evaluator/14 import tests, complete Rust suites (876 default /
855 bitpack library cases,123 integration targets per mode), ALL48 GDB/DWARF
cases and the original four FIFO/wide CI GDB smoke checks pass. Both ACTUAL274
corpora,71 UI/71 JSON/56 codes/two apply-recompile migrations per mode, strict
Clippy,80 script tests/36 waveform-checker tests/12 specified artifacts,
72 direct native/debug programs and four aggregate timeline/VCD/parity runs pass.

This accepts deletion of the process constant-seed collector and per-process
clone. Only actual selected-instance parameters remain scoped execution state;
typed module constants borrow original contracts through DeclarationIndex.
The alias-scope registry guard correction is also accepted: alias bodies keep
declaration scope and substituted generic arguments keep caller scope. The
guard still does not resolve Rust imports, opaque/generated types or arbitrary
name/type relationships. Its inventory passes83 classified fields and strict
mode correctly rejects38 genuine remaining declaration migrations.

Source e0aba04a8591090061561223a49f462d811e1cd65b3367a1d9be6c0142a8730a
and corpus04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf
match before/after and independent review. Code-only excluding Markdown:
9ce460e64a6e7be27d88c195272ef1be7a51e76d97173a1e1cc4472b5e9ea41c.
Only acceptance Markdown and one-month log pruning afterward. The pinned
dependency, Rust1.90/jobs1/threads2/host debug1, generated debug checks and
2GiB hard/1.5GiB soft/no-swap/no-core limits remain unchanged.

Separate field-metadata and borrowed-trait-default drafts are NOT accepted or
included. Their first focused checks stopped at test compilation because the
tests tried to sort non-Ord DefIds; set-based deduplication retains the same
identity assertions, and fresh focused checks are running/queued. Failed logs
remain evidence, not passes. Full integration gates remain required for them.

All FIVE sections remain required at about **86% of foundations**, not the
whole compiler: applied generic/associated contexts, remaining typed values/
defaults/static attributes, integer/fractional suffix migration, remaining
declaration registries and residual hooks stay open. No time/integer semantic
changes; no push, PR or wiki publication.

Previous local acceptance (2026-11-21): combined full service
`1795232592570177627-1846924` completed `exited/success/0`,
04:43:12–05:50:05 CET, peak 1,612,693,504 bytes under unchanged normal limits.
All 818 frontend/56 evaluator/14 import tests, complete Rust suites (875 default /
854 bitpack library cases, 122 integration targets per mode), all 48 GDB/DWARF
cases and strict Clippy pass. The original CI FIFO/wide smoke script also passes
all four ordinary/debug GDB inspection/stepping and exact baseline VCD checks.

Both ACTUAL 274-program corpora pass. Each mode passes 71 UI/71 JSON cases,
all 56 codes and both apply/recompile migrations. All 78 script-runner tests,
36 waveform-checker tests, 12 specified artifacts, 72 direct native/debug
programs and four aggregate timeline/VCD/parity runs pass. The registry ratchet
passes with 83 classified fields while strict mode correctly rejects the 39
remaining declaration-migration entries; that expected rejection is not a
claim that migration is finished.

Source `f9a44592948d685a7b746798d3447e9beff9553505f10c8c92e2d0506f4ece3e`
and corpus `04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf`
match before/after and independent review. Code-only excluding Markdown:
`248c2f140fd5f5046dfa6220ed06d459cae21f67d19193a5285482edf212f36f`.
Only acceptance Markdown changes afterward. The clean libfst pin, Rust 1.90,
one Cargo job/two test threads, host debug1 and generated debug coverage remain
unchanged.

This accepts the fixed-port metadata/scope, elaborated record identity and
initial registry-ratchet combination described below. The corrected static
visibility regression remains intact. The separate process-constant collector
deletion and guard alias-definition-scope correction are NOT included; their
focused jobs are still running/queued and are not acceptance evidence here.
The guard's documented import/opaque/name-resolution limitations remain.

All five sections stay required. Fully applied generic/associated contexts,
remaining typed values/defaults/static attributes, integer/fractional suffix
migration, actual remaining declaration registries and residual hooks remain
open. Estimate stays about **86% of foundations**, not the whole compiler.
No push, PR or wiki publication is implied.

Implementation notes (scope accepted by the gate above): static port-bound attributes,
elaborated record identities and the declaration-registry ratchet are now
combined on the accepted prefix/method/indexed-alias checkpoint. The isolated
focused checks below preceded the complete combined acceptance above.

Ports receive owning entity/member identities; the shared source index borrows
port and impl-value types, and constant attributes reuse the existing array
bounds/attribute code without reading runtime signal contents. Static functions
still cannot reach instance ports. The first attempt violated that rule and
is rejected; corrected focused unit `1795232167694811290-1830855` passes the
unchanged visibility regression, all 812 frontend/14 imports, strict Clippy and
both-mode ordinary/debug native bounds/parity fixtures, at unchanged source
`6d163722`. This covers fixed port/impl-value shapes and typed evaluator locals,
not every applied generic or runtime-local environment required by issue #295.

Record checks no longer keep `Elaborator.record_structs`. Elaborated named
types retain their resolved owner alongside presentation text, and field/base
queries borrow `DeclarationIndex`. The Rust API representation is now
`EType::Named { name, definition }` rather than `EType::Named(String)`.
Focused unit `1795196954456433068-1773481` passes 812 frontend/14 imports,
strict Clippy and relevant old/new native suites in both modes at source
`459a46a4`, covering aliases, distinct same-leaf records, inherited fields,
same-named generic parameters and reactive ordinary/debug waveform parity.

`python3 scripts/check_registries.py` rejects new/unclassified or stale
name-keyed state fields in types/elab/IR; `--list` is read-only and `--strict`
also rejects every remaining declaration-migration entry. The reviewed
inventory distinguishes real migration debt from operator/affix/member symbols,
scoped execution state and serialized runtime output. CI and CI-local run the
guard and its tests. Isolated unit `1795196566646662466-1767488` passes all
78 script tests and exact strict-debt rejection at source `aa58d56b`. Combining
the record deletion removes only that actual inventory entry: 39 declaration
migrations remain. A green allowlist is not identity-migration completion.
This spelling-based source check is not Rust name resolution and cannot prove
the absence of opaque/imported/definition-site-shadowed aliases, generated
tables or handwritten leaf searches; consumer audits and identity tests remain
required.

All five foundation sections remain required. Applied generic/associated
contexts, remaining typed values/defaults/static attributes, integer/fractional
suffix migration, the actual remaining registries and residual hooks stay open.
Estimate remains about **86% of foundations**, not the whole compiler. No new
commit, push, PR or wiki publication was authorized by focused evidence alone.

Previous local acceptance (2026-11-20, reviewed 2026-11-21): corrected full
unit `1795195132946984971-1731183` completed `exited/success/0`,
18:18:52–19:20:24 CET, peak 1,612,435,456 bytes within unchanged normal limits.
All 816 frontend/55 evaluator/14 import tests, complete Rust suites (873 default /
852 bitpack library tests and 120 integration targets per mode), all 48
GDB/DWARF cases and strict Clippy pass. Both ACTUAL 274-program corpora pass;
each mode passes 71 UI/71 JSON cases, all 56 codes and both apply/recompile
migrations. All 70 runner/36 waveform-checker tests, 12 specified artifacts,
72 direct native/debug programs and four aggregate timeline/VCD/parity runs pass.

The original CI FIFO/wide debugger smoke script also completed successfully in
unit `1795197948405838756-1786320`, finishing at 19:26:52 CET, peak
1,264,537,600 bytes. All four ordinary/debug GDB inspection/stepping runs and
their exact baseline waveform comparisons pass. Its fresh checkout-local target
does not reuse another worktree's root package artifacts.

Source `30689ee7ba98677769a9013cf06f959b7bd1625a9c6f43dc259cbfcb5e6ca597`
and corpus `04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf`
match before/after both gates and independent review. Code-only excluding
Markdown: `284c27618392f09be4ad8d9151fd79b353531a8b9ba5d69abf1ecd97feff5536`.
Only acceptance Markdown changes afterward; the clean libfst pin, Rust 1.90,
one Cargo job/two test threads, host debug1 and generated debug checks remain
unchanged.

This accepts the source-registered radix-prefix contracts, selected constant
method implementation/Self/trait contexts, and canonical indexed-alias repair
described below. The original wide/Unicode formatter and new nested-string
fixture both execute successfully without weakening assertions. Earlier failed
or stale-artifact runs remain rejected evidence; dated pending notes below are
superseded only for this fully checked combination.

Static-port attributes, the registry ratchet and elaborated record identities
remain isolated followups, NOT part of this accepted source. All five sections
remain required. Fully applied generic/associated contexts, remaining typed
values/defaults/static attributes, integer/fractional suffix migration, actual
remaining declaration registries and residual hooks stay open. Estimate remains
about **86% of compiler foundations**, not the whole compiler. No push, PR or
wiki publication is implied.

Previous local acceptance (2026-11-19, reviewed 2026-11-20): corrected full
guarded unit `1795083437681592050-1555130` completed `exited/success/0`,
11:17:17–12:18:53 CET, peak 1,612,705,792 bytes under unchanged normal limits.
All 811 frontend/14 import tests, both complete Rust suites (868 default /
847 bitpack library tests, 119 integration targets per mode and all 48
GDB/DWARF cases), strict Clippy, 70 diagnostic-runner tests and 12 specified
artifacts pass. Both ACTUAL 274-program corpora pass; each mode passes 71 UI /
71 JSON cases, 56/56 codes and both apply/recompile migrations. All 72 direct
native/debug programs and four aggregate timeline/VCD/parity runs pass.

Source `6d8cc95773d64b69078816691a5bc0247a5ee2a5fa74a45a14ba4bb6093dad0c`
and corpus `04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf`
match before/after and independent review. Code-only fingerprint excluding
Markdown is `e481d79b9a13bdc1a51ba62a6fdad29df647a9aa7cd515aed0afd0195b9fca47`;
only acceptance Markdown changes afterward. The clean libfst pin, Rust 1.90,
one Cargo job/two test threads, host debug1 and generated debug checks remain
unchanged.

This accepts the shared checked real-suffix implementation-call adapter and
deletion of its old AST walkers after the complete successful-answer oracle.
Hardware still uses ordinary source-aware inlining: Self/aliased constructor
identities and result layouts are resolved without discarding debugger frames.
It also accepts borrowed view contracts, resolver-owned overload groups and
removal of all four copied checker/IR view maps. Aliased/array views select
their original backing/directions and preserve reactive wiring in both import
orders and ordinary/debug builds, with exact waveform parity.

The failed earlier combined attempt is retained below; its pending notes are
superseded only for this corrected, fully checked scope. The separate prefix
followup is not accepted; its working integration below still needs full checks.
All five sections remain required;
the estimate remains about **86% of compiler foundations**, not the whole
compiler. Integer/fractional suffixes, typed/applied contexts, values/defaults,
remaining registries/boundary guard and residual hooks stay open. No push,
PR or wiki publication is implied.

Working followup (2026-11-20; not accepted): registered radix prefixes select
their unique original implementation target rather than matching Logic/unsigned
names. Normalized element/family identity, literal widths and fixed labels
survive; missing/ambiguous/incompatible providers stay unknown. Indexed family
aliases reuse the declaration normalizer while existing path-only/bootstrap
string handling is retained. Isolated focused unit `1795192856022926626-1682017`
passes all 806 frontend/14 imports, strict Clippy and both-mode ordinary/debug
native ULogic-backed Radix values with exact typed identity, bit values and
waveform parity, at source `2a6374ad` unchanged before/after. This does not
complete the other residual hook consumers.

Ordinary constant method calls now keep the same selected implementation,
Self and trait arguments as operator/suffix calls, rather than invoking only
the detached function body. Contextual arguments use the selected contract;
distinct implementations of one default body remain ambiguous. New regressions
cover module order, borrowed source pointers, nested defaults, nominal owners,
inherent priority, enum/real/natural arguments and strict signed bindings.
These changes are integrated locally with the prefix followup. Combined focused
unit `1795193237433161941-1689002` completed `exited/success/0`,
17:47:17–17:51:24 CET, peak 1,610,874,880 bytes. Both method groups,
all 55 evaluator/815 frontend/14 import tests, strict Clippy and both-mode
native prefix/view/derived/suffix fixtures pass. Source `ecd8ec6a` matches
before/after; original typed identities, bit assertions and waveform parity
remain intact. This focused proof is not full combined acceptance.

A first method-focused attempt reused a stale library from another checkout
in the formerly shared Cargo target and ran zero new method tests; it is NOT
passing semantic evidence. New combined verification has a primary-only target
directory and asserts the exact new test names before filtered execution.
The ENTIRE combined Rust/native/GDB/ACTUAL-corpus/UI/JSON matrix on the new
dedicated target, with unchanged source/corpus and dependency checks, is pending
before acceptance or commit. All five sections and remaining work above stay
in scope; estimate remains about 86% of foundations, no push/PR/wiki.

The first combined full attempt `1795193583578417888-1694331` at
`37adee5a` is rejected: the existing wide/Unicode formatting case lost packed
widths on two record assignment snapshots. The run was deliberately stopped
after preserving that actual Cargo failure; stopped `dead/success/0` is not
a passing matrix. The minimal nested-string alias native regression reproduces
the same failure without changing the original test.

The correction fills an unconstrained array dimension at the shared alias
normalizer rather than nesting another open dimension. Layout queries now
accept every changed canonical alias result, including a borrowed original
target, while retaining the established bootstrap fallback for unchanged or
unknown types. It preserves nested dimensions, element identities and the
custom ULogic radix family rather than special-casing string names.

Corrected focused unit `1795194907081603245-1725754` completed
`exited/success/0`, 18:15:07–18:16:49 CET, peak 1,423,073,280 bytes, at
source `a9d541f5` unchanged before/after. All 816 frontend/55 evaluator/
14 import tests, strict Clippy, both-mode prefix/string/view/derived/suffix
native fixtures and the unchanged original wide/Unicode formatter pass.
The fresh corrected complete matrix remains pending before acceptance/commit.
The separate static-attribute worktree is not integrated or covered by this
proof. Every remaining five-section requirement remains open as stated above.

Previous local acceptance (2026-11-15, reviewed 2026-11-19): full guarded unit
`1794709495191787889-1083259` completed `exited/success/0`, 03:24:55–04:30:29
CET, peak 1,612,738,560 bytes under normal 2 GiB hard/1.5 GiB soft limits.
It passes 804 frontend/14 import tests, both complete Rust suites (861 default /
840 bitpack library tests, every integration/doc-test and 23 GDB/DWARF tests per
mode), strict all-target/all-feature Clippy, 70 runner tests and 12 artifacts.
Both ACTUAL 274-program corpora pass. Each mode passes 71 UI/71 JSON cases,
56/56 active codes and both apply/recompile migrations. All 72 direct native/
debug binaries and four aggregate VCD/parity checks pass. The new family
native case passes both storage modes/import orders/debug settings, exact
labels, reactive binary/Z/X indexing and exact ordinary/debug waveform parity.

Source `ff2b254a0f51b24fa7478d6b3539fded74b88bb8f1b71c8a54d876facb989756`
and ACTUAL corpus
`04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf`
match before/after and independent review. Code-only fingerprint excluding
Markdown stays
`80bbb41db520a7125c9228bb91b259a12cc0770fc2b60d41346e73f77938bc24`.
Only acceptance Markdown/logs change afterward; Rust 1.90/one Cargo job/host
debug1, generated debug assertions, corpus and pinned dependencies are unchanged.

This accepts Checker.structs removal and cycle-safe borrowed declaration-ID
contracts, plus shared alias/applied/transitive array representation queries in
the frontend, IR and Process lowerer. Constructors and locals retain fixed
dimensions, labels and source-owned enum metadata without an unnecessary raw
resize around an already-contextualized packed literal. No arbitrary scalar/
record-array newtype feature, std encoding or LLVM/runtime change is introduced.
The provisional privacy finding was not confirmed: the baseline already rejects
the invalid declaration before producing metadata.

Blanket integer-binding widening is NOT accepted: existing tests and
language.md require declared integers to fit signed64 even when analysis
intermediates are wider. Preserve those contracts during suffix migration.
All five sections remain required; estimate stays about **86% of foundations**,
not the whole compiler. Typed values/defaults/applied contexts, suffix-oracle
migration, remaining registries/boundary guard and residual hooks remain open.
No push, PR or wiki publication.

Real-suffix comparison passed (2026-11-19; production migration not accepted): a thin shared
implementation-call entry retains the borrowed ImplDecl, Self owner and
trait-argument substitutions. The existing derived projection exposes the
checked real payload without erasing its nominal return contract. The old
real folder stayed authoritative while every old successful answer was compared
bit-for-bit against shared evaluation, including direct Rust-test calls. Full
guarded unit `1795075962968146515-1411936` completed `exited/success/0` at
10:18:21 CET, peak 1,612,599,296 bytes: 806 frontend/14 import tests, 863 default /
842 bitpack library tests, all 118 integration targets per mode and 46 GDB tests,
strict Clippy, both ACTUAL 274-program corpora, 71 UI/71 JSON cases and 56/56 codes
per mode, both migrations, 72 direct native/debug cases and four aggregate
waveform checks pass. Source `c843f242` and corpus `04c35557` match before/after;
full fingerprints and retained evidence are in [chat.md](../../chat.md).

That proof permits removing the old real-expression/block walkers and temporary
oracle. The working production adapter now uses the shared checked impl call
directly. Native coverage adds real-backed aliases with Self returns, locals
and helper calls in hardware and processes, both import orders/debug settings
with exact waveform parity. Tests also cover default trait arguments, nominal
owner separation and exact IEEE payloads. The oracle-free version still needs
the COMPLETE matrix before acceptance or commit. Integer/fractional suffix paths
and all other requirements remain open; no signed binding or native-word check
is weakened. The borrowed-view followup below is integrated locally after its
focused proof; the full combined gate must still pass before acceptance.

Oracle-free focused proof: unit `1795080213577614067-1492397` completed
`exited/success/0`, peak 1,508,880,384 bytes, at source `fda72cb0`. Its four
targeted groups, all 806 frontend tests and strict frontend Clippy pass. The
extended native suffix test passes both storage modes, import orders and
ordinary/debug settings (eight binaries), with exact waveform parity. An earlier
native failure exposed hardware's remaining context-free suffix inliner; the
first correction retained its original ImplDecl/FnDecl and folded real results.
That focused proof did NOT establish debugger correctness: the subsequent full
combined attempt below caught a lost suffix source frame. No old real AST
evaluator is restored and no assertion is relaxed. Complete corrected acceptance
is still required.

Combined full attempt `1795080656664268298-1500304` at source `68940e0d`
is NOT accepted: all 810 frontend tests and the native/waveform groups passed,
but default GDB finished 22 passed/1 failed. The existing hardware-expression
test could no longer stop at the real suffix's return line or inspect its local.
The run was deliberately stopped after preserving that actual Cargo/GDB failure;
its stopped `dead/success/0` is not a passing gate. Later corpus/debugger stages
were not completed on that invalid snapshot.

The working correction keeps normal source-aware hardware inlining instead of
substituting a constant without provenance. Constructor queries follow impl
Self's existing lexical owner DefId through aliases, and the borrowed suffix
target supplies the return layout. Process constant folding continues to use
the shared checked interpreter. A new owner-identity regression and GDB case
cover same-leaf aliases, Self constructors and real helper/parameter/local source
stops alongside the unchanged original assertion and native parity cases. This
correction is not accepted until its focused debugger checks and the ENTIRE
corrected combined Rust/native/ACTUAL-corpus/UI/JSON matrix pass.

Corrected focused proof (2026-11-19): unit
`1795082426085714670-1540418` completed `exited/success/0`, peak
1,505,808,384 bytes, with source `597125b0` unchanged before/after. All
811 frontend tests, strict frontend Clippy, both-mode suffix/view native
fixtures and all 24 GDB tests per mode pass. The existing hardware return
stop and new Self/helper/local-value test both pass without weaker assertions.
The full corrected combined acceptance retry is pending; this focused result
does not replace either complete Rust suite, ACTUAL corpus or diagnostic gates.

Borrowed-view focused proof: isolated unit `1795080323550328579-1496036`
completed `exited/success/0` at 10:28:28 CET, peak 1,610,874,880 bytes, source
`f904ea76` unchanged before/after. Four new identity/layout/direction groups,
all 808 frontend tests, 14 imports and strict frontend Clippy pass. The native
alias-wiring fixture passes both storage modes, import orders and ordinary/debug
settings (eight binaries), reactive parent/child behavior and exact waveform
parity. It is now integrated with the oracle-free real-suffix change in the
primary checkout; neither focused proof replaces full combined verification.

Previous local acceptance (2026-11-14): full guarded unit
`1794682069805603352-663925` completed `exited/success/0` at 21:28:16 CET,
peak 1,612,697,600 bytes under the normal 2 GiB hard/1.5 GiB soft caps.
It passes 797 frontend/14 import tests, both complete Rust suites (854 default /
833 bitpack library tests, every integration/doc-test and all 23 GDB/DWARF
tests per mode), strict all-target/all-feature Clippy, 70 diagnostic-runner
tests including 36 waveform cases and all 12 specified artifacts. Both ACTUAL
274-program corpora pass. Each mode passes 71 UI/71 JSON cases, 56/56 active
codes and both apply/recompile migrations. All 72 direct native/debug binaries
and four aggregate VCD/timeline/parity checks pass. Source fingerprint
`7ab1cdda9331fd903cae55eaca327a99e4a957e4788b91c7c3e9b3b574270792`
and ACTUAL corpus
`04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf`
match before/after and independent review. Only acceptance Markdown/logs change
after that proof; code-only fingerprint excluding Markdown stays
`aab457e22986ce239b300ec1ebe67a1d755a189bfe623926a96ae0ad3d197acc`.

This accepts shared parameter-ID generic inference and partial-error recovery,
borrowed original field typing/privacy, normalized alias conversion/layout,
typed ordinary/Unicode and enum-context strings, nonempty open constant shapes,
generated tuple declarations/references registered in the existing resolver,
and cross-file constant-attribute span preservation. Optional guarded queue
support passes argument checks and real bounded serialization/OOM integration.
It does not accept the separate struct-registry-removal draft or finish all
five sections. The estimate remains about **86% of compiler foundations**,
not the whole compiler; applied generic/associated contexts, remaining typed
values/defaults, suffix-folder migration, remaining registries and residual
hooks remain open. No push, PR or wiki publication is implied. Earlier pending
notes below are superseded only for this tested scope.

Previous local acceptance (2026-11-12): full guarded unit
`1794493076641606005-2865384` completed `exited/success/0`, 15:17:56–16:21:31
CET, peak 1,612,836,864 bytes under the normal 2 GiB cap. It passes all 776
frontend and 14 import API tests, both complete Rust suites (833 default /
812 bitpack library tests, every integration group and all 23 DWARF/debugger
cases per mode), strict all-target/all-feature Clippy, 70 runner tests including
36 waveform-checker cases and all 12 specified example artifacts. Both ACTUAL
274-program corpora pass. Each mode passes 71 UI and 71 JSON cases, covering
56/56 active codes and both apply/recompile migrations. All 68 additional
direct native/debug binaries and four aggregate VCD/timeline/parity runs pass.

Source `3d7efdb7009c9a241c3d70103299c0aaa4ddffc8da4d3b004712a8da6cfc6327`
and ACTUAL corpus
`04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf`
match before/after the full run and independent acceptance review. Rust 1.90,
one Cargo job and the normal memory limits are retained. The host test profile
uses `CARGO_PROFILE_TEST_DEBUG=1` to avoid observed debug=2 memory-pressure
thrashing; generated Siox `--debug` flags and every GDB/DWARF assertion remain
unchanged. The complete test-target builds finish in 1m34s/default and
1m31s/bitpack. Only acceptance documentation and coordination notes change
after the frozen gate; no compiler, manifest, dependency or syntax changes.

This accepts the nominal/explicit-From/record-boundary interpreter followup,
typed constant materialization and canonical origin sharing, tuple/natural/
labelled-array layouts and runtime constant projection, borrowed C aggregate
and generic-function contracts, finite generic layouts, parameter-ID inference,
Boolean generic-scope state and resolved-role generic bounds described below.
It does NOT complete all five sections. The engineering estimate is about
**86% of compiler foundations**, not the whole compiler. Generic/associated
applied contexts, packed/string/open/enum-range values, structural defaults,
suffix-folder migration, remaining declaration registries and residual hooks
are still open. No push, PR or wiki publication is implied. Older pending
statuses below are the historical sequence, superseded only for this accepted
scope; their remaining-work requirements are not removed.

Working followup (2026-11-12, not fully accepted): generic argument selection is
shared by trait-bound checks, return inference and instantiated-body checking.
It prefers healthy non-literal arguments, retains partial tuple/array types and
suppresses only unknown subparts, without hiding known argument mismatches or
other generic parameters. Incomplete bindings disable memoization rather than
all body checks. An earlier focused run passed the three original diagnostic
regressions and all 43 generic-focused tests. The combined draft additionally
keeps known concrete body checks even when no generic binding is available;
generic bodies are deliberately checked at calls, not abstractly at definition.
The earlier empty-binding shortcut assumed otherwise and has been removed.
Its corrected per-call diagnostic and field-recovery cases require a fresh gate.

The same pending change removes `field_decl_types` and the copied field
visibility registry. Field typing, existence, contextual initialization,
privacy, inherited ordering and positional counts read the original declarations
through checked owner `DefId`s and cycle-safe normalized base links. Privacy
stays with the original field owner; explicit views keep their exposure and
foreign-view restrictions. Source-pointer, same-leaf, alias, metadata, natural
scalar and cycle regressions are included. Other struct/base/method/view maps,
overloaded applied-view identity and applied-generic field substitution remain
open. The new native integration test exercises contextual generic returns and
borrowed inherited fields in simulation. Neither the 86% estimate nor the
accepted checkpoint above is advanced before validation.

Corrective preview (2026-11-13): unit `1794523918296588147-159507` completed
failed/101, peak 1,119,854,592 bytes. All 792 frontend tests pass, including
44 generic, 16 enum-refresh and 51 interpreter cases; strict frontend Clippy
passes. The first native test rejects `Wrapped(packet)` through `PacketAlias`,
exposing the old conversion ancestry walk's unnormalized string lookup.
The pending fix follows borrowed struct declarations by `DefId` and normalizes
their bases, preserving total-conversion and field-adding boundaries. Added
coverage clears the compatibility map and checks aliases, kernel wrappers,
unrelated identities and reversed module order. That new test and the native
and complete full matrix still require validation. The replacement queued job
runs preview then full under the same normal lock; a stopped, not-yet-started
preview-only waiter supplies no pass evidence. No commit or push yet.

The following combined preview `1794568985926332580-1268526` stops at the new
ancestry test's invalid fixture syntax (792 other frontend tests pass; peak
907,948,032 bytes). Source newtypes cannot also declare fields. The corrected
test parses an ordinary record and sets its base directly in the test AST to
retain the defensive field-adding-boundary assertion; no parser or production
semantics change is needed. Native and full validation remain pending.

The corrected fixture passes in unit `1794569480633480190-1333038`: all 793
frontend cases and strict frontend Clippy pass. Its native executable still
refuses Process 1 block 0. Focused diagnostic `1794569734188053522-1370912`
(failed/101, peak 325,771,264 bytes) retains the actual TestExecutable IR:
the alias-derived Wrapped declaration/local has no layout, while Packet has
its full fields and generic results are correctly real. The pending shared
layout-query fix normalizes aliases before examining inherited struct fields,
using the existing borrowed declaration normalizer. Added native layout/label
assertions and the original runtime checks require a fresh full gate; LLVM
preflight and runtime behavior are not weakened. No acceptance or push yet.

The first layout-query preview `1794571783477945220-1481979` fails the
existing record-match layout regression (792 other frontend cases pass).
Internal declaration-layout anchors use declared-name spans, not source
reference spans. The correction preserves those already-identified terminal
structs through the existing identity query, while alias uses still require
shared normalization. No unknown-name fallback or test weakening is added;
the full gate remains required.

Preview `1794571934215427524-1510673` restores all 793 frontend cases and
strict frontend Clippy, and the generic/borrowed-field native regression passes
ordinary/debug execution in default storage mode. Its next native test exposes
an open-array materialization gap: the checked TEXT value has two Unicode
characters, but its declared Char[] representation still has no range, so
hardware TEXT[index] has no form. The pending adapter completes only missing
array ranges from the checked value during materialization and binds that same
layout, retaining strict validation of declared labels/direction/length. A
non-LLVM IR test covers open Unicode and integer arrays plus descending labels;
the original dynamic native cases remain. Empty/null, packed and generic
representations remain open. Bitpack and full validation are not yet accepted.

Preview `1794574171878896579-1654103` passes all 794 frontend cases, strict
frontend Clippy and the ordinary/debug default-storage generic native probe.
Dynamic TEXT indexing now lowers, but LABELLED'left becomes an Invalid source
value during canonical hardware CFG derivation. The pending system-attribute
adapter first asks the existing shared interpreter about the borrowed original
expression, then retains the existing signal/layout fallback. The IR regression
now covers all numeric shape attributes and source-defined nonzero Boolean
encoding for ascending. Native/bitpack/full acceptance is still required.

Preview `1794592048582872904-2519140` passes all 794 frontend cases, strict
frontend Clippy and ordinary/debug default generic native execution. The shared
attribute adapter removes the previous scheduler-view error. The string native
executable instead reports unsupported procedural Process 1 block 0. Focused
diagnostic `1794594862590995741-2659332` retains its actual TestExecutable IR:
the known two-character TEXT initializer still carries an open Char[] layout,
whereas LABELLED retains 6..4. The pending source binding fix reuses literal
shape inference for nonempty open array constants, including Unicode strings,
before lowering their initializers. The LLVM-free constant-layout regression
checks exact character extents, widths, module identities and declared ranges
in both module orders. Empty/null shapes stay unchanged; no LLVM/runtime bypass
or native assertion is removed. This is unaccepted work: both storage/debug
modes and the complete actual-corpus/native/diagnostic/waveform gate remain.

The corrected open-literal binding passes all 794 frontend cases and both native
probes in default/bitpack, each ordinary/debug, with unchanged preview
fingerprints. Four aggregate native/VCD timeline/declaration/waveform checks
also pass. The wider full gate then rejects generic_data_aliases_test because
its tuple binding has no type/layout; this is not complete acceptance. Focused
diagnostic `1794597949921118321-2846785` retains the actual pair snapshot with
missing widths, while the packet's instantiated field is intact.

Working followup (2026-11-14, unverified): tuple lowering now allocates distinct
generated-source spans and appends generated struct/type-parameter definitions
and references to the existing resolver facts. Original source IDs are retained;
resolution is not rerun and layout/name fallbacks are not added. The normal and
generated paths share definition insertion, and compiler statistics include the
new definitions. Regressions check old-ID preservation, distinct generated
binders, shared normalizer/argument substitution for single/nested/aliased
tuples in both module orders, and no added source/definitions without tuples.
All native cases and the complete acceptance matrix remain required. This
does not finish the remaining registries, typed contexts or hook cleanup.

The Nov14 combined gate verifies those generated identities: all 796 frontend
tests, 14 import cases, strict frontend/all-target Clippy, both generic/string
native probes in all storage/debug modes, four aggregate VCD/parity checks and
all 68 direct native executions pass, including the formerly failing ACTUAL
generic-data-alias case. Complete Rust library suites pass 853/default and
832/bitpack cases. Both integration runs then expose a cross-file span join
in constant attribute evaluation; the default ACTUAL corpus is 273/274 with
only type_attributes_test.siox failing identically. Default UI/JSON checks pass
(71 cases each, all 56 codes and both suggestion migrations). The known-failing
owned service was intentionally stopped during the bitpack corpus phase, not
accepted. Its preserved stage logs are
`/home/max/.cache/siox-jobs/foundations-types-strings-pre-span-fix-20261114`.

The pending correction passes the original enclosing SysAttr span to the shared
aggregate-attribute evaluator rather than joining a substituted source base to
a library token. New coverage exercises both file orders, successful Unicode
shape reads, unsupported/runtime reads and retained arithmetic-error origins.
Span invariants, source identities, shared evaluation and native assertions are
unchanged. That correction and the complete Rust/integration/DWARF, ACTUAL
corpus, UI/JSON, native and waveform matrix require a fresh gate. No completion
estimate increase, commit or push is claimed; all five sections remain in scope.

Isolated followup (2026-11-14, not yet verified):
`/home/max/siox-foundations-structs-next` removes the checker's copied
`structs` registry entirely. Construction, conversion, nominal classification,
member diagnostics, storage checks and struct-Resolve restrictions borrow the
original declarations by resolver identity. Field/base walkers retain cycle
guards instead of clearing a copied cyclic base. The shared declaration index
now queries array representations through aliases and applied/transitive
fieldless bases, reusing existing parameter-ID substitution. Family discovery,
element discovery and width/label reading consume that query; direct bases
remain borrowed. The separate semantic array-family/element compatibility maps
still exist and need their own identity migration.

Two new frontend groups cover same-leaf declarations, reversed module order,
direct/aliased/transitive array labels, applied generic bases, exact source
borrowing, empty-alias storage rejection and invalid source cycles. A native
regression exercises two same-named imported logic-array families,
descending labels, meta-value reads and reactive projections in both import
orders and ordinary/generated-debug builds. Ordinary Unicode arrays retain
their separate native regression. Existing privacy/conversion/cycle
coverage is retained. No native result is claimed before those tests run.
This isolated work neither modifies nor accepts the primary's span-fix
snapshot, whose full gate is now terminal green. No syntax, runtime, dependency
or std encoding changes are involved.
The complete acceptance gate and all remaining five-section requirements still
apply; no estimate increase, commit or push yet.

The isolated IR array-family adapter also consumes the same borrowed query.
Its duplicate spelling/fixpoint classifier is removed, so aliases and applied
bases do not acquire one classification during checking and another during
lowering. Legacy metadata names are serialized only after selecting each
declaration ID. An IR regression checks alias/applied families, unrelated
same-leaf scalar/record types and reversed module order. That addition, like
the native regression and the rest of this isolated batch, remains unverified.

The isolated representation boundary preserves the existing distinction:
ordinary character/integer/real arrays retain their element layouts; nominal
enum/logic families remain packed with source-owned enum metadata. Transitive
labels and enum element lookup reuse the shared query. A separate LLVM-free
IR regression asserts exact ordinary element domains, widths and labels,
plus alias-derived and value-parameterized packed-enum metadata. Earlier test
fixtures wrongly assumed arbitrary scalar/record-array and unknown-type-
parameter newtype bases were legal; they are not. The speculative ordinary
nominal-array lowering branch is withdrawn rather than expanding the language
as part of an internal refactor. These changes need focused and full testing.
Internal layout paths retain their explicit declaration identities during shared
normalization. Tests also pin that borrowing and reject unresolved familiar
spellings; this does not introduce a name-based recovery path.

The retained baseline probe rejects `Family(Record[])` with E-P003 before
emitting metadata, so it does **not** confirm the provisional primary privacy
finding. Recovery tests retain that declaration error while checking that the
rejected nominal array cannot inherit its element's fields, that indexed
private access is still diagnosed, and that a view cannot expose unindexed
element fields. These are defensive recovery checks, not support for such a
declaration or evidence of an accepted-program privacy regression.
Borrowed field ancestry also stops at indexed storage rather than inheriting
an array element's fields. Added helper/view regressions distinguish that
boundary from normal path/generic ancestry and explicit per-element access.

The normal bounded launcher now has optional `--wait` lock acquisition inside
the service, fixing observed starvation between an external waiter's unlock
and the compiler launch. Default busy status 75, literal forwarding and memory
caps are unchanged. Argument-only and real queued-execution/isolation tests
pass; the complete combined compiler gate still requires execution. This
is verification infrastructure, not a new compiler or language feature.

The typed-string followup is now integrated locally, pending the fresh gate.
Decoded literals reuse `Value::Array`: Unicode code points, source-selected
enum members in enum-array contexts, and existing label/direction/length and
binding rules. Three new string groups and all 51 interpreter tests passed in
the isolated preview; native string-backed metadata, static projection and
runtime-indexed array reads in both directions have not run yet.
Enum-refresh regressions now expect ordinary/Unicode string lengths to be
known before normalization, while array slices retain a genuinely deferred
case. Exact source spans, diagnostic order and one-time emission across refresh
remain asserted. General string operators, null-array bounds, packed families
and generic contexts remain open; this does not advance the 86% estimate.

Previous local acceptance (reviewed 2026-11-12, typed values/identity/ranges):
guarded unit `1794150078265241988-398244` completed exited/success/0 on
November 8, peak 1,612,656,640 bytes under the normal 2 GiB cap. The complete
matrix passes 754 frontend and 14 import API tests, both Rust suites (811
default / 790 bitpack library cases, all integration tests and all 23 DWARF
tests per mode), strict all-target/all-feature Clippy, 70 runner tests including
36 waveform-checker tests, and 12 specified example artifacts. Both ACTUAL
274-program corpora pass; each storage mode passes 71 text UI and 71 JSON
cases, all 56 active diagnostic codes and two apply/recompile migrations.
All 68 additional directly executed native/debug binaries and four aggregate
native/VCD runs pass, including exact declaration/value/timing and normalized
cross-mode/debug waveform parity. The scalar binding, same-leaf impl constants
and range-sized testbench regressions are included, not excluded from coverage.

Source fingerprint `79f088631fca9d539b75e0724990abdd90ad23fa4a723a3f41b63a11303278cd`
and actual-corpus fingerprint
`04c355570b5c35d44ceb8a387e3ad735b606b9764886295c178d3d5356ffb4cf`
match before/after the gate and independent November 12 current-state review.
Only acceptance Markdown and coordination-log rotation change afterward. The
paired corpus commit is local `38f4e01`; its 20 original peer files remain
untracked and excluded. The successful gate's temporary timeout override was
removed after completion, without restarting the job or raising memory limits.

This accepts integer/natural/real/aggregate and inclusive-integer-range contexts,
declaration-owned constant materialization, explicit scalar format boundaries,
and implementation-aware operator selection. It does NOT accept the separate
derived/From/record-boundary draft. The engineering estimate stays about 85%
of compiler foundations, not the whole compiler. All five sections remain
required: generic/associated applied contexts, packed/string/open/enum-range
values, structural defaults, suffix-folder migration, remaining declaration
registries and residual name-hook consumers are still unfinished. No push or
wiki publication is implied.

Working followup (2026-11-12, integrated draft; NOT accepted): the isolated
nominal-value changes are now on top of verified checkpoint `d91db7d`, without
copying its stale staged baseline or undoing scalar/range/constant-ID fixes.
Nongeneric derived structs reuse one-base Record storage; Cow projections walk
checked original-owner ancestry, preserve enum variant identities and reject
sibling conversions. Inherited fields validate each declared base. Ordinary
record boundaries also check field counts/domains recursively, retaining Arc
storage for unchanged NaN/signed-zero data.

Explicit constructors select the registered From trait's explicit/default body
before automatic derivation. Static Self retains the implementation owner;
same-instance default constructor recursion can reach the raw representation.
Missing/effectful/ambiguous or unresolved generic bodies stay nonconstant rather
than silently deriving. From does not supply implicit Into conversions. Operator
selection checks resolved identities against registered language roles,
including seeded builtin trait identities that retain their roles after a
library declaration is registered. Unregistered same-named Eq/From traits
cannot impersonate the differently named registered Equal/Make.

Six frontend groups, the enum-alias resolver regression, all 47 consteval groups
and legacy/shared suffix probes pass in preview 1794472043927530646-1792921.
Its full frontend run has 759 passes and two failures from strict hook-ID
selection dropping seeded builtin identities. The local repair uses the
resolver's registered-role table and adds explicit/default From fallback and
unregistered-impostor coverage; this newer snapshot is awaiting guarded testing.
The native ordinary/debug x module-order integration test has not run yet.
The suffix probes are NOT the required full-corpus compatibility oracle:
the production integer/real suffix folders and fractional scale shortcut remain.
Generic/array-based derivation, generic callable contexts and the remaining
five-section work are still open. Another full frozen gate is required before
this followup can be committed; it is excluded from the acceptance above.

Combined registry draft (2026-11-12, UNACCEPTED): the reviewed registry changes
are now integrated with the nominal-value/alias/registered-hook fixes above,
before a fresh combined validation matrix. Only our blocked 64 MiB launcher
was stopped for this intentional input change; no build or peer job was stopped.
The earlier partial results do not accept this larger source snapshot.

C aggregate checking borrows base/field contracts by its checked Named DefId;
shared normalization makes transparent fixed-array alias chains behave like
their representation. The persistent string-keyed struct_field_types graph is
deleted. Finite-layout analysis borrows structs from the declaration index,
computes which generic parameters are actually stored, then builds a local
DefId containment graph. Unused parameters do not create false cycles; stored
nested arguments, aliases, tuples, arrays and bases do participate. Breadth-first
reporting keeps the shortest cycle's first field/base source span. Base-only
cycles already diagnosed by resolution do not cascade.

The duplicate generic_fns signature registry is also deleted. Bounds checking,
body instantiation and return inference borrow FnDecl contracts rather than
cloning parameter lists/types at collection and every call. Positive module
provenance comes from Item::Fn; foreign/bare prototypes stay available through
the general accessor without acquiring a module-body role. Re-insertion keeps
known module provenance. New regressions cover borrowed identity, forward and
foreign contracts, same-leaf/inferred returns, reordered named generic arguments,
finite nested/unused arguments, transitive forwarding, aliases and source spans.
A native ordinary/debug fixture exercises finite generic records through real
hardware ports and changed simulation values, with waveform parity.

All this registry work is still UNCOMPILED. A new combined frontend/native
preview and full ordinary/bitpack Rust/DWARF/corpus/UI/JSON gate are required.
Other copied struct/full-field/visibility tables, generic constant contexts,
additional value representations, suffix migration and hook cleanup remain.
This does not complete registry migration or any remaining section by itself.

IR materialization followup (2026-11-12, UNCOMPILED): the combined preview
passes all 768 frontend tests and strict frontend Clippy but stops in the first
native derived fixture. IR's legacy collector re-lowers constructors instead
of consuming typed nominal/natural/real results. A new declared-constant entry
point reuses the same interpreter and WordConstants parameter/attribute context.
IR projects its supported checked values against the existing SourceLayout into
the existing scalar/real/field/table storage, without another expression walker.
Nominal bases unwrap only at this physical boundary; record fields, tuple slots,
enum encodings and array labels/direction are retained. Registered range values
also populate the existing range adapter. Unsupported packed/open/generic
representations keep their legacy consumers pending their separate migration.

The positional array cache now accepts only already ascending zero-based runs;
other arrays retain ordered labelled leaves instead of being silently sorted.
New IR/declaration-entry checks and native dynamic descending/nonzero-index
assertions accompany this change. These repairs are NOT yet compiled or
accepted; another combined preview and the complete frozen gate remain required.

Local acceptance (2026-11-06, combined main integration and identity fixes):
the integration combines accepted foundations with main `801ec40`, including
implicit/static receivers, generate processes, timed waits and entity methods.
All original conflicts are resolved; #399's hardware-match fix is ported without
restoring leaf-name declaration lookup. Newer main is not fully merged, and
the isolated numeric followup is not part of this accepted snapshot.

Into selection retains complete checked source/target contracts and carries
the selected method DefId into both lowering paths. Qualified aliases normalize
through the existing layout path. Inference and diagnostics share declared
index bounds, including nonzero/negative labels on plain, nested and entity
arrays. Entity-array slots all exist; E-P022 is retired/reserved, omitted wiring
uses defaults/W-P021 and invalid labels remain E-P003. Affix tables borrow
normalized targets and suffix functions/bodies/parameters; both lowering
consumers select the suffix method and consume checked result identity. Generic
body memoization uses function/binder DefIds and full checked types instead of
display names. Same-leaf owners, aliases and reversed module/call orders have
regressions. Shared constant match/range evaluation preserves source ordering.

Frozen gate 293943 completed `exited/success/0` within the normal 2 GiB guard,
peak 1,643,999,232 bytes. It passes 732 frontend and 14 import API tests, both
complete Rust suites (789 default / 768 bitpack library cases and all 23 DWARF
tests per mode), strict all-target/all-feature Clippy, 64 runner tests and
12 specified example artifacts. Both actual 273-program corpora pass. Each
storage mode passes 71 text UI and 71 JSON cases, covers all 56 active codes
and applies/recompiles both migrations. Both 26-function native API groups and
all 56 directly executed native/debug/VCD runs pass. Affix and generic-cache
regressions are verified on this combined source, not just their prototypes.

Source fingerprint `3202805696c2bd2bf30d02880c0ffd1ca72b9d2abd718ed35d7661071fec2a2b`
and actual-corpus fingerprint
`1fb5b0208ca0eb19c0cb7d06b09f0886aa7dfdaa167cb8c7f7cdba665f61b9a9`
match before/after validation and the independent current-state review.
Only acceptance wording and coordination logs change afterward. Local commits
are permitted by this gate; nothing is pushed or published to the wiki.
The paired corpus migration is local commit `d548a52`; its 20 original peer
files remain untracked and excluded from the commit.

The engineering estimate remains about **85% of compiler foundations**, not
the whole compiler. All five sections remain required. Fully typed values and
attributes, remaining declaration registries and residual hook consumers are
not complete. Generic conversions need per-instantiation lowering facts, and
the old scalar suffix folders still need migration to the shared evaluator.
The separate real/natural draft is not claimed accepted here.

Local followup in progress (2026-11-06): the integration now combines that draft
with owned aggregate values in the existing interpreter. Arrays retain element
types and declared labels/direction; records retain nominal owners and declaration
order; tuples retain their element types. Storage is shared through Arc, and
unchanged nested conformance keeps its allocation even with NaN/signed-zero
elements. Typed module constants, arguments, locals, assignments and returns
use the same borrowed declaration contracts. Static array metadata falls back
to shared values only when the existing stage adapter cannot answer; Boolean
direction comes from the registered enum. Integer/natural/real domains stay
distinct, including checked explicit conversions and contextual numeric literals.
The combined snapshot is UNACCEPTED until its focused/full Rust, native,
actual-corpus and diagnostic gates pass. The earlier isolated runs compiled,
but stopped on fixture phase/privilege errors and an attribute failure-reason
regression; those repairs remain in the combined regression suite. Generic/
packed/range values, structural defaults, further statements/attributes and
remaining registries/hooks are still required. This is not five-section completion.

Followup preview (2026-11-08): guarded job 4010271 completed successfully within
the normal 2 GiB limit. All 750 frontend tests, 40 shared-evaluator tests and
strict frontend Clippy pass. The actual aggregate-constant fixture runs in all
four ordinary/debug and default/bitpack combinations; all four DUT signal
declarations and exact value/timing sequences agree across modes. Constants
retain their declared array labels and nominal scope through Process IR;
operator expansion retains the selected implementation when multiple instances
share one trait-default function. This is preview evidence, not full acceptance.
The VCD profile and negative tests are now versioned; all 36 checker tests pass,
including missing declaration metadata and unchanged two-value reader behavior.
The complete Rust, DWARF, actual-corpus and diagnostics matrix must pass on this
final source before committing. Generic/associated-call contexts, additional
constant value forms and the remaining registries/hooks are still open.

Full-gate followup (2026-11-08): unit 4112157 additionally passes all 23 default
DWARF tests, but the default Rust integration suite exposes #247: the `4kB`
constant's minimal 13-bit executable value was assigned its declared 64-bit
integer-backed scalar layout without an actual binding boundary. Stop that
owned failed matrix; its dead service is not a passing gate. The repair inserts
an explicit scalar RawResize only when a known initializer width differs from
the declaration, preserving the initializer's original width and leaving
aggregate validation strict. A reversed-module-order IR regression covers
custom suffixes, aliases and fractional time, including positive values whose
highest minimal-width bit is set. It compiled, but its first validation call
incorrectly passed zero signals for a design containing a DUT. The fixture now
supplies the actual signal count and reports all validation errors. Rerun that focus and
issues_243_249 in both storage modes before the complete frozen matrix. No
acceptance/commit/push is claimed, and the isolated newtype draft stays separate.

Constant identity/range followup (2026-11-08): the next complete matrix passes
both Rust suites, all 23 DWARF tests per mode and strict Clippy, but both actual
corpora stop at 271/274. The remaining hardware paths publish impl constants
under bare names even though reads now use resolved declarations. Both
declarations and reads now use the same declaration-ID key in existing legacy
tables, so same-named module and impl constants cannot capture one another.
Inclusive integer ranges now reuse the registered range record in the shared
typed evaluator; aliases, pure helper returns and array bounds retain endpoint
order. Process declarations/layouts consume those shared range values. No
separate range-value registry or new constant-value variant is added. The text
UI fixture changes only the ordering of three unchanged enum diagnostics to
source order; error counts/messages/spans are retained.

Focused unit 392171 completed exited/success/0, peak 1,550,114,816 bytes:
754 frontend tests, strict frontend Clippy and all twelve directly executed
regression binaries pass (three previous corpus failures x ordinary/debug x
default/bitpack). New frontend coverage checks same-leaf module/impl identities,
per-instance renamed parameters, range aliases/helper calls, negative/singleton
endpoints, nominal lookalikes and effectful bounds. This is preview evidence;
the complete frozen matrix must rerun before any local commit. Open/enum ranges,
packed/string values, generic contexts, structural defaults, remaining
registries and hook consumers are not complete. The estimate remains about
85% of compiler foundations; the separate derived/From draft is still uncompiled.

Local acceptance (reviewed 2026-11-04, checker alias registry migration): shared
declaration normalization replaces the checker's copied alias table and separate
expansion walkers. Frozen gate 2281715 passes both complete Rust suites
(768 default/747 bitpack library cases), 711 frontend cases, strict Clippy,
64 runner tests, 56 direct native/debug/VCD runs, five native API cases per mode,
12 artifacts and both actual 273-program corpora. Each mode passes 66 UI and
66 JSON cases covering all 54 active codes and two apply/recompile migrations.
The source/corpus fingerprints still match the successful gate's before/after
values at review. The estimate remains about **85% of compiler foundations**;
fully typed constants, other registries and residual hooks are not complete.

Previous local acceptance (2026-10-30, borrowed enum members, alias-qualified values and
lexical associated owners): the checker's final enum-name compatibility table
is removed. Both complete Rust suites pass (765 default/744 bitpack library
cases, all 23 DWARF tests per mode), 708 frontend cases, strict Clippy, 64 runner
tests, five native API cases per mode, 164 direct native/debug/VCD checks,
12 artifacts and both actual 273-program corpora. Each packing mode passes
66 UI and 66 JSON cases, all 54 active codes and two apply/recompile migrations.
Frozen gate 1939640 is terminal successful with matching source/corpus hashes.
The engineering estimate is about **85% of compiler foundations**, not the whole
compiler. Fully typed values/static attributes, remaining registries, callable
availability and residual hooks remain open; this is not five-section completion.

Previous local acceptance (2026-10-29, shared scalar contexts/local statements, typed
initializer/storage boundaries, shared enum inheritance and selected packed
elements): both complete Rust suites pass (757 default/736 bitpack library cases,
all 23 DWARF tests per mode), 700 frontend cases, strict Clippy, 62 runner tests,
136 native/debug/VCD checks, 12 artifacts and both actual 272-program corpora.
Both modes pass 66 UI and 66 JSON cases covering all 54 active codes and two
apply/recompile migrations. Source/corpus fingerprints match before/after frozen
gate 1055479. The engineering estimate is about **84% of compiler foundations**,
not the whole compiler. Fully typed values, remaining registries and residual
hooks are still open; this accepts a slice, not all five sections.

Previous local acceptance (2026-10-28, generated enum member identity, shared enum phase facts,
typed enum/character constants and source-owned
operator/condition tables, applied layouts and known trait value types/arguments/signatures,
borrowed contracts/default visibility,
selected enum owners, range contracts, Boolean identities and resolved aliases):
both complete Rust suites pass, including 719 default/698 bitpack library tests
and all 23 DWARF tests per mode. The gate also passes 662 no-LLVM library tests,
strict Clippy, 52 runner tests, 128 ordinary/debug native and VCD checks, and both
actual 271-program corpora. Both modes pass 66 human UI and 66 JSON/schema cases,
cover all 54 active codes and apply/recompile both migration fixtures.
Compiler, corpus and local wiki fingerprints match before/after the frozen
gate. Retained evidence and fixture migrations are recorded in
[chat.md](../../chat.md). This is local acceptance, not a claim that the remaining
proposal sections are complete.

Upstream integration acceptance (2026-10-25): the same full matrix passes with
compiler main `a1c5d0f` and paper main `e8e995e` merged into the owned proposal
branches. This includes the new natural/math, C ABI, void-extern effect and
macro-expansion API tests. Shared declaration contracts remain borrowed; the
effect call graph does not restore the removed cloned signature/constant maps.
Source, actual corpus and paper fingerprints again match before/after the gate.

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
| `resolve.rs` | `check_generic_arity` width folder | removed; `types/layouts.rs` uses shared analysis |
| `elab.rs` | `eval` | shared-interpreter parameter adapter |
| `ir/lower_helpers/builders.rs` | `eval_const`, `eval_const_fns`, `eval_const_stmts` | shared-interpreter binding/function adapters |
| `ir/lower_helpers/substitute.rs` | `fold_const` | already an IR evaluator adapter |
| `ir/lower/collect.rs` | `fold_const`, `eval_const` | aggregate collection and an IR evaluator adapter |

The traversal and a borrowed declaration index are now shared. Free/foreign
function contracts, constants and transparent aliases are selected by `DefId`.
Checker analysis and elaboration can evaluate integer-signature free functions;
elaboration also supplies parameter/loop values, and IR supplies bound
attributes and legacy scalar values. Enum/character metadata and IR constants
now also share typed evaluation; uniform checker/elaboration contexts, packed/
real values and attributes remain necessary so constant status does not depend
on stage.

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

Calls borrow the root environment and resolved parameter IDs, allocating only their
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

**Implemented typed enum/character migration (2026-10-27).** The
same interpreter now carries character values and enum discriminants with
their owning `DefId`, rather than treating every scalar as integer bits.
Function frames bind resolved parameter/receiver IDs. Declaration types and
helper return types supply character-literal context, including aliases and
`Self`; a qualified variant cannot be captured by a same-named parameter.
IR enum/character constants and `LogicEncoding`/operator tables use this path.
Enum comparisons yield the registered Bool's source values, and logical
operators and non-Bool conditions execute their source operator/`Boolean` impls.
An unrelated enum's `true`/`false` member names are not Boolean flags. The
full comparison gate passes both complete Rust suites and actual 270-program
corpora, plus native/VCD and diagnostic checks, with all three frozen snapshots
unchanged. Where source identity or source Boolean behavior fixes an old answer,
the temporary oracle requires an observed semantic difference rather than
silently exempting unknown results. After this comparison, the old table walker
and its oracle are deleted. Final oracle-free validation passes the same full
matrix, again with identical compiler/corpus/local wiki fingerprints. Retained
comparison and final evidence is recorded in `chat.md`.

The new native scalar-wire fixture covers helper/constant-backed logic,
nonstandard discriminants, parallel resolution and reactive X/Z waveforms.
Initial packed-wire exploration also exposed pre-existing default-logic and
four-bit companion assumptions (including a five-bit custom enum indexing
width mismatch). Generalizing that representation is still open; scalar
constant support does not claim arbitrary custom packed encodings work.

**Implemented shared enum phase facts (2026-10-28).** The declaration index
borrows enum and implementation contracts, including inherited source members.
Analysis owns computed discriminants keyed by enum-owner and member `DefId`;
`Typed` and elaborated `Hierarchy` share them through `Arc`. Checker diagnostics
and IR symbols, defaults and widths consume these facts. After attribute/length
normalization, only unknown values are refined and newly decidable errors are
reported once, in source order. An unknown explicit value no longer silently
restarts implicit numbering at zero; a known explicit value can reset it.

The shared interpreter retains symbolic enum member identity independently of
physical encoding. This lets conditional helpers work through supplied function
contracts without loading unrelated provider ASTs, and lets Bool's own encoding
helpers branch before those encodings are known. Source operators still execute
their registered bodies; unknown ordering and non-Bool lookalikes stay unknown.
Native coverage checks helper-backed values/defaults, inherited owners,
post-normalization values and reactive symbols. Full comparison gate 3007314
passes both complete Rust suites and actual 271-program corpora, all 128 direct
native/VCD checks and both 66-case diagnostic matrices, with unchanged source,
corpus and local-wiki fingerprints. The temporary audit and old enum indexing,
numbering/default/width calculations are deleted. Final oracle-free gate 3109217
passes that same full matrix with matching fingerprints. Only proposal
acceptance wording and coordination logs change afterward. This accepts the
shared phase-fact migration, not the remaining typed contexts or registries.

**Implemented generated enum member identity (2026-10-28).** Real repeated
macro expansions reuse member declaration spans. Contract and diagnostic queries
now select the resolver's retained `(owner DefId, member)` entry rather than
the last declaration recorded at that span. Regressions check source pointers,
member parent IDs, inheritance, helper-backed numeric ordering and an earlier
owner's negative-value diagnostic. The native fixture explicitly declares its
numeric `Ord` implementation; this does not claim general source-operator
dispatch is complete in every constant context. Focused frontend/native/debug/
waveform checks pass in both storage modes. Full frozen gate 3243634 passes
the complete matrix above, with identical source/corpus/local-wiki fingerprints
before and after validation. Only acceptance wording and coordination logs
change afterward. General macro expansion identity remains separate work.

**Implemented shared scalar contexts and local statements (accepted 2026-10-29).**
One borrowed `Source` context supplies declaration-ID-selected constructors,
operators and Boolean conditions to the shared interpreter. Checker numeric
queries, elaboration parameters/widths/generate conditions and IR word/call/
enum metadata queries use it. Receiver methods borrow the same contracts,
preferring inherent bodies over trait candidates. Registered hook identities,
rather than unrelated leaf names, select source semantics. Integer projection
happens at numeric or stored-word boundaries, not before argument evaluation.
Temporary duplicate contexts, comparison APIs and audit counters are removed.

Initialized scalar locals and immediate local/parameter assignments retain
resolved IDs, lexical cleanup and isolated invocation frames. Enum/character
locals preserve their declared owner, including aliases and `Self`.
Uninitialized slots cannot fall through to caller bindings. Explicit non-scalar
signatures/local annotations stay unknown to this scalar interpreter rather
than acquiring fabricated integer semantics. Nonlocal/delayed writes and
structural defaults are not supported by that service.

Previously supported packed initializer calls instead reuse existing typed
source lowering and the canonical Process integer folder; this is not a second
AST interpreter or complete packed constant support. Collect declarations and
enum/logic/array representation facts before folding constants. Bind concrete
arguments and local assignments at their declared formats, while unconstrained
parameters retain actual argument shapes. Reuse identical representation kinds
despite different declaration spans without merging nominal types, widths or
directions. Ordinary runtime calls retain their source/debugger boundaries;
only actual constant initializers may fold those calls away.

The Process folder reads declared layouts for resizes/slices, distinguishing
packed wrapping from checked natural/ranged values. Discarded dynamic/foreign
arguments and explicit metavalue planes do not qualify as closed integer
constants. Multiword accumulation uses checked multiplication, not shifts that
can discard high bits. Values beyond the host query domain remain unknown;
their original arbitrary-width native IR is retained. No source width or ABI
word-count limit is introduced.

Typed storage binding shares the canonical dependency-ordered width calculation.
An append cursor analyses new nodes using existing width fields; arena rewrites
invalidate it. Already-wide arithmetic avoids redundant widening slices, while
small negative values retain contextual widening. This fixes the 30-alias
emission stack overflow without an LLVM workaround or increased stack limit.
Reconstruction also propagates the existing full-element fact through selections,
so a checked packed-index mux cannot truncate an enum discriminant through its
former one-bit binding. Explicit raw-bit selections keep their storage boundary.

**Verification.** Frozen full gate 1055479 finishes `exited/success/0`, peak
1,613,459,456 bytes within the normal 2 GiB guard. It passes 700 frontend cases,
both complete Rust suites (757 default/736 bitpack library cases, all 23 DWARF,
18 hardware-source and eight recursion cases per mode), strict Clippy,
62 runner tests, 12 Phase 1 artifacts, 136 directly executed native/debug/VCD
checks and both actual 272-program corpora. Each mode passes 66 UI and 66 JSON
cases, covers all 54 active codes and applies/recompiles both migration fixtures.
Source/corpus fingerprints match before and after. Detailed failed attempts,
focused proofs and the final evidence paths remain in [chat.md](../../chat.md).

Native coverage includes reversed source ordering and local assignments
(runtime 13 to 21 at 1 ns while constants/generation remain 13/14), packed
initializer values eight/144 and a three-bit local wrapping to zero, computed
width truncation, source-call stops and saved wide aliases, multiword values
above `u128`, enum derivation/identity and static/dynamic/nested/multiword packed
reads with metadata-only reactive updates. The packed-index VCDs match in all
four builds. Strict waveform mutation tests reject missing signals, wrong widths/
values/times and missing/reversed delta updates. Nonstandard-encoding and raw-bit
control cases pin reconstruction without hardcoding production discriminants.

**Remaining.** Unify callable/attribute availability through resolved contracts.
The real/natural/aggregate and inclusive-integer-range followup is accepted as
recorded above. Support packed/string values, open/enum ranges, structural
defaults and additional pure-function statements. Supply all statically sized
attributes, including port/local environments for issue #295; the current IR
adapter only reads already-bound field/constant-index attribute paths.
Associated/generic callable availability and arbitrary typed metadata are still
open. Audit bound-word projection so unsupported module constants cannot become
untyped scalar parameters, while valid explicit packed-to-integer metadata
conversions keep working. All five proposal sections remain in scope.

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

**Implemented resolved layout widths (2026-10-27).** Width arithmetic no longer
belongs to `resolve::check_generic_arity`; that pass keeps names and argument
counts without collecting literal values or a second declaration-shape map.
`types/layouts.rs` uses the shared evaluator and prepared parameter-ID bindings
for struct fields/bases, entity ports and applied aliases. Struct source
contracts join the borrowed declaration index, not another copied registry.
Type arguments are checked in declaration order rather than HashMap order.

A root-type mode of the existing shared AST walk validates every source type
use once, including constructors, associated-call qualifiers, trait arguments
and otherwise unused generic bodies. Nested types use the layout checker;
repeated valid applications of the same type are not mistaken for recursion.
Definition errors are not repeated at every application, invalid aliases keep
the normalizer's diagnostic, and argument-induced failures label the source
layout while pointing to the application. Constants, integer-signature calls,
named forwarding, transparent aliases, derived types, range counts and checked
signed-division boundaries have regressions. Unknown symbolic widths remain
unknown; negative labels in an inclusive directional range remain valid.

Before removing the old folder, a temporary successful-answer oracle inspected
every source generic application through the complete Rust suites and actual
269-program corpora in both storage modes. All old successful answers on valid
applications agreed. The old signed-minimum division panic has no successful
answer; the new wider analysis reports its out-of-layout result instead.
The new native fixture checks a named function width, applied record alias and
reactive three-bit waveform. UI/JSON coverage pins eleven width errors and an
intentional incomplete-literal warning. After deleting the temporary oracle,
the complete Rust/native/corpus/diagnostic matrix passed again with matching
compiler, corpus and local wiki fingerprints. Retained evidence is in `chat.md`.

Enum operator tables now use that same interpreter with typed enum owners;
there is no separate enum expression walker. Do not mark the proposal complete
merely because traversal is shared: uniform stage contexts, fully typed values,
the remaining registries and residual hook consumers are still open.

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

**Generic binder followup (accepted locally 2026-11-12).**
Generic-body type bindings now use resolver parameter `DefId`s instead of
names. Direct parameter matching and return/array/tuple inference use those
same identities. Ordered instantiation keys are built during binding; a
missing identity disables caching without skipping other known arguments or
concrete body checks. No same-name fallback is introduced. The generic-scope
walk state retains only the Boolean its static checks consume, removing
cloned parameter-name sets while preserving every nested save/restore boundary.
Four regression groups cover distinct same-name function/struct parameters,
unresolved paths, array/tuple inference, nested/recursive calls, module order,
incomplete binder identities and generic-static checks without scope leakage.
This builds on the pending borrowed-signature migration, not a restored
`generic_fns` registry. The latest full acceptance above validates this migration,
not the other declaration registries or all applied generic contexts.

**Implemented first registry.** `resolve::declarations::DeclarationIndex` borrows
module constants, free/foreign functions and transparent alias contracts,
keyed by `DefId`. The checker reads contracts/initializers directly instead of
keeping separate `generic_bodies`, `const_types`, `const_values`, `fn_arity`,
`fn_param_types` and `fn_return_types` maps. IR's separate free-function map is
gone; its existing API delegates to this index. The same item collector builds
the index in each immutable phase, after source rewrites rather than retaining
references across a mutable AST pass. `Typed` therefore keeps its owned side
tables without acquiring a source lifetime.

Struct declarations now join that index for applied-layout queries. Bases,
fields, ports, alias targets and parameter lists remain borrowed source nodes;
pointer regressions pin the contract. Existing computed struct/member facts
and other remaining registries still need their own identity migration.

**Borrowed views (local integration 2026-11-19, full gate pending).**
`Checker.views`, `Checker.view_dirs`, `Lowering.views` and `Lowering.view_dirs`
are removed. Resolution records an overload-group ID on the actual view
declaration path; same-name roles in separate modules remain separate groups.
The shared declaration index borrows original ViewDecl/target/field nodes and
selects an applied role by group and normalized backing owner, following
`Indexed(View(...))` elements through aliases. Ports retain the selected view's
DefId; input-write checks, persisted layout directions and actual native port
wiring consume its source fields. Ambiguous bare roles remain unknown. Legacy
method/output descriptors are rendered only after selection, not used as view
contract lookup keys. Other method/trait/operator/IR registries and complete
generic/value applications remain required; this is not all registry completion.

Tests retain source-pointer assertions, reversed module/import order, overloaded
groups, backing and whole-role aliases, generic/array shapes, duplicate normalized
owners, input writes, layout metadata and native reactive wiring. Retained failed
attempts caught the real resolver registration path and array-element AST shape;
neither assertion was removed. Focused unit `1795080323550328579-1496036` is green
as recorded above. Acceptance still requires the complete combined Rust/native/
ACTUAL-corpus/debugger/UI/JSON matrix with unchanged input fingerprints.

Local integration followup (2026-11-06, awaiting the full gate): affix tables
borrow their implementation's target Type; suffix inference normalizes that
contract through the shared declaration index rather than scanning names.
Process suffix entries borrow functions, bodies and parameters and compare
normalized target DefIds. Both lowering paths select the suffix method rather
than a helper, resolve input aliases and consume checked result identity.
Generic-instantiation memoization likewise uses function/binder IDs and full
checked Ty arguments, not diagnostic display strings. Regressions preserve
aliases, same-name nominal/array families, ordering, diagnostics and source
pointers. Missing binder identities disable caching rather than validation.
General generic value arguments, prefix result defaults, old suffix scalar
folders and the other registries remain open; this is not five-section completion.

**Pending Into identity migration (2026-11-05, not accepted).** The local
integration replaces the qualifier-stripping conversion signature set with
checked `(target, source)` contracts indexed by the `into` method's `DefId`.
`Typed::Conversion` carries that selected ID into both native lowering paths;
the function index no longer reconstructs an Into body key from type leaves.
Textual layout projections retain qualification and are not selection authority.
Collection follows array-family/element discovery, so a forward-declared nominal
family cannot be frozen as a scalar. Unconstrained shapes retain the storage
site's widths, while concrete contracts check their declared widths and element
identities. Tests cover equal leaves, aliases, both module orders and native
ordinary/debug hardware/procedural stores and returns. These new tests and the
diagnostic snapshots are awaiting guarded validation; the earlier ten native
passes do not validate this newer change. Other registries remain open.

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
names; entity aliases normalize to that entity head, while other transparent
named-target aliases still need migration. Type attributes
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
No new implicit binder or grammar rule is introduced.

**Implemented borrowed trait contracts (2026-10-25).** The shared declaration
index also borrows trait declarations by `DefId`. Resolution reads requirements,
receiver presence and parameter counts from it once; type checking no longer
repeats missing-method diagnostics or maintains separate required/default/
visibility contract maps. Defaults retain source identity through import renames
and equal trait leaves. Computed method facts are still owned by the checker;
there is no cloned trait tree or extra copied default-signature registry.

Inherited instance and associated defaults now retain their trait's visibility.
A red-before/green-after test exposed private defaults callable outside their
module. Public and module-local access remain valid; existing inherent or
explicit implementations keep their visibility and signatures. Tests also cover
pointer identity, rebuilding after source rewrites, distinct contracts/default
result types and one diagnostic per missing method. Complete generic trait
signature validation and other registries remain open. The complete
Rust/native/corpus/UI/JSON matrix passes with all three frozen fingerprints
matching before and after validation; this does not complete the proposal.

**Implemented known trait signature conformance (2026-10-25).** Type checking
compares declared parameters/results with the selected source trait signature,
using the shared parameter-ID substitution and alias normalizer on both sides.
Checks include scalar/nominal/array/tuple types, known widths, named trait
type/value arguments, `Self` and value-versus-void results. Assignment's
integer/vector and nominal-family coercions cannot satisfy a signature. Array
labels retain the existing compatibility rules; unknown widths remain unknown.
Synthetic `Self` is accepted as a type argument without admitting value locals.

Core declarations now describe existing behavior: `From<Source>` takes `Source`
and returns `Self`; affix hooks return `Self`, retaining their intrinsic empty
defaults. Unrelated user traits named `From` are unchanged. Method-generic
specialization, symbolic width relationships and complete applied nominal
argument identity remain follow-ups rather than spelling-based guesses.
The new source-labelled UI/JSON case and the complete Rust/native/corpus gates
pass in both storage modes with matching compiler/corpus/paper fingerprints.
Invalid receiver/arity shapes do not cascade into dependent signature errors.
No new grammar, runtime, LLVM or source value semantics are added.

**Implemented structural trait argument validation (2026-10-26).** The shared
parameter-ID binder now distinguishes structural count, unknown-name,
duplicate and type/value-kind failures. Resolution reports these at the
application and labels the selected source declaration. Sparse explicit
parameter-kind facts preserve type/value roles through renamed generic
forwarding, while recovery bindings remain unknown. Alias normalization uses
the same binder without a second argument interpreter. Invalid applications
do not cascade into dependent receiver/arity or semantic signature errors.
The complete Rust/native/corpus/UI/JSON gates pass in both storage modes with
matching compiler/corpus/paper fingerprints. This does not finish semantic
value-argument typing, method generics, symbolic width
relationships or full applied nominal argument identity.

**Implemented known trait value-argument types (2026-10-26).** Type checking
prepares one parameter-ID binding context per implementation and shares it
between dependent value bounds and method signatures. Value expressions stay
borrowed; source-pointer tests cover that contract. This removes repeated
binding of the same arguments for every parameter/result comparison.

Known value arguments use ordinary expression/assignment-context checks,
including aliases, constant/function contracts, renamed value forwarding,
nominal identity, contextual tuple/array/character literals and existing literal
integer ranges. Bad value types suppress dependent method comparisons. Opaque
builtin recovery names and unresolved generic bounds remain unknown rather
than acquiring guessed semantics. No second constant evaluator or runtime/LLVM
path is added. The source-labelled UI case and complete Rust/native/corpus/
diagnostic gates pass in both storage modes with matching source/corpus/wiki
fingerprints. A core-less array/tuple regression keeps nested opaque facts
unknown, not just top-level builtin recovery names.
Complete constantness, method generics, symbolic widths and applied nominal
argument identity still need work.

**Transparent alias normalization.** The raw declaration index now belongs
to `resolve`, so metadata does not need an upward dependency on `types`. One
normalizer follows resolved alias IDs and preserves applied generic arguments,
index wrappers and the original expression spans. Unchanged types and bare alias
targets are borrowed; only a changed outer wrapper is copied. Cycles and missing
identities yield no invented entity. Resolver products retain derived
alias-to-entity IDs (not cloned alias trees) for metadata and ordered process
classification.

Checking, structural-root selection, metadata fallbacks and named entity targets
use those identities; elaboration and IR consume the canonical shape when binding
parameters and collecting arrays. A caller's same-named constant cannot capture
an argument written in the alias definition. Alias instances remain structural,
so `const` and function/process placement checks apply to them too. Testbench
lowering uses the existing generate collector for assignment-style array DUTs,
and constructor statements share ordered connection lowering instead of becoming
packed data assignments. Fixed named element connections retain runtime source
order. Dynamic destinations whose runtime connections cannot be routed remain
an explicit `E-P017`, never silently discarded calls; full dynamic constructor
routing is still open.

Generic aliases now remain declarations through raw resolution: parameters and
their uses receive identities in the alias's scope. `resolve::normalize_aliases`
then instantiates applications through the same declaration index, before
metadata attachment and type checking. Substitution selects parameter `DefId`s,
not equal spellings, so private constants/functions keep their definition-site
bindings and arguments keep their caller bindings. Type/value kinds, names,
duplicates and arity are checked; already-reported count errors do not cascade.

Matching postorder read/write AST walks collect only changed owned shapes while
contracts are borrowed, then apply them without copying whole modules. Queues
distinguish the rewritten node kind and preserve repeated-span traversal order,
including tuple `let` projections. Actual nested type arguments normalize before
the definition-cycle guard, so `Select<Select<integer>>` is not a false cycle.
Alias arrays, view targets, tuples, trait arguments, constructor chains and integer-signature
classification reuse the same normalizer. Old ordinary-alias substitution and
declaration removal are gone from `syntax::imports`; only the separate
format-struct syntax rewrite remains there. Typed evaluation, additional
declaration registries and dynamic constructor routing are still open.

Non-generic constructors use this resolved boundary too. This removes syntax's
second alias-target map, cloned per-module scopes and constructor qualification
walk, and prevents a private width function/constant from being rebound to an
equal name in the caller. Standalone ordinary aliases keep their existing type
checking behavior; this does not change unconstrained-array sizing rules.

Recursive aliases hidden inside tuples or transformed generic arguments are
checked through that same normalization path, not just the derivation-head
graph. This retains the existing rejection of `type Tree<T> = (T, Tree<(T, T)>);`
after deleting syntax's early expansion. The complete Rust and corpus gates
cover that rejection alongside valid finite nesting.
Like direct derivation cycles, nested alias cycles are invalid even when unused.
Import tests isolate their intentionally recursive declaration in a negative
module rather than loading it as a dependency of unrelated positive fixtures.

The AST walk includes trait implementation arguments as well as method
signatures, so an operator's aliased input/output contract is materialized at
the same boundary. Raw traversal and native addition regressions cover both.
Parameter-kind classification follows resolved trait heads through applications,
so `T: Trait<Args>` remains a type parameter rather than a value parameter.

**Implemented selected enum owners (2026-10-25).** A derived enum shares its
base's variant declarations, not its nominal identity. Resolution now records
the selected owner when it differs from the inherited variant's parent; ordinary
variants reuse that parent without an extra entry. The shared owner query feeds
typing, pattern checking and IR, including when generated qualifier/path spans
coincide. A red-before/green-after regression covers explicit macro paths;
patterns, IR keys and sparse storage have additional checks. Boolean literals
use the same recording path. No enum bodies or value tables are copied, and no
new surface syntax is introduced. Both complete Rust suites, both actual
267-program corpora, 84 native/VCD checks and UI/JSON gates pass with exact
compiler/corpus/paper fingerprints. This does not complete the other registries.

**Implemented shared enum inheritance contracts (accepted 2026-10-29).** The checker no
longer owns separate own-variant/base-name tables or expands inheritance again.
Its compatibility variant names come from shared owner/member facts, including
members whose discriminants remain unknown. Conversion queries receive checked
types; enum ancestry follows declaration IDs and compares inherited member IDs,
not matching variant spellings. The remaining effective-variant name table and
other string-keyed registries are still open identity migrations.

Regressions cover transparent alias bases, renamed imports, equal enum/member
names across modules in both source orders, unknown discriminants and coincident
generated member spans. Combined focused validation and the subsequent full
frozen gate 1055479 pass the matrix described above, with matching source/corpus
fingerprints. The remaining compatibility-name table is not a completed
declaration-ID migration; the earlier failed full gate remains historical failure.

**Implemented borrowed enum queries (accepted 2026-10-30).** The checker's final
effective-variant name table is removed, with no replacement registry. Pattern
coverage, character membership and member checks borrow names from shared member
IDs on demand. Conversion/default classification retains resolved paths and
alias-normalized checked types. Shared declaration queries handle alias-qualified
patterns, constants and Process values without making an array alias an enum.

Synthetic impl `Self` retains its lexical owner ID. Associated lookup normalizes
aliases while preserving explicit methods and nominal-kind boundaries: an
unbound generic parameter cannot select an unrelated same-spelled concrete type,
and a result type never determines its function's owner. Declared `new()` bodies
precede synthesized defaults in both hardware and procedural lowering. No LLVM
workaround, extra signature table or source representation limit is introduced.

Regressions cover equal leaves, renamed aliases, reversed module order,
coincident generated spans, unknown discriminants, constructor arity, complete
matches and selected member contracts. Five native API cases isolate direct and
forwarded calls, procedural/hardware zero-argument overrides and alias constants.
The multi-module fixture checks different encodings, reactive outputs and strict
VCD traces in ordinary/debug and default/bitpack builds. Terminal successful
frozen gate 1939640 passes the full matrix above with matching fingerprints.
Other string-keyed registries and all remaining foundation requirements stay open.

**Implemented checker alias registry migration (accepted 2026-11-04).** The checker no
longer collects a separate string-keyed, cloned alias-target table or keeps its
own expansion guard and alias-chain walker. Written types, literal privacy,
declared array labels and numeric ranges use the existing borrowed declaration
normalizer. Applied aliases retain parameter identities and definition-site
constants, including nested type arguments. Checked-type recursion consumes the
completed normalization rather than traversing each child's aliases again.

Focused validation passes all 711 frontend tests and strict frontend Clippy.
Regressions cover nested/named generic arguments, provider-owned enum and width
constants despite caller shadowing, array labels and unconstrained array sizing,
private struct construction, initializer/parameter/port numeric ranges and cyclic
aliases returning an error type without cascading expansion. Retained full gate
2281715 passes the default/bitpack Rust, native/VCD, actual-corpus and diagnostic
matrix above, with matching before/after fingerprints. The unchanged source and
corpus still match those fingerprints at acceptance review. The transient unit
is no longer loaded after the host restart; its default `not-found` properties
are not used as pass evidence. The full retained results and success-conditional
follow-up launch establish completion. The estimate remains about 85% of compiler
foundations, with all five sections still required.

**Implemented cross-module owner coverage (2026-10-25).** Additional tests
combine equal enum/base/variant leaves, renamed imports, reversed module order
and generated coincident qualifier spans. They check distinct resolved and typed
owners, rejected wrong-owner returns/patterns and qualified IR keys. The existing
native identity fixture also checks differing source discriminants for the same
inherited variant name through macros in hardware and processes, retaining its
earlier defaults/constants/field/symbol checks. Focused checks and the complete
Rust/native/corpus/UI/JSON matrix pass without any production change, with all
three frozen fingerprints matching before and after validation.

**Completion criterion (still open).** No stage after `resolve` looks a declaration up by its leaf
name; equal leaf names in different modules are covered by a test per
registry.

## 5. Remove residual name-based hook lookups

The lang-item mechanism is implemented: `Resolved::lang` maps roles to `DefId`,
`core` is embedded by the compiler, and `core`/`std` declare hook roles through
`attr lang`. Built-in macros live in `core::macros` over `builtin #`.
The current library surface is in [std.md](../std.md); macro semantics are in
[language.md](../language.md), §3.30. These are no longer proposals.

**Remaining problem.** Type classification still has leaf-name dispatch such
as `types/keys.rs::ty_from_head`, and frontend-only fixtures retain fallback
identities. Do not confuse a registered hook table with complete removal of
name matching.

**Proposal.** Audit classification and hook consumers, replacing library-type
spelling checks with resolved identities, lang roles or source-owned layout/
encoding metadata as appropriate. Keep the grammar's kernel types distinct
from ordinary library declarations. Remove fixture fallbacks only when their
callers load core or explicitly supply equivalent identities.

**Generic-bound consumer followup (accepted locally 2026-11-12).**
Bounds select their trait head by resolver `DefId` and reuse
`is_compiler_trait` for existing implicit kernel capabilities. The old
`builtin_trait` spelling/namespace test is deleted: an ordinary trait under
`core::` or `std::` no longer acquires a kernel impl merely from its path.
Explicit implementations, applied-view/array forwarding and registered hook
roles remain authoritative. Two regression groups cover library lookalikes,
renamed registered operator hooks, import aliases, seeded fallback roles,
explicit impls and reversed module order. This migrates one consumer, not all
trait registries or the whole hook proposal. The latest full combined gate above
includes these regressions; the earlier preview alone did not validate them.

**Implemented Boolean result types (2026-10-24).** Comparisons, `'event`, `'ascending` and
the runtime `exists` contract now request the registered `bool` declaration,
matching the IR's existing lookup. A missing role stays unknown; an unrelated
type named `Bool` cannot supply it. Raw tests cover a renamed registered enum,
both module orders and absence of the role. Lightweight checker fixtures load
the actual `core::primitive` source instead of retaining a production spelling
fallback for their local stand-in. Native corpus coverage keeps a user `Bool`
distinct across hardware and named processes. The full gate above validates
this migration; library-type classification and the literal migration below
are separate work.
The native event case also exposed an existing destination-layout preflight gap:
one-bit storage-event values were emit-capable but rejected as local
initializers. The support check now accepts that already-supported one-bit case
without permitting wider layouts or invalid storage references.

**Implemented Boolean literal identity (2026-10-25).** Bare `true`/`false`
expressions and patterns now use a parser-internal marker, resolved through the
registered `bool` enum rather than a visible declaration named `Bool`. The shared
enum-owner query serves typing, pattern domains and IR, including a registered
derived enum whose variants belong to its base. Explicit enum paths stay
explicit through printing and macro expansion, even with coincident generated
spans. Missing/non-enum providers and missing variants receive `E-P036`, never
an inferred user-type fallback. Source declarations still own the discriminants.
Renamed/reordered providers, both module orders, negative pattern domains,
round trips and macros have regressions; native coverage also exercises hardware
and named processes. The full matrix above passes with unchanged compiler,
corpus and paper fingerprints; these checks do not finish other hook consumers.
Boolean metadata checks use the registered type/owner too, not the qualifier's
spelling. The old directive-to-`true` metadata synthesis is removed: compiler
directives are not attribute values. Waveform checks retain the runtime's
symbolic enum representation, including `true`/`false` transitions, rather than
assuming Boolean ports are encoded as VCD wire bits.

**Implemented range identity (2026-10-25).** Range expressions select the
existing `range` lang role rather than `ty_from_head("Range")`. Index and
IndexAssign checking consumes the same inferred input type as other indices.
Both method-lowering paths and Process custom indexing use checked/resolved
identities; generated range argument types retain the provider's declaration
span instead of being rebound by leaf spelling. Explicit user records named
`Range` remain distinct overload inputs. Renamed providers, both module orders,
missing roles, read/write contract rejection and generated IR have regressions;
native coverage separates those contracts in hardware and named processes.
Intrinsic indexing, partial-range bounds and source-owned range fields are
unchanged. The full matrix above passes with unchanged compiler, corpus and
paper fingerprints; other hook consumers and remaining proposal sections stay open.
Native write coverage exposed two older gaps: range procedure arguments were
left as unpacked control ranges, and `index_assign` methods were selected by
name rather than their complete contract. Contextual range arguments now reuse
the same declaration-bound constructor and ordinary aggregate value lowering.
IndexAssign implementations enter the operator registry; both index and value
types select the body in hardware and procedures. Ambiguous multi-candidate
queries stay unresolved rather than depending on source order. Tests retain
integer/real value overloads, descending/negative bounds, ordinary range values
and mixed intrinsic slices instead of reducing coverage to read-only indexing.

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

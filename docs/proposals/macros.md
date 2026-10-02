# Macros

Status: **slices 1 and 2 implemented** (language §3.30): declarations,
forms, fragment kinds, all four invocation positions, hygiene, resolution
through `use`, `--emit expanded`, repetition, call-site locations and
expansion notes, and the built-in macros declared in `core` over
`builtin #`. What remains is listed under [Later slices](#later-slices).

`macro` declares a user-defined syntax transformation. It is the third
compile-time mechanism, next to the two that already exist:

| | form | what it does |
| --- | --- | --- |
| metadata | `attr keep for probe = true;` | stores data on a declaration |
| directive | `#[test]`, `#[allow(..)]` | changes what the compiler does; a fixed set |
| macro | `twice!(a)` | generates siox syntax, written by users |

A macro is justified when an abstraction must generate *structure* (signals,
processes, items) or keep *syntax* (an assertion's source text) that a
function cannot. Prefer a function for behavior, a generic entity for
parameterized hardware, and a `for` generate for regular repetition.

rustc is the reference throughout. siox follows Rust's *declarative macros
2.0* (`macro name(..) { .. }`, the unstable `decl_macro` feature) rather than
`macro_rules!`: macros are ordinary items, imported with `use`, with
definition-site hygiene.

## Declaring a macro

```siox
pub macro twice($x: expr) {
    $x + $x
}
```

```text
MacroDecl := "pub"? "macro" Ident "(" MacroParams? ")" "{" TokenTree* "}"
MacroParam := "$" Ident ":" FragmentKind
```

- A macro is a **module-level item**. It is not declared inside an `impl`, a
  function or a process.
- Visibility is the usual `pub`/private, and macros are **order-independent**
  like every other item: a module may invoke a macro declared below it.
- The body is a sequence of token trees with balanced `()`, `[]`, `{}`. It is
  not parsed until the macro is invoked, because only the invocation decides
  whether it is an expression, statements or items.
- A parameter is written `$name`, everywhere: in the parameter list and in the
  body. `$` appears in no other siox syntax.

### Fragment kinds

| kind | matches | substituted as |
| --- | --- | --- |
| `expr` | one expression | one operand: `$x * 2` with `$x = a + b` is `(a + b) * 2` |
| `ident` | one identifier | that identifier, with the caller's identity |
| `type` | one type: `unsigned[8]`, `Packet<Bit>` | that type |
| `path` | a path: `State::Idle`, `std::math::max` | that path |
| `stmt` | one statement, without its `;` | that statement |
| `item` | one item: `struct`, `fn`, `entity`, ... | that item |
| `tokens` | any balanced token sequence | the tokens, verbatim |

An argument is checked against its kind when the macro is invoked: a `type`
parameter given `1 +` is reported at the argument, not inside the body.

### Several forms

A macro name may be declared more than once with different parameter lists,
like the two forms of `assert!`:

```siox
pub macro check($cond: expr) { assert!($cond); }
pub macro check($cond: expr, $msg: expr) { assert!($cond, $msg); }
```

An invocation uses the first form, in declaration order, whose parameter
count matches and whose every argument matches its fragment kind. No form
matching is an error that lists the forms. (Rust writes alternatives as arms
of one `macro`; separate declarations read like the rest of siox, and there is
no pattern language to learn.)

## Invoking a macro

`name!(...)`, `name![...]` or `name!{...}`, with a path for an imported or
qualified macro: `debug::probe!(x)`. Arguments are separated by top-level
commas. The **position** of the invocation decides what the expansion must
be:

| position | expansion | example |
| --- | --- | --- |
| expression | exactly one expression | `let y: unsigned[8] = twice!(a);` |
| statement (in a block or process) | zero or more statements | `swap!(a, b);` |
| implementation member | zero or more members (`let`, `process`, statements, `attr`) | `probe!(sum, unsigned[8]);` |
| module item | zero or more items | `register_bank!(regs, 4);` |

In statement, member and item position a `()` or `[]` invocation ends with `;`
and a `{}` invocation does not, as in Rust.

An expression macro cannot expand to statements: siox has no block
expressions (`{ … }` is concatenation). A macro that needs a local declaration
is a statement macro.

## Hygiene

Macros are hygienic at the **definition site**, like Rust macros 2.0:

- **Names the body introduces are private to the expansion.** A `let`,
  `for` variable, label or item declared by a name written in the body gets a
  fresh identity, so it neither captures nor is captured by a caller's name:

  ```siox
  pub macro swap($a: ident, $b: ident) {
      let tmp: Bit = $a;
      $a = $b;
      $b = tmp;
  }
  ```

  `swap!(tmp, x)` works: the macro's `tmp` and the caller's `tmp` are
  different variables. The fresh identity shows as `tmp#1` in diagnostics,
  waveforms and `--emit expanded`.
- **Names the caller passes keep the caller's identity.** `$name: ident`
  declares or refers to the caller's name; this is how a macro deliberately
  declares something the caller can use (`declare!(counter, unsigned[8])`).
- **Free names in the body resolve where the macro is declared.** A body that
  calls `helper()` or names `Packet` means the `helper` and `Packet` visible in
  the macro's module, whatever the caller has imported. They must also be
  visible to the caller (`pub`), because the expanded code lives at the call
  site.
- Field names after `.` and struct-literal fields (`.data = …`) are not
  names in this sense and are never renamed.

## Resolution

Macros live in the macro namespace (language §3.4): `fn probe` and
`macro probe` coexist, and `probe(…)`/`probe!(…)` are never ambiguous. They are
found like any item:

```siox
use debug::probe;          // import it
probe!(x);
debug::probe!(x);          // or qualify it
use debug::*;              // globs, renames, `pub use` re-exports: all as for items
```

The built-in macros `assert!`, `print!` and `warn!` (core) are visible
everywhere, as the prelude is. A user macro of the same name shadows a
built-in one in the module that declares or imports it.

## Expansion

Expansion is a pass over the parsed modules, before imports are desugared and
names resolved:

```text
parse → expand macros → desugar imports → attach attributes → resolve → types → …
```

1. Collect every module's macro declarations.
2. Walk every module. For each invocation that names a user macro, select the
   form, check the arguments, substitute them into the body, apply hygiene,
   and parse the result in the invocation's position. Replace the invocation
   with the result.
3. Expand the result again: a macro may invoke macros.
4. Remove the macro declarations, and the imports that name only macros.

Expanded code is checked exactly as if it had been written by hand: it gets
no exemption from type checking, visibility, driver or clock rules.

- **Depth.** Expansion nests at most 128 deep, rustc's default
  `recursion_limit`. Going deeper is an error naming the chain of macros.
- **No generated macros.** An expansion that declares a `macro` is an error.
  Generated macros would need a fixed point between expansion and resolution
  for little benefit.
- **Unused imports.** An imported macro that is never invoked is reported by
  `unused_import`, like any import.

## Diagnostics

Every token keeps the span it was written at:

- a mistake in an **argument** points at the argument, at the call site;
- a mistake in the **body** points into the macro's declaration, with a note
  `in an expansion of `name!`` — from the parser and from every later stage
  alike.

**Call-site locations.** A built-in `assert!`, `warn!` or `print!` written in
a macro body reports the location of the *outermost* invocation when it fires
at run time, as Rust's `line!()` and `panic!` do: a failing `check!(ready)`
names the line that says `check!`, not the line inside `check`'s body.

`sioxc --emit expanded file.siox` prints the entry module after expansion, so
the generated hardware is never a mystery.

## Built-in macros

`assert!`, `warn!`, `print!` and the new `error!` are ordinary macros
declared in `core::macros` and exported by `core::prelude`, as Rust declares
`assert!` in `core` with `#[rustc_builtin_macro]`:

```siox
pub macro assert($cond: expr, $rest: expr...) { builtin # assert($cond, $rest) }
pub macro warn($cond: expr, $rest: expr...) { builtin # warn($cond, $rest) }
pub macro print($args: expr...) { builtin # print($args) }
pub macro error($rest: expr...) { builtin # assert(false, $rest) }
```

- **`builtin # name(args)`** is the compiler primitive behind them: an
  expression whose meaning the compiler supplies. It is accepted only in the
  body of a macro declared in `core`; anywhere else it is an error. The
  primitives are `assert`, `warn` and `print`.
- **`error!("message")`** fails the simulation unconditionally, with its
  source location, like Rust's `panic!`; it replaces the
  `assert!(false, "message")` idiom.
- Because they are macros, the built-ins follow every macro rule: they can be
  imported, re-exported and shadowed by a user macro of the same name, and a
  failure reports the outermost invocation.
- Nothing about how they type-check or run changes: the primitive is the
  same compiler operation the old special-cased call was.

## Not part of this design

- `macro_rules!` syntax, procedural macros, or any compile-time execution of
  user code. A macro substitutes syntax; it runs nothing.
- User-defined `#[…]` attributes or directives. `#[…]` stays a fixed set; a
  macro may *emit* `#[test]` or `#[allow(..)]`, or an `attr` binding.
- Inspecting types, values or generated hardware during expansion.
- Identifier concatenation (`${prefix}${i}`). Arrays and generate loops name
  repeated hardware better.

## Repetition

A macro takes a list of arguments with a **variadic** last parameter, written
with `...` after its kind. It matches zero or more remaining arguments, each
of that kind:

```siox
pub macro all($conds: expr...) {
    for macro $c in $conds { assert!($c); }
}

pub macro any($first: expr, $rest: expr...) {
    $first for macro $c in $rest { or $c }
}

pub macro all_of($cs: expr...) {
    for macro $c in $cs join and { $c }
}

pub macro trace($fmt: expr, $args: expr...) {
    print!($fmt, $args);
}
```

In the body:

| form | expands to |
| --- | --- |
| `for macro $x in $xs { … }` | the braces' contents once per argument, with `$x` bound to it |
| `for macro $x in $xs join T { … }` | the same, with the token `T` between repetitions |
| `$xs` | every argument, separated by commas: forwards the list |
| `$xs'length` | the number of arguments, as an integer literal |

`for macro` nests, and its body may use every parameter of the macro. It is
expansion-time repetition; the generated code contains no loop. Ordinary
parameters may be iterated too, as a list of one. `for`, `macro`, `in` and
`join` are only read this way inside a macro body; `join` is not a keyword.

The shape follows the proposal's earlier sketch instead of Rust's `$(…),*`:
it reads like the `for` loops and generates around it, and the separator is
named rather than encoded in punctuation.

## Later slices

- **Expansion notes in the language server**: generated declarations shown as
  generated, not as handwritten source.
- **Which invocation.** A note on an error in a body names the macro but not
  the call that expanded it; spans carry no expansion identity yet.

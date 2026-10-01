# Split `using` into `use` and `type`

Status: **partly implemented** (language §3.4). Done: `use` and `type`
replace `using` (now an error naming the replacement); single, grouped and
renamed imports (`use L = a::b::C;`, `{L = C}`); `pub use`; `pub type`; std
and the corpus migrated. Remaining, each additive: nested groups and `self`
in groups, glob imports, `self::`/`super::` paths, imports inside blocks,
generic `type` aliases, and the separate value/type/macro namespaces.

siox currently spells two different operations with one keyword:

```siox
using std::logic::{Bit, Logic};   // an import: changes name resolution
using Word = unsigned[32];        // a type alias: declares a new type name
```

This proposal replaces `using` with Rust's design: `use` for imports and
`type` for transparent type aliases. It follows Rust's import model
throughout: paths, groups, nesting, `self`, globs, re-exports, namespaces, and
the shadowing rules. It makes **one deliberate change**: an import is renamed
with `=`, not `as`.

```siox
use std::logic::Logic;                 // import
use std::logic::{Bit, Logic};          // grouped import
use AxiMaster = bus::axi::Master;      // renamed import   (Rust: `as AxiMaster`)
pub use std::logic::{Bit, Logic};      // re-export
use bus::axi::*;                       // glob import

type Word = unsigned[32];              // transparent type alias
```

## Decision

- **`using` is removed.** `use` takes over every import and re-export form.
  `type` takes over every alias.
- **`use` follows Rust.** Everything `use` does in Rust it does here, with the
  same meaning. The two exceptions are listed under
  [Differences from Rust](#differences-from-rust).
- **No `as`.** A renamed import puts the new name on the left of `=`, as
  `type`, `let` and `const` already do: `use AxiMaster = bus::axi::Master;`.
- **`type` follows Rust.** A `type` alias is transparent: the alias and its
  target are the same type. A new *nominal* type stays the newtype form,
  `struct Word(Bit[]);` (language.md §3.28).

`use`, `type` and `super` become keywords. None of them is used as an
identifier anywhere in `std/` or the corpus today.

## Why split them

An import and an alias answer different questions:

```text
use ...     which existing declaration should be visible here, and under what name?
type ...    which type does this new type name denote?
```

`use` doesn't care what kind of declaration its path names. It works the same
for a struct, entity, function, constant, module, enum variant or (later)
macro. `type` accepts only a type. With one keyword, `using std::assert;` would
be an import while `using Word = unsigned[32];` declares a type. The parser can
tell them apart, but the language model is muddier, and it gets worse as more
declaration kinds become importable.

## Imports

### Single imports

```siox
use std::logic::Logic;
```

This binds the final path segment, `Logic`, in the current scope to the
existing declaration. Nothing is copied. Any declaration may be imported: a
module, struct, enum, enum variant, entity, view, trait, function, constant,
attribute, type alias, or (with [macros.md](macros.md)) a macro:

```siox
use std::math;              // a module: now `math::PI`, `math::max(a, b)`
use std::math::PI;          // a constant
use std::math::max;         // a function
use std::numeric::Byte;     // a type alias
use std::attrs::test;       // an attribute
use self::State::{Idle, Busy};  // enum variants of this module's `State`
```

### Renamed imports

Rust writes `use bus::axi::Master as AxiMaster;`. siox writes:

```siox
use AxiMaster = bus::axi::Master;
```

```text
use <local-name> = <path>;
```

The binding is the declaration itself under a new local name. A renamed
generic struct keeps its parameters, a renamed enum keeps its variants
(`AxiMode::Burst`), and a renamed module keeps its members:

```siox
use axi = protocols::axi;   // axi::Master, axi::Slave
```

### Groups

```siox
use std::logic::{Bit, Logic};
```

This is exactly `use std::logic::Bit; use std::logic::Logic;`. As in Rust, a
group may nest, may rename members, and may name its own prefix with `self`:

```siox
use std::{
    logic::{Bit, Logic},
    math::{self, PI},              // imports `math` itself and `math::PI`
    numeric::{Word8 = Byte},       // renamed inside a group
};

use bus::axi::{
    self,                          // `axi`
    AxiMaster = Master,
    AxiSlave = Slave,
};

use protocols::{wb = self};        // a renamed `self`: Rust's `{self as wb}`
```

A path inside a group is relative to the group's prefix. Groups are pure
shorthand and add no module semantics.

### Operators

As in Rust, an operator expression never needs an import. `a + b` finds its
`impl Operator<"+", In, Out> for T` through the operand's type, wherever that
impl is declared. There is no per-operator name to import: Rust's `Add`, `Sub`
and so on are collapsed into the single `std::ops::Operator` trait (language.md
§3.25). The compiler bootstraps that name, so writing an `impl Operator<…>`
needs no import either. Rust, by contrast, needs `use std::ops::Add;` or a
qualified path before `impl Add for T`.

The old quoted form (`using a::{"+"}`, `impl "+" for T`) was removed earlier
and remains an error.

One operator rule has no Rust counterpart, because Rust has no user-defined
operators. A user operator's `#[precedence = N]` affects how expressions
*parse*. The compiler reads those precedences only from modules reachable
through the transitive import graph (language.md §3.25). `use` keeps that
graph exactly as `using` defines it today: a module reached by any `use` form,
including a glob, a group or a block-scoped `use`, contributes its operators.

### Glob imports

```siox
use bus::axi::*;
```

This imports every public name of `bus::axi`, with Rust's priority rule: **a
glob import is weaker than every explicit name.** A local declaration or an
explicit `use` shadows a glob-imported name of the same namespace silently. Two
globs that bring the same name into the same namespace are an error only when
that name is used. Declaring both globs is not an error.

### Re-exports

```siox
pub use std::logic::{Bit, Logic};
pub use AxiMaster = bus::axi::Master;
pub use bus::axi::*;
```

`pub use` publishes the imported binding as part of this module's interface.
The declaration stays owned by its original module, and identity is preserved:
`std::prelude::Bit` and `std::logic::Bit` are the same type. This is what
`pub using` does today, and `std/prelude.siox` depends on it from day one.

### Paths

A `use` path starts the way it does in Rust:

| root | meaning | today |
| --- | --- | --- |
| `std::…` | the standard library (the `--std` directory) | same |
| `a::b::…` | a module of this project, from its root | same |
| `self::…` | the current module | new |
| `super::…` | the parent module; `super::super::…` goes further up | new |

A bare project path plays the role of Rust's `crate::…`, because siox has no
crate or package concept yet. That belongs to the future project framework
(see [Open questions](#open-questions)).

Fully-qualified paths remain valid everywhere without any import, and they
select exactly the module they name.

### Where `use` may appear

As in Rust, a `use` may appear:

- at module level;
- at the top of any block: a `fn` body, a `process` body, or a nested block.

The binding is visible from the start to the end of that scope, regardless of
where in the scope the `use` is written. Imports are order-independent, as are
all declarations.

As in Rust, `use` may **not** appear among the members of an `impl` (including
an entity implementation), a struct or enum body, or an entity interface.
Import at module level instead.

## Namespaces

As in Rust, a name lives in one of several namespaces, and one `use` imports
the name in **every** namespace where the path resolves:

| namespace | declarations |
| --- | --- |
| type | modules, structs, enums, entities, views, traits, `type` aliases |
| value | functions, constants, enum variants |
| macro | macros: today's built-in `assert!`, `print!`, `warn!`, later [macros.md](macros.md) |
| attribute | `attr` declarations: looked up only in attribute position, as today |

This means a module and a macro of the same name do not collide.
`std::assert` (the module) and a macro `assert!` can coexist, and `foo` and
`foo!` are never ambiguous at a use site.

This proposal does not change which *declarations* may share a name within a
module. It only says how an import binds.

## Name conflicts

These follow Rust:

- Two explicit imports of the same name into the same namespace are an error,
  even when both name the same declaration.
- An explicit import that collides with a local declaration in the same
  namespace is an error. Neither one silently wins.
- Glob imports are shadowed (see [Glob imports](#glob-imports)).
- The implicit `std::prelude` sits beneath everything. Any local declaration
  or import shadows a prelude name, which is how the resolver already treats
  the prelude.

The fix for a real conflict is an explicit rename:

```siox
use FooWord = foo::Word;
use BarWord = bar::Word;
```

## Type aliases

```text
TypeAlias := "pub"? "type" Identifier GenericParams? "=" Type ";"
```

```siox
type Word = unsigned[32];
pub type Byte = integer<0..255>;
pub type string = Char[];
type Pair<T> = Packet<T>;
```

As in Rust:

- **Transparent.** `Word` *is* `unsigned[32]` for type identity, width
  checking, operator resolution, and every other rule. A distinct type
  uses the newtype form, `struct Word(unsigned[32]);`.
- **Generic.** An alias may take generic parameters, written in the same
  binder syntax as other declarations. Today an alias name must be a single
  identifier, so this is new.
- **Usable as its target.** Associated items, enum variants (`Alias::Variant`),
  struct literals, conversions and entity instantiation (`let u: Alias =
  { … };`) all work through the alias.
- **Not a module.** As in Rust, a `use` path cannot pass through an alias:
  `use Alias::Variant;` is an error. Write `use Target::Variant;`.

### `use X = T` versus `type X = T`

Both can name an existing type under a new name. They differ the way Rust's
`use … as` and `type` differ:

| | `use AxiMaster = bus::axi::Master;` | `type AxiMaster = bus::axi::Master;` |
| --- | --- | --- |
| right-hand side | a **path** to any declaration | any **type expression** (`unsigned[32]`, `Packet<Bit>`) |
| namespaces bound | every namespace the path resolves in | type namespace only |
| generic target | stays generic (`AxiMaster<W>`) | must be applied, or the alias declares its own parameters |
| is it a declaration? | no, a binding to the original | yes, a new name that denotes a type |

Rule of thumb: rename something with `use`. Name a type you could not name
with a path (a sized vector, a ranged integer, an applied generic) with
`type`.

## Module files: the one structural difference from Rust

Rust needs `mod foo;` to load `foo.rs`. siox has no `mod` declaration: a
project path in a `use` loads the module file it names, mapped relative to the
entry file (`use bus::spi::Master;` loads `bus/spi.siox`, which must declare
`module bus::spi;`). `std::` paths map to the `--std` directory. That stays
exactly as `using` does it today.

This is file discovery inside one project, not dependency management. `use`
never downloads a package, searches installed libraries, selects a vendor
library, or configures compilation units. Those belong to the future project
framework (see the `sioxc`/project split), not to name resolution.

## Differences from Rust

1. **Renaming uses `=`, not `as`.** `use AxiMaster = bus::axi::Master;` and
   `{AxiMaster = Master}` in a group. siox has no `as` keyword, and this adds
   none.
2. **No `mod` declarations.** A `use` of a project path loads the module file,
   as described above.

Everything else, including `self`, `super`, groups, nesting, globs, `pub use`,
block-scoped imports, namespaces and conflict rules, means what it means in
Rust.

### Why not `as`

`as` would add a keyword for something existing declaration syntax already
expresses. The `=` form also puts the introduced name first, like every other
siox declaration:

```siox
use AxiMaster = bus::axi::Master;
type Word = unsigned[32];
const WIDTH: integer = 32;
let count: unsigned[8];
```

Each reads `keyword new-name … source`, and the keyword says what relationship
is being declared.

## Interaction with macros

With [macros.md](macros.md), macros are ordinary declarations in the macro
namespace and need no special import syntax. Suppose `checks.siox` declares
`module checks;` and `pub macro assert_ready($x: expr) { … }`. Then:

```siox
use checks::assert_ready;
use check = checks::assert_ready;   // renamed

assert_ready!(ready);
check!(ready);
```

A macro that generates items may emit `use` and `type` declarations, which
then follow the ordinary rules above. This is one practical reason to make the
split before macros land: the macro system never needs to handle the
overloaded `using`.

## Interaction with directives and attributes

A directive applies to a `use` only where it has defined meaning, for example
lint control from [compiler-directives.md](compiler-directives.md):

```siox
#[allow(unused_import)]
use std::math::PI;
```

Whether a `type` alias is a valid target for a declarative attribute is decided
by that attribute's declaration ([attribute-system.md](attribute-system.md)).
This proposal adds no import-specific metadata.

## Grammar

```text
UseDecl    := "pub"? "use" UseTree ";"
UseTree    := Path
            | Path "::" "*"
            | Path "::" "{" UseList "}"
            | Identifier "=" Path                  // renamed import
UseList    := UseMember ("," UseMember)* ","?
UseMember  := "self"
            | Identifier "=" "self"
            | RelPath
            | RelPath "::" "*"
            | RelPath "::" "{" UseList "}"
            | Identifier "=" RelPath
Path       := ("self" | "super" | Identifier) ("::" ("super" | Identifier))*
RelPath    := Identifier ("::" Identifier)*

TypeAlias  := "pub"? "type" Identifier GenericParams? "=" Type ";"
```

`use` is decided by its first token. A renamed import is `use Identifier =`, a
lookahead of two tokens. `type` is decided by its first token.

## Migration

The migration is mechanical, and `sioxc --emit source` is the rewriting tool:

| today | becomes |
| --- | --- |
| `using a::b::C;` | `use a::b::C;` |
| `using a::b::{C, D};` | `use a::b::{C, D};` |
| `pub using …;` | `pub use …;` |
| `using X = T;` | `type X = T;` |
| `pub using X = T;` | `pub type X = T;` |

Every `using X = …` today is a type alias, because the right-hand side is
parsed as a type. Renamed imports of non-types don't exist yet, so **every
existing alias becomes `type`** and none becomes `use X = …`. The current tree
holds 19 `using` lines in `std/` and about 240 in the corpus. Eleven are
aliases, including `std::numeric`'s `Byte`…`Positive` and `std::text`'s
`string`.

Order:

1. Parse `use` (single, grouped, `pub`) and `type`, with the same resolution
   `using` has today. Teach the printer to emit them.
2. Migrate `std/` and the corpus with `--emit source`.
3. Make `using` an error that names its replacement (``write `use a::b::C;` ``
   or ``write `type Word = …;` ``), as `wait` was replaced by `await`.
4. Add what `using` never had: renamed imports, nested groups and `self`,
   globs, `self::`/`super::` roots, block-scoped `use`, and generic `type`
   aliases. Each of these is additive.

## Open questions

- **`crate::`.** Should a bare project path stay root-relative, or should
  siox move to Rust 2018 paths (`crate::bus::spi`) once the project framework
  defines what a crate is?
- **Trait methods in scope.** Rust requires a trait to be in scope to call its
  methods, and has `use Trait as _;` to import one anonymously. Does siox
  adopt that rule, and if so, is the anonymous form `use _ = path::Trait;`?
- **Documentation.** Does a `type` alias appear as its own documented
  declaration, or only as a reference to its target?

## Non-goals

- An `as` keyword.
- `mod` declarations.
- Package or dependency discovery, or vendor-library configuration.
- Conditional imports (no `cfg`; see compiler-directives.md).
- Nominal `type` aliases (use the newtype form).
- Import-specific metadata.

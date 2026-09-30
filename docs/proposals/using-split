# Split `using` into `use` and `type`

Status: **proposal**. Nothing here is implemented.

Siox currently uses `using` for two different operations:

- importing an existing declaration into the current scope;
- introducing a transparent type alias.

Those operations are related only in that both introduce a local name. They differ semantically, and that difference becomes more important once macros become ordinary importable declarations.

This proposal replaces `using` with two constructs:

```siox
use <path>;                    // import
use <name> = <path>;           // renamed import
use <path>::{<members>};       // grouped import

type <name> = <Type>;          // transparent type alias
```

`use` affects name visibility.

`type` creates a type alias.

No `as` keyword is introduced.

## Decision

The existing `using` keyword is removed.

Imports use `use`:

```siox
use std::logic::Logic;
use std::logic::ULogic;
```

Transparent type aliases use `type`:

```siox
type Word = unsigned[32];
type Address = unsigned[64];
```

An import may be renamed with assignment syntax:

```siox
use AxiMaster = bus::axi::Master;
```

A group of declarations may be imported from one path:

```siox
use std::{a, b};
```

which is equivalent to:

```siox
use std::a;
use std::b;
```

The distinction is semantic:

```text
use
    makes an existing declaration visible under a local name

type
    introduces a transparent alias for a type
```

## Why split them

The current `using` forms describe two different operations.

An import:

```siox
using std::logic::Logic;
```

changes name resolution.

A type alias:

```siox
using Word = unsigned[32];
```

creates a new type-level declaration whose meaning is another type.

The common syntax hides that distinction.

The split makes each declaration answer one question:

```text
use ...
    what existing declaration should be visible here?

type ...
    what type does this new type name denote?
```

That becomes increasingly useful as the number of declaration kinds grows.

A module may contain:

```siox
pub struct Word { ... }
pub entity Fifo { ... }
pub fn encode(...) { ... }
pub const WIDTH = 32;
pub macro assert_ready(...) { ... }
```

All of them can be imported through the same mechanism:

```siox
use package::Word;
use package::Fifo;
use package::encode;
use package::WIDTH;
use package::assert_ready;
```

`use` does not care what kind of declaration the path denotes.

`type` does.

## Imports

The simplest form imports one declaration:

```siox
use std::logic::Logic;
```

The final path component becomes available in the current scope:

```siox
let x: Logic;
```

Conceptually:

```text
std::logic::Logic
        ↓
local name `Logic`
```

The declaration itself is not copied.

`use` only introduces a local binding to an existing declaration.

## Renamed imports

A declaration may be imported under another local name:

```siox
use AxiMaster = bus::axi::Master;
```

Afterward:

```siox
let port: AxiMaster;
```

refers to:

```siox
bus::axi::Master
```

The grammar is:

```text
use <local-name> = <path>;
```

This avoids adding an `as` keyword.

The syntax also follows existing declaration-like assignment forms:

```siox
type Word = unsigned[32];
use AxiMaster = bus::axi::Master;
```

In both cases, the name being introduced appears on the left of `=`.

The difference is carried by the keyword:

```text
type
    defines a type alias

use
    defines a local import alias
```

## Grouped imports

Multiple declarations from one namespace may be imported together:

```siox
use std::{a, b};
```

This is shorthand for:

```siox
use std::a;
use std::b;
```

A more realistic example:

```siox
use std::logic::{Bit, Logic, ULogic};
```

is equivalent to:

```siox
use std::logic::Bit;
use std::logic::Logic;
use std::logic::ULogic;
```

The group is relative to the path preceding `::{`.

This allows related imports to remain visually grouped without repeating their common prefix.

## Nested groups

Nested grouping may be supported:

```siox
use std::{
    logic::{Bit, Logic},
    testing::{assert, assert_eq},
};
```

which is equivalent to:

```siox
use std::logic::Bit;
use std::logic::Logic;
use std::testing::assert;
use std::testing::assert_eq;
```

Nested groups are purely syntactic shorthand.

They do not introduce new module semantics.

If implementation simplicity is preferred for the first version, nested groups may be deferred while retaining the top-level grouped form.

## Renaming inside groups

Grouped imports should support the same renamed-import form:

```siox
use bus::axi::{
    AxiMaster = Master,
    AxiSlave = Slave,
};
```

The right-hand side is resolved relative to the group's path.

This is equivalent to:

```siox
use AxiMaster = bus::axi::Master;
use AxiSlave = bus::axi::Slave;
```

This keeps one renaming syntax everywhere:

```text
local-name = imported-name
```

No second aliasing form is required.

## Type aliases

A transparent type alias uses `type`:

```siox
type Word = unsigned[32];
```

`Word` is not a new nominal type.

It denotes the same type as:

```siox
unsigned[32]
```

Therefore:

```text
Word == unsigned[32]
```

for type identity.

This is deliberately different from nominal derivation:

```siox
struct Word(unsigned[32]);
```

which creates a distinct type.

The distinction is:

```siox
type Word = unsigned[32];
```

```text
transparent alias
Word is unsigned[32]
```

versus:

```siox
struct Word(unsigned[32]);
```

```text
nominal derivation
Word is represented by unsigned[32]
but Word is not unsigned[32]
```

Both are useful and should remain visibly different.

## Why `type` should not import

This should not be treated as an import:

```siox
type AxiMaster = bus::axi::Master;
```

It is valid only if `bus::axi::Master` is a type, and it creates a type alias.

By contrast:

```siox
use AxiMaster = bus::axi::Master;
```

imports the declaration itself under another local name.

That distinction matters because `use` can refer to declaration kinds that `type` cannot:

```siox
use check = std::testing::assert;
use make_fifo = std::fifo::make_fifo;
use DefaultWidth = config::WIDTH;
```

assuming those declarations exist.

The resolver therefore does not need to know the declaration kind before parsing a `use`.

It only resolves the path afterward.

## Macros

The split is particularly useful once macros are ordinary module declarations.

For example:

```siox
module testing {
    pub macro assert_eq($left: expr, $right: expr) {
        ...
    }
}
```

may be imported normally:

```siox
use testing::assert_eq;

assert_eq!(a, b);
```

No special macro import syntax is necessary.

Likewise:

```siox
use std::{
    assert,
    assert_eq,
    panic,
};
```

can import several macros or a mixture of declaration kinds.

The invocation syntax still identifies the construct:

```siox
assert_eq!(a, b);     // macro
foo(a, b);            // function
```

Import syntax controls visibility only.

It does not determine how the declaration is used.

## One import mechanism for all declarations

`use` should operate over the module namespace rather than over a fixed list of declaration categories.

The same syntax should therefore work for:

```siox
use pkg::MyStruct;
use pkg::MyEnum;
use pkg::MyTrait;
use pkg::MyEntity;
use pkg::my_function;
use pkg::MY_CONST;
use pkg::my_macro;
```

If Siox maintains separate namespaces internally for types, values, and macros, the resolver may still do so.

That should not require separate import syntax.

The imported path determines what declaration is being made visible.

## Name conflicts

An import that would introduce a duplicate name in the same namespace is an error.

For example:

```siox
use foo::Word;
use bar::Word;
```

should fail if both resolve into the same namespace and no other language rule disambiguates them.

The programmer can resolve the conflict explicitly:

```siox
use FooWord = foo::Word;
use BarWord = bar::Word;
```

Explicit renaming is preferable to import-order shadowing.

Imports should therefore not silently override one another.

## Existing local declarations

The same rule applies when an imported name conflicts with a local declaration:

```siox
struct Word { ... }

use package::Word;
```

This should be diagnosed rather than making resolution depend on declaration order.

The programmer may instead write:

```siox
use PackageWord = package::Word;
```

This keeps name resolution deterministic and local.

## Visibility

`use` introduces a name into the current module or scope.

Whether imports may themselves be exported should follow Siox's normal visibility model.

If re-export is required, the natural form is:

```siox
pub use std::logic::Logic;
```

and:

```siox
pub use AxiMaster = bus::axi::Master;
```

This means the current module publicly exposes the imported declaration under that local name.

The underlying declaration does not become owned by the re-exporting module.

This is useful for facade modules:

```siox
module prelude {
    pub use std::logic::{Bit, Logic};
    pub use std::testing::{assert, assert_eq};
}
```

If public re-export is not needed immediately, `pub use` may be deferred without changing the basic syntax.

## Scope

Imports should initially be module-level declarations.

Allowing arbitrary block-local imports:

```siox
fn foo() {
    use package::bar;
}
```

adds relatively little capability and complicates name-resolution scopes.

Unless there is a concrete use case, the initial rule should remain:

> `use` appears at module/declarative scope.

This can be relaxed later without changing the syntax.

## Wildcard imports

This proposal does not require:

```siox
use std::*;
```

Wildcard imports make source meaning depend more heavily on unrelated declarations added to another module later.

That works against local readability and can make package evolution create unexpected name conflicts.

Grouped imports already provide a compact explicit alternative:

```siox
use std::{Bit, Logic, Bool, assert};
```

Wildcard imports should therefore be omitted initially.

They can be reconsidered if large prelude-style modules demonstrate a concrete need.

## Importing modules

A path may refer to a module if modules are ordinary resolvable declarations.

For example:

```siox
use axi = protocols::axi;
```

would allow:

```siox
axi::Master
axi::Slave
```

Whether modules themselves are importable should follow from the module-resolution model rather than require a separate syntax.

If modules are not first-class namespace bindings, this form should simply be rejected.

No additional grammar is necessary.

## `use` is not dependency discovery

`use` resolves declarations that already exist in the Siox module graph.

It does not:

- download a package;
- select a vendor library;
- search installed dependencies;
- configure compilation units;
- choose a backend library;
- manipulate project dependencies.

Those operations belong to the future project/build system.

For example:

```siox
use std::logic::Logic;
```

means:

> Resolve `std::logic::Logic` from the module graph and make it locally visible.

It does not mean:

> Find or install a package called `std`.

This keeps source-level name resolution separate from build-system dependency resolution.

## Grammar

Conceptually:

```text
UseDecl :=
    "use" Path ";"
  | "use" Identifier "=" Path ";"
  | "use" Path "::" "{" UseMembers "}" ";"

UseMembers :=
    UseMember ("," UseMember)* ","?

UseMember :=
    Identifier
  | Identifier "=" RelativePath
  | Identifier "::" "{" UseMembers "}"

TypeAlias :=
    "type" Identifier GenericParams? "=" Type ";"
```

The exact parser grammar may differ.

The semantic distinction should remain fixed.

## Examples

Single import:

```siox
use std::logic::Logic;
```

Grouped import:

```siox
use std::logic::{Bit, Logic, ULogic};
```

Renamed import:

```siox
use U8 = std::numeric::unsigned8;
```

Nested imports:

```siox
use std::{
    logic::{Bit, Logic},
    testing::{assert, assert_eq},
};
```

Renamed grouped imports:

```siox
use bus::axi::{
    MasterPort = Master,
    SlavePort = Slave,
};
```

Type alias:

```siox
type Word = unsigned[32];
```

Generic type alias:

```siox
type Word<T> = Packet<T>;
```

if generic aliases are supported.

Nominal type:

```siox
struct Word(unsigned[32]);
```

Macro import:

```siox
use std::testing::assert;

assert!(ready);
```

Macro rename:

```siox
use check = std::testing::assert;

check!(ready);
```

## Why not `as`

A common import-renaming syntax would be:

```text
use bus::axi::Master as AxiMaster;
```

This proposal deliberately avoids it.

`as` would add another keyword for a capability that can already be expressed clearly using existing declaration syntax:

```siox
use AxiMaster = bus::axi::Master;
```

The assignment form has another advantage: the introduced name appears first.

Compare:

```siox
use AxiMaster = bus::axi::Master;
type Word = unsigned[32];
```

Both read:

```text
construct local-name = source
```

The keyword then states what kind of relationship is being declared.

No additional reserved word is necessary.

## Why not retain `using`

Keeping:

```siox
using std::logic::Logic;
using Word = unsigned[32];
```

saves one keyword but makes `using` mean two unrelated things.

That cost grows as more declaration kinds become importable.

With macros:

```siox
using std::testing::assert;
```

would mean an import, while:

```siox
using Word = unsigned[32];
```

would mean a type declaration.

The parser can distinguish them, but the language model is less precise than:

```siox
use std::testing::assert;
type Word = unsigned[32];
```

The split therefore adds a keyword while reducing semantic overload.

`type` is also useful independently as a clear vocabulary word for future type-level declarations.

## Interaction with macros

Macros may emit imports or type aliases if item-generation macros are allowed:

```siox
macro standard_logic() {
    use std::logic::{Bit, Logic};
}
```

or:

```siox
macro word_type($name: ident, $width: const) {
    type $name = unsigned[$width];
}
```

Generated `use` declarations follow ordinary name-resolution rules.

Generated `type` aliases follow ordinary type-declaration rules.

The macro system therefore does not need special handling for the old overloaded `using` construct.

This is one practical reason to make the distinction explicit before macros become part of the language.

## Interaction with compiler directives

Compiler directives may appear on imports or aliases only where they have defined semantics.

For example, lint suppression could potentially apply:

```siox
#[allow(unused_import)]
use std::debug::probe;
```

That is separate from the import mechanism itself.

`use` does not carry compiler behavior.

## Interaction with declarative attributes

Declarative attributes attach metadata according to their target rules.

Whether `use` or `type` may be valid attribute targets is determined by the attribute declaration:

```siox
attr foo: Bool for type = false;
```

if `type` aliases are made a valid target category.

No import-specific metadata mechanism is introduced by this proposal.

## Migration

Migration is mechanical.

Existing imports:

```siox
using std::logic::Logic;
```

become:

```siox
use std::logic::Logic;
```

Existing type aliases:

```siox
using Word = unsigned[32];
```

become:

```siox
type Word = unsigned[32];
```

If existing syntax supports import aliases through `using`, those become:

```siox
use LocalName = package::Name;
```

A migration may proceed in stages:

1. Add `use` for ordinary imports.
2. Add `type` for transparent type aliases.
3. Add renamed `use <name> = <path>`.
4. Add grouped imports.
5. Migrate `std` and the corpus.
6. Warn on `using`.
7. Remove `using`.

Grouped and nested imports are additive and need not block the initial split.

## Open questions

- Are `use` declarations module-only, or may they appear in local scopes?
- Should `pub use` be supported immediately?
- Are modules themselves importable and renameable?
- Should nested grouped imports be available in the first implementation?
- Should grouped imports permit renaming from the start?
- Are wildcard imports deliberately unsupported, or merely deferred?
- Do values, types, and macros occupy separate namespaces, and if so how are import conflicts diagnosed?
- Should generic type aliases be supported immediately?
- Is a type alias allowed to alias views and constrained types exactly as written?
- Should aliases participate in documentation as independent named declarations or merely as references to their target?

None of these questions require retaining `using`.

## Non-goals

- Package or dependency discovery.
- Vendor-library configuration.
- Conditional imports.
- Wildcard imports in the initial design.
- A new `as` keyword.
- Making `type` aliases nominal.
- Giving imports runtime semantics.
- Giving macro imports special syntax.
- Using `use` as a build-system command.

## Bottom line

`using` currently combines two operations that should have separate vocabulary.

The replacement is:

```siox
use std::logic::Logic;
use std::logic::{Bit, ULogic};
use AxiMaster = bus::axi::Master;

type Word = unsigned[32];
```

The resulting rule is simple:

```text
use
    refers to an existing declaration and changes local visibility

type
    introduces a transparent name for a type
```

This becomes more valuable once functions, entities, traits, constants, macros, and other declarations all participate in the module system.

One import mechanism can then handle all of them, while type aliasing remains explicitly type-level.

The split reduces semantic overloading, avoids adding `as`, and gives the macro system a clean ordinary import model.

//! Visibility, privacy, and module-qualified identity.

use super::*;

#[test]
/// Private struct members are scoped to the owning type's module, while
/// `pub` ones cross the boundary.
fn private_struct_members_are_type_scoped_and_pub_crosses_the_boundary() {
    let provider = "module model;\n\
            pub struct Packet { hidden: integer, pub visible: integer }\n\
            impl Packet { fn secret(self) -> integer { return self.hidden; } pub fn get(self) -> integer { return self.hidden; } }\n";
    let consumer = "module user;\n\
            using model::{Packet};\n\
            fn inspect(p: Packet) -> integer { return p.hidden + p.visible + p.secret() + p.get(); }\n";
    let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
    let private = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_MEMBER))
        .count();
    assert_eq!(
        private, 2,
        "private field and method are rejected, public ones pass"
    );
}

#[test]
/// Two structs with the same leaf name in different modules keep separate
/// visibility domains.
fn equal_struct_leaves_keep_independent_visibility_domains() {
    let a = "module a::record;\n\
            pub struct Pair { hidden: integer, pub shown: integer }\n\
            impl Pair { fn secret(self) -> integer { return self.hidden; } pub fn open(self) -> integer { return self.shown; } }\n";
    let b = "module b::record;\n\
            pub struct Pair { pub hidden: integer, shown: integer }\n\
            impl Pair { pub fn secret(self) -> integer { return self.hidden; } fn open(self) -> integer { return self.shown; } }\n";
    let user = "module user;\n\
            fn a_hidden(value: a::record::Pair) -> integer { return value.hidden; }\n\
            fn a_shown(value: a::record::Pair) -> integer { return value.shown; }\n\
            fn b_hidden(value: b::record::Pair) -> integer { return value.hidden; }\n\
            fn b_shown(value: b::record::Pair) -> integer { return value.shown; }\n\
            fn a_secret(value: a::record::Pair) -> integer { return value.secret(); }\n\
            fn a_open(value: a::record::Pair) -> integer { return value.open(); }\n\
            fn b_secret(value: b::record::Pair) -> integer { return value.secret(); }\n\
            fn b_open(value: b::record::Pair) -> integer { return value.open(); }\n";
    let sink = check_modules(&[(a, FileId(0)), (b, FileId(1)), (user, FileId(2))]);
    let private: Vec<&Diagnostic> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_MEMBER))
        .collect();
    assert_eq!(
        private.len(),
        4,
        "each declaration keeps its own field and method visibility: {:?}",
        private
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
    );
    assert!(private
        .iter()
        .any(|diagnostic| diagnostic.message.contains("a::record::Pair.hidden")));
    assert!(private
        .iter()
        .any(|diagnostic| diagnostic.message.contains("a::record::Pair::secret")));
    assert!(private
        .iter()
        .any(|diagnostic| diagnostic.message.contains("b::record::Pair.shown")));
    assert!(private
        .iter()
        .any(|diagnostic| diagnostic.message.contains("b::record::Pair::open")));
}

#[test]
/// Compiler hook traits are selected by their resolved declaration, not by a
/// matching leaf name, so a user trait of the same name is not mistaken for
/// one.
fn compiler_hook_traits_are_selected_by_declaration_not_leaf() {
    let ops = "module std::ops; pub trait Boolean {}";
    let custom = "module custom; \
            pub trait Boolean {} pub struct Flag(integer); impl Boolean for Flag {}";
    let canonical = "module canonical; \
            pub struct Flag(integer); impl std::ops::Boolean for Flag {}";
    let user = "module user; \
            fn custom_condition(value: custom::Flag) -> integer { \
                if value { return 1; } return 0; \
            } \
            fn canonical_condition(value: canonical::Flag) -> integer { \
                if value { return 1; } return 0; \
            }";
    let sink = check_modules(&[
        (ops, FileId(0)),
        (custom, FileId(1)),
        (canonical, FileId(2)),
        (user, FileId(3)),
    ]);
    let condition_errors: Vec<_> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == Some(codes::TYPE_MISMATCH)
                && diagnostic.message.contains("condition")
        })
        .collect();
    assert_eq!(
        condition_errors.len(),
        1,
        "only the namespaced custom Boolean is not the compiler hook: {:#?}",
        sink.diagnostics()
    );
}

#[test]
/// A user trait named like an operator trait does not thereby define a
/// language operator.
fn custom_operator_trait_does_not_define_language_operators() {
    let custom = "module custom; \
            pub trait Operator<op: string, input, output> { \
                fn apply(self, rhs: input) -> output; \
            } \
            pub struct Token(integer); \
            impl Operator<\"+\", Token, Token> for Token { \
                fn apply(self, rhs: Token) -> Token { return self; } \
            }";
    let user = "module user; \
            entity E { a: custom::Token in, b: custom::Token in, y: custom::Token out } \
            impl E { y = a + b; }";
    let sink = check_modules(&[(custom, FileId(0)), (user, FileId(1))]);
    assert!(
        sink.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == Some(codes::TYPE_MISMATCH)
                && diagnostic.message.contains("no `+` operator")
        }),
        "a same-leaf custom trait must not become the Operator hook: {:#?}",
        sink.diagnostics()
    );
}

#[test]
/// Private members are scoped to the owning type, so unrelated code in the
/// same module still cannot reach them.
fn unrelated_code_in_the_defining_module_cannot_use_private_members() {
    let sink = check_modules(&[(
            "module model;\n\
             struct Packet { hidden: integer, pub visible: integer }\n\
             impl Packet {\n\
                 fn secret(self) -> integer { return self.hidden; }\n\
                 fn own_access(self) -> integer { return self.hidden + self.secret(); }\n\
             }\n\
             struct Inspector(integer);\n\
             impl Inspector { fn inspect(self, p: Packet) -> integer { return p.hidden + p.secret(); } }\n\
             fn inspect(p: Packet) -> integer { return p.hidden + p.secret() + p.visible; }\n",
            FileId(0),
        )]);
    let private: Vec<&Diagnostic> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_MEMBER))
        .collect();
    assert_eq!(
            private.len(),
            4,
            "the owner's impl may access both members; an unrelated impl and module function may not: {:?}",
            private
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
        );
}

#[test]
/// Constructing a struct with private fields requires the owning
/// implementation.
fn private_struct_literals_require_the_owning_implementation() {
    let sink = check_modules(&[(
        "module model;\n\
             struct Packet { hidden: integer, pub visible: integer }\n\
             impl Packet { fn make() -> Packet { return { .hidden = 1, .visible = 2 }; } }\n\
             fn make() -> Packet { return { .hidden = 1, .visible = 2 }; }\n",
        FileId(0),
    )]);
    let private = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_MEMBER))
        .count();
    assert_eq!(
        private, 1,
        "only the module function's construction is outside Packet's private domain"
    );
}

#[test]
/// A nested literal cannot bypass the private-construction rule.
fn nested_explicit_struct_literals_cannot_bypass_private_construction() {
    let sink = check_modules(&[(
        "module model;\n\
             struct Packet { hidden: integer, pub visible: integer }\n\
             impl Packet {\n\
                 fn own() -> integer { return Packet { .hidden = 1, .visible = 2 }.hidden; }\n\
             }\n\
             fn inspect() -> integer { return Packet { .hidden = 1, .visible = 2 }.visible; }\n",
        FileId(0),
    )]);
    let private: Vec<&Diagnostic> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_MEMBER))
        .collect();
    assert_eq!(
        private.len(),
        1,
        "the nested construction outside Packet's impl is rejected exactly once: {:?}",
        private
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
    );
    assert!(private[0].message.contains("cannot construct"));
}

#[test]
/// An impl split across blocks in the type's own module keeps private
/// access.
fn a_split_impl_in_the_types_module_keeps_private_access() {
    let declaration = "module model;\nstruct Packet { hidden: integer }\n";
    let implementation =
        "module model;\nimpl Packet { fn get(self) -> integer { return self.hidden; } }\n";
    let sink = check_modules(&[(declaration, FileId(0)), (implementation, FileId(1))]);
    assert!(sink
        .diagnostics()
        .iter()
        .all(|diagnostic| diagnostic.code != Some(codes::PRIVATE_MEMBER)));
}

#[test]
/// An entity instance exposes its ports but not its implementation state.
fn entity_instances_expose_ports_but_not_implementation_state() {
    let sink = check_modules(&[(
        "module model;\n\
             entity Device { value: integer out }\n\
             impl Device { let hidden: integer = 0; value = hidden; }\n\
             fn hidden(device: Device) -> integer { return device.hidden; }\n\
             fn misspelled(device: Device) -> integer { return device.missing; }\n\
             fn port(device: Device) -> integer { return device.value; }\n",
        FileId(0),
    )]);
    assert!(sink.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == Some(codes::PRIVATE_MEMBER) && diagnostic.message.contains("hidden")
    }));
    assert!(sink.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == Some(codes::UNKNOWN_NAME) && diagnostic.message.contains("missing")
    }));
    assert!(sink.diagnostics().iter().all(|diagnostic| {
        diagnostic.code != Some(codes::PRIVATE_MEMBER) || !diagnostic.message.contains("value")
    }));
}

#[test]
/// Entity helper methods stay local to `self` until cross-hierarchy call
/// semantics exist (E-P024).
fn entity_helpers_are_local_to_self_until_instance_calls_have_semantics() {
    let sink = check_modules(&[(
        "module model;\n\
             entity Device {}\n\
             impl Device {\n\
                 fn helper(self) -> integer { return 1; }\n\
                 fn local(self) -> integer { return self.helper(); }\n\
                 fn cross(self, other: Device) -> integer { return other.helper(); }\n\
             }\n",
        FileId(0),
    )]);
    let calls: Vec<&Diagnostic> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == Some(codes::PRIVATE_MEMBER)
                && diagnostic.message.contains("called through an instance")
        })
        .collect();
    assert_eq!(calls.len(), 1, "self helper calls remain local");
}

#[test]
/// An applied view is an explicit structural interface rather than a
/// convenience alias.
fn an_applied_view_is_an_explicit_structural_interface() {
    let provider = "module bus;\n\
            pub struct Stream { data: integer }\n\
            pub view Source for Stream { data out }\n\
            pub entity Producer { bus: Stream Source }\n\
            impl Producer { bus.data = 1; }\n";
    let sink = check_modules(&[(provider, FileId(0))]);
    assert!(
        sink.diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code != Some(codes::PRIVATE_MEMBER)),
        "the view deliberately exposes its backing field"
    );
}

#[test]
/// Public entity instance methods remain rejected pending cross-hierarchy
/// call semantics.
fn public_entity_instance_methods_wait_for_cross_hierarchy_call_semantics() {
    let sink = check_modules(&[(
            "module m;\npub entity Device { value: integer out }\nimpl Device { pub fn read(self) -> integer { return value; } }\n",
            FileId(0),
        )]);
    assert!(sink.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == Some(codes::PRIVATE_MEMBER)
            && diagnostic.message.contains("cannot be public yet")
    }));
}

#[test]
/// A public entity associated function with no receiver is an ordinary
/// namespaced function.
fn public_entity_associated_functions_are_namespaced_functions() {
    let provider = "module model;\n\
            pub entity Device {}\n\
            impl Device {\n\
                pub fn visible(value: integer) -> integer { return value + 1; }\n\
                fn hidden(value: integer) -> integer { return value + 2; }\n\
            }\n";
    let consumer = "module user;\n\
            using model::{Device};\n\
            fn inspect(value: integer) -> integer {\n\
                return Device::visible(value) + Device::hidden(value);\n\
            }\n";
    let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
    let private: Vec<&Diagnostic> = sink
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(codes::PRIVATE_MEMBER))
        .collect();
    assert_eq!(
        private.len(),
        1,
        "only the private associated function fails"
    );
    assert_eq!(sink.error_count(), 1);
    assert!(private[0].message.contains("hidden"));
}

#[test]
/// An entity associated function has no instance scope, so it cannot reach
/// ports or state.
fn entity_associated_functions_have_no_instance_scope() {
    let sink = check_modules(&[(
        "module m;\n\
             entity Device { input: integer in }\n\
             impl Device { pub fn read() -> integer { return input; } }\n",
        FileId(0),
    )]);
    assert!(sink.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == Some(codes::UNKNOWN_NAME) && diagnostic.message.contains("input")
    }));
}

#[test]
/// A private trait keeps its implementation methods private.
fn a_private_trait_keeps_its_implementation_methods_private() {
    let provider = "module model;\n\
            pub struct Value(integer);\n\
            trait Hidden { fn reveal(self) -> integer; }\n\
            impl Hidden for Value { fn reveal(self) -> integer { return 1; } }\n";
    let consumer = "module user;\nusing model::{Value};\nfn inspect(value: Value) -> integer { return value.reveal(); }\n";
    let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
    assert!(sink.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == Some(codes::PRIVATE_MEMBER) && diagnostic.message.contains("reveal")
    }));
}

#[test]
/// A view does not publish the backing struct's methods.
fn a_view_does_not_publish_backing_struct_methods() {
    let provider = "module bus;\n\
            pub struct Stream { data: integer }\n\
            pub view Source for Stream { data out }\n\
            impl Stream { fn secret(self) -> integer { return self.data; } }\n\
            pub entity Producer { bus: Stream Source }\n";
    let consumer = "module user;\nusing bus::{Producer};\nimpl Producer { let seen: integer = bus.secret(); }\n";
    let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
    assert!(sink.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == Some(codes::PRIVATE_MEMBER) && diagnostic.message.contains("secret")
    }));
}

#[test]
/// A view declared in another module cannot publish private backing fields.
fn a_foreign_view_cannot_publish_private_backing_fields() {
    let provider = "module bus;\npub struct Stream { data: integer }\n";
    let consumer = "module user;\nusing bus::{Stream};\npub view Source for Stream { data out }\npub entity Producer { bus: Stream Source }\n";
    let sink = check_modules(&[(provider, FileId(0)), (consumer, FileId(1))]);
    assert!(sink.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == Some(codes::PRIVATE_MEMBER)
            && diagnostic.message.contains("cannot expose private field")
    }));
}

#[test]
/// A module-qualified free call keeps that module's contract.
fn module_qualified_free_calls_keep_their_contracts() {
    let errors = check_src(
        "module m;\n\
             fn take(value: integer) -> Logic { return 'X'; }\n\
             fn same<T>(first: T, second: T) -> T { return first; }\n\
             entity E { y: Bit out }\n\
             impl E {\n\
               let i: integer = 1;\n\
               let r: real = 1.5;\n\
               m::take(r);\n\
               m::take();\n\
               m::same(i, r);\n\
               if m::take(i) { y = '1'; } else { y = '0'; }\n\
             }\n",
    );
    assert_eq!(
        errors, 4,
        "qualification must not discard arity, generic, argument, or return facts"
    );
}

#[test]
/// Functions with the same leaf name in different modules keep distinct
/// contracts.
fn equal_function_leaves_keep_module_specific_contracts() {
    let a = "module a;\npub fn convert(value: integer) -> integer { return value; }\n";
    let b = "module b;\npub fn convert(value: real) -> real { return value; }\n";
    let user = "module user;\nfn use_both() {\n  let i: integer = a::convert(1);\n  let r: real = b::convert(1.5);\n}\n";
    let sink = check_modules(&[(a, FileId(0)), (b, FileId(1)), (user, FileId(2))]);
    assert!(
        sink.diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code != Some(codes::TYPE_MISMATCH)),
        "qualified calls must not borrow the other module's signature: {:?}",
        sink.diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
    );
    assert!(sink
        .diagnostics()
        .iter()
        .all(|diagnostic| diagnostic.code != Some(codes::DUPLICATE_ITEM)));
}

#[test]
/// Constants with the same leaf name in different modules keep distinct
/// types.
fn equal_constant_leaves_keep_module_specific_types() {
    let integer = "module a;\npub const VALUE: integer = 11;\n";
    let real = "module b;\npub const VALUE: real = 2.5;\n";
    let valid = "module user;\nfn use_both() {\n  let i: integer = a::VALUE;\n  let r: real = b::VALUE;\n}\n";
    let sink = check_modules(&[(integer, FileId(0)), (real, FileId(1)), (valid, FileId(2))]);
    assert!(sink.diagnostics().iter().all(|diagnostic| {
        !matches!(
            diagnostic.code,
            Some(codes::TYPE_MISMATCH | codes::DUPLICATE_ITEM)
        )
    }));

    let invalid =
        "module user;\nfn wrong() {\n  let i: integer = b::VALUE;\n  let r: real = a::VALUE;\n}\n";
    let sink = check_modules(&[
        (integer, FileId(3)),
        (real, FileId(4)),
        (invalid, FileId(5)),
    ]);
    assert_eq!(
            sink.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Some(codes::TYPE_MISMATCH))
                .count(),
            1,
            "the real constant must not be typed from the integer declaration; integer-to-real promotion remains legal"
        );
}

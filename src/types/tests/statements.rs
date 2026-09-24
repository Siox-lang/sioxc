//! Statements, stimulus, placement, loops, and indexing.

use super::*;

#[test]
/// A loop variable shadows an outer binding and takes the element type.
fn loop_variables_shadow_outer_types_with_element_types() {
    let errors = check_src(
        "module m;\n\
             #[test] entity T {}\n\
             impl T {\n\
               let item: real = 3.5;\n\
               let bits: Bit[2] = \"10\";\n\
               for item in 0..1 {\n\
                 assert!(item >= 0, \"integer range item\");\n\
                 for item in bits {\n\
                   assert!(item == '0' or item == '1', \"Bit array item\");\n\
                 }\n\
                 assert!(item <= 1, \"integer item restored\");\n\
               }\n\
               assert!(item == 3.5, \"outer real restored\");\n\
             }\n",
    );
    assert_eq!(errors, 0, "each loop body needs its own value-type scope");
}

#[test]
/// A constant array index outside the declared bounds is rejected.
fn data_array_index_is_bound_checked() {
    let oob = |src: &str| diag_codes(src).iter().any(|c| c.contains("E-P003"));
    // A plain count is 0-based: `v[9]` used to read `v[3]` in silence.
    let plain = "module m;\nentity E { y: Bit out, }\nimpl E {\n  let v: Logic[4];\n  y = if v[IX] == '0' { '1' } else { '0' };\n}\n";
    assert!(oob(&plain.replace("IX", "9")));
    assert!(!oob(&plain.replace("IX", "3")));
    // A declared range is indexed by that range, not by `0..len-1`.
    let ranged = "module m;\nentity E { y: Bit out, }\nimpl E {\n  let v: Logic[15..8];\n  y = if v[IX] == '0' { '1' } else { '0' };\n}\n";
    assert!(!oob(&ranged.replace("IX", "15")));
    assert!(!oob(&ranged.replace("IX", "8")));
    assert!(oob(&ranged.replace("IX", "7")));
    assert!(oob(&ranged.replace("IX", "16")));

    // Packed nominal array families retain the same nonzero labels. Cover both
    // an impl-local declaration and a port, whose metadata is collected
    // on different checker paths.
    let packed_local = "module m; using std::bits::unsigned; using std::logic::Logic; \
            entity E { y: Logic out } impl E { let v: unsigned[15..8]; y = v[IX]; }";
    assert!(!oob(&packed_local.replace("IX", "15")));
    assert!(!oob(&packed_local.replace("IX", "8")));
    assert!(oob(&packed_local.replace("IX", "7")));
    let packed_port = "module m; using std::bits::unsigned; using std::logic::Logic; \
            entity E { v: unsigned[15..8] in, y: Logic out } impl E { y = v[IX]; }";
    assert!(!oob(&packed_port.replace("IX", "15")));
    assert!(oob(&packed_port.replace("IX", "16")));
}

/// An entity may be instantiated at the root layer of another entity's
/// body, or inside a generate `for`/`if` — nowhere else. A `match` arm and
/// a function body used to be accepted and then quietly dropped by
/// elaboration, so the design ran as though the instance had never been
/// written; a function failed later still, with "contains an Unknown".
#[test]
fn an_entity_cannot_be_instantiated_outside_a_generate() {
    let count = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::INSTANCE_PLACEMENT))
            .count()
    };
    const CELL: &str = "module m;\nentity Cell { i: unsigned[8] in, o: unsigned[8] out }\n\
                            impl Cell { o = i; }\n";

    let in_match = count(&format!(
            "{CELL}entity E {{ s: unsigned[2] in, y: unsigned[8] out }}\n\
             impl E {{ y = 0; match s {{ 0 => {{ let c: Cell = {{ .i = 5 }}; }} _ => {{ y = 1; }} }} }}\n"
        ));
    assert_eq!(in_match, 1, "a `match` arm");

    let in_fn = count(&format!(
            "{CELL}fn helper(x: unsigned[8]) -> unsigned[8] {{ let c: Cell = {{ .i = x }}; return 1; }}\n\
             entity E {{ y: unsigned[8] out }}\nimpl E {{ y = helper(5); }}\n"
        ));
    assert_eq!(in_fn, 1, "a function body");

    // The legal placements: root layer, and a generate `for`/`if`. The
    // behavioural-`if` case is elaboration's to report, not this stage's.
    let root = count(&format!(
        "{CELL}entity E {{ y: unsigned[8] out }}\n\
             impl E {{ let c: Cell = {{ .i = 5 }}; y = c.o; }}\n"
    ));
    assert_eq!(root, 0, "the root layer of an entity body");

    let generate = count(&format!(
        "{CELL}entity E {{ y: unsigned[8] out }}\n\
             impl E {{ let s: Cell[2]; for i in 0..1 {{ s[i] = Cell {{ .i = i }}; }}\n\
             if 1 == 1 {{ let g: Cell = {{ .i = 3 }}; y = g.o; }} else {{ y = 0; }} }}\n"
    ));
    assert_eq!(generate, 0, "a generate `for` and a generate `if`");

    // A generic parameter names data, never an instance, even when an
    // entity happens to share its name. Checking the head against the
    // entity table alone rejected `let held: T` in a generic method.
    let shadowed_method = count(
        "module m;\nentity T { i: unsigned[8] in, o: unsigned[8] out }\nimpl T { o = i; }\n\
             struct Box<T> { v: T }\n\
             impl<T> Box<T> { fn get(self) -> T { let held: T = self.v; return held; } }\n",
    );
    assert_eq!(shadowed_method, 0, "`T` here is the impl's parameter");

    let shadowed_match = count(
        "module m;\nentity T { i: unsigned[8] in, o: unsigned[8] out }\nimpl T { o = i; }\n\
             entity Sel<T> { s: unsigned[2] in, d: T in, y: T out }\n\
             impl<T> Sel<T> { y = d; match s { 0 => { let a: T; y = d; } _ => { y = d; } } }\n",
    );
    assert_eq!(
        shadowed_match, 0,
        "and in a `match` arm of a generic entity"
    );

    // The entity is still an entity outside that binder.
    let unshadowed = count(
        "module m;\nentity T { i: unsigned[8] in, o: unsigned[8] out }\nimpl T { o = i; }\n\
             entity E { s: unsigned[2] in, y: unsigned[8] out }\n\
             impl E { y = 0; match s { 0 => { let a: T = { .i = 1 }; } _ => { y = 1; } } }\n",
    );
    assert_eq!(unshadowed, 1, "no binder in scope, so `T` is the entity");
}

/// A statement expression that is not a call was dropped by lowering's
/// catch-all without a word, so a misspelled name compiled clean and a
/// stray `continue;` — a Rust habit siox does not have — looked accepted
/// while the `for` body ran every iteration anyway.
#[test]
fn a_statement_with_no_effect_is_reported() {
    // Count E-P019 alone: `q + 1;` also trips the width checker, and this
    // test is about the statement being reported at all, not about what
    // else the discarded expression happens to be wrong about.
    let no_effect = |src: &str| {
        let src = format!("{src}{VEC}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.diagnostics()
            .iter()
            .filter(|d| d.code == Some(codes::NO_EFFECT_STATEMENT))
            .count()
    };
    let body = |stmts: &str| {
        no_effect(&format!(
            "module m;\nentity E {{ y: unsigned[8] out }}\n\
                 impl E {{ let q: unsigned[8] = 3; {stmts} y = q; }}\n"
        ))
    };
    assert_eq!(body("zzz_undefined_name;"), 1, "a misspelled bare name");
    assert_eq!(body("q;"), 1, "a name that does resolve is still dead");
    assert_eq!(body("q + 1;"), 1, "a computed value that goes nowhere");
    assert_eq!(
        body("for i in 0..2 { continue; }"),
        1,
        "`continue` has no meaning in an unrolled loop"
    );
    assert_eq!(body("if q == 3 { zzz; }"), 1, "nested in a block");

    // A call is the one statement shape that does something.
    assert_eq!(body(""), 0, "the same body without the dead statement");
    let call = no_effect(
        "module m;\nstruct S { v: unsigned[8] }\n\
             impl S { fn bump(self) { self.v = self.v + 1; } }\n\
             entity E { y: unsigned[8] out }\n\
             impl E { let s: S = { .v = 3 }; s.bump(); y = s.v; }\n",
    );
    assert_eq!(call, 0, "a method call as a statement");
}

/// Stimulus in an entity body was dropped by lowering without a word —
/// most dangerously `assert!`, which let a check written into a design
/// silently never run.
#[test]
fn stimulus_outside_a_testbench_is_reported() {
    let hw = |body: &str| {
        check_src(&format!(
            "module m;\nentity E {{ y: unsigned[8] out, }}\nimpl E {{ y = 1; {body} }}\n"
        ))
    };
    assert_eq!(hw("await 1ns;"), 1, "await needs simulation time");
    assert_eq!(
        hw(r#"assert!(y == 1, "x");"#),
        1,
        "an assertion needs a run"
    );
    assert_eq!(hw(r#"print!("hi");"#), 1, "printing needs a run");
    assert_eq!(hw(""), 0, "plain hardware is unaffected");

    // All of it is exactly what a testbench is for.
    let tb = check_src(
            "module m;\n#[test] entity T {}\n\
             impl T { let y: unsigned[8] = 1; await 1ns; assert!(y == 1, \"x\"); print!(\"hi\"); }\n",
        );
    assert_eq!(tb, 0, "a testbench may drive and check");
}

/// A constant bit index past the end of a packed vector lowered to
/// `Unknown` and only failed later with a generic engine message.
#[test]
fn out_of_bounds_constant_index_is_rejected() {
    let ent = "module m;\nentity E { a: unsigned[8] in, y: unsigned[8] out, }\nimpl E { y = ";
    assert_eq!(
        check_src(&format!("{ent}a[9]; }}\n")),
        1,
        "bit 9 of a unsigned[8]"
    );
    assert_eq!(
        check_src(&format!("{ent}a[15..8]; }}\n")),
        2,
        "both slice bounds"
    );
    assert_eq!(
        check_src(
            "module m;\nentity E { a: unsigned[8] in, y: Logic out, }\n\
                 impl E { y = a[7]; }\n"
        ),
        0,
        "the top bit is in range"
    );
    assert_eq!(
        check_src(&format!("{ent}a[7..0]; }}\n")),
        0,
        "a full-width slice"
    );
    // A runtime index can't be checked statically and must stay allowed.
    let dynamic = "module m;\nentity E { a: unsigned[8] in, i: unsigned[8] in, y: Logic out, }\nimpl E { y = a[i]; }\n";
    assert_eq!(check_src(dynamic), 0);

    // An instance array is declared with a plain count, so it is 0-based
    // and checkable the same way.
    let inst = |i: u32| {
        format!(
                "module m;\nentity Sub {{ a: unsigned[8] in, y: unsigned[8] out, }}\nimpl Sub {{ y = a; }}\n\
                 entity E {{ a: unsigned[8] in, y: unsigned[8] out, }}\nimpl E {{ let s: Sub[4]; y = s[{i}].y; }}\n"
            )
    };
    assert_eq!(check_src(&inst(9)), 1, "instance 9 of a Sub[4]");
    assert_eq!(check_src(&inst(3)), 0, "the last instance is in range");
}

#[test]
/// Built-in indexing requires a numeric index value.
fn intrinsic_indices_require_numeric_index_values() {
    let tb = |body: &str| {
        format!(
            "module m;\n#[test] entity T {{}}\nimpl T {{\n\
                 let bits: unsigned[8] = 0;\n{body}\n}}\n"
        )
    };
    assert_eq!(
        check_src(&tb("let r: real = 1.5; let value: Logic = bits[r];")),
        1,
        "a real is not a bit index"
    );
    assert_eq!(
        check_src(&tb("let flag: Bool = true; let value: Logic = bits[flag];")),
        1,
        "an enum value is not a bit index"
    );
    assert_eq!(
        check_src(&tb("let r: real = 1.5; bits[r] = '1';")),
        1,
        "indexed writes enforce the same index domain"
    );
    assert_eq!(
        check_src(&tb("let r: real = 1.5; bits[0..r] = 0;")),
        1,
        "slice bounds are numeric index values too"
    );
    assert_eq!(
        check_src(&tb(
            "let index: unsigned[3] = 2; let value: Logic = bits[index];"
        )),
        0,
        "packed numeric values remain valid dynamic indices"
    );
}

#[test]
/// `for` requires a range or an iterable array.
fn for_loops_require_ranges_or_iterable_arrays() {
    let tb = |range: &str| {
        format!(
                "module m;\n#[test] entity T {{}}\nimpl T {{ for item in {range} {{ print!(\"{{}}\", item); }} }}\n"
            )
    };
    assert_eq!(check_src(&tb("5")), 1, "an integer is not iterable");
    assert_eq!(check_src(&tb("true")), 1, "an enum is not iterable");
    assert_eq!(
        check_src(&tb("1.5..2.5")),
        2,
        "both range endpoints must be integer-like"
    );
    assert_eq!(check_src(&tb("0..3")), 0, "integer ranges remain valid");
}

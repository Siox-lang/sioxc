//! The compiler API's macro expansion log, which an editor reads to show
//! generated code apart from handwritten source and to walk a nested
//! expansion back to the call the user wrote.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

const SOURCE: &str = "module demo;
macro twice($x: expr) { $x + $x }
macro quad($x: expr) { twice!(twice!($x)) }
macro make_fn($name: ident) { fn $name() -> integer { return 42; } }
make_fn!(answer);
entity E { y: integer out, }
impl E { y = quad!(1) + answer(); }
";

#[test]
fn expansions_name_their_call_their_macro_and_their_parent() {
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(SourceInput::memory("demo.siox", SOURCE), Emit::Metadata),
    );
    assert!(
        compilation.succeeded(),
        "{}",
        compilation.render_diagnostics()
    );
    let text_at = |span: siox::diag::Span| &SOURCE[span.start as usize..span.end as usize];
    let user: Vec<_> = compilation
        .macro_expansions
        .iter()
        .filter(|expansion| expansion.name.starts_with("demo::"))
        .collect();

    // The item macro: a call in handwritten source, generating a function.
    let make_fn = user
        .iter()
        .find(|e| e.name == "demo::make_fn")
        .expect("make_fn!");
    assert_eq!(text_at(make_fn.call), "make_fn!(answer)");
    assert_eq!(make_fn.depth, 0);
    assert_eq!(make_fn.parent, None);
    assert!(
        make_fn.text.contains("fn answer() -> integer"),
        "{}",
        make_fn.text
    );

    // `quad!(1)` expands to two nested `twice!` calls, one level each.
    let quad_index = compilation
        .macro_expansions
        .iter()
        .position(|e| e.name == "demo::quad")
        .expect("quad!");
    let quad = &compilation.macro_expansions[quad_index];
    assert_eq!(text_at(quad.call), "quad!(1)");
    assert_eq!(text_at(quad.definition), "quad");
    assert!(quad.text.contains("twice!"), "{}", quad.text);
    // The outer `twice!` substitutes its argument twice, so the inner
    // `twice!(1)` is expanded once per copy: three expansions in all.
    let twices: Vec<_> = user.iter().filter(|e| e.name == "demo::twice").collect();
    assert_eq!(twices.len(), 3, "{user:#?}");
    let outer = twices.iter().find(|e| e.depth == 1).expect("outer twice!");
    assert_eq!(outer.parent, Some(quad_index));
    let outer_index = compilation
        .macro_expansions
        .iter()
        .position(|e| e == **outer)
        .expect("outer index");
    let inners: Vec<_> = twices.iter().filter(|e| e.depth == 2).collect();
    assert_eq!(inners.len(), 2, "{user:#?}");
    assert!(inners.iter().all(|e| e.parent == Some(outer_index)));
    assert_eq!(outer.text, "twice!(1) + twice!(1)");
    // A nested call is written in a macro body, not where the user called.
    assert!(
        text_at(outer.call).starts_with("twice!"),
        "{}",
        text_at(outer.call)
    );
    assert!(outer.call.start > quad.definition.start);
}

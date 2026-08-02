//! An arithmetic generic argument names the spelling that works.
//!
//! A generic argument is a *postfix* expression (`parse_generic_value`), a
//! deliberate restriction: `Bank<K > 2>` would otherwise be ambiguous, the
//! problem Rust solves by requiring braces. Parentheses already sidestep it —
//! `Bank<(K * 2)>` parses and elaborates to 6 — but nothing said so. Writing
//! `Bank<K * 2>` stopped the argument after `K` and then reported "expected
//! `>` to close a generic argument list", pointing at the `*` as if the list
//! were malformed rather than the expression unparenthesised. The cure was one
//! character away and undiscoverable from the message.
//!
//! The reverse direction is the risk this fix carries: `>>` closes two nested
//! generics (`close_generic` splits it in place), so an operator check that
//! includes it rejects every bound like `struct Wrap<T: Meter<8>>`. Both
//! directions are asserted here.

use siox::diag::{DiagnosticSink, Severity, SourceMap};

/// Parse `src` and return every error message.
fn parse_errors(src: &str) -> Vec<String> {
    let mut sources = SourceMap::new();
    let file = sources.add("test.siox", src);
    let mut sink = DiagnosticSink::new();
    siox::syntax::parse_module(file, src, &mut sink);
    sink.diagnostics()
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.clone())
        .collect()
}

fn in_generic(arg: &str) -> String {
    format!(
        "module m;\n\
         using std::bits::{{unsigned}};\n\
         entity Bank<W: integer> {{ y: unsigned[8] out }}\n\
         impl<W: integer> Bank<W> {{ y = W; }}\n\
         entity Use {{ y: unsigned[8] out }}\n\
         impl Use {{ let b: Bank<{arg}> = {{}}; y = b.y; }}\n"
    )
}

#[test]
fn an_unparenthesised_operator_points_at_the_parentheses() {
    // Each operator the restriction can bite on, and the argument shapes that
    // reach it: bare, folded from a constant, and behind a named argument.
    for (arg, op) in [
        ("K * 2", "*"),
        ("K + 1", "+"),
        ("K - 1", "-"),
        ("K / 2", "/"),
        ("K << 1", "<<"),
        ("W = K + 1", "+"),
    ] {
        let errors = parse_errors(&in_generic(arg));
        let hint = format!("`{op}` needs parentheses here: write `(a {op} b)`");
        // The hint must be the *only* error: the check consumes the rest of
        // the expression so the argument list still closes. Without that
        // recovery the hint is followed by the cascade it was meant to
        // replace, and a message competing with three others is not a cure.
        assert_eq!(
            errors,
            vec![hint],
            "`Bank<{arg}>` should report the parentheses hint and nothing else"
        );
    }
}

#[test]
fn the_spellings_that_work_are_left_alone() {
    // The parenthesised forms the hint recommends must actually parse — a hint
    // pointing at a second error would be worse than the original message.
    for arg in ["(K * 2)", "(K + 1)", "(K << 1)", "6", "K", "(K)", "W = 6"] {
        let errors = parse_errors(&in_generic(arg));
        assert!(
            errors.is_empty(),
            "`Bank<{arg}>` should parse cleanly, got {errors:?}"
        );
    }
}

#[test]
fn a_bound_closing_on_a_shift_token_still_parses() {
    // `Meter<8>>` ends on a single `>>` token that closes both lists. Treating
    // it as a shift operator here rejected every generic bound in the corpus.
    let src = "module m;\n\
               using std::bits::{unsigned};\n\
               trait Meter<W: integer> { fn raw(self) -> unsigned[8]; }\n\
               struct Wrap<T: Meter<8>> { inner: T }\n";
    let errors = parse_errors(src);
    assert!(
        errors.is_empty(),
        "a bound ending in `>>` should parse, got {errors:?}"
    );
}

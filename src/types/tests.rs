//! Tests for semantic analysis: type checking and its diagnostics.

use super::*;
use crate::diag::FileId;

mod attributes;
mod calls;
mod declarations;
mod matches;
mod operators;
mod statements;
mod tuples;
mod visibility;
mod writes;

const VEC: &str = "\n\
        enum Bit { '0', '1' }\n\
        enum Logic { '0', '1', 'Z', 'X', 'U', 'W', 'L', 'H', '-' }\n\
        enum Bool { false, true }\n\
        enum Ordering { Less, Equal, Greater }\n\
        trait ClockLike { fn rising() -> Bool; }\n\
        impl Boolean for Bit { fn as_bool() -> Bool { return true; } }\n\
        impl Boolean for Bool { fn as_bool() -> Bool { return self; } }\n\
        impl ClockLike for Bit { fn rising() -> Bool { return false; } }\n\
        impl And<Bool, Bool> for Bool { fn and(rhs: Bool) -> Bool { return self; } }\n\
        impl Or<Bool, Bool> for Bool { fn or(rhs: Bool) -> Bool { return self; } }\n\
        impl Not<Bool> for Bool { fn not() -> Bool { return self; } }\n\
        impl And<Bit, Bit> for Bit { fn and(rhs: Bit) -> Bit { return self; } }\n\
        impl Or<Bit, Bit> for Bit { fn or(rhs: Bit) -> Bit { return self; } }\n\
        impl Not<Bit> for Bit { fn not() -> Bit { return self; } }\n\
        impl And<Logic, Logic> for Logic { fn and(rhs: Logic) -> Logic { return self; } }\n\
        impl Or<Logic, Logic> for Logic { fn or(rhs: Logic) -> Logic { return self; } }\n\
        impl Not<Logic> for Logic { fn not() -> Logic { return self; } }\n\
        impl<T: And<T, T>> And<T, T> for T[] { fn and(rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Or<T, T>> Or<T, T> for T[] { fn or(rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Not<T>> Not<T> for T[] { fn not() -> T[] { return self; } }\n\
        struct unsigned(Logic[]);\n\
        impl Add<unsigned, unsigned> for unsigned { fn add(rhs: unsigned) -> unsigned { return self; } }\n\
        impl Div<unsigned, unsigned> for unsigned { fn div(rhs: unsigned) -> unsigned { return self; } }\n\
        impl Eq<unsigned> for unsigned { fn eq(rhs: unsigned) -> Bool { return true; } }\n\
        impl Ord<unsigned> for unsigned { fn lt(rhs: unsigned) -> Bool { return false; } fn le(rhs: unsigned) -> Bool { return true; } }\n\
        struct signed(Logic[]);\n";

/// Type-check `src` with the vector prelude appended and return its error
/// count.
fn check_src(src: &str) -> usize {
    let src = format!("{src}{VEC}");
    let src = src.as_str();
    let mut sink = DiagnosticSink::new();
    let mut module = crate::syntax::parse_module(FileId(0), src, &mut sink);
    crate::syntax::attributes::attach(std::slice::from_mut(&mut module), &mut sink);
    assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    let parse_resolve_errors = sink.error_count();
    check(std::slice::from_ref(&module), &resolved, &mut sink);
    sink.error_count() - parse_resolve_errors
}

/// Type-check several sources as one program and return the sink.
fn check_modules(sources: &[(&str, FileId)]) -> DiagnosticSink {
    let mut sink = DiagnosticSink::new();
    let mut modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    crate::syntax::attributes::attach(&mut modules, &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    check(&modules, &resolved, &mut sink);
    sink
}

/// Collect the diagnostic codes checking `src` produces.
fn diag_codes(src: &str) -> Vec<String> {
    let src = format!("{src}{VEC}");
    let mut sink = DiagnosticSink::new();
    let mut module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
    crate::syntax::attributes::attach(std::slice::from_mut(&mut module), &mut sink);
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    check(std::slice::from_ref(&module), &resolved, &mut sink);
    sink.diagnostics()
        .iter()
        .map(|d| format!("{:?}", d.code))
        .collect()
}

/// The number of warnings with a given code emitted while checking `src`.
fn warnings(src: &str, code: &str) -> usize {
    let src = format!("{src}{VEC}");
    let src = src.as_str();
    let mut sink = DiagnosticSink::new();
    let mut module = crate::syntax::parse_module(FileId(0), src, &mut sink);
    crate::syntax::attributes::attach(std::slice::from_mut(&mut module), &mut sink);
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    check(std::slice::from_ref(&module), &resolved, &mut sink);
    sink.diagnostics()
        .iter()
        .filter(|d| d.code == Some(code))
        .count()
}

//! Tests for semantic analysis: type checking and its diagnostics.

use super::*;
use crate::diag::FileId;

mod attributes;
mod calls;
mod declarations;
mod matches;
mod operators;
mod statements;
mod visibility;
mod writes;

const VEC: &str = "\n\
        enum Bit { '0', '1' }\n\
        enum Logic { '0', '1', 'Z', 'X', 'U', 'W', 'L', 'H', '-' }\n\
        enum Bool { false, true }\n\
        enum Ordering { Less, Equal, Greater }\n\
        trait ClockLike { fn rising(self) -> Bool; }\n\
        impl Boolean for Bit { fn as_bool(self) -> Bool { return true; } }\n\
        impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }\n\
        impl ClockLike for Bit { fn rising(self) -> Bool { return false; } }\n\
        impl And<Bool, Bool> for Bool { fn and(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Or<Bool, Bool> for Bool { fn or(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Not<Bool> for Bool { fn not(self) -> Bool { return self; } }\n\
        impl And<Bit, Bit> for Bit { fn and(self, rhs: Bit) -> Bit { return self; } }\n\
        impl Or<Bit, Bit> for Bit { fn or(self, rhs: Bit) -> Bit { return self; } }\n\
        impl Not<Bit> for Bit { fn not(self) -> Bit { return self; } }\n\
        impl And<Logic, Logic> for Logic { fn and(self, rhs: Logic) -> Logic { return self; } }\n\
        impl Or<Logic, Logic> for Logic { fn or(self, rhs: Logic) -> Logic { return self; } }\n\
        impl Not<Logic> for Logic { fn not(self) -> Logic { return self; } }\n\
        impl<T: And<T, T>> And<T, T> for T[] { fn and(self, rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Or<T, T>> Or<T, T> for T[] { fn or(self, rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Not<T>> Not<T> for T[] { fn not(self) -> T[] { return self; } }\n\
        struct unsigned(Logic[]);\n\
        impl Add<unsigned, unsigned> for unsigned { fn add(self, rhs: unsigned) -> unsigned { return self; } }\n\
        impl Div<unsigned, unsigned> for unsigned { fn div(self, rhs: unsigned) -> unsigned { return self; } }\n\
        impl Eq<unsigned> for unsigned { fn eq(self, rhs: unsigned) -> Bool { return true; } }\n\
        impl Ord<unsigned> for unsigned { fn lt(self, rhs: unsigned) -> Bool { return false; } fn le(self, rhs: unsigned) -> Bool { return true; } }\n\
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

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
        impl Operator<\"and\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"or\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"not\", Bool, Bool> for Bool { fn apply(self) -> Bool { return self; } }\n\
        impl Operator<\"and\", Bit, Bit> for Bit { fn apply(self, rhs: Bit) -> Bit { return self; } }\n\
        impl Operator<\"or\", Bit, Bit> for Bit { fn apply(self, rhs: Bit) -> Bit { return self; } }\n\
        impl Operator<\"not\", Bit, Bit> for Bit { fn apply(self) -> Bit { return self; } }\n\
        impl Operator<\"and\", Logic, Logic> for Logic { fn apply(self, rhs: Logic) -> Logic { return self; } }\n\
        impl Operator<\"or\", Logic, Logic> for Logic { fn apply(self, rhs: Logic) -> Logic { return self; } }\n\
        impl Operator<\"not\", Logic, Logic> for Logic { fn apply(self) -> Logic { return self; } }\n\
        impl<T: Operator<\"and\", T, T>> Operator<\"and\", T, T> for T[] { fn apply(self, rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Operator<\"or\", T, T>> Operator<\"or\", T, T> for T[] { fn apply(self, rhs: T[]) -> T[] { return self; } }\n\
        impl<T: Operator<\"not\", T, T>> Operator<\"not\", T, T> for T[] { fn apply(self) -> T[] { return self; } }\n\
        struct unsigned(Logic[]);\n\
        impl Operator<\"+\", unsigned, unsigned> for unsigned { fn apply(self, rhs: unsigned) -> unsigned { return self; } }\n\
        impl Operator<\"/\", unsigned, unsigned> for unsigned { fn apply(self, rhs: unsigned) -> unsigned { return self; } }\n\
        impl Operator<\"<=>\", unsigned, Ordering> for unsigned { fn apply(self, rhs: unsigned) -> Ordering { return Equal; } }\n\
        struct signed(Logic[]);\n";

/// Type-check `src` with the vector prelude appended and return its error
/// count.
fn check_src(src: &str) -> usize {
    let src = format!("{src}{VEC}");
    let src = src.as_str();
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
    assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    let parse_resolve_errors = sink.error_count();
    check(std::slice::from_ref(&module), &resolved, &mut sink);
    sink.error_count() - parse_resolve_errors
}

/// Type-check several sources as one program and return the sink.
fn check_modules(sources: &[(&str, FileId)]) -> DiagnosticSink {
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    check(&modules, &resolved, &mut sink);
    sink
}

/// Collect the diagnostic codes checking `src` produces.
fn diag_codes(src: &str) -> Vec<String> {
    let src = format!("{src}{VEC}");
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
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
    let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
    let resolved = crate::resolve::resolve(std::slice::from_ref(&module), &mut sink);
    check(std::slice::from_ref(&module), &resolved, &mut sink);
    sink.diagnostics()
        .iter()
        .filter(|d| d.code == Some(code))
        .count()
}

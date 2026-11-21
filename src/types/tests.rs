//! Tests for semantic analysis: type checking and its diagnostics.

use super::*;
use crate::diag::FileId;

mod attributes;
mod calls;
mod declaration_index;
mod declarations;
mod generic_layouts;
mod lang_items;
mod matches;
mod operators;
mod statements;
mod trait_contracts;
mod tuples;
mod visibility;
mod writes;

const VEC: &str = "\n\
        enum Bit { '0', '1' }\n\
        enum Logic { '0', '1', 'Z', 'X', 'U', 'W', 'L', 'H', '-' }\n\
        use core::primitive::Bool;\n\
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
        impl Prefix<\"x\", Char[]> for unsigned {}\n\
        impl Prefix<\"o\", Char[]> for unsigned {}\n\
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
    let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
    let mut modules = with_core(vec![module], &mut sink);
    assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    crate::resolve::attributes::attach(&mut modules, &resolved, &mut sink);
    crate::resolve::attributes::fold_reads(&mut modules, &resolved, &mut sink);
    let parse_resolve_errors = sink.error_count();
    check(&modules, &resolved, &mut sink);
    sink.error_count() - parse_resolve_errors
}

/// Type-check several sources as one program and return the sink.
fn check_modules(sources: &[(&str, FileId)]) -> DiagnosticSink {
    let mut sink = DiagnosticSink::new();
    let modules: Vec<Module> = sources
        .iter()
        .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
        .collect();
    let mut modules = with_core(modules, &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    crate::resolve::attributes::attach(&mut modules, &resolved, &mut sink);
    crate::resolve::attributes::fold_reads(&mut modules, &resolved, &mut sink);
    check(&modules, &resolved, &mut sink);
    sink
}

/// Collect the diagnostic codes checking `src` produces.
fn diag_codes(src: &str) -> Vec<String> {
    let src = format!("{src}{VEC}");
    let mut sink = DiagnosticSink::new();
    let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
    let mut modules = with_core(vec![module], &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    crate::resolve::attributes::attach(&mut modules, &resolved, &mut sink);
    crate::resolve::attributes::fold_reads(&mut modules, &resolved, &mut sink);
    check(&modules, &resolved, &mut sink);
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
    let mut modules = with_core(vec![module], &mut sink);
    let resolved = crate::resolve::resolve(&modules, &mut sink);
    crate::resolve::attributes::attach(&mut modules, &resolved, &mut sink);
    crate::resolve::attributes::fold_reads(&mut modules, &resolved, &mut sink);
    check(&modules, &resolved, &mut sink);
    sink.diagnostics()
        .iter()
        .filter(|d| d.code == Some(code))
        .count()
}

/// Use the real Boolean declaration instead of a spelling-based test fallback.
fn with_core(mut modules: Vec<Module>, sink: &mut DiagnosticSink) -> Vec<Module> {
    if !modules.iter().any(|module| {
        module
            .path
            .segments
            .iter()
            .map(|part| part.text.as_str())
            .eq(["core", "primitive"])
    }) {
        let file = FileId(
            modules
                .iter()
                .map(|module| module.span.file.0)
                .max()
                .unwrap_or(0)
                + 1,
        );
        modules.push(crate::syntax::parse_module(
            file,
            include_str!("../../core/primitive.siox"),
            sink,
        ));
    }
    modules
}

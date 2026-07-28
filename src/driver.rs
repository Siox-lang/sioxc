//! `sioxc` — the siox compiler driver (spec Stage 12).
//!
//! This binary is a thin compiler driver. One input is compiled per invocation;
//! flags select the output artifact. Project discovery, dependency builds, test
//! execution, and simulation tooling belong in a future Cargo-like tool.
//!
//! Usage (rustc-shaped — a bare file compiles it):
//! ```text
//! sioxc <file>            # compile the #[top] design to a native object
//! sioxc <file> --emit metadata  # analyze without code generation
//! sioxc <file> --emit llvm-ir   # emit textual LLVM IR
//! sioxc <file> --test           # compile a native test executable
//! ```
//! Exit code is nonzero on compilation failure.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod build;

use clap::{Parser, ValueEnum};
use siox::diag::{DiagnosticSink, Severity, SourceMap};
use siox::syntax::ast::{Item, Module, Path as AstPath, UsingKind};
use siox::syntax::token::{Token, TokenKind};
use siox::syntax::{lexer::Lexer, parser, pretty};

#[derive(Parser)]
#[command(name = "sioxc", version, about = "The siox compiler (Phase 1)")]
struct Cli {
    /// The `.siox` file to compile (builds its `#[top]` design). Bare
    /// `sioxc foo.siox` compiles the file, like `rustc foo.rs`.
    file: PathBuf,
    /// The top entity to build (default: the single `#[top]` entity).
    #[arg(long)]
    top: Option<String>,
    /// Output object path for a bare build (default: `<file>.o`).
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Compile `#[test]` entities into a native test executable.
    #[arg(long)]
    test: bool,
    /// Compiler artifact to emit.
    #[arg(long, value_enum, default_value_t = Emit::Object)]
    emit: Emit,
    /// Include frontend token/item tracing in textual frontend output.
    #[arg(short, long)]
    verbose: bool,
    /// Directory holding the standard library (`std::logic` -> `<dir>/logic.siox`).
    #[arg(long, global = true, default_value = "std")]
    std: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Emit {
    /// Native object file (the default).
    Object,
    /// Type-check and elaborate without code generation.
    Metadata,
    /// Canonical source reconstructed from the AST.
    Source,
    /// Raw lexer tokens.
    Tokens,
    /// Debug AST.
    Ast,
    /// Elaborated instance tree.
    Tree,
    /// Normalized digital IR.
    Ir,
    /// LLVM textual IR.
    LlvmIr,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    if cli.test {
        if !matches!(cli.emit, Emit::Object) {
            eprintln!("error: --test currently requires --emit object");
            return ExitCode::FAILURE;
        }
        return cmd_build_test(&cli.file, &cli.std, cli.out.as_deref());
    }
    match cli.emit {
        Emit::Object => cmd_build(&cli.file, &cli.std, cli.top.as_deref(), cli.out.as_deref()),
        Emit::Metadata => cmd_check(&cli.file, &cli.std, cli.verbose),
        Emit::Tokens => cmd_tokens(&cli.file),
        Emit::Source => match run_frontend(&cli.file, &cli.std, cli.verbose) {
            Ok(fe) => {
                print!("{}", pretty::print_module(fe.entry()));
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Emit::Ast => match run_frontend(&cli.file, &cli.std, cli.verbose) {
            Ok(fe) => {
                println!("{:#?}", fe.entry());
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Emit::Ir => cmd_ir(&cli.file, &cli.std),
        Emit::LlvmIr => cmd_emit_llvm(&cli.file, &cli.std),
        Emit::Tree => cmd_tree(&cli.file, &cli.std),
    }
}

/// Everything the frontend produces, with diagnostics not yet rendered so a
/// caller can keep running later stages on the same sink.
struct FrontendOut {
    sources: SourceMap,
    /// The entry module first, then any transitively-loaded `std::` modules.
    modules: Vec<Module>,
    sink: DiagnosticSink,
}

impl FrontendOut {
    /// The entry file's module (the one the command was pointed at).
    fn entry(&self) -> &Module {
        &self.modules[0]
    }
}

/// Read, lex and parse `path`, then transitively load the `std::` modules it
/// imports from `std_root`. With `trace`, narrates the lex/parse steps. Does not
/// render diagnostics — the caller decides when. `Err` only on a read failure.
fn lex_parse(path: &Path, std_root: &Path, trace: bool) -> Result<FrontendOut, ExitCode> {
    if path.is_dir() {
        eprintln!(
            "error: {} is a directory; running a whole directory is not supported yet — \
             pass one .siox file (e.g. `sioxc --test {}/<file>.siox`)",
            path.display(),
            path.display()
        );
        return Err(ExitCode::FAILURE);
    }
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", path.display());
            return Err(ExitCode::FAILURE);
        }
    };

    let mut sources = SourceMap::new();
    let file = sources.add(path.display().to_string(), src.clone());
    let mut sink = DiagnosticSink::new();

    if trace {
        eprintln!("== lex ({}) ==", path.display());
    }
    let tokens = Lexer::new(file, &src).tokenize(&mut sink);
    let mut custom_operators = discover_std_operators(std_root);
    custom_operators.extend(parser::discover_custom_operators(&src, &tokens));
    if trace {
        let trivia = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Comment)
            .count();
        eprintln!("   {} tokens ({} comment trivia)", tokens.len(), trivia);
        dump_tokens(&src, &tokens);
        eprintln!("\n== parse ==");
    }
    let module = parser::Parser::new(&src, tokens, &mut sink)
        .with_custom_operators(&custom_operators)
        .parse_module();
    if trace {
        dump_items(&module);
    }

    let mut fe = FrontendOut {
        sources,
        modules: vec![module],
        sink,
    };
    load_std_deps(&mut fe, std_root, trace, &custom_operators);
    Ok(fe)
}

/// Transitively parse the `std::` modules imported by the already-loaded
/// modules, mapping `std::a::b` to `<std_root>/a/b.siox`. A missing file is left
/// unresolved so name resolution reports it against the `using`.
fn load_std_deps(
    fe: &mut FrontendOut,
    std_root: &Path,
    trace: bool,
    custom_operators: &std::collections::HashMap<String, u8>,
) {
    let mut loaded: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut queue: Vec<AstPath> = using_bases(fe.entry());
    // A wrong `--std` used to surface as a pile of "no `unsigned` in `std::bits`"
    // import errors, which blames the library rather than the path. Say it
    // once, plainly — but only when the file actually imports `std`, since a
    // bare-kernel file legitimately runs with no std at all.
    let wants_std = queue
        .iter()
        .any(|b| b.segments.first().is_some_and(|s| s.text == "std"));
    if wants_std && !std_root.is_dir() {
        fe.sink.emit(
            siox::diag::Diagnostic::error(format!(
                "no standard library at `{}`",
                std_root.display()
            ))
            .with_code(siox::diag::codes::UNRESOLVED_IMPORT)
            .help("pass `--std <dir>` pointing at the `std/` directory (it holds `logic.siox`, `bits.siox`, ...)"),
        );
        return;
    }
    // The prelude is implicitly imported by every file (like VHDL's
    // std.standard): auto-load `std::prelude`, which transitively pulls the
    // core modules, so e.g. `signed` always compares signed. Skipped silently
    // when the std root has no prelude (bare-kernel test setups).
    if std_root.join("prelude.siox").exists() {
        let seg = |t: &str| siox::syntax::ast::Ident {
            text: t.to_string(),
            span: siox::diag::Span::new(siox::diag::FileId(0), 0..0),
        };
        queue.push(AstPath {
            segments: vec![seg("std"), seg("prelude")],
            span: siox::diag::Span::new(siox::diag::FileId(0), 0..0),
        });
    }
    while let Some(base) = queue.pop() {
        let Some(file) = std_file(std_root, &base) else {
            continue;
        };
        if !loaded.insert(file.clone()) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        if trace {
            eprintln!("== load {} ==", file.display());
        }
        let fid = fe.sources.add(file.display().to_string(), src.clone());
        let tokens = Lexer::new(fid, &src).tokenize(&mut fe.sink);
        let module = parser::Parser::new(&src, tokens, &mut fe.sink)
            .with_custom_operators(custom_operators)
            .parse_module();
        queue.extend(using_bases(&module));
        fe.modules.push(module);
    }
}

/// Pre-scan std declarations so custom operator precedence is available before
/// parsing the entry module. This pass is intentionally syntax-light and does
/// not report diagnostics; the full parse remains authoritative.
fn discover_std_operators(std_root: &Path) -> std::collections::HashMap<String, u8> {
    fn visit(dir: &Path, out: &mut std::collections::HashMap<String, u8>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "siox") {
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let mut sink = DiagnosticSink::new();
                let tokens = Lexer::new(siox::diag::FileId(0), &src).tokenize(&mut sink);
                out.extend(parser::discover_custom_operators(&src, &tokens));
            }
        }
    }
    let mut out = std::collections::HashMap::new();
    visit(std_root, &mut out);
    out
}

/// The `base` path of every `using base::{...}` import in a module.
fn using_bases(m: &Module) -> Vec<AstPath> {
    m.items
        .iter()
        .filter_map(|it| match it {
            Item::Using(u) => match &u.kind {
                UsingKind::Import { base, .. } => Some(base.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Map a `std::a::b` import path to `<std_root>/a/b.siox`. Non-`std` bases are
/// resolved within the already-loaded modules, so they map to no file.
fn std_file(std_root: &Path, base: &AstPath) -> Option<PathBuf> {
    let segs: Vec<&str> = base.segments.iter().map(|s| s.text.as_str()).collect();
    if segs.first() != Some(&"std") {
        return None;
    }
    let mut p = std_root.to_path_buf();
    for s in &segs[1..] {
        p.push(s);
    }
    p.set_extension("siox");
    Some(p)
}

/// Lex + parse, then render diagnostics and fail on parse errors. Used by the
/// commands whose later stages are still stubs.
fn run_frontend(path: &Path, std_root: &Path, trace: bool) -> Result<FrontendOut, ExitCode> {
    let fe = lex_parse(path, std_root, trace)?;
    render_diagnostics(&fe.sources, &fe.sink);
    if fe.sink.has_errors() {
        eprintln!("\nfrontend failed: {} error(s)", fe.sink.error_count());
        return Err(ExitCode::FAILURE);
    }
    if trace {
        eprintln!("\nfrontend ok: {} item(s) parsed", fe.entry().items.len());
    }
    Ok(fe)
}

/// The frontend plus the resolve/typecheck results, diagnostics not yet
/// rendered. Stage banners are narrated to stderr as it runs.
struct Semantic {
    fe: FrontendOut,
    typed: siox::types::Typed,
}

/// Run parse -> resolve -> typecheck, narrating each stage. Renders diagnostics
/// and returns `Err` only when parsing itself failed (later stages still run on
/// a parseable-but-flawed tree so all diagnostics surface at once).
fn run_semantic(path: &Path, std_root: &Path, trace: bool) -> Result<Semantic, ExitCode> {
    let mut fe = lex_parse(path, std_root, trace)?;

    if fe.sink.has_errors() {
        render_diagnostics(&fe.sources, &fe.sink);
        eprintln!(
            "\nparse failed: {} error(s); later stages skipped",
            fe.sink.error_count()
        );
        return Err(ExitCode::FAILURE);
    }
    eprintln!(
        "== stage 2: parse == {} item(s) in {} module(s)",
        fe.entry().items.len(),
        fe.modules.len()
    );

    let modules = fe.modules.as_slice();

    let before = fe.sink.error_count();
    let resolved = siox::resolve::resolve(modules, &mut fe.sink);
    eprintln!(
        "== stage 3: resolve == {} definitions, {} diagnostic(s)",
        resolved.defs().len(),
        fe.sink.error_count() - before
    );

    let before = fe.sink.error_count();
    let typed = siox::types::check(modules, &resolved, &mut fe.sink);
    eprintln!(
        "== stage 4: typecheck == {} diagnostic(s)",
        fe.sink.error_count() - before
    );

    Ok(Semantic { fe, typed })
}

/// `siox check`: parse -> resolve -> typecheck. `-v` adds the token/item dump.
fn cmd_check(path: &Path, std_root: &Path, verbose: bool) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, verbose) {
        Ok(s) => s,
        Err(code) => return code,
    };
    // Elaborate + lower so structural diagnostics (multiple drivers, possible
    // latch, unused signals) surface at check time, not only under test/sim.
    // Skip if earlier stages already failed — later stages assume a clean AST.
    if !sem.fe.sink.has_errors() {
        let modules = sem.fe.modules.as_slice();
        let hier = siox::elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
        let _ = siox::ir::lower_in(
            modules,
            &hier,
            &mut sem.fe.sink,
            path.parent().unwrap_or_else(|| Path::new("")),
        );
    }
    eprintln!();
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        eprintln!("\ncheck failed: {} error(s)", sem.fe.sink.error_count());
        ExitCode::FAILURE
    } else {
        eprintln!("check ok");
        ExitCode::SUCCESS
    }
}

/// `sioxc build`: compile one top-level design to a native object (the DUT,
/// `sx_*` ABI). The top is `--top <Entity>` or the single `#[top]` entity;
/// only that top and its instantiated children are built (no testbenches).
fn cmd_build(path: &Path, std_root: &Path, top: Option<&str>, out: Option<&Path>) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let modules = sem.fe.modules.as_slice();

    let top = match resolve_top(modules, top) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("siox build: {e}");
            return ExitCode::FAILURE;
        }
    };
    let hier = siox::elab::elaborate_top(modules, &sem.typed, &mut sem.fe.sink, &top);
    if hier.roots.is_empty() {
        eprintln!("siox build: no entity named `{top}`");
        return ExitCode::FAILURE;
    }
    let design = siox::ir::lower_in(
        modules,
        &hier,
        &mut sem.fe.sink,
        path.parent().unwrap_or_else(|| Path::new("")),
    );
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }
    let obj = out
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| path.with_extension("o"));
    if let Some(s) = design.signals.iter().find(|s| s.width == 0) {
        eprintln!(
            "siox build: `{}` has an unresolved width — `{top}` is parametric; \
             build a concrete top (or a wrapper that fixes its parameters)",
            s.path
        );
        return ExitCode::FAILURE;
    }
    match siox::llvm::emit_object(&design, &obj) {
        Ok(()) => {
            eprintln!(
                "compiled `{top}` -> {} ({} signals)",
                obj.display(),
                design.signals.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("siox build: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Pick the top entity to build: an explicit `--top`, else the single
/// `#[top]`-attributed entity. Ambiguity or absence is an error.
fn resolve_top(modules: &[Module], explicit: Option<&str>) -> Result<String, String> {
    if let Some(t) = explicit {
        return Ok(t.to_string());
    }
    let tops: Vec<&str> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|it| match it {
            Item::Entity(e)
                if e.attrs
                    .iter()
                    .any(|a| a.name.segments.last().map(|s| s.text.as_str()) == Some("top")) =>
            {
                Some(e.name.text.as_str())
            }
            _ => None,
        })
        .collect();
    match tops.as_slice() {
        [t] => Ok(t.to_string()),
        [] => Err("no #[top] entity; name one with --top <Entity>".into()),
        _ => Err(format!(
            "multiple #[top] entities ({}); pick one with --top",
            tops.join(", ")
        )),
    }
}

/// `sioxc --test`: compile `#[test]` stimulus into a standalone native test
/// executable. The compiler never runs the produced program.
fn cmd_build_test(path: &Path, std_root: &Path, out: Option<&Path>) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let modules = sem.fe.modules.as_slice();
    let hier = siox::elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    let design = siox::ir::lower_in(
        modules,
        &hier,
        &mut sem.fe.sink,
        path.parent().unwrap_or_else(|| Path::new("")),
    );
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }
    let bin = out
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| path.with_extension("test"));
    match build::build(modules, &hier, &design, &bin) {
        Ok(()) => {
            eprintln!(
                "built test binary {} (run it to execute the testbench)",
                bin.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("sioxc --test: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `siox emit-llvm`: run the pipeline through lowering and print the LLVM IR
/// the compiled backend emits. IR to stdout; stage trace/diagnostics to stderr.
fn cmd_emit_llvm(path: &Path, std_root: &Path) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let modules = sem.fe.modules.as_slice();
    let hier = siox::elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    let design = siox::ir::lower_in(
        modules,
        &hier,
        &mut sem.fe.sink,
        path.parent().unwrap_or_else(|| Path::new("")),
    );
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }
    // Report codegen-blocking IR (bad ids and Unknown values) cleanly
    // rather than letting the emitter panic.
    let issues = design.validate();
    if !issues.is_empty() {
        eprintln!("cannot emit LLVM:");
        for i in &issues {
            eprintln!("  - {i}");
        }
        return ExitCode::FAILURE;
    }
    print!("{}", siox::llvm::emit_module_ir(&design));
    ExitCode::SUCCESS
}

/// `siox tree`: run the semantic pipeline, elaborate the instance hierarchy, and
/// print it. The tree goes to stdout; the stage trace and diagnostics to stderr.
fn cmd_tree(path: &Path, std_root: &Path) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };

    let modules = sem.fe.modules.as_slice();
    let before = sem.fe.sink.error_count();
    let hier = siox::elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    eprintln!(
        "== stage 5: elaborate == {} instance(s), {} root(s), {} diagnostic(s)",
        hier.instances.len(),
        hier.roots.len(),
        sem.fe.sink.error_count() - before
    );

    eprintln!();
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    eprintln!();
    print!("{}", hier.to_tree_string());
    if sem.fe.sink.has_errors() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `siox ir`: run the pipeline through elaboration, lower to the digital IR, and
/// print it. The IR goes to stdout; the stage trace and diagnostics to stderr.
fn cmd_ir(path: &Path, std_root: &Path) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };

    let modules = sem.fe.modules.as_slice();
    let hier = siox::elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    eprintln!(
        "== stage 5: elaborate == {} instance(s)",
        hier.instances.len()
    );

    let before = sem.fe.sink.error_count();
    let design = siox::ir::lower_in(
        modules,
        &hier,
        &mut sem.fe.sink,
        path.parent().unwrap_or_else(|| Path::new("")),
    );
    eprintln!(
        "== stage 6: lower == {} signal(s), {} driver(s), {} event block(s), {} diagnostic(s)",
        design.signals.len(),
        design.drivers.len(),
        design.event_blocks.len(),
        sem.fe.sink.error_count() - before
    );

    eprintln!();
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    eprintln!();
    print!("{}", design.to_ir_string());
    if sem.fe.sink.has_errors() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_tokens(path: &Path) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let mut sources = SourceMap::new();
    let file = sources.add(path.display().to_string(), src.clone());
    let mut sink = DiagnosticSink::new();
    let tokens = Lexer::new(file, &src).tokenize(&mut sink);
    dump_tokens(&src, &tokens);
    render_diagnostics(&sources, &sink);
    if sink.has_errors() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Print `index  KIND  "source text"` for every token, with the location of
/// the first token on each source line.
fn dump_tokens(src: &str, tokens: &[Token]) {
    for (i, t) in tokens.iter().enumerate() {
        let slice = &src[t.span.start as usize..t.span.end as usize];
        let shown = match t.kind {
            TokenKind::Eof => "<eof>".to_string(),
            _ => format!("{slice:?}"),
        };
        let kind = format!("{:?}", t.kind);
        eprintln!("   {i:>4}  {kind:<13} {shown}");
    }
}

/// Print a one-line summary of each top-level item the parser produced.
fn dump_items(m: &Module) {
    let path = m
        .path
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("::");
    eprintln!("   module {path}");
    for item in &m.items {
        let (kind, name) = describe_item(item);
        eprintln!("     {kind:<7} {name}");
    }
}

fn describe_item(item: &Item) -> (&'static str, String) {
    match item {
        Item::Fn(f) => ("fn", f.name.text.clone()),
        Item::ExternBlock { abi, fns, .. } => ("extern", format!("\"{abi}\" ({} fns)", fns.len())),
        Item::Using(u) => {
            let name = match &u.kind {
                UsingKind::Alias { name, .. } => name.text.clone(),
                UsingKind::Import { base, names } => {
                    let base = base
                        .segments
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>();
                    let names = names
                        .iter()
                        .map(|n| n.text.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if base.is_empty() {
                        names
                    } else {
                        format!("{}::{{{names}}}", base.join("::"))
                    }
                }
            };
            ("using", name)
        }
        Item::Const(c) => ("const", c.name.text.clone()),
        Item::Struct(s) => ("struct", s.name.text.clone()),
        Item::View(v) => ("view", v.name.text.clone()),
        Item::Enum(e) => ("enum", e.name.text.clone()),
        Item::Entity(e) => {
            let tag = if e.is_extern { "extern " } else { "" };
            ("entity", format!("{tag}{}", e.name.text))
        }
        Item::Impl(i) => {
            let target = pretty::type_str(&i.target);
            let name = match &i.trait_ {
                Some(tr) => {
                    let tr = tr
                        .segments
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>();
                    format!("{} for {target}", tr.join("::"))
                }
                None => target,
            };
            ("impl", name)
        }
        Item::Trait(t) => ("trait", t.name.text.clone()),
        Item::AttrDecl(a) => ("attr", a.name.text.clone()),
    }
}

/// Minimal renderer: `severity[code]: message` plus a `--> file:line:col`
/// location and any related labels. The full Stage-10 format comes later.
fn render_diagnostics(sources: &SourceMap, sink: &DiagnosticSink) {
    for diag in sink.diagnostics() {
        let sev = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
            Severity::Help => "help",
        };
        match diag.code {
            Some(code) => eprintln!("{sev}[{code}]: {}", diag.message),
            None => eprintln!("{sev}: {}", diag.message),
        }
        if let Some(span) = diag.primary {
            let (line, col) = sources.line_col(span.file, span.start);
            let name = sources
                .get(span.file)
                .map(|f| f.name.as_str())
                .unwrap_or("<unknown>");
            eprintln!("  --> {name}:{line}:{col}");
        }
        for label in &diag.labels {
            let (line, col) = sources.line_col(label.span.file, label.span.start);
            eprintln!("   = {} (at {line}:{col})", label.message);
        }
        if let Some(help) = &diag.help {
            eprintln!("   = help: {help}");
        }
    }
}

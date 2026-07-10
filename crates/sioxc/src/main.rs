//! `sioxc` — the siox compiler driver (spec Stage 12).
//!
//! Pipeline wiring lives here: each subcommand runs the compiler up to the
//! stage it needs and prints the result. With `--verbose` (and always for the
//! later-stage commands) it narrates each pipeline step to stderr so you can
//! watch how the compiler turns source into data.
//!
//! Usage (rustc-shaped — a bare file compiles it):
//! ```text
//! sioxc <file>            # compile the #[top] design to a native object
//! sioxc check  <file>     # parse + resolve + typecheck, report success/errors
//! sioxc parse  <file>     # parse, print canonical source
//! sioxc sim    <file>     # elaborate + lower + simulate (--wave <out.vcd>)
//! sioxc test   <path>     # build and run #[test] entities (--no-run to just build)
//! sioxc ast    <file>     # debug: pretty-printed AST
//! sioxc ir     <file>     # debug: normalized digital IR
//! sioxc tree   <file>     # debug: elaborated instance hierarchy
//! sioxc tokens <file>     # debug: raw lexer token stream
//! ```
//! Exit code is nonzero on failed checks/tests.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[cfg(feature = "llvm")]
mod build;

use clap::{Parser, Subcommand};
use siox_diag::{DiagnosticSink, Severity, SourceMap};
use siox_syntax::ast::{Item, Module, Path as AstPath, UsingKind};
use siox_syntax::token::{Token, TokenKind};
use siox_syntax::{lexer::Lexer, parser, pretty};

#[derive(Parser)]
#[command(name = "sioxc", version, about = "The siox compiler (Phase 1)")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// The `.siox` file to compile (builds its `#[top]` design). Bare
    /// `sioxc foo.siox` compiles the file, like `rustc foo.rs`.
    #[cfg(feature = "llvm")]
    file: Option<PathBuf>,
    /// The top entity to build (default: the single `#[top]` entity).
    #[cfg(feature = "llvm")]
    #[arg(long)]
    top: Option<String>,
    /// Output object path for a bare build (default: `<file>.o`).
    #[cfg(feature = "llvm")]
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Directory holding the standard library (`std::logic` -> `<dir>/logic.siox`).
    #[arg(long, global = true, default_value = "std")]
    std: PathBuf,
    /// Backend slot width: `auto` uses 128-bit slots only when the design has
    /// signals wider than 64 bits (u128 is register-pair native on 64-bit
    /// CPUs); `64`/`128` force a width. Wider slots trade speed for range.
    #[arg(long, global = true, default_value = "auto")]
    slot: String,
    /// Execution engine for `siox test`: `llvm` (JIT-compiled, the default) or
    /// `interp` (the interpreter/reference oracle). `llvm` auto-falls back to
    /// the interpreter for designs it can't compile yet (e.g. >64-bit signals)
    /// or a build without the LLVM toolchain.
    #[arg(long, global = true, default_value = "llvm")]
    backend: String,
    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Parse, resolve and type-check a source file.
    Check {
        file: PathBuf,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Parse a source file and print canonical source.
    Parse {
        file: PathBuf,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Elaborate and simulate a design.
    Sim {
        file: PathBuf,
        /// Write a VCD waveform to this path.
        #[arg(long)]
        wave: Option<PathBuf>,
    },
    /// Build and run `#[test]` entities (optionally filtered by name).
    Test {
        path: PathBuf,
        /// Run only test entities whose name contains this string.
        filter: Option<String>,
        /// Compile the test into a native binary but do not run it.
        #[cfg(feature = "llvm")]
        #[arg(long)]
        no_run: bool,
        /// Output path for `--no-run` (default: `<file>.sim`).
        #[cfg(feature = "llvm")]
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Debug: print the pretty-printed AST.
    Ast {
        file: PathBuf,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Debug: print the normalized digital IR.
    Ir { file: PathBuf },
    /// Debug: print the LLVM IR emitted by the compiled backend.
    #[cfg(feature = "llvm")]
    EmitLlvm { file: PathBuf },
    /// Debug: print the elaborated instance hierarchy.
    Tree { file: PathBuf },
    /// Debug: print the raw lexer token stream.
    Tokens { file: PathBuf },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let std_root = cli.std;
    let slot = cli.slot;
    let backend = cli.backend;

    // Bare `sioxc foo.siox` compiles the file (like `rustc foo.rs`).
    #[cfg(feature = "llvm")]
    let cmd = match cli.cmd {
        Some(c) => c,
        None => {
            return match cli.file {
                Some(f) => cmd_build(&f, &std_root, cli.top.as_deref(), cli.out.as_deref()),
                None => {
                    use clap::CommandFactory;
                    Cli::command().print_help().ok();
                    ExitCode::FAILURE
                }
            };
        }
    };
    #[cfg(not(feature = "llvm"))]
    let cmd = match cli.cmd {
        Some(c) => c,
        None => {
            use clap::CommandFactory;
            Cli::command().print_help().ok();
            return ExitCode::FAILURE;
        }
    };

    match cmd {
        Command::Tokens { file } => cmd_tokens(&file),
        Command::Parse { file, verbose } => match run_frontend(&file, &std_root, verbose) {
            Ok(fe) => {
                print!("{}", pretty::print_module(fe.entry()));
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Command::Ast { file, verbose } => match run_frontend(&file, &std_root, verbose) {
            Ok(fe) => {
                println!("{:#?}", fe.entry());
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Command::Check { file, verbose } => cmd_check(&file, &std_root, verbose),
        Command::Sim { file, wave } => match wave {
            Some(out) => cmd_wave(&file, &std_root, &out),
            None => cmd_test(&file, &std_root, None, &slot, &backend),
        },
        #[cfg(feature = "llvm")]
        Command::Test { path, filter, no_run, out } => {
            if no_run {
                cmd_test_no_run(&path, &std_root, out.as_deref())
            } else {
                cmd_test(&path, &std_root, filter.as_deref(), &slot, &backend)
            }
        }
        #[cfg(not(feature = "llvm"))]
        Command::Test { path, filter } => {
            cmd_test(&path, &std_root, filter.as_deref(), &slot, &backend)
        }
        Command::Ir { file } => cmd_ir(&file, &std_root),
        #[cfg(feature = "llvm")]
        Command::EmitLlvm { file } => cmd_emit_llvm(&file, &std_root),
        Command::Tree { file } => cmd_tree(&file, &std_root),
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
    if trace {
        let trivia = tokens.iter().filter(|t| t.kind == TokenKind::Comment).count();
        eprintln!("   {} tokens ({} comment trivia)", tokens.len(), trivia);
        dump_tokens(&src, &tokens);
        eprintln!("\n== parse ==");
    }
    let module = parser::Parser::new(&src, tokens, &mut sink).parse_module();
    if trace {
        dump_items(&module);
    }

    let mut fe = FrontendOut { sources, modules: vec![module], sink };
    load_std_deps(&mut fe, std_root, trace);
    Ok(fe)
}

/// Transitively parse the `std::` modules imported by the already-loaded
/// modules, mapping `std::a::b` to `<std_root>/a/b.siox`. A missing file is left
/// unresolved so name resolution reports it against the `using`.
fn load_std_deps(fe: &mut FrontendOut, std_root: &Path, trace: bool) {
    let mut loaded: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut queue: Vec<AstPath> = using_bases(fe.entry());
    // The prelude is implicitly imported by every file (like VHDL's
    // std.standard): auto-load `std::prelude`, which transitively pulls the
    // core modules, so e.g. `int` always compares signed. Skipped silently
    // when the std root has no prelude (bare-kernel test setups).
    if std_root.join("prelude.siox").exists() {
        let seg = |t: &str| siox_syntax::ast::Ident {
            text: t.to_string(),
            span: siox_diag::Span::new(siox_diag::FileId(0), 0..0),
        };
        queue.push(AstPath {
            segments: vec![seg("std"), seg("prelude")],
            span: siox_diag::Span::new(siox_diag::FileId(0), 0..0),
        });
    }
    while let Some(base) = queue.pop() {
        let Some(file) = std_file(std_root, &base) else { continue };
        if !loaded.insert(file.clone()) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&file) else { continue };
        if trace {
            eprintln!("== load {} ==", file.display());
        }
        let fid = fe.sources.add(file.display().to_string(), src.clone());
        let tokens = Lexer::new(fid, &src).tokenize(&mut fe.sink);
        let module = parser::Parser::new(&src, tokens, &mut fe.sink).parse_module();
        queue.extend(using_bases(&module));
        fe.modules.push(module);
    }
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
    typed: siox_types::Typed,
}

/// Run parse -> resolve -> typecheck, narrating each stage. Renders diagnostics
/// and returns `Err` only when parsing itself failed (later stages still run on
/// a parseable-but-flawed tree so all diagnostics surface at once).
fn run_semantic(path: &Path, std_root: &Path, trace: bool) -> Result<Semantic, ExitCode> {
    let mut fe = lex_parse(path, std_root, trace)?;

    if fe.sink.has_errors() {
        render_diagnostics(&fe.sources, &fe.sink);
        eprintln!("\nparse failed: {} error(s); later stages skipped", fe.sink.error_count());
        return Err(ExitCode::FAILURE);
    }
    eprintln!(
        "== stage 2: parse == {} item(s) in {} module(s)",
        fe.entry().items.len(),
        fe.modules.len()
    );

    let modules = fe.modules.as_slice();

    let before = fe.sink.error_count();
    let resolved = siox_resolve::resolve(modules, &mut fe.sink);
    eprintln!(
        "== stage 3: resolve == {} definitions, {} diagnostic(s)",
        resolved.defs().len(),
        fe.sink.error_count() - before
    );

    let before = fe.sink.error_count();
    let typed = siox_types::check(modules, &resolved, &mut fe.sink);
    eprintln!("== stage 4: typecheck == {} diagnostic(s)", fe.sink.error_count() - before);

    Ok(Semantic { fe, typed })
}

/// `siox check`: parse -> resolve -> typecheck. `-v` adds the token/item dump.
fn cmd_check(path: &Path, std_root: &Path, verbose: bool) -> ExitCode {
    let sem = match run_semantic(path, std_root, verbose) {
        Ok(s) => s,
        Err(code) => return code,
    };
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
#[cfg(feature = "llvm")]
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
    let hier = siox_elab::elaborate_top(modules, &sem.typed, &mut sem.fe.sink, &top);
    if hier.roots.is_empty() {
        eprintln!("siox build: no entity named `{top}`");
        return ExitCode::FAILURE;
    }
    let design = siox_ir::lower(modules, &hier, &mut sem.fe.sink);
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }
    let obj = out.map(|p| p.to_path_buf()).unwrap_or_else(|| path.with_extension("o"));
    if let Some(s) = design.signals.iter().find(|s| s.width == 0) {
        eprintln!(
            "siox build: `{}` has an unresolved width — `{top}` is parametric; \
             build a concrete top (or a wrapper that fixes its parameters)",
            s.path
        );
        return ExitCode::FAILURE;
    }
    if let Some(s) = design.signals.iter().find(|s| s.width > 64) {
        eprintln!("siox build: signal `{}` is {} bits; the LLVM backend is 64-bit only", s.path, s.width);
        return ExitCode::FAILURE;
    }
    match siox_llvm::emit_object(&design, &obj) {
        Ok(()) => {
            eprintln!("compiled `{top}` -> {} ({} signals)", obj.display(), design.signals.len());
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
#[cfg(feature = "llvm")]
fn resolve_top(modules: &[Module], explicit: Option<&str>) -> Result<String, String> {
    if let Some(t) = explicit {
        return Ok(t.to_string());
    }
    let tops: Vec<&str> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|it| match it {
            Item::Entity(e)
                if e.attrs.iter().any(|a| {
                    a.name.segments.last().map(|s| s.text.as_str()) == Some("top")
                }) =>
            {
                Some(e.name.text.as_str())
            }
            _ => None,
        })
        .collect();
    match tops.as_slice() {
        [t] => Ok(t.to_string()),
        [] => Err("no #[top] entity; name one with --top <Entity>".into()),
        _ => Err(format!("multiple #[top] entities ({}); pick one with --top", tops.join(", "))),
    }
}

/// `siox test --no-run`: compile the `#[test]` stimulus into a standalone
/// native simulator binary, but do not run it. Like `cargo test --no-run`.
#[cfg(feature = "llvm")]
fn cmd_test_no_run(path: &Path, std_root: &Path, out: Option<&Path>) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let modules = sem.fe.modules.as_slice();
    let hier = siox_elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    let design = siox_ir::lower(modules, &hier, &mut sem.fe.sink);
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }
    let bin = out.map(|p| p.to_path_buf()).unwrap_or_else(|| path.with_extension("sim"));
    match build::build(modules, &hier, &design, &bin) {
        Ok(()) => {
            eprintln!("built test binary {} (run it to execute the testbench)", bin.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("siox test --no-run: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `siox emit-llvm`: run the pipeline through lowering and print the LLVM IR
/// the compiled backend emits. IR to stdout; stage trace/diagnostics to stderr.
#[cfg(feature = "llvm")]
fn cmd_emit_llvm(path: &Path, std_root: &Path) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let modules = sem.fe.modules.as_slice();
    let hier = siox_elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    let design = siox_ir::lower(modules, &hier, &mut sem.fe.sink);
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }
    // Report codegen-blocking IR (bad ids, Unknown, wide signals) cleanly
    // rather than letting the emitter panic.
    let mut issues = design.validate();
    if let Some(s) = design.signals.iter().find(|s| s.width > 64) {
        issues.push(format!(
            "signal `{}` is {} bits; the LLVM backend is 64-bit-word only",
            s.path, s.width
        ));
    }
    if !issues.is_empty() {
        eprintln!("cannot emit LLVM:");
        for i in &issues {
            eprintln!("  - {i}");
        }
        return ExitCode::FAILURE;
    }
    print!("{}", siox_llvm::emit_module_ir(&design));
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
    let hier = siox_elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
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
    let hier = siox_elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    eprintln!("== stage 5: elaborate == {} instance(s)", hier.instances.len());

    let before = sem.fe.sink.error_count();
    let design = siox_ir::lower(modules, &hier, &mut sem.fe.sink);
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

/// `siox test`: run the `#[test]` entities (optionally filtered by name)
/// through the simulator and report pass/fail. Exits nonzero if any test fails
/// (or the pipeline errored).
/// Run the `#[test]` entities through the JIT-compiled backend, driving the
/// same test runner with a JIT engine instead of the interpreter.
#[cfg(feature = "llvm")]
fn run_tests_llvm(
    modules: &[Module],
    hier: &siox_elab::Hierarchy,
    design: &siox_ir::Design,
    filter: Option<&str>,
) -> Result<Vec<siox_run::TestResult>, String> {
    // The JIT is 64-bit-word only; reject wide designs (the interpreter
    // handles them) and any IR the backend can't compile.
    if let Some(s) = design.signals.iter().find(|s| s.width > 64) {
        return Err(format!("signal `{}` is {} bits; the LLVM backend is 64-bit only", s.path, s.width));
    }
    let issues = design.validate();
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }
    eprintln!("backend: llvm (JIT)");
    Ok(siox_llvm::with_jit(design, |jit| {
        siox_run::run_tests_with_engine(modules, hier, design, filter, || {
            jit.reset();
            Box::new(JitEngine { jit, design }) as Box<dyn siox_run::Engine>
        })
    }))
}

/// Adapts a JIT-compiled design to the test runner's [`siox_run::Engine`].
#[cfg(feature = "llvm")]
struct JitEngine<'a, 'ctx> {
    jit: &'a siox_llvm::Jit<'ctx>,
    design: &'a siox_ir::Design,
}

#[cfg(feature = "llvm")]
impl siox_run::Engine for JitEngine<'_, '_> {
    fn set(&mut self, sig: siox_ir::SignalId, value: u128) {
        self.jit.set(sig.0, value as u64);
    }
    fn read(&self, sig: siox_ir::SignalId) -> u128 {
        self.jit.read(sig.0) as u128
    }
    fn settle(&mut self) {
        self.jit.settle();
    }
    fn design(&self) -> &siox_ir::Design {
        self.design
    }
}

/// Without the `llvm` feature, `--backend=llvm` is unavailable.
#[cfg(not(feature = "llvm"))]
fn run_tests_llvm(
    _modules: &[Module],
    _hier: &siox_elab::Hierarchy,
    _design: &siox_ir::Design,
    _filter: Option<&str>,
) -> Result<Vec<siox_run::TestResult>, String> {
    Err("this build has no llvm backend (rebuild with `--features llvm`)".to_string())
}

/// Run the `#[test]` entities on the interpreter (the reference oracle).
#[cfg(feature = "interp")]
fn run_interp(
    modules: &[Module],
    hier: &siox_elab::Hierarchy,
    design: &siox_ir::Design,
    filter: Option<&str>,
    slot: &str,
) -> Vec<siox_run::TestResult> {
    let width = slot_width(slot);
    if width == siox_sim::SlotWidth::W128
        || (width == siox_sim::SlotWidth::Auto && siox_sim::needs_wide(design))
    {
        eprintln!("slot: 128-bit (native u128 on this target)");
    }
    siox_sim::run_tests_with(modules, hier, design, filter, width)
}

/// Parse the `--slot` flag into a sim slot width.
#[cfg(feature = "interp")]
fn slot_width(s: &str) -> siox_sim::SlotWidth {
    match s {
        "64" => siox_sim::SlotWidth::W64,
        "128" => siox_sim::SlotWidth::W128,
        _ => siox_sim::SlotWidth::Auto,
    }
}

fn cmd_test(
    path: &Path,
    std_root: &Path,
    filter: Option<&str>,
    slot: &str,
    backend: &str,
) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };
    if sem.fe.sink.has_errors() {
        render_diagnostics(&sem.fe.sources, &sem.fe.sink);
        return ExitCode::FAILURE;
    }

    let modules = sem.fe.modules.as_slice();
    let hier = siox_elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    let design = siox_ir::lower(modules, &hier, &mut sem.fe.sink);
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }

    // `--slot` only matters to the interpreter.
    #[cfg(not(feature = "interp"))]
    let _ = slot;

    // LLVM is the default engine. With the `interp` feature it falls back to the
    // interpreter (oracle / >64-bit); without it, an un-JIT-able design errors.
    let results = if backend == "interp" {
        #[cfg(feature = "interp")]
        {
            run_interp(modules, &hier, &design, filter, slot)
        }
        #[cfg(not(feature = "interp"))]
        {
            eprintln!("backend `interp` is not in this build (rebuild with `--features interp`)");
            return ExitCode::FAILURE;
        }
    } else {
        match run_tests_llvm(modules, &hier, &design, filter) {
            Ok(r) => r,
            Err(e) => {
                #[cfg(feature = "interp")]
                {
                    eprintln!("backend: interp (llvm unavailable: {e})");
                    run_interp(modules, &hier, &design, filter, slot)
                }
                #[cfg(not(feature = "interp"))]
                {
                    eprintln!("backend: llvm unavailable: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };
    // libtest-style report (the rustc parallel).
    println!("\nrunning {} test{}", results.len(), if results.len() == 1 { "" } else { "s" });
    let mut failures: Vec<(&str, String)> = Vec::new();
    for r in &results {
        if r.passed {
            println!("test {} ... ok", r.name);
        } else {
            println!("test {} ... FAILED", r.name);
            let loc = r
                .span
                .map(|s| {
                    let (line, col) = sem.fe.sources.line_col(s.file, s.start);
                    let name = sem.fe.sources.get(s.file).map(|f| f.name.as_str()).unwrap_or("?");
                    format!(" ({name}:{line}:{col})")
                })
                .unwrap_or_default();
            let msg = r.failure.as_deref().unwrap_or("assertion failed");
            failures.push((&r.name, format!("{msg}{loc}")));
        }
    }
    if !failures.is_empty() {
        println!("\nfailures:");
        for (name, why) in &failures {
            println!("    {name}: {why}");
        }
    }
    let failed = failures.len();
    let passed = results.len() - failed;
    let verdict = if failed == 0 { "ok" } else { "FAILED" };
    println!("\ntest result: {verdict}. {passed} passed; {failed} failed");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Trace the first `#[test]` for waveform export — via the JIT when available,
/// else the interpreter.
fn trace_first_test(
    modules: &[Module],
    hier: &siox_elab::Hierarchy,
    design: &siox_ir::Design,
) -> Option<(siox_run::TestResult, Vec<siox_run::Sample>)> {
    #[cfg(feature = "llvm")]
    {
        let jittable = design.signals.iter().all(|s| s.width <= 64) && design.validate().is_empty();
        if jittable {
            return siox_llvm::with_jit(design, |jit| {
                siox_run::run_test_traced_with_engine(modules, hier, design, None, || {
                    jit.reset();
                    Box::new(JitEngine { jit, design }) as Box<dyn siox_run::Engine>
                })
            });
        }
    }
    #[cfg(feature = "interp")]
    {
        return siox_sim::run_test_traced(modules, hier, design, None);
    }
    #[allow(unreachable_code)]
    None
}

/// `siox sim --wave <out.vcd>`: run the first test entity with tracing and write
/// its waveform as VCD.
fn cmd_wave(path: &Path, std_root: &Path, out: &Path) -> ExitCode {
    let mut sem = match run_semantic(path, std_root, false) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let modules = sem.fe.modules.as_slice();
    let hier = siox_elab::elaborate(modules, &sem.typed, &mut sem.fe.sink);
    let design = siox_ir::lower(modules, &hier, &mut sem.fe.sink);
    render_diagnostics(&sem.fe.sources, &sem.fe.sink);
    if sem.fe.sink.has_errors() {
        return ExitCode::FAILURE;
    }

    let Some((result, samples)) = trace_first_test(modules, &hier, &design) else {
        eprintln!("no #[test] entity found to trace (or no backend can run it)");
        return ExitCode::FAILURE;
    };

    let mut file = match std::fs::File::create(out) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: cannot write {}: {e}", out.display());
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = siox_wave::write_vcd(&mut file, &design, &samples) {
        eprintln!("error: writing VCD: {e}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "wrote {} ({} samples) for `{}` [{}]",
        out.display(),
        samples.len(),
        result.name,
        if result.passed { "pass" } else { "fail" }
    );
    ExitCode::SUCCESS
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
    let path = m.path.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("::");
    eprintln!("   module {path}");
    for item in &m.items {
        let (kind, name) = describe_item(item);
        eprintln!("     {kind:<7} {name}");
    }
}

fn describe_item(item: &Item) -> (&'static str, String) {
    match item {
        Item::Fn(f) => ("fn", f.name.text.clone()),
        Item::Using(u) => {
            let name = match &u.kind {
                UsingKind::Alias { name, .. } => name.text.clone(),
                UsingKind::Import { base, names } => {
                    let base = base.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>();
                    let names =
                        names.iter().map(|n| n.text.as_str()).collect::<Vec<_>>().join(", ");
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
        Item::Enum(e) => ("enum", e.name.text.clone()),
        Item::Entity(e) => {
            let tag = if e.is_extern { "extern " } else { "" };
            ("entity", format!("{tag}{}", e.name.text))
        }
        Item::Impl(i) => {
            let target = pretty::type_str(&i.target);
            let name = match &i.trait_ {
                Some(tr) => {
                    let tr = tr.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>();
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
            let name = sources.get(span.file).map(|f| f.name.as_str()).unwrap_or("<unknown>");
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

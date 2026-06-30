//! `siox` command-line driver (spec Stage 12).
//!
//! Pipeline wiring lives here: each subcommand runs the compiler up to the
//! stage it needs and prints the result. With `--verbose` (and always for the
//! later-stage commands) it narrates each pipeline step to stderr so you can
//! watch how the compiler turns source into data.
//!
//! Commands (spec Stage 12):
//! ```text
//! siox check  <file>     # parse + resolve + typecheck, report success/errors
//! siox parse  <file>     # parse, print canonical source
//! siox sim    <file>     # elaborate + lower + simulate (--wave <out.vcd>)
//! siox test   <path>     # discover and run #[test] entities
//! siox ast    <file>     # debug: pretty-printed AST
//! siox ir     <file>     # debug: normalized digital IR
//! siox tree   <file>     # debug: elaborated instance hierarchy
//! siox tokens <file>     # debug: raw lexer token stream
//! ```
//! Exit code is nonzero on failed checks/tests.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use siox_diag::{DiagnosticSink, Severity, SourceMap};
use siox_syntax::ast::{Item, Module, UsingKind};
use siox_syntax::token::{Token, TokenKind};
use siox_syntax::{lexer::Lexer, parser, pretty};

#[derive(Parser)]
#[command(name = "siox", version, about = "The siox digital HDL toolchain (Phase 1)")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
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
    /// Discover and run `#[test]` entities.
    Test { path: PathBuf },
    /// Debug: print the pretty-printed AST.
    Ast {
        file: PathBuf,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Debug: print the normalized digital IR.
    Ir { file: PathBuf },
    /// Debug: print the elaborated instance hierarchy.
    Tree { file: PathBuf },
    /// Debug: print the raw lexer token stream.
    Tokens { file: PathBuf },
}

/// A pipeline stage that is wired into the CLI but not yet implemented. Used to
/// tell the user exactly where the compiler currently stops.
struct Pending {
    stage: u8,
    name: &'static str,
    krate: &'static str,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Tokens { file } => cmd_tokens(&file),
        Command::Parse { file, verbose } => match run_frontend(&file, verbose) {
            Ok(fe) => {
                print!("{}", pretty::print_module(&fe.module));
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Command::Ast { file, verbose } => match run_frontend(&file, verbose) {
            Ok(fe) => {
                println!("{:#?}", fe.module);
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Command::Check { file, verbose } => cmd_check(&file, verbose),
        Command::Sim { file, wave } => {
            let mut stages = vec![
                Pending { stage: 5, name: "elaborate", krate: "siox-elab" },
                Pending { stage: 6, name: "lower (IR)", krate: "siox-ir" },
                Pending { stage: 7, name: "simulate", krate: "siox-sim" },
            ];
            if wave.is_some() {
                stages.push(Pending { stage: 9, name: "waveform", krate: "siox-wave" });
            }
            run_then_report(&file, &stages)
        }
        Command::Test { path } => run_then_report(
            &path,
            &[
                Pending { stage: 5, name: "elaborate", krate: "siox-elab" },
                Pending { stage: 6, name: "lower (IR)", krate: "siox-ir" },
                Pending { stage: 8, name: "run tests", krate: "siox-sim" },
            ],
        ),
        Command::Ir { file } => run_then_report(
            &file,
            &[
                Pending { stage: 5, name: "elaborate", krate: "siox-elab" },
                Pending { stage: 6, name: "lower (IR)", krate: "siox-ir" },
            ],
        ),
        Command::Tree { file } => run_then_report(
            &file,
            &[Pending { stage: 5, name: "elaborate", krate: "siox-elab" }],
        ),
    }
}

/// Everything the frontend produces, with diagnostics not yet rendered so a
/// caller can keep running later stages on the same sink.
struct FrontendOut {
    sources: SourceMap,
    module: Module,
    sink: DiagnosticSink,
}

/// Read, lex and parse a file. With `trace`, narrates the lex/parse steps to
/// stderr. Does not render diagnostics — the caller decides when. `Err` only on
/// a read failure.
fn lex_parse(path: &Path, trace: bool) -> Result<FrontendOut, ExitCode> {
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

    Ok(FrontendOut { sources, module, sink })
}

/// Lex + parse, then render diagnostics and fail on parse errors. Used by the
/// commands whose later stages are still stubs.
fn run_frontend(path: &Path, trace: bool) -> Result<FrontendOut, ExitCode> {
    let fe = lex_parse(path, trace)?;
    render_diagnostics(&fe.sources, &fe.sink);
    if fe.sink.has_errors() {
        eprintln!("\nfrontend failed: {} error(s)", fe.sink.error_count());
        return Err(ExitCode::FAILURE);
    }
    if trace {
        eprintln!("\nfrontend ok: {} item(s) parsed", fe.module.items.len());
    }
    Ok(fe)
}

/// `siox check`: parse -> resolve -> typecheck, narrating each stage. `-v` adds
/// the token/item dump from the frontend.
fn cmd_check(path: &Path, verbose: bool) -> ExitCode {
    let mut fe = match lex_parse(path, verbose) {
        Ok(f) => f,
        Err(code) => return code,
    };

    if fe.sink.has_errors() {
        render_diagnostics(&fe.sources, &fe.sink);
        eprintln!("\nparse failed: {} error(s); resolve/typecheck skipped", fe.sink.error_count());
        return ExitCode::FAILURE;
    }
    eprintln!("== stage 2: parse == {} item(s)", fe.module.items.len());

    let modules = std::slice::from_ref(&fe.module);

    let before = fe.sink.error_count();
    let resolved = siox_resolve::resolve(modules, &mut fe.sink);
    eprintln!(
        "== stage 3: resolve == {} definitions, {} diagnostic(s)",
        resolved.defs().len(),
        fe.sink.error_count() - before
    );

    let before = fe.sink.error_count();
    let _typed = siox_types::check(modules, &resolved, &mut fe.sink);
    eprintln!("== stage 4: typecheck == {} diagnostic(s)", fe.sink.error_count() - before);

    eprintln!();
    render_diagnostics(&fe.sources, &fe.sink);
    if fe.sink.has_errors() {
        eprintln!("\ncheck failed: {} error(s)", fe.sink.error_count());
        ExitCode::FAILURE
    } else {
        eprintln!("check ok");
        ExitCode::SUCCESS
    }
}

/// Run the frontend (always traced), then report the still-unimplemented
/// pipeline stages this command would need.
fn run_then_report(path: &Path, pending: &[Pending]) -> ExitCode {
    match run_frontend(path, true) {
        Ok(_) => {
            eprintln!();
            for p in pending {
                eprintln!(
                    "== stage {}: {} == not yet implemented (crate `{}`)",
                    p.stage, p.name, p.krate
                );
            }
            eprintln!("\npipeline stops after parse; later stages are still stubs.");
            ExitCode::SUCCESS
        }
        Err(code) => code,
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
    let path = m.path.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("::");
    eprintln!("   module {path}");
    for item in &m.items {
        let (kind, name) = describe_item(item);
        eprintln!("     {kind:<7} {name}");
    }
}

fn describe_item(item: &Item) -> (&'static str, String) {
    match item {
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

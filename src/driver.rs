//! `sioxc` command-line adapter.
//!
//! Pipeline orchestration belongs to [`siox::compiler`]. This module owns only
//! argument parsing and terminal presentation; it never provides a second
//! compiler path for tools to accidentally diverge from.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use siox::compiler::{Artifact, CompileRequest, Compiler, Emit, FileArtifact, SourceInput};
use siox::syntax::ast::{Item, UsingKind};
use siox::syntax::pretty;

#[derive(Parser)]
#[command(name = "sioxc", version, about = "The siox compiler (Phase 1)")]
struct Cli {
    /// The `.siox` file to compile (builds its `#[top]` design). Bare
    /// `sioxc foo.siox` compiles the file, like `rustc foo.rs`.
    file: PathBuf,
    /// The top entity to build (default: the single `#[top]` entity).
    #[arg(long)]
    top: Option<String>,
    /// Output path for a native object or test executable.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Compile `#[test]` entities into a native test executable.
    #[arg(long)]
    test: bool,
    /// Compiler artifact to emit.
    #[arg(long, value_enum, default_value_t = CliEmit::Object)]
    emit: CliEmit,
    /// Include frontend token/item tracing in textual frontend output.
    #[arg(short, long)]
    verbose: bool,
    /// Directory holding the standard library (`std::logic` -> `<dir>/logic.siox`).
    #[arg(long, global = true, default_value = "std")]
    std: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum CliEmit {
    Object,
    Metadata,
    Source,
    Tokens,
    Ast,
    Tree,
    Ir,
    LlvmIr,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    if cli.test && cli.emit != CliEmit::Object {
        eprintln!("error: --test currently requires --emit object");
        return ExitCode::FAILURE;
    }

    let emit = if cli.test {
        Emit::TestExecutable
    } else {
        match cli.emit {
            CliEmit::Object => Emit::Object { top: cli.top },
            CliEmit::Metadata => Emit::Metadata,
            CliEmit::Source => Emit::Source,
            CliEmit::Tokens => Emit::Tokens,
            CliEmit::Ast => Emit::Ast,
            CliEmit::Tree => Emit::Tree,
            CliEmit::Ir => Emit::Ir,
            CliEmit::LlvmIr => Emit::LlvmIr,
        }
    };
    let mut request = CompileRequest::new(SourceInput::path(&cli.file), emit.clone());
    if let Some(output) = cli.out {
        request = request.with_output(output);
    }

    let compilation = Compiler::new(cli.std).compile(request);
    report_stages(&compilation, &emit);
    if cli.verbose {
        report_frontend(&compilation);
    }

    let rendered = compilation.render_diagnostics();
    if !rendered.is_empty() {
        eprint!("{rendered}");
    }
    if compilation.diagnostics.has_errors() {
        if compilation.resolved.is_none() && !compilation.modules.is_empty() {
            eprintln!(
                "parse failed: {} error(s); later stages skipped",
                compilation.diagnostics.error_count()
            );
        } else if compilation.typed.is_some()
            && compilation.hierarchy.is_none()
            && matches!(
                emit,
                Emit::Tree | Emit::Ir | Emit::LlvmIr | Emit::Object { .. } | Emit::TestExecutable
            )
        {
            eprintln!(
                "semantic analysis failed: {} error(s); later stages skipped",
                compilation.diagnostics.error_count()
            );
        }
    }
    if let Some(failure) = &compilation.failure {
        eprintln!("sioxc: {failure}");
    }

    if let Some(artifact) = &compilation.artifact {
        match artifact {
            Artifact::Text(text) => match emit {
                // Preserve the historical token trace on stderr; other
                // textual compiler artifacts are ordinary stdout.
                Emit::Tokens => eprint!("{text}"),
                _ => print!("{text}"),
            },
            Artifact::File { kind, path } => match kind {
                FileArtifact::Object => eprintln!(
                    "compiled -> {} ({} signals)",
                    path.display(),
                    compilation.stats.signals.unwrap_or(0)
                ),
                FileArtifact::TestExecutable => eprintln!(
                    "built test binary {} (run it to execute the testbench)",
                    path.display()
                ),
            },
        }
    }

    if compilation.succeeded() {
        if emit == Emit::Metadata {
            eprintln!("check ok");
        }
        ExitCode::SUCCESS
    } else {
        if emit == Emit::Metadata && compilation.diagnostics.has_errors() {
            eprintln!(
                "check failed: {} error(s)",
                compilation.diagnostics.error_count()
            );
        }
        ExitCode::FAILURE
    }
}

fn report_stages(compilation: &siox::compiler::Compilation, emit: &Emit) {
    if compilation.resolved.is_some() {
        eprintln!(
            "== stage 2: parse == {} item(s) in {} module(s)",
            compilation.stats.entry_items, compilation.stats.modules
        );
        eprintln!(
            "== stage 3: resolve == {} definitions",
            compilation.stats.definitions.unwrap_or(0)
        );
    }
    if compilation.typed.is_some() {
        eprintln!("== stage 4: typecheck == complete");
    }
    if matches!(emit, Emit::Tree | Emit::Ir) && compilation.hierarchy.is_some() {
        eprintln!(
            "== stage 5: elaborate == {} instance(s), {} root(s)",
            compilation.stats.instances.unwrap_or(0),
            compilation.stats.roots.unwrap_or(0)
        );
    }
    if *emit == Emit::Ir && compilation.design.is_some() {
        eprintln!(
            "== stage 6: lower == {} signal(s), {} driver(s), {} event block(s)",
            compilation.stats.signals.unwrap_or(0),
            compilation.stats.drivers.unwrap_or(0),
            compilation.stats.event_blocks.unwrap_or(0)
        );
    }
}

fn report_frontend(compilation: &siox::compiler::Compilation) {
    if let (Some(file), Some(source)) = (
        compilation.entry_file,
        compilation
            .entry_file
            .and_then(|file| compilation.sources.get(file)),
    ) {
        eprintln!("== lex ({}) ==", source.name);
        let comments = compilation
            .entry_tokens
            .iter()
            .filter(|token| token.kind == siox::syntax::token::TokenKind::Comment)
            .count();
        eprintln!(
            "   {} tokens ({} comment trivia, file {})",
            compilation.entry_tokens.len(),
            comments,
            file.0
        );
    }
    if let Some(module) = compilation.entry() {
        let path = module
            .path
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("::");
        eprintln!("== parse ==\n   module {path}");
        for item in &module.items {
            let (kind, name) = describe_item(item);
            eprintln!("     {kind:<7} {name}");
        }
    }
}

fn describe_item(item: &Item) -> (&'static str, String) {
    match item {
        Item::Fn(function) => ("fn", function.name.text.clone()),
        Item::ExternBlock { abi, fns, .. } => ("extern", format!("\"{abi}\" ({} fns)", fns.len())),
        Item::Using(using) => {
            let name = match &using.kind {
                UsingKind::Alias { name, .. } => name.text.clone(),
                UsingKind::Import { base, names } => {
                    let base = base
                        .segments
                        .iter()
                        .map(|segment| segment.text.as_str())
                        .collect::<Vec<_>>()
                        .join("::");
                    let names = names
                        .iter()
                        .map(|name| name.text.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if base.is_empty() {
                        names
                    } else {
                        format!("{base}::{{{names}}}")
                    }
                }
            };
            ("using", name)
        }
        Item::Const(constant) => ("const", constant.name.text.clone()),
        Item::Struct(structure) => ("struct", structure.name.text.clone()),
        Item::View(view) => ("view", view.name.text.clone()),
        Item::Enum(enumeration) => ("enum", enumeration.name.text.clone()),
        Item::Entity(entity) => {
            let prefix = if entity.is_extern { "extern " } else { "" };
            ("entity", format!("{prefix}{}", entity.name.text))
        }
        Item::Impl(implementation) => {
            let target = pretty::type_str(&implementation.target);
            let name = implementation
                .trait_
                .as_ref()
                .map_or(target.clone(), |trait_| {
                    let trait_ = trait_
                        .segments
                        .iter()
                        .map(|segment| segment.text.as_str())
                        .collect::<Vec<_>>()
                        .join("::");
                    format!("{trait_} for {target}")
                });
            ("impl", name)
        }
        Item::Trait(trait_) => ("trait", trait_.name.text.clone()),
        Item::AttrDecl(attribute) => ("attr", attribute.name.text.clone()),
    }
}

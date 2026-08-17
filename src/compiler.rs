//! Stable embedding boundary for the siox compiler.
//!
//! [`Compiler`] owns configuration shared by repeated compilations (currently
//! the standard-library root). A [`CompileRequest`] names one source input and
//! one artifact. [`Compilation`] retains diagnostics and every successfully
//! completed phase product, allowing editors to inspect a failed compilation
//! without recreating the pipeline or scraping command-line output.
//!
//! The embedding API never prints and never executes generated artifacts.
//! `sioxc` is a thin command-line adapter over this module.

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::diag::{Diagnostic, DiagnosticSink, FileId, Severity, SourceMap};
use crate::elab::Hierarchy;
use crate::ir::Design;
use crate::resolve::Resolved;
use crate::syntax::ast::{Item, Module, Path as AstPath, UsingKind};
use crate::syntax::token::{Token, TokenKind};
use crate::syntax::{lexer::Lexer, parser, pretty};
use crate::types::Typed;

#[cfg(feature = "llvm")]
#[path = "driver/build.rs"]
mod build;

/// Source presented to the compiler.
///
/// An in-memory input still has a path. Editors should use the document's real
/// path so relative compile-time fixture reads and default artifact names have
/// the same meaning as a disk compilation.
#[derive(Clone, Debug)]
pub enum SourceInput {
    Path(PathBuf),
    Memory { path: PathBuf, text: String },
}

impl SourceInput {
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self::Path(path.into())
    }

    pub fn memory(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        Self::Memory {
            path: path.into(),
            text: text.into(),
        }
    }

    pub fn name(&self) -> &Path {
        match self {
            Self::Path(path) | Self::Memory { path, .. } => path,
        }
    }

    fn read(&self) -> Result<String, CompileFailure> {
        match self {
            Self::Path(path) => {
                if path.is_dir() {
                    return Err(CompileFailure::new(
                        FailureKind::Input,
                        format!(
                            "{} is a directory; expected one .siox source file",
                            path.display()
                        ),
                    ));
                }
                std::fs::read_to_string(path).map_err(|error| {
                    CompileFailure::new(
                        FailureKind::Input,
                        format!("cannot read {}: {error}", path.display()),
                    )
                })
            }
            Self::Memory { text, .. } => Ok(text.clone()),
        }
    }
}

/// The product requested from one compiler invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Emit {
    /// Parse, resolve, type-check, elaborate all analyzable entities, and run
    /// structural IR diagnostics without retaining a textual artifact.
    Metadata,
    /// Canonical source reconstructed from the entry AST.
    Source,
    /// Entry-file lexer tokens.
    Tokens,
    /// Debug representation of the entry AST.
    Ast,
    /// Elaborated `#[top]`/`#[test]` instance tree.
    Tree,
    /// Normalized digital IR.
    Ir,
    /// LLVM textual IR.
    LlvmIr,
    /// Native object exposing the `sx_*` design ABI.
    Object { top: Option<String> },
    /// Standalone native executable containing all `#[test]` entities.
    TestExecutable,
}

impl Emit {
    pub fn object() -> Self {
        Self::Object { top: None }
    }
}

/// One complete compiler request.
#[derive(Clone, Debug)]
pub struct CompileRequest {
    pub input: SourceInput,
    pub emit: Emit,
    /// Required destination override for file artifacts. Textual artifacts are
    /// returned in memory and ignore this field.
    pub output: Option<PathBuf>,
}

impl CompileRequest {
    pub fn new(input: SourceInput, emit: Emit) -> Self {
        Self {
            input,
            emit,
            output: None,
        }
    }

    pub fn with_output(mut self, path: impl Into<PathBuf>) -> Self {
        self.output = Some(path.into());
        self
    }
}

/// A successfully materialized artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Artifact {
    Text(String),
    File { kind: FileArtifact, path: PathBuf },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileArtifact {
    Object,
    TestExecutable,
}

/// Non-language failure category. Source-language failures are ordinary
/// structured diagnostics in [`Compilation::diagnostics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Input,
    Configuration,
    Selection,
    Validation,
    Backend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileFailure {
    pub kind: FailureKind,
    pub message: String,
}

impl CompileFailure {
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for CompileFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompileFailure {}

/// Counts from completed phases. These are presentation-neutral and let a CLI
/// or build tool report progress without parsing compiler text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompilationStats {
    pub entry_items: usize,
    pub modules: usize,
    pub definitions: Option<usize>,
    pub instances: Option<usize>,
    pub roots: Option<usize>,
    pub signals: Option<usize>,
    pub drivers: Option<usize>,
    pub event_blocks: Option<usize>,
}

/// Result of one request, including useful partial products on failure.
pub struct Compilation {
    pub sources: SourceMap,
    pub entry_file: Option<FileId>,
    pub entry_tokens: Vec<Token>,
    pub modules: Vec<Module>,
    pub resolved: Option<Resolved>,
    pub typed: Option<Typed>,
    pub hierarchy: Option<Hierarchy>,
    pub design: Option<Design>,
    pub diagnostics: DiagnosticSink,
    pub artifact: Option<Artifact>,
    pub failure: Option<CompileFailure>,
    pub stats: CompilationStats,
}

impl Compilation {
    fn empty() -> Self {
        Self {
            sources: SourceMap::new(),
            entry_file: None,
            entry_tokens: Vec::new(),
            modules: Vec::new(),
            resolved: None,
            typed: None,
            hierarchy: None,
            design: None,
            diagnostics: DiagnosticSink::new(),
            artifact: None,
            failure: None,
            stats: CompilationStats::default(),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.failure.is_none() && !self.diagnostics.has_errors()
    }

    pub fn entry(&self) -> Option<&Module> {
        self.modules.first()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.diagnostics.diagnostics()
    }

    /// Render diagnostics in the same compact form used by `sioxc`.
    /// Structured consumers should read [`Self::diagnostics`] instead.
    pub fn render_diagnostics(&self) -> String {
        let mut out = String::new();
        for diagnostic in self.diagnostics() {
            let severity = match diagnostic.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Note => "note",
                Severity::Help => "help",
            };
            match diagnostic.code {
                Some(code) => {
                    let _ = writeln!(out, "{severity}[{code}]: {}", diagnostic.message);
                }
                None => {
                    let _ = writeln!(out, "{severity}: {}", diagnostic.message);
                }
            }
            if let Some(span) = diagnostic.primary {
                let (line, column) = self.sources.line_col(span.file, span.start);
                let name = self
                    .sources
                    .get(span.file)
                    .map(|source| source.name.as_str())
                    .unwrap_or("<unknown>");
                let _ = writeln!(out, "  --> {name}:{line}:{column}");
            }
            for label in &diagnostic.labels {
                let (line, column) = self.sources.line_col(label.span.file, label.span.start);
                let _ = writeln!(out, "   = {} (at {line}:{column})", label.message);
            }
            if let Some(help) = &diagnostic.help {
                let _ = writeln!(out, "   = help: {help}");
            }
        }
        out
    }
}

/// Reusable compiler configuration. It is intentionally backend-neutral when
/// the crate is built without the `llvm` feature.
#[derive(Clone, Debug)]
pub struct Compiler {
    std_root: PathBuf,
}

impl Compiler {
    pub fn new(std_root: impl Into<PathBuf>) -> Self {
        Self {
            std_root: std_root.into(),
        }
    }

    pub fn std_root(&self) -> &Path {
        &self.std_root
    }

    /// Run the requested pipeline. No output is printed and generated
    /// executables are never run.
    pub fn compile(&self, request: CompileRequest) -> Compilation {
        let mut result = Compilation::empty();
        let path = request.input.name().to_path_buf();
        let source = match request.input.read() {
            Ok(source) => source,
            Err(failure) => {
                result.failure = Some(failure);
                return result;
            }
        };

        let file = result
            .sources
            .add(path.display().to_string(), source.clone());
        result.entry_file = Some(file);
        result.entry_tokens = Lexer::new(file, &source).tokenize(&mut result.diagnostics);

        if request.emit == Emit::Tokens {
            result.artifact = Some(Artifact::Text(tokens_string(&source, &result.entry_tokens)));
            return result;
        }

        let mut operators = discover_std_operators(&self.std_root);
        operators.extend(parser::discover_custom_operators(
            &source,
            &result.entry_tokens,
        ));
        let entry = parser::Parser::new(
            &source,
            std::mem::take(&mut result.entry_tokens),
            &mut result.diagnostics,
        )
        .with_custom_operators(&operators)
        .parse_module();
        // Retain the tokens for editor consumers. Re-lexing is deterministic
        // and avoids cloning every token before the parser takes ownership.
        result.entry_tokens = Lexer::new(file, &source).tokenize(&mut DiagnosticSink::new());
        result.modules.push(entry);
        load_std_deps(
            &mut result.sources,
            &mut result.modules,
            &mut result.diagnostics,
            &self.std_root,
            &operators,
        );
        result.stats.entry_items = result.entry().map_or(0, |module| module.items.len());
        result.stats.modules = result.modules.len();

        if result.diagnostics.has_errors() {
            return result;
        }

        match request.emit {
            Emit::Source => {
                result.artifact = result.entry().map(pretty::print_module).map(Artifact::Text);
                return result;
            }
            Emit::Ast => {
                result.artifact = result
                    .entry()
                    .map(|module| Artifact::Text(format!("{module:#?}\n")));
                return result;
            }
            Emit::Tokens => unreachable!(),
            _ => {}
        }

        let resolved = crate::resolve::resolve(&result.modules, &mut result.diagnostics);
        result.stats.definitions = Some(resolved.defs().len());
        let typed = crate::types::check(&result.modules, &resolved, &mut result.diagnostics);
        result.resolved = Some(resolved);
        result.typed = Some(typed);
        if result.diagnostics.has_errors() {
            return result;
        }

        let typed = result.typed.as_ref().expect("type checking completed");
        let resolved = result.resolved.as_ref().expect("resolution completed");
        let hierarchy = match &request.emit {
            Emit::Metadata => crate::elab::elaborate_for_check(
                &result.modules,
                resolved,
                typed,
                &mut result.diagnostics,
            ),
            Emit::Object { top } => {
                let top = match select_top(&result.modules, top.as_deref()) {
                    Ok(top) => top,
                    Err(failure) => {
                        result.failure = Some(failure);
                        return result;
                    }
                };
                let hierarchy = crate::elab::elaborate_top(
                    &result.modules,
                    resolved,
                    typed,
                    &mut result.diagnostics,
                    &top,
                );
                if hierarchy.roots.is_empty() {
                    result.failure = Some(CompileFailure::new(
                        FailureKind::Selection,
                        format!("no entity named `{top}`"),
                    ));
                    return result;
                }
                hierarchy
            }
            _ => crate::elab::elaborate(&result.modules, resolved, typed, &mut result.diagnostics),
        };
        result.stats.instances = Some(hierarchy.instances.len());
        result.stats.roots = Some(hierarchy.roots.len());

        if request.emit == Emit::Tree {
            result.artifact = Some(Artifact::Text(hierarchy.to_tree_string()));
            result.hierarchy = Some(hierarchy);
            return result;
        }

        let base_dir = path.parent().unwrap_or_else(|| Path::new(""));
        let design = crate::ir::lower_in(
            &result.modules,
            resolved,
            &hierarchy,
            &mut result.diagnostics,
            base_dir,
        );
        result.stats.signals = Some(design.signals.len());
        result.stats.drivers = Some(design.drivers.len());
        result.stats.event_blocks = Some(design.event_blocks.len());
        result.hierarchy = Some(hierarchy);
        result.design = Some(design);

        if request.emit == Emit::Ir {
            result.artifact = result
                .design
                .as_ref()
                .map(|design| Artifact::Text(design.to_ir_string()));
            return result;
        }
        if request.emit == Emit::Metadata || result.diagnostics.has_errors() {
            return result;
        }

        match request.emit {
            Emit::LlvmIr => self.emit_llvm_ir(&mut result),
            Emit::Object { .. } => {
                let output = request.output.unwrap_or_else(|| path.with_extension("o"));
                self.emit_object(&mut result, output);
            }
            Emit::TestExecutable => {
                let output = request
                    .output
                    .unwrap_or_else(|| path.with_extension("test"));
                self.emit_test_executable(&mut result, output);
            }
            Emit::Metadata | Emit::Source | Emit::Tokens | Emit::Ast | Emit::Tree | Emit::Ir => {}
        }
        result
    }

    #[cfg(feature = "llvm")]
    fn validate_design(result: &mut Compilation) -> bool {
        let design = result.design.as_ref().expect("lowering completed");
        let issues = design.validate();
        if issues.is_empty() {
            true
        } else {
            result.failure = Some(CompileFailure::new(
                FailureKind::Validation,
                format!(
                    "cannot generate native code:\n  - {}",
                    issues.join("\n  - ")
                ),
            ));
            false
        }
    }

    #[cfg(feature = "llvm")]
    fn emit_llvm_ir(&self, result: &mut Compilation) {
        if !Self::validate_design(result) {
            return;
        }
        match crate::llvm::emit_module_ir(result.design.as_ref().expect("validated design")) {
            Ok(text) => result.artifact = Some(Artifact::Text(text)),
            Err(error) => {
                result.failure = Some(CompileFailure::new(FailureKind::Backend, error));
            }
        }
    }

    #[cfg(not(feature = "llvm"))]
    fn emit_llvm_ir(&self, result: &mut Compilation) {
        result.failure = Some(backend_unavailable());
    }

    #[cfg(feature = "llvm")]
    fn emit_object(&self, result: &mut Compilation, output: PathBuf) {
        let design = result.design.as_ref().expect("lowering completed");
        if let Some(signal) = design.signals.iter().find(|signal| signal.width == 0) {
            result.failure = Some(CompileFailure::new(
                FailureKind::Validation,
                format!(
                    "`{}` has an unresolved width; build a concrete top or a wrapper that fixes its parameters",
                    signal.path
                ),
            ));
            return;
        }
        if !Self::validate_design(result) {
            return;
        }
        match crate::llvm::emit_object(result.design.as_ref().expect("validated design"), &output) {
            Ok(()) => {
                result.artifact = Some(Artifact::File {
                    kind: FileArtifact::Object,
                    path: output,
                });
            }
            Err(error) => {
                result.failure = Some(CompileFailure::new(FailureKind::Backend, error));
            }
        }
    }

    #[cfg(not(feature = "llvm"))]
    fn emit_object(&self, result: &mut Compilation, _output: PathBuf) {
        result.failure = Some(backend_unavailable());
    }

    #[cfg(feature = "llvm")]
    fn emit_test_executable(&self, result: &mut Compilation, output: PathBuf) {
        if !Self::validate_design(result) {
            return;
        }
        let hierarchy = result.hierarchy.as_ref().expect("elaboration completed");
        let design = result.design.as_ref().expect("lowering completed");
        match build::build(&result.modules, hierarchy, design, &output) {
            Ok(()) => {
                result.artifact = Some(Artifact::File {
                    kind: FileArtifact::TestExecutable,
                    path: output,
                });
            }
            Err(error) => {
                result.failure = Some(CompileFailure::new(FailureKind::Backend, error));
            }
        }
    }

    #[cfg(not(feature = "llvm"))]
    fn emit_test_executable(&self, result: &mut Compilation, _output: PathBuf) {
        result.failure = Some(backend_unavailable());
    }
}

#[cfg(not(feature = "llvm"))]
fn backend_unavailable() -> CompileFailure {
    CompileFailure::new(
        FailureKind::Backend,
        "this siox library was built without the `llvm` feature",
    )
}

fn select_top(modules: &[Module], explicit: Option<&str>) -> Result<String, CompileFailure> {
    if let Some(top) = explicit {
        return Ok(top.to_string());
    }
    let tops: Vec<&str> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Entity(entity)
                if entity.attrs.iter().any(|attribute| {
                    attribute
                        .name
                        .segments
                        .last()
                        .is_some_and(|name| name.text == "top")
                }) =>
            {
                Some(entity.name.text.as_str())
            }
            _ => None,
        })
        .collect();
    match tops.as_slice() {
        [top] => Ok((*top).to_string()),
        [] => Err(CompileFailure::new(
            FailureKind::Selection,
            "no #[top] entity; name one explicitly",
        )),
        _ => Err(CompileFailure::new(
            FailureKind::Selection,
            format!(
                "multiple #[top] entities ({}); select one explicitly",
                tops.join(", ")
            ),
        )),
    }
}

fn load_std_deps(
    sources: &mut SourceMap,
    modules: &mut Vec<Module>,
    sink: &mut DiagnosticSink,
    std_root: &Path,
    operators: &HashMap<String, u8>,
) {
    let mut loaded = HashSet::new();
    let mut queue = using_bases(&modules[0]);
    let wants_std = queue.iter().any(|base| {
        base.segments
            .first()
            .is_some_and(|segment| segment.text == "std")
    });
    if wants_std && !std_root.is_dir() {
        sink.emit(
            Diagnostic::error(format!("no standard library at `{}`", std_root.display()))
                .with_code(crate::diag::codes::UNRESOLVED_IMPORT)
                .help("configure Compiler with the directory containing logic.siox, bits.siox, and the other std modules"),
        );
        return;
    }
    if std_root.join("prelude.siox").exists() {
        let segment = |text: &str| crate::syntax::ast::Ident {
            text: text.to_string(),
            span: crate::diag::Span::new(FileId(0), 0..0),
        };
        queue.push(AstPath {
            segments: vec![segment("std"), segment("prelude")],
            span: crate::diag::Span::new(FileId(0), 0..0),
        });
    }

    while let Some(base) = queue.pop() {
        let Some(path) = std_file(std_root, &base) else {
            continue;
        };
        if !loaded.insert(path.clone()) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = sources.add(path.display().to_string(), source.clone());
        let tokens = Lexer::new(file, &source).tokenize(sink);
        let module = parser::Parser::new(&source, tokens, sink)
            .with_custom_operators(operators)
            .parse_module();
        queue.extend(using_bases(&module));
        modules.push(module);
    }
}

fn discover_std_operators(std_root: &Path) -> HashMap<String, u8> {
    fn visit(directory: &Path, operators: &mut HashMap<String, u8>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, operators);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "siox")
            {
                let Ok(source) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let mut sink = DiagnosticSink::new();
                let tokens = Lexer::new(FileId(0), &source).tokenize(&mut sink);
                operators.extend(parser::discover_custom_operators(&source, &tokens));
            }
        }
    }

    let mut operators = HashMap::new();
    visit(std_root, &mut operators);
    operators
}

fn using_bases(module: &Module) -> Vec<AstPath> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Using(using) => match &using.kind {
                UsingKind::Import { base, .. } => Some(base.clone()),
                UsingKind::Alias { .. } => None,
            },
            _ => None,
        })
        .collect()
}

fn std_file(std_root: &Path, base: &AstPath) -> Option<PathBuf> {
    let segments: Vec<&str> = base
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect();
    if segments.first() != Some(&"std") {
        return None;
    }
    let mut path = std_root.to_path_buf();
    for segment in &segments[1..] {
        path.push(segment);
    }
    path.set_extension("siox");
    Some(path)
}

fn tokens_string(source: &str, tokens: &[Token]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        let text = &source[token.span.start as usize..token.span.end as usize];
        let shown = match token.kind {
            TokenKind::Eof => "<eof>".to_string(),
            _ => format!("{text:?}"),
        };
        let kind = format!("{:?}", token.kind);
        let _ = writeln!(out, "   {index:>4}  {kind:<13} {shown}");
    }
    out
}

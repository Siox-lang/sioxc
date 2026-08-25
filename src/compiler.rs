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

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::diag::{Diagnostic, DiagnosticSink, FileId, Severity, SourceMap};
use crate::elab::Hierarchy;
use crate::ir::Design;
use crate::resolve::Resolved;
use crate::syntax::ast::{Item, Module};
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
    /// Build a test executable a debugger can follow: the generated C is
    /// attributed back to its `.siox` lines and compiled unoptimized with
    /// debug info, so `break file.siox:34` and stepping work. Off by default,
    /// because simulation throughput matters for long runs.
    pub debug: bool,
}

impl CompileRequest {
    pub fn new(input: SourceInput, emit: Emit) -> Self {
        Self {
            input,
            emit,
            output: None,
            debug: false,
        }
    }

    /// Build for a debugger: siox line mapping and no optimization.
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
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
                if let Some(snippet) = self.sources.snippet(span.file, span.start) {
                    let _ = writeln!(out, "{snippet}");
                }
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

        let mut operators = parser::discover_custom_operators(&source, &result.entry_tokens);
        let dependencies = discover_dependencies(
            &source,
            &result.entry_tokens,
            &path,
            path.parent().unwrap_or_else(|| Path::new(".")),
            &self.std_root,
            &mut operators,
            &mut result.diagnostics,
        );
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
        for dependency in dependencies {
            let file = result.sources.add(
                dependency.path.display().to_string(),
                dependency.source.clone(),
            );
            let tokens = Lexer::new(file, &dependency.source).tokenize(&mut result.diagnostics);
            let module = parser::Parser::new(&dependency.source, tokens, &mut result.diagnostics)
                .with_custom_operators(&operators)
                .parse_module();
            result.modules.push(module);
        }
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
                let top = match select_top(&result.modules, resolved, top.as_deref()) {
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
                self.emit_test_executable(&mut result, output, request.debug);
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

    /// An unbound parameter reaches the backend as a zero width. Both native
    /// paths hit it the same way -- a parametric `#[top]` with nothing fixing
    /// its parameters -- so both say the same actionable thing, rather than one
    /// naming the fix and the other reporting "unknown width (0)" per signal
    /// from the generic validator.
    #[cfg(feature = "llvm")]
    fn reject_unresolved_widths(result: &mut Compilation) -> bool {
        let design = result.design.as_ref().expect("lowering completed");
        if let Some(signal) = design.signals.iter().find(|signal| signal.width == 0) {
            let path = signal.path.clone();
            result.failure = Some(CompileFailure::new(
                FailureKind::Validation,
                format!(
                    "`{path}` has an unresolved width; build a concrete top or a wrapper that fixes its parameters"
                ),
            ));
            return false;
        }
        true
    }

    #[cfg(feature = "llvm")]
    fn emit_object(&self, result: &mut Compilation, output: PathBuf) {
        if !Self::reject_unresolved_widths(result) {
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
    fn emit_test_executable(&self, result: &mut Compilation, output: PathBuf, debug: bool) {
        if !Self::validate_design(result) {
            return;
        }
        let resolved = result.resolved.as_ref().expect("resolution completed");
        let hierarchy = result.hierarchy.as_ref().expect("elaboration completed");
        let design = result.design.as_ref().expect("lowering completed");
        match build::build(
            &result.modules,
            resolved,
            hierarchy,
            design,
            &result.sources,
            debug,
            &output,
        ) {
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
    fn emit_test_executable(&self, result: &mut Compilation, _output: PathBuf, _debug: bool) {
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

fn select_top(
    modules: &[Module],
    resolved: &Resolved,
    explicit: Option<&str>,
) -> Result<String, CompileFailure> {
    let entities: Vec<_> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Entity(entity) => resolved.declared(entity.name.span).map(|id| {
                let qualified = resolved
                    .qualified_name(id)
                    .unwrap_or_else(|| entity.name.text.clone());
                (entity, qualified)
            }),
            _ => None,
        })
        .collect();

    if let Some(top) = explicit {
        if let Some((_, qualified)) = entities.iter().find(|(_, qualified)| qualified == top) {
            return Ok(qualified.clone());
        }
        let matches: Vec<&str> = entities
            .iter()
            .filter(|(entity, _)| entity.name.text == top)
            .map(|(_, qualified)| qualified.as_str())
            .collect();
        return match matches.as_slice() {
            [qualified] => Ok((*qualified).to_string()),
            [] => Err(CompileFailure::new(
                FailureKind::Selection,
                format!("no entity named `{top}`"),
            )),
            _ => Err(CompileFailure::new(
                FailureKind::Selection,
                format!(
                    "entity name `{top}` is ambiguous ({}); select one by its qualified name",
                    matches.join(", ")
                ),
            )),
        };
    }

    let tops: Vec<&str> = entities
        .iter()
        .filter(|(entity, _)| {
            entity.attrs.iter().any(|attribute| {
                attribute
                    .name
                    .segments
                    .last()
                    .is_some_and(|name| name.text == "top")
            })
        })
        .map(|(_, qualified)| qualified.as_str())
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

struct DependencySource {
    path: PathBuf,
    source: String,
}

/// Read the exact transitive import graph before the full parse and collect
/// every custom-operator declaration it contains. Expression grouping needs
/// the complete precedence table up front; parsing a dependency only after its
/// importer would make that dependency's operators appear undeclared in the
/// importer. Discovery is lexical, so malformed expressions do not hide later
/// `using` declarations and unrelated project files never affect the grammar.
fn discover_dependencies(
    entry_source: &str,
    entry_tokens: &[Token],
    entry_path: &Path,
    source_root: &Path,
    std_root: &Path,
    operators: &mut HashMap<String, u8>,
    sink: &mut DiagnosticSink,
) -> Vec<DependencySource> {
    let load_key = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut loaded = HashSet::from([load_key(entry_path)]);
    let mut queue: VecDeque<Vec<String>> =
        discover_import_modules(entry_source, entry_tokens).into();
    if std_root.join("prelude.siox").exists() {
        queue.push_back(vec!["std".to_string(), "prelude".to_string()]);
    }

    let mut missing_std_reported = false;
    let mut dependencies = Vec::new();
    while let Some(module) = queue.pop_front() {
        let is_std = module.first().is_some_and(|segment| segment == "std");
        if is_std && !std_root.is_dir() {
            if !missing_std_reported {
                sink.emit(
                    Diagnostic::error(format!("no standard library at `{}`", std_root.display()))
                        .with_code(crate::diag::codes::UNRESOLVED_IMPORT)
                        .help("configure Compiler with the directory containing logic.siox, bits.siox, and the other std modules"),
                );
                missing_std_reported = true;
            }
            continue;
        }
        let path = module_file(source_root, std_root, &module);
        if !loaded.insert(load_key(&path)) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut discovery_sink = DiagnosticSink::new();
        let tokens = Lexer::new(FileId(0), &source).tokenize(&mut discovery_sink);
        operators.extend(parser::discover_custom_operators(&source, &tokens));
        queue.extend(discover_import_modules(&source, &tokens));
        dependencies.push(DependencySource { path, source });
    }
    dependencies
}

/// Module paths named by top-level `using` declarations. This intentionally
/// recognizes only the two import spellings and skips aliases:
/// `using a::b::Name;` -> `a::b`, `using a::b::{Name}` -> `a::b`.
fn discover_import_modules(source: &str, tokens: &[Token]) -> Vec<Vec<String>> {
    let token_text = |token: &Token| {
        source
            .get(token.span.start as usize..token.span.end as usize)
            .unwrap_or("")
    };
    let mut modules = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Using {
            continue;
        }
        let mut cursor = index + 1;
        let mut segments = Vec::new();
        while let Some(segment) = tokens.get(cursor) {
            if segment.kind != TokenKind::Ident {
                break;
            }
            segments.push(token_text(segment).to_string());
            cursor += 1;
            if tokens
                .get(cursor)
                .is_some_and(|next| next.kind == TokenKind::ColonColon)
                && tokens
                    .get(cursor + 1)
                    .is_some_and(|next| next.kind == TokenKind::Ident)
            {
                cursor += 1;
                continue;
            }
            break;
        }
        if segments.is_empty()
            || tokens
                .get(cursor)
                .is_some_and(|next| next.kind == TokenKind::Eq)
        {
            continue;
        }
        let braced = tokens
            .get(cursor)
            .is_some_and(|next| next.kind == TokenKind::ColonColon)
            && tokens
                .get(cursor + 1)
                .is_some_and(|next| next.kind == TokenKind::LBrace);
        if !braced {
            segments.pop();
        }
        if !segments.is_empty() {
            modules.push(segments);
        }
    }
    modules
}

fn module_file(source_root: &Path, std_root: &Path, segments: &[String]) -> PathBuf {
    let is_std = segments.first().is_some_and(|segment| segment == "std");
    let mut path = if is_std {
        std_root.to_path_buf()
    } else {
        source_root.to_path_buf()
    };
    for segment in &segments[usize::from(is_std)..] {
        path.push(segment);
    }
    path.set_extension("siox");
    path
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

#[cfg(test)]
mod tests {
    use super::{discover_import_modules, select_top, FailureKind};
    use crate::diag::{DiagnosticSink, FileId};
    use crate::resolve;
    use crate::syntax;
    use crate::syntax::lexer::Lexer;

    #[test]
    fn lexical_dependency_discovery_matches_both_import_spellings() {
        let source = "module user;\n\
            using alpha::math::{Value, \"%%\"};\n\
            pub using beta::logic::Flag;\n\
            using Alias = gamma::Ignored;\n\
            using Local;\n";
        let mut sink = DiagnosticSink::new();
        let tokens = Lexer::new(FileId(0), source).tokenize(&mut sink);
        assert_eq!(
            discover_import_modules(source, &tokens),
            [
                vec!["alpha".to_string(), "math".to_string()],
                vec!["beta".to_string(), "logic".to_string()],
            ]
        );
    }

    #[test]
    fn explicit_top_requires_qualification_when_entity_leaves_collide() {
        let mut sink = DiagnosticSink::new();
        let modules = [
            syntax::parse_module(
                FileId(0),
                "module a; pub entity Root {} impl Root {}",
                &mut sink,
            ),
            syntax::parse_module(
                FileId(1),
                "module b; pub entity Root {} impl Root {}",
                &mut sink,
            ),
        ];
        let resolved = resolve::resolve(&modules, &mut sink);
        let ambiguous = select_top(&modules, &resolved, Some("Root")).unwrap_err();
        assert_eq!(ambiguous.kind, FailureKind::Selection);
        assert!(ambiguous.message.contains("a::Root"));
        assert!(ambiguous.message.contains("b::Root"));
        assert_eq!(
            select_top(&modules, &resolved, Some("b::Root")).unwrap(),
            "b::Root"
        );
    }
}

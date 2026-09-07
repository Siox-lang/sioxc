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
use crate::testbench::TestPlan;
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
    /// Read the source from disk at this path.
    Path(PathBuf),
    /// Compile text held in memory, such as an editor's unsaved buffer.
    Memory {
        /// The path the buffer stands for. Relative compile-time reads and
        /// default artifact names resolve against it.
        path: PathBuf,
        /// The buffer's contents.
        text: String,
    },
}

impl SourceInput {
    /// An input read from `path` on disk.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self::Path(path.into())
    }

    /// An in-memory input standing for `path`.
    pub fn memory(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        Self::Memory {
            path: path.into(),
            text: text.into(),
        }
    }

    /// The path this input is known by, whichever form it takes.
    pub fn name(&self) -> &Path {
        match self {
            Self::Path(path) | Self::Memory { path, .. } => path,
        }
    }

    /// The source text, read from disk or taken from the in-memory buffer.
    /// A directory is rejected here rather than producing a confusing parse error.
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
    /// Elaborated structural or explicitly selected instance tree.
    Tree,
    /// Normalized digital IR.
    Ir,
    /// LLVM textual IR.
    LlvmIr,
    /// Native object exposing the `sx_*` design ABI.
    Object {
        /// Which uninstantiated structural root to compile. `None` picks the
        /// only root, and is an error when several exist.
        top: Option<String>,
    },
    /// Standalone native executable containing all `#[test]` entities.
    TestExecutable,
}

impl Emit {
    /// Emit an object for the design's single structural root.
    pub fn object() -> Self {
        Self::Object { top: None }
    }
}

/// One complete compiler request.
#[derive(Clone, Debug)]
pub struct CompileRequest {
    /// The source to compile.
    pub input: SourceInput,
    /// Which product to produce.
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
    /// A request for `emit` from `input`, with no output override and
    /// debug info off.
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

    /// Set the destination path for a file artifact.
    pub fn with_output(mut self, path: impl Into<PathBuf>) -> Self {
        self.output = Some(path.into());
        self
    }
}

/// A successfully materialized artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Artifact {
    /// A textual product returned in memory, such as an IR or AST dump.
    Text(String),
    /// A product written to disk.
    File {
        /// What kind of file was written.
        kind: FileArtifact,
        /// Where it was written.
        path: PathBuf,
    },
}

/// Which kind of file a [`Artifact::File`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileArtifact {
    /// A native object exposing the `sx_*` design ABI.
    Object,
    /// A self-contained executable that runs the design's `#[test]` entities.
    TestExecutable,
}

/// Non-language failure category. Source-language failures are ordinary
/// structured diagnostics in [`Compilation::diagnostics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The source could not be read at all.
    Input,
    /// The request did not name a workable root — none exist, or several do
    /// and none was chosen.
    Selection,
    /// Lowered IR failed a structural invariant, which is a compiler bug
    /// rather than a fault in the source.
    Validation,
    /// Code generation, the C compiler, or the linker failed.
    Backend,
}

/// A non-language failure: why the pipeline could not finish, as opposed to
/// what was wrong with the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileFailure {
    /// Which stage-independent category the failure falls into.
    pub kind: FailureKind,
    /// Human-readable explanation.
    pub message: String,
}

impl CompileFailure {
    /// A failure of `kind` carrying `message`.
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for CompileFailure {
    /// Render as just the message, so `?` and `unwrap` read naturally.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompileFailure {}

/// Counts from completed phases. These are presentation-neutral and let a CLI
/// or build tool report progress without parsing compiler text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompilationStats {
    /// Top-level items in the entry file.
    pub entry_items: usize,
    /// Modules loaded, entry file and standard library together.
    pub modules: usize,
    /// Definitions produced by resolution; `None` if it did not run.
    pub definitions: Option<usize>,
    /// Instances in the elaborated hierarchy; `None` if elaboration did not
    /// run.
    pub instances: Option<usize>,
    /// Uninstantiated structural roots found.
    pub roots: Option<usize>,
    /// Signals in the lowered design; `None` if lowering did not run.
    pub signals: Option<usize>,
    /// Combinational drivers in the lowered design.
    pub drivers: Option<usize>,
    /// Event blocks in the lowered design.
    pub event_blocks: Option<usize>,
}

/// Result of one request, including useful partial products on failure.
pub struct Compilation {
    /// Every file the compilation loaded, so spans can be rendered.
    pub sources: SourceMap,
    /// The entry file's id within [`Compilation::sources`].
    pub entry_file: Option<FileId>,
    /// The entry file's tokens, retained even when later stages fail.
    pub entry_tokens: Vec<Token>,
    /// Parsed modules, entry file first. Parsing is best-effort, so this can
    /// hold a partial tree alongside error diagnostics.
    pub modules: Vec<Module>,
    /// Resolution output; `None` if resolution did not run.
    pub resolved: Option<Resolved>,
    /// Type-checking output; `None` if it did not run.
    pub typed: Option<Typed>,
    /// The elaborated instance hierarchy; `None` if elaboration did not run.
    pub hierarchy: Option<Hierarchy>,
    /// Resolved native tests bound to their elaborated hierarchy roots.
    /// Present only when compiling a test executable.
    pub test_plan: Option<TestPlan>,
    /// The lowered digital IR; `None` if lowering did not run.
    pub design: Option<Design>,
    /// Every diagnostic emitted, at any severity.
    pub diagnostics: DiagnosticSink,
    /// The requested product, when it was produced.
    pub artifact: Option<Artifact>,
    /// Why the pipeline stopped, for failures that are not source-language
    /// diagnostics.
    pub failure: Option<CompileFailure>,
    /// Counts from the phases that completed.
    pub stats: CompilationStats,
}

impl Compilation {
    /// A compilation with nothing completed yet. Every phase product starts
    /// absent and is filled in as its stage succeeds.
    fn empty() -> Self {
        Self {
            sources: SourceMap::new(),
            entry_file: None,
            entry_tokens: Vec::new(),
            modules: Vec::new(),
            resolved: None,
            typed: None,
            hierarchy: None,
            test_plan: None,
            design: None,
            diagnostics: DiagnosticSink::new(),
            artifact: None,
            failure: None,
            stats: CompilationStats::default(),
        }
    }

    /// Whether an artifact can be trusted: no failure, and no error-severity
    /// diagnostic.
    pub fn succeeded(&self) -> bool {
        self.failure.is_none() && !self.diagnostics.has_errors()
    }

    /// The entry file's parsed module, if parsing produced one.
    pub fn entry(&self) -> Option<&Module> {
        self.modules.first()
    }

    /// Every diagnostic emitted, in emission order.
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
    /// A compiler resolving `std::` imports under `std_root`.
    pub fn new(std_root: impl Into<PathBuf>) -> Self {
        Self {
            std_root: std_root.into(),
        }
    }

    /// The standard-library root this compiler resolves `std::` against.
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
        let (hierarchy, test_plan) = match &request.emit {
            Emit::Metadata => (
                crate::elab::elaborate_for_check(
                    &result.modules,
                    resolved,
                    typed,
                    &mut result.diagnostics,
                ),
                None,
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
                (hierarchy, None)
            }
            Emit::TestExecutable => {
                let (hierarchy, plan) = crate::testbench::elaborate(
                    &result.modules,
                    resolved,
                    typed,
                    &mut result.diagnostics,
                );
                (hierarchy, Some(plan))
            }
            _ => (
                crate::elab::elaborate(&result.modules, resolved, typed, &mut result.diagnostics),
                None,
            ),
        };
        if request.emit == Emit::TestExecutable && test_plan.as_ref().is_none_or(TestPlan::is_empty)
        {
            result.failure = Some(CompileFailure::new(
                FailureKind::Selection,
                "no enabled canonical std `#[test]` entity to build a test binary from",
            ));
            result.hierarchy = Some(hierarchy);
            result.test_plan = test_plan;
            return result;
        }
        result.stats.instances = Some(hierarchy.instances.len());
        result.stats.roots = Some(hierarchy.roots.len());
        result.test_plan = test_plan;

        if request.emit == Emit::Tree {
            result.artifact = Some(Artifact::Text(hierarchy.to_tree_string()));
            result.hierarchy = Some(hierarchy);
            return result;
        }

        let base_dir = path.parent().unwrap_or_else(|| Path::new(""));
        let mut design = crate::ir::lower_in(
            &result.modules,
            resolved,
            &hierarchy,
            &mut result.diagnostics,
            base_dir,
        );
        crate::test_ir::lower(
            &result.modules,
            resolved,
            typed,
            &hierarchy,
            result.test_plan.as_ref(),
            &mut design,
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
    /// Run the design's structural invariants, turning any violation into an
    /// error diagnostic. A failure here is a compiler bug rather than a fault in
    /// the source, so it reports as [`FailureKind::Validation`].
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
    /// Emit textual LLVM IR for the validated design.
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
    /// Stub for builds without the `llvm` feature: reports that the backend is
    /// unavailable rather than silently emitting nothing.
    fn emit_llvm_ir(&self, result: &mut Compilation) {
        result.failure = Some(backend_unavailable());
    }

    /// An unbound parameter reaches the backend as a zero width. Both native
    /// paths hit it the same way -- a parametric root with nothing fixing
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
                    "`{path}` has an unresolved width; build a concrete root or a wrapper that fixes its parameters"
                ),
            ));
            return false;
        }
        true
    }

    #[cfg(feature = "llvm")]
    /// Emit a native object exposing the `sx_*` design ABI.
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
    /// Stub for builds without the `llvm` feature.
    fn emit_object(&self, result: &mut Compilation, _output: PathBuf) {
        result.failure = Some(backend_unavailable());
    }

    #[cfg(feature = "llvm")]
    /// Build a standalone executable containing every `#[test]` entity.
    fn emit_test_executable(&self, result: &mut Compilation, output: PathBuf, debug: bool) {
        if !Self::validate_design(result) {
            return;
        }
        let resolved = result.resolved.as_ref().expect("resolution completed");
        let hierarchy = result.hierarchy.as_ref().expect("elaboration completed");
        let design = result.design.as_ref().expect("lowering completed");
        match build::build(build::BuildRequest {
            modules: &result.modules,
            resolved,
            hierarchy,
            design,
            sources: &result.sources,
            debug,
            output: &output,
        }) {
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
    /// Stub for builds without the `llvm` feature.
    fn emit_test_executable(&self, result: &mut Compilation, _output: PathBuf, _debug: bool) {
        result.failure = Some(backend_unavailable());
    }
}

#[cfg(not(feature = "llvm"))]
/// The failure returned by every backend entry point when the crate was
/// built without the `llvm` feature.
fn backend_unavailable() -> CompileFailure {
    CompileFailure::new(
        FailureKind::Backend,
        "this siox library was built without the `llvm` feature",
    )
}

/// Pick the structural root to compile.
///
/// Roots are entities nothing instantiates. `explicit` names one directly and
/// may be module-qualified to break a tie between equal leaf names. With no
/// explicit choice, exactly one root must exist -- `#[top]` is vendor metadata
/// and deliberately does not participate.
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
                (id, entity, qualified)
            }),
            _ => None,
        })
        .collect();

    if let Some(top) = explicit {
        if let Some((_, _, qualified)) = entities.iter().find(|(_, _, qualified)| qualified == top)
        {
            return Ok(qualified.clone());
        }
        let matches: Vec<&str> = entities
            .iter()
            .filter(|(_, entity, _)| entity.name.text == top)
            .map(|(_, _, qualified)| qualified.as_str())
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

    let roots = crate::elab::structural_root_entities(modules, resolved);
    let tops: Vec<&str> = entities
        .iter()
        .filter(|(id, _, _)| roots.contains(id))
        .map(|(_, _, qualified)| qualified.as_str())
        .collect();
    match tops.as_slice() {
        [top] => Ok((*top).to_string()),
        [] => Err(CompileFailure::new(
            FailureKind::Selection,
            "no structural root entity; name one explicitly with `--top`",
        )),
        _ => Err(CompileFailure::new(
            FailureKind::Selection,
            format!(
                "multiple structural root entities ({}); select one explicitly with `--top`",
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

/// The file backing a module path: under the standard-library root for a
/// `std::` path, otherwise beside the entry file.
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

/// Render the token stream for `--emit tokens`, one token per line with its
/// kind and source text.
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
    /// Dependency discovery has to find imports through both spellings, since
    /// `using a::b::{c}` and `pub using a::b::c` name the same module.
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
    /// When two modules export an entity of the same name, an explicit `--top`
    /// must be qualified; the bare leaf is ambiguous and has to say so.
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

    #[test]
    /// `#[top]` is vendor metadata, not a build directive: default root
    /// selection must ignore it and use structural reachability instead.
    fn default_object_root_is_structural_not_vendor_metadata() {
        let mut sink = DiagnosticSink::new();
        let modules = [
            syntax::parse_module(
                FileId(0),
                "module vendor; pub attr top: integer for entity;",
                &mut sink,
            ),
            syntax::parse_module(
                FileId(1),
                "module design; #[vendor::top = 1] entity Preferred {} entity Other {}",
                &mut sink,
            ),
        ];
        let resolved = resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());
        let crate::syntax::ast::Item::Entity(preferred) = &modules[1].items[0] else {
            panic!("expected preferred entity");
        };
        let metadata = resolved
            .resolved(preferred.attrs[0].name.span)
            .and_then(|id| resolved.def(id))
            .expect("vendor metadata remains resolved for output consumers");
        assert_eq!(metadata.module.as_deref(), Some("vendor"));

        let failure = select_top(&modules, &resolved, None).unwrap_err();
        assert_eq!(failure.kind, FailureKind::Selection);
        assert!(failure.message.contains("design::Preferred"));
        assert!(failure.message.contains("design::Other"));
        assert!(failure.message.contains("--top"));
    }

    #[test]
    /// With exactly one uninstantiated entity, no `--top` is needed.
    fn sole_uninstantiated_entity_is_the_default_object_root() {
        let mut sink = DiagnosticSink::new();
        let modules = [syntax::parse_module(
            FileId(0),
            "module design; entity Child {} impl Child {} \
             entity Root {} impl Root { let child: Child = {}; }",
            &mut sink,
        )];
        let resolved = resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());
        assert_eq!(
            select_top(&modules, &resolved, None).unwrap(),
            "design::Root"
        );
    }
}

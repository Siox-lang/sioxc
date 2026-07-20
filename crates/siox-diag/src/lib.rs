//! Source spans and diagnostics shared by every stage of the siox compiler.
//!
//! This crate is foundational: the lexer, parser, resolver, type checker,
//! elaborator and simulator all attach [`Span`]s to their data and emit
//! [`Diagnostic`]s through a common [`DiagnosticSink`].
//!
//! Spec: see `docs/spec.md` Stage 10 (Diagnostics and lint
//! rules) for the required error/warning catalogue and the rendered format.

use std::ops::Range;

/// Identifies a single loaded source file within a [`SourceMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

/// A byte range within a single source file.
///
/// Spans are half-open `[start, end)` byte offsets, mirroring `&str` slicing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, range: Range<u32>) -> Self {
        Span { file, start: range.start, end: range.end }
    }

    /// Smallest span covering both `self` and `other` (must share a file).
    pub fn to(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file);
        Span { file: self.file, start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}

/// Owns the text of every source file and maps [`FileId`]s back to names.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

pub struct SourceFile {
    pub name: String,
    pub text: String,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap::default()
    }

    /// Registers a file's text and returns its id.
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile { name: name.into(), text: text.into() });
        id
    }

    pub fn get(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    /// 1-based `(line, column)` for a byte offset, for diagnostic rendering.
    ///
    /// Columns count bytes within the line (good enough for ASCII source).
    /// Unknown files or out-of-range offsets clamp to `(1, 1)`.
    pub fn line_col(&self, file: FileId, offset: u32) -> (u32, u32) {
        let Some(src) = self.get(file) else { return (1, 1) };
        let offset = (offset as usize).min(src.text.len());
        let mut line = 1u32;
        let mut col = 1u32;
        for &b in &src.text.as_bytes()[..offset] {
            if b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

/// Severity of a [`Diagnostic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

/// A secondary span attached to a diagnostic ("declared here", etc.).
#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// A single compiler message. See the spec Stage 10 rendered example:
///
/// ```text
/// error[E-P0XX]: cannot assign to input port `ready`
///   --> stream.siox:42:9
/// ```
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable code such as `E-P001`. See [`codes`] for the catalogue.
    pub code: Option<&'static str>,
    pub message: String,
    pub primary: Option<Span>,
    pub labels: Vec<Label>,
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            code: None,
            message: message.into(),
            primary: None,
            labels: Vec::new(),
            help: None,
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Warning, ..Diagnostic::error(message) }
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub fn at(mut self, span: Span) -> Self {
        self.primary = Some(span);
        self
    }

    pub fn label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label { span, message: message.into() });
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

/// Collects diagnostics emitted while compiling. Stages push into this and
/// the CLI renders/counts at the end.
#[derive(Default)]
pub struct DiagnosticSink {
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticSink {
    pub fn new() -> Self {
        DiagnosticSink::default()
    }

    pub fn emit(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Error).count()
    }
}

/// Stable diagnostic codes. Filled in as each stage lands its checks.
///
/// Spec Stage 10 requires every diagnostic to carry a code, a main span, a
/// clear message, optional help, and related spans.
pub mod codes {
    // Errors
    pub const UNKNOWN_NAME: &str = "E-P001";
    pub const DUPLICATE_ITEM: &str = "E-P002";
    pub const TYPE_MISMATCH: &str = "E-P003";
    pub const WRITE_TO_INPUT_PORT: &str = "E-P004";
    pub const MISSING_PORT_CONNECTION: &str = "E-P005";
    pub const INVALID_ATTR_TARGET: &str = "E-P006";
    pub const INVALID_ATTR_VALUE_TYPE: &str = "E-P007";
    pub const INVALID_METHOD_CALL: &str = "E-P008";
    pub const INVALID_PATTERN: &str = "E-P009";
    pub const PHASE2_SYNTAX: &str = "E-P010";
    pub const UNRESOLVED_IMPORT: &str = "E-P011";
    /// A `let` binding without a type annotation (`let x = ...`): Phase 1 is
    /// type-strict — every binding declares its type (`let x: T [= ...]`).
    pub const MISSING_TYPE_ANNOTATION: &str = "E-P012";
    /// An entity instance declared with `const`. An entity is a hardware
    /// instance, not a compile-time value — declare it with `let`.
    pub const CONST_ENTITY_INSTANCE: &str = "E-P013";

    // Warnings
    pub const MULTIPLE_DRIVERS: &str = "W-P001";
    pub const POSSIBLE_LATCH: &str = "W-P002";
    pub const UNUSED_SIGNAL: &str = "W-P003";
    pub const UNUSED_PARAM: &str = "W-P004";
    pub const UNUSED_IMPORT: &str = "W-P005";
    pub const UNREACHABLE_MATCH_ARM: &str = "W-P006";
    pub const NON_EXHAUSTIVE_MATCH: &str = "W-P007";
    pub const SUSPICIOUS_LOGIC_COMPARE: &str = "W-P008";
    pub const SUSPICIOUS_RESET: &str = "W-P009";
    pub const COMBINATIONAL_LOOP: &str = "W-P010";
}

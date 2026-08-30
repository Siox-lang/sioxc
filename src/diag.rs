//! Source spans and diagnostics shared by every stage of the siox compiler.
//!
//! This crate is foundational: the lexer, parser, resolver, type checker,
//! elaborator and simulator all attach [`Span`]s to their data and emit
//! [`Diagnostic`]s through a common [`DiagnosticSink`].
//!
//! Spec: see `docs/language.md` Stage 10 (Diagnostics and lint
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
        Span {
            file,
            start: range.start,
            end: range.end,
        }
    }

    /// Smallest span covering both `self` and `other` (must share a file).
    pub fn to(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file);
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
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
        self.files.push(SourceFile {
            name: name.into(),
            text: text.into(),
        });
        id
    }

    pub fn get(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    /// 1-based `(line, column)` for a byte offset, for diagnostic rendering.
    ///
    /// Columns count bytes within the line (good enough for ASCII source).
    /// Unknown files or out-of-range offsets clamp to `(1, 1)`.
    /// The rendered source snippet for a span: the line it starts on, with a
    /// caret under the column.
    ///
    /// ```text
    ///    |
    /// 18 |     assert!(y == 99, "the counter should be 99 here");
    ///    |     ^
    /// ```
    ///
    /// `None` when the span names no file. Shared so a runtime failure in a
    /// generated executable and a compile diagnostic draw the same picture --
    /// the alternative is two renderers that drift.
    pub fn snippet(&self, file: FileId, offset: u32) -> Option<String> {
        let source = self.get(file)?;
        let (line, column) = self.line_col(file, offset);
        let text = source.text.lines().nth(line as usize - 1)?;
        // A tab occupies one column in `line_col` but renders wider, so it is
        // carried into the caret row to keep the two aligned.
        let indent: String = text
            .chars()
            .take(column as usize - 1)
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        // At least two, so the `|` column lines up with the `   = help:` rows
        // the renderer already emits for labels and help.
        let gutter = line.to_string().len().max(2);
        Some(format!(
            "{blank:>gutter$} |\n{line:>gutter$} | {text}\n{blank:>gutter$} | {indent}^",
            blank = "",
            gutter = gutter,
        ))
    }

    pub fn line_col(&self, file: FileId, offset: u32) -> (u32, u32) {
        let Some(src) = self.get(file) else {
            return (1, 1);
        };
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
        Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error(message)
        }
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
        self.labels.push(Label {
            span,
            message: message.into(),
        });
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
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
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
    // E-P005 (MISSING_PORT_CONNECTION) retired: an unconnected input is not an
    // error — it holds its default value (§3.29). See `UNCONNECTED_INPUT` below.
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
    /// One signal driven from several parallel contexts whose type has no
    /// `impl Resolve` to fold them (spec 3.14) — e.g. two producers wired to
    /// one bus net. A resolved type (`Logic`) folds instead.
    pub const CONFLICTING_DRIVERS: &str = "E-P014";
    /// A function that recursed past the inline depth limit: hardware
    /// recursion must terminate at elaboration, so this has no finite circuit.
    pub const UNBOUNDED_RECURSION: &str = "E-P015";
    /// A qualified path or `using` that accesses a private declaration from a
    /// different source module.
    pub const PRIVATE_IMPORT: &str = "E-P016";
    /// An expression with no hardware form — a chained runtime index
    /// (`m[i][j]`) is the usual one. It lowered to an anonymous `Unknown`,
    /// which validation could only report as "the driver for `x` contains an
    /// Unknown", naming neither the expression nor its line.
    pub const UNSUPPORTED_EXPR: &str = "E-P017";
    /// The left of an assignment is not a place: `f(x) = v`. A target is a
    /// signal, a field, an index, a slice, or a concatenation of those.
    pub const INVALID_ASSIGN_TARGET: &str = "E-P018";
    /// A statement that cannot do anything: `x;`, `a + 1;`, a stray
    /// `continue;`. Only a call has an effect, so lowering's catch-all dropped
    /// every other shape without a word and a misspelled name compiled clean.
    pub const NO_EFFECT_STATEMENT: &str = "E-P019";
    /// An entity instantiated somewhere structural elaboration cannot reach:
    /// a `match` arm, a function body, or a behavioural (non-generate) `if`.
    /// Instances are gathered from an entity's root layer and from generate
    /// `for`/`if` only, so these used to vanish without a word.
    pub const INSTANCE_PLACEMENT: &str = "E-P020";
    /// A `let` initializer that is not a constant. An initializer is the
    /// signal's power-on value, folded at elaboration; one that reads another
    /// signal cannot be folded and was dropped, leaving the signal at its
    /// type's default with no word said.
    pub const NON_CONSTANT_INITIALIZER: &str = "E-P021";
    /// An in-range element of an instance array was omitted by concrete
    /// generate elaboration, but its ports are referenced as though the child
    /// existed. This differs from E-P003: the index is inside the declaration.
    pub const INSTANCE_NOT_ELABORATED: &str = "E-P022";
    /// A file requested by a compile-time `read<T>` initializer
    /// could not be opened, or its contents do not fit the declared target.
    pub const COMPILE_TIME_IO: &str = "E-P023";
    /// A private struct field or inherent method used outside the owning
    /// type's implementation domain, private entity state accessed through an
    /// instance, or an attempt to publish an entity method before
    /// cross-hierarchy calls have defined hardware semantics.
    pub const PRIVATE_MEMBER: &str = "E-P024";
    /// A public declaration whose signature contains a private nominal type.
    /// Such an API would be exported but impossible to name from its users.
    pub const PRIVATE_INTERFACE: &str = "E-P025";
    /// An inherent implementation violates the ownership/coherence rules:
    /// only the defining module may add inherent members to a nominal type.
    pub const IMPL_COHERENCE: &str = "E-P026";
    /// A `process` appears outside an inherent entity implementation. Processes
    /// describe concurrent entity behavior; functions already provide a
    /// sequential scope for ordinary types and trait implementations.
    pub const PROCESS_PLACEMENT: &str = "E-P027";
    /// A native test declares more independently scheduled foreground
    /// processes than the Phase-1 test scheduler can execute concurrently.
    pub const TEST_PROCESS_SCHEDULING: &str = "E-P028";

    // Warnings
    // W-P001 retired: parallel drivers are legal when their type implements
    // `Resolve`, and otherwise are the E-P014 `CONFLICTING_DRIVERS` error.
    pub const POSSIBLE_LATCH: &str = "W-P002";
    pub const UNUSED_SIGNAL: &str = "W-P003";
    pub const UNUSED_PARAM: &str = "W-P004";
    pub const UNUSED_IMPORT: &str = "W-P005";
    pub const UNREACHABLE_MATCH_ARM: &str = "W-P006";
    pub const NON_EXHAUSTIVE_MATCH: &str = "W-P007";
    pub const SUSPICIOUS_LOGIC_COMPARE: &str = "W-P008";
    pub const SUSPICIOUS_RESET: &str = "W-P009";
    pub const COMBINATIONAL_LOOP: &str = "W-P010";
    /// An `out` port that is never driven inside its entity.
    pub const UNDRIVEN_OUTPUT: &str = "W-P011";
    /// A sub-instance `in` port left unconnected — it holds its default value
    /// (§3.29) rather than being an error (an unconnected input is just
    /// undriven; "always initialized, may be undriven").
    pub const UNCONNECTED_INPUT: &str = "W-P012";
    /// A signal assigned unconditionally twice in one block: the earlier
    /// assignment can never be observed (drivers in a context override).
    pub const DEAD_ASSIGNMENT: &str = "W-P014";
    /// An attribute `std::attrs` declares and resolution accepts, but which no
    /// stage reads: writing it has no effect and nothing else would say so.
    pub const UNIMPLEMENTED_ATTR: &str = "W-P015";
    /// A struct literal that names only some of the type's fields. The rest
    /// take their default, which is usually intended — but silently, and a
    /// literal is also where a field is most often forgotten.
    pub const INCOMPLETE_STRUCT_LITERAL: &str = "W-P016";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The caret row must line up with the text row, including the gutter that
    /// carries the line number. Both the compiler's diagnostics and a runtime
    /// failure in a generated executable render through this, so a drift here
    /// misaligns both.
    #[test]
    fn a_snippet_puts_the_caret_under_its_column() {
        let mut sources = SourceMap::new();
        let text = "module m;\nlet value = 1;\n";
        let file = sources.add("t.siox", text);
        // The `=` on line 2 is at column 11.
        let offset = text.find('=').unwrap() as u32;
        let snippet = sources.snippet(file, offset).expect("a snippet");
        let rows: Vec<&str> = snippet.lines().collect();
        assert_eq!(rows.len(), 3, "blank, text and caret rows: {snippet}");
        assert!(rows[1].contains("let value = 1;"), "{snippet}");
        let column = rows[2].find('^').expect("a caret");
        assert_eq!(
            rows[1].as_bytes()[column],
            b'=',
            "the caret should sit under the column it names:\n{snippet}"
        );
    }

    /// A tab is one column to `line_col` but renders wider, so it has to be
    /// carried into the caret row or every line indented with tabs misaligns.
    #[test]
    fn a_tab_indent_is_carried_into_the_caret_row() {
        let mut sources = SourceMap::new();
        let text = "\t\tvalue = 1;\n";
        let file = sources.add("t.siox", text);
        let offset = text.find('v').unwrap() as u32;
        let snippet = sources.snippet(file, offset).expect("a snippet");
        let caret = snippet.lines().nth(2).expect("a caret row");
        let indent = &caret[caret.find('|').unwrap() + 2..];
        assert_eq!(
            indent, "\t\t^",
            "tabs should be reproduced, not replaced by spaces: {caret:?}"
        );
    }

    /// A span whose file is not in the map has nothing to show.
    #[test]
    fn an_unknown_file_has_no_snippet() {
        let sources = SourceMap::new();
        assert!(sources.snippet(FileId(7), 0).is_none());
    }
}

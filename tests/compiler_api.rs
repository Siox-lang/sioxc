//! The embedding boundary must be sufficient for editors and project tools:
//! in-memory input, structured diagnostics, retained phase products, and
//! presentation-neutral artifacts all go through one public API.

use siox::compiler::{Artifact, CompileRequest, Compiler, Emit, FailureKind, SourceInput};

fn compiler() -> Compiler {
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
}

#[test]
fn in_memory_analysis_retains_every_completed_phase() {
    let source = r#"
module embedded;
using std::bits::unsigned;

#[top] entity Pass {
    value: unsigned[8] out
}

impl Pass {
    value = 3;
}
"#;
    let compilation = compiler().compile(CompileRequest::new(
        SourceInput::memory("/virtual/embedded.siox", source),
        Emit::Metadata,
    ));

    assert!(
        compilation.succeeded(),
        "embedded analysis failed:\n{}{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    assert_eq!(
        compilation
            .sources
            .get(compilation.entry_file.unwrap())
            .unwrap()
            .text,
        source
    );
    assert_eq!(
        compilation.entry().unwrap().path.segments[0].text,
        "embedded"
    );
    assert!(compilation.resolved.is_some());
    assert!(compilation.typed.is_some());
    assert!(compilation.hierarchy.is_some());
    assert!(compilation.design.is_some());
    assert!(compilation.stats.modules > 1, "the prelude was not loaded");
    assert_eq!(compilation.stats.signals, Some(1));
}

#[test]
fn language_errors_are_structured_results_not_host_failures() {
    let source = "module broken;\nentity Bad { value: Missing in }\n";
    let compilation = compiler().compile(CompileRequest::new(
        SourceInput::memory("/workspace/broken.siox", source),
        Emit::Metadata,
    ));

    assert!(!compilation.succeeded());
    assert!(compilation.failure.is_none());
    let diagnostic = compilation
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code == Some(siox::diag::codes::UNKNOWN_NAME))
        .expect("missing unknown-name diagnostic");
    let span = diagnostic.primary.expect("diagnostic lost its source span");
    let file = compilation
        .sources
        .get(span.file)
        .expect("span file missing");
    assert_eq!(file.name, "/workspace/broken.siox");
    assert_eq!(compilation.sources.line_col(span.file, span.start), (2, 21));
    assert!(compilation.resolved.is_some());
    assert!(compilation.typed.is_some());
    assert!(compilation.hierarchy.is_none());
}

#[test]
fn textual_artifacts_are_returned_in_memory() {
    let compilation = compiler().compile(CompileRequest::new(
        SourceInput::memory("formatted.siox", "module formatted;\nentity E {}\n"),
        Emit::Source,
    ));

    assert!(compilation.succeeded());
    let Some(Artifact::Text(source)) = compilation.artifact else {
        panic!("canonical source was not returned as text")
    };
    assert!(source.starts_with("module formatted;"));
    assert!(source.contains("entity E"));
}

#[test]
fn input_failures_are_separate_and_typed() {
    let missing = std::env::temp_dir().join(format!(
        "siox_missing_embedding_input_{}_{}",
        std::process::id(),
        line!()
    ));
    let compilation = compiler().compile(CompileRequest::new(
        SourceInput::path(&missing),
        Emit::Metadata,
    ));

    assert!(!compilation.succeeded());
    assert!(compilation.diagnostics().is_empty());
    assert_eq!(compilation.failure.unwrap().kind, FailureKind::Input);
}

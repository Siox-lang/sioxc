//! Late lowering diagnostics must render at their originating source line.

use std::process::Command;

fn compile(name: &str, source: &str) -> (std::path::PathBuf, std::process::Output) {
    let dir = std::env::temp_dir().join(format!("siox_late_spans_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    (file, output)
}

fn rendered(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string() + &String::from_utf8_lossy(&output.stderr)
}

#[test]
fn an_ir_lint_renders_at_the_port_declaration() {
    let source = "module m;\n\
        using std::bits::{unsigned};\n\
        entity E {\n\
          forgotten: unsigned[8] out,\n\
        }\n\
        impl E {}\n";
    let (file, output) = compile("undriven", source);
    let text = rendered(&output);
    assert!(
        output.status.success(),
        "warning-only compile failed:\n{text}"
    );
    assert!(text.contains("W-P011"), "missing lint code:\n{text}");
    assert!(
        text.contains(&format!("{}:4:", file.display())),
        "lint did not point at the port declaration:\n{text}"
    );
}

#[test]
fn a_compile_time_file_error_renders_at_the_let_declaration() {
    let source = "module m;\n\
        using std::bits::{unsigned};\n\
        entity E {}\n\
        impl E {\n\
          let data: unsigned[8][2] = read<unsigned[8]>(\"__siox_missing_span_fixture__.bin\");\n\
        }\n";
    let (file, output) = compile("missing_file", source);
    let text = rendered(&output);
    assert!(!output.status.success(), "missing file compiled:\n{text}");
    assert!(text.contains("E-P023"), "missing file-I/O code:\n{text}");
    assert!(
        text.contains(&format!("{}:5:", file.display())),
        "file error did not point at the let declaration:\n{text}"
    );
}

#[test]
fn a_diagnostic_shows_its_source_line_with_a_caret() {
    // The location line is followed by the source and a caret under the column
    // it names. A runtime failure in a generated executable renders through the
    // same helper, so the two cannot drift apart.
    let source = "module m;\n\
                  using std::bits::{unsigned};\n\
                  entity E { y: unsigned[8] out }\n\
                  impl E { y = nonexistent; }\n";
    let (_, output) = compile("caret", source);
    let text = rendered(&output);
    let row = text
        .lines()
        .find(|line| line.trim_start().starts_with("4 | "))
        .unwrap_or_else(|| panic!("the offending line should be shown, got:\n{text}"));
    let caret = text
        .lines()
        .find(|line| line.contains('^'))
        .unwrap_or_else(|| panic!("a caret row should follow it, got:\n{text}"));
    let column = caret.find('^').unwrap();
    assert_eq!(
        row.as_bytes().get(column).copied(),
        Some(b'n'),
        "the caret should point at the unknown name, got:\n{text}"
    );
}

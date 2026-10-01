//! How `using` maps module paths to files, and what a mismatch reports.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

/// A throwaway project: `top.siox` importing `bus::spi` and `bus::uart`, where
/// `bus/uart.siox` declares `uart_decl` as its module.
fn project(name: &str, uart_decl: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_module_files_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("bus")).unwrap();
    std::fs::write(
        dir.join("bus/spi.siox"),
        "module bus::spi;\npub entity Master { o: Bit out }\nimpl Master { o = '1'; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("bus/uart.siox"),
        format!(
            "module {uart_decl};\npub entity Other {{ o: Bit out }}\nimpl Other {{ o = '0'; }}\n"
        ),
    )
    .unwrap();
    let entry = dir.join("top.siox");
    std::fs::write(
        &entry,
        "module top;\nuse bus::spi::{Master};\nuse bus::uart::{Other};\n\
         entity Top { a: Bit out, b: Bit out }\n\
         impl Top { let m: Master = { .o = a }; let u: Other = { .o = b }; }\n",
    )
    .unwrap();
    (dir, entry)
}

fn compile(entry: &std::path::Path) -> siox::compiler::Compilation {
    Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(CompileRequest::new(
        SourceInput::path(entry),
        Emit::Metadata,
    ))
}

/// `using bus::uart::{..}` reads `bus/uart.siox`. When that file declares a
/// different module, the error says so, in that file. It used to report only
/// "no module `bus::uart` was loaded" at the import, with help claiming that
/// "only `std::` paths are read from disk" -- in the same compilation that
/// had just read `bus/spi.siox` from disk and imported it without complaint.
#[test]
fn a_file_declaring_the_wrong_module_is_reported_at_its_declaration() {
    let (dir, entry) = project("mismatch", "wrongname");
    let compilation = compile(&entry);
    let rendered = compilation.render_diagnostics();

    assert!(
        !compilation.succeeded(),
        "a mismatch must fail:\n{rendered}"
    );
    assert!(
        rendered.contains("declares `module wrongname`") && rendered.contains("bus::uart"),
        "the mismatch is named:\n{rendered}"
    );
    assert!(
        rendered.contains("uart.siox"),
        "the report points at the dependency file:\n{rendered}"
    );
    assert!(
        !rendered.contains("only `std::` paths are read from disk"),
        "no diagnostic may claim only std is read from disk:\n{rendered}"
    );
    // `bus::spi` is declared correctly beside it and must not be reported.
    assert!(!rendered.contains("`bus::spi`"), "{rendered}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The control case: with the declaration matching its path, the same
/// project compiles cleanly.
#[test]
fn a_file_declaring_its_own_path_loads() {
    let (dir, entry) = project("match", "bus::uart");
    let compilation = compile(&entry);
    assert!(
        compilation.succeeded(),
        "a matching declaration loads:\n{}",
        compilation.render_diagnostics()
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A module path with no file behind it names the file it looked for.
#[test]
fn a_missing_module_file_names_the_path_it_looked_for() {
    let (dir, entry) = project("missing", "bus::uart");
    std::fs::remove_file(dir.join("bus/uart.siox")).unwrap();
    let rendered = compile(&entry).render_diagnostics();
    assert!(
        rendered.contains("bus/uart.siox"),
        "the expected file is named:\n{rendered}"
    );
    assert!(
        !rendered.contains("only `std::` paths are read from disk"),
        "{rendered}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

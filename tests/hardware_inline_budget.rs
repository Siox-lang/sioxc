//! The hardware (design-IR) lowering copies an inlined body's `let` values
//! into every use, so a library body that reuses values heavily compounds.
//! `std::float`'s operators did, and compiling `s = a * b` on two binary32
//! ports took more than 8 GB before anything was reported. The inline budget
//! stops it at a bounded size with one clear error.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn an_oversized_hardware_inline_is_an_error_not_unbounded_growth() {
    let dir = std::env::temp_dir().join(format!("siox_inline_budget_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.siox");
    std::fs::write(
        &entry,
        "module main;\nuse std::float::float;\n\
         entity E { a: float[8..-23] in, b: float[8..-23] in, s: float[8..-23] out }\n\
         impl E { s = a * b; }\n",
    )
    .unwrap();
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(SourceInput::path(&entry), Emit::Metadata),
    );
    let _ = std::fs::remove_dir_all(dir);
    let rendered = compilation.render_diagnostics();
    assert!(!compilation.succeeded());
    assert!(rendered.contains("in the hardware lowering"), "{rendered}");
    assert!(
        !rendered.contains("recursed deeper"),
        "the cascade behind the budget is not reported:\n{rendered}"
    );
}

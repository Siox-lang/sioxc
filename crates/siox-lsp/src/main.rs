//! `siox-lsp` — the siox language server.
//!
//! Skeleton only: it accepts the invocation the docs describe
//! (`siox-lsp --stdio --std <dir>`) and exits reporting that the server is not
//! yet implemented. The real server will speak LSP over stdin/stdout and reuse
//! the `siox` library's frontend (`siox::syntax` → `siox::types`) to publish
//! live diagnostics; see `docs/interoperability.md` for the intended surface.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Minimal argument handling so the documented invocation is accepted.
    let mut stdio = false;
    let mut std_dir = String::from("./std");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stdio" => stdio = true,
            "--std" => {
                i += 1;
                match args.get(i) {
                    Some(dir) => std_dir = dir.clone(),
                    None => {
                        eprintln!("siox-lsp: --std needs a directory");
                        return ExitCode::from(2);
                    }
                }
            }
            "-h" | "--help" => {
                println!("siox-lsp — the siox language server\n\nUSAGE:\n    siox-lsp --stdio [--std <dir>]\n");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("siox-lsp: unknown argument `{other}`");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    let _ = (stdio, std_dir);
    // The backend-independent frontend is linked and ready — this is what the
    // server will drive (parse → resolve → type-check → lints) to publish
    // diagnostics. It pulls in no LLVM backend. The LSP protocol layer over
    // stdio is not built yet.
    let _frontend = siox::diag::DiagnosticSink::new();
    eprintln!("siox-lsp: not yet implemented (frontend linked, no LLVM backend)");
    ExitCode::from(69) // EX_UNAVAILABLE
}

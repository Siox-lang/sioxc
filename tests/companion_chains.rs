//! A chain of guarded writes to one local keeps each write's companion
//! graph bounded. Each write's companion selects between the new value's and
//! the previous one's; returned inline, every later companion carried the
//! whole chain of selections back to the first write (about 360 values per
//! write), and compile time grew quadratically (200 writes: 280 s). A large
//! one now binds to a temporary, so no graph much exceeds that limit.

#[cfg(feature = "llvm")]
#[test]
fn guarded_write_chains_keep_companion_graphs_bounded() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    use siox::ir::{ProcessInstruction, ProcessValueId};
    let dir = std::env::temp_dir().join(format!("siox_companion_chains_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("chain");
    let writes = (0..48)
        .map(|index| format!("local[{}] = local[{}]; ", index % 8, (index + 3) % 8))
        .collect::<String>();
    let source = format!(
        "module chain;\nuse std::bits::unsigned;\n\
         entity E {{ enable: Bool in, a: unsigned[8] in, word: unsigned[8] out }}\n\
         impl E {{ if enable {{ let local: unsigned[8] = a; {writes} word = local; }} else {{ word = 0; }} }}\n\
         #[test] entity T {{}}\n\
         impl T {{ let enable: Bool = true; let a: unsigned[8] = 250; \
         let dut: E = {{ .enable = enable, .a = a }}; \
         check: process {{ await 1ns; print!(\"word={{}}\", dut.word); }} }}\n"
    );
    // Companion lowering recurses once per write; `sioxc` compiles on a
    // 1 GiB thread, and a 2 MiB test thread is too small for a debug build.
    let output = binary.clone();
    let path = dir.join("chain.siox");
    let compilation = std::thread::Builder::new()
        .stack_size(1 << 28)
        .spawn(move || {
            Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
                CompileRequest::new(SourceInput::memory(path, source), Emit::TestExecutable)
                    .with_output(&output),
            )
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    // The largest value graph any one instruction reads.
    let design = compilation.design.as_ref().unwrap();
    let values = &design.process_ir.values;
    let mut largest = 0;
    for process in &design.process_ir.processes {
        for block in &process.blocks {
            for instruction in &block.instructions {
                let ProcessInstruction::Assign { value, .. } = instruction else {
                    continue;
                };
                let mut seen = std::collections::HashSet::new();
                let mut pending: Vec<ProcessValueId> = vec![*value];
                while let Some(id) = pending.pop() {
                    if seen.insert(id) {
                        siox::ir::for_each_process_value_dependency(
                            &values[id.0 as usize].kind,
                            |dependency| pending.push(dependency),
                        );
                    }
                }
                largest = largest.max(seen.len());
            }
        }
    }
    assert!(
        largest < 12_000,
        "a companion graph spans the chain: {largest} values"
    );
    let output = std::process::Command::new(&binary)
        .output()
        .expect("test executable runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.lines().any(|line| line == "word=255"), "{stdout}");
}

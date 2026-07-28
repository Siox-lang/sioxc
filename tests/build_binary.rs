//! `siox build` produces a runnable native simulator binary (stage B5.1).
//! Only meaningful with the `llvm` feature + a clang toolchain.

use std::process::Command;

#[test]
fn test_no_run_builds_a_runnable_binary() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let out = std::env::temp_dir().join(format!("siox_counter_{}", std::process::id()));

    // Build from the repo root so `./std` resolves. The counter fixture lives
    // in-tree (the runnable `.siox` corpus moved to the siox-tests repo, but a
    // self-contained fixture keeps this integration test independent).
    let root = env!("CARGO_MANIFEST_DIR");
    let fixture = "tests/fixtures/counter_test.siox";
    let status = Command::new(siox)
        .current_dir(root)
        .args(["--test", fixture, "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "sioxc --test failed");
    assert!(out.exists(), "no binary produced");

    // The binary runs the testbench and exits 0 on PASS.
    let run = Command::new(&out).status().unwrap();
    assert!(run.success(), "native simulator returned {:?}", run.code());
    let _ = std::fs::remove_file(&out);
}

#[test]
fn native_testbench_exchanges_more_than_two_words() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let stem = format!("siox_wide_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.siox"));
    let out = std::env::temp_dir().join(&stem);
    std::fs::write(
        &source,
        "module wide_test;
         using std::bits::unsigned;
         #[test] entity WideTest {}
         impl WideTest {
             let value: unsigned[192];
             value = 1020847100762815390427017310442723737601;
             assert!(value == 1020847100762815390427017310442723737601,
                     \"all three words survive\");
         }",
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(root)
        .args([
            "--test",
            source.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "wide native build failed");
    let run = Command::new(&out).status().unwrap();
    assert!(run.success(), "wide native test returned {:?}", run.code());
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(out);
}

//! Native test fixtures are opened by the generated executable, not `sioxc`.

use std::process::{Command, Output};

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string() + &String::from_utf8_lossy(&output.stderr)
}

#[test]
fn generated_tests_own_and_read_the_current_runtime_files() {
    let dir = std::env::temp_dir().join(format!("siox_runtime_io_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("runtime_io.siox");
    let bytes = dir.join("bytes.bin");
    let wide = dir.join("wide.bin");
    let short = dir.join("short.bin");
    let scalar = dir.join("scalar.bin");
    let integer = dir.join("integer.bin");
    let message = dir.join("message.txt");
    let fixed = dir.join("fixed.txt");
    let hardware = dir.join("hardware.bin");
    let hardware_wide = dir.join("hardware-wide.bin");
    let binary = dir.join("runtime_io_test");
    std::fs::write(
        &source,
        "module runtime_io;\n\
         using std::text::{string, unicode};\n\
         using Text = string;\n\
         entity Rom { data: unsigned[8] out, wide: unsigned[128] out }\n\
         impl Rom {\n\
           let image: unsigned[8][1] = read<unsigned[8]>(\"hardware.bin\");\n\
           let wide_image: unsigned[128] = read<unsigned[128]>(\"hardware-wide.bin\");\n\
           data = image[0];\n\
           wide = wide_image;\n\
         }\n\
         #[test] entity RuntimeIo {}\n\
         impl RuntimeIo {\n\
           let baked: unsigned[8];\n\
           let baked_wide: unsigned[128];\n\
           let rom: Rom = { .data = baked, .wide = baked_wide };\n\
           let words: unsigned[16][2] = read<unsigned[16]>(\"bytes.bin\");\n\
           let wide: unsigned[128][1] = read<unsigned[128]>(\"wide.bin\");\n\
           let short: unsigned[8][4..2] = read<unsigned[8]>(\"short.bin\");\n\
           let scalar: unsigned[16] = read<unsigned[16]>(\"scalar.bin\");\n\
           let raw: integer = read<integer>(\"integer.bin\");\n\
           let message: string = read<string>(\"message.txt\");\n\
           let again: string = read<string>(\"message.txt\");\n\
           let alias: Text = read<Text>(\"message.txt\");\n\
           let fixed: string[4] = read<string>(\"fixed.txt\");\n\
           let total: integer = 0;\n\
           for character in message { total = total + unicode(character); }\n\
           await 1ns;\n\
           assert!(baked == 17, \"hardware image remains compile-time data\");\n\
           assert!(baked_wide[63..0] == unsigned[64](0x0706050403020100), \"hardware wide low word\");\n\
           assert!(baked_wide[127..64] == unsigned[64](0x0f0e0d0c0b0a0908), \"hardware wide high word\");\n\
           assert!(exists(\"bytes.bin\") and not exists(\"absent.bin\"), \"exists is runtime\");\n\
           assert!(words[0] == 4660, \"first little-endian word\");\n\
           assert!(words[1] == 43981, \"second little-endian word\");\n\
           assert!(wide[0][63..0] == unsigned[64](0x0706050403020100), \"wide low word\");\n\
           assert!(wide[0][127..64] == unsigned[64](0x0f0e0d0c0b0a0908), \"wide high word\");\n\
           assert!(short[4] == 90 and short[3] == 0 and short[2] == 0, \"labels and zero-fill\");\n\
           assert!(scalar == 4660, \"scalar construction from binary integer\");\n\
           assert!(raw == 0x0807060504030201, \"read<integer> is raw little-endian binary\");\n\
           assert!(message == \"hé🦀\", \"runtime text: {}\", message);\n\
           assert!(message == again, \"two runtime strings compare by value\");\n\
           assert!(message == alias, \"read type aliases preserve UTF-8 semantics\");\n\
           assert!(message'length == 3, \"Unicode length is code points\");\n\
           assert!(unicode(message[1]) == 233, \"Unicode indexing\");\n\
           assert!(unicode(message[99]) == 129408, \"dynamic array fallback\");\n\
           assert!(total == 129745, \"dynamic string iteration\");\n\
           assert!(unicode(fixed[0]) == 79 and unicode(fixed[1]) == 75, \"fixed runtime text\");\n\
           assert!(unicode(fixed[2]) == 0 and unicode(fixed[3]) == 0, \"fixed text zero-fills\");\n\
         }\n",
    )
    .unwrap();

    // Runtime fixtures do not exist while sioxc builds. The hardware image
    // does: hardware `read` remains an elaboration-time ROM initializer.
    let _ = std::fs::remove_file(&bytes);
    let _ = std::fs::remove_file(&wide);
    let _ = std::fs::remove_file(&short);
    let _ = std::fs::remove_file(&scalar);
    let _ = std::fs::remove_file(&integer);
    let _ = std::fs::remove_file(&message);
    let _ = std::fs::remove_file(&fixed);
    std::fs::write(&hardware, [17]).unwrap();
    std::fs::write(&hardware_wide, (0u8..16).collect::<Vec<_>>()).unwrap();
    let built = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(built.status.success(), "build failed:\n{}", text(&built));

    // Files created only after compilation are the values the testbench sees.
    // Mutating the hardware image proves that its original value was baked in.
    std::fs::write(&bytes, [0x34, 0x12, 0xcd, 0xab]).unwrap();
    std::fs::write(&wide, (0u8..16).collect::<Vec<_>>()).unwrap();
    std::fs::write(&short, [90]).unwrap();
    std::fs::write(&scalar, [0x34, 0x12]).unwrap();
    std::fs::write(&integer, [1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    std::fs::write(&message, "hé🦀").unwrap();
    std::fs::write(&fixed, "OK").unwrap();
    std::fs::write(&hardware, [99]).unwrap();
    let passed = Command::new(&binary).output().unwrap();
    assert!(
        passed.status.success(),
        "runtime read failed:\n{}",
        text(&passed)
    );

    std::fs::remove_file(&message).unwrap();
    let missing = Command::new(&binary).output().unwrap();
    let missing_text = text(&missing);
    assert!(
        !missing.status.success(),
        "missing file passed:\n{missing_text}"
    );
    assert!(
        missing_text.contains("read<string>") && missing_text.contains("message.txt"),
        "missing-file failure was not actionable:\n{missing_text}"
    );

    std::fs::write(&message, "hé🦀").unwrap();
    std::fs::write(&bytes, [1, 2, 3, 4, 5]).unwrap();
    let oversized = Command::new(&binary).output().unwrap();
    let oversized_text = text(&oversized);
    assert!(
        !oversized.status.success(),
        "oversized fixture passed:\n{oversized_text}"
    );
    assert!(
        oversized_text.contains("5 bytes do not fit"),
        "capacity failure was not precise:\n{oversized_text}"
    );

    std::fs::write(&bytes, [0x34, 0x12, 0xcd, 0xab]).unwrap();
    std::fs::write(&message, [0xff, 0xfe]).unwrap();
    let invalid = Command::new(&binary).output().unwrap();
    let invalid_text = text(&invalid);
    assert!(
        !invalid.status.success(),
        "invalid UTF-8 passed:\n{invalid_text}"
    );
    assert!(
        invalid_text.contains("not valid UTF-8"),
        "UTF-8 failure was not precise:\n{invalid_text}"
    );

    std::fs::write(&message, "hé🦀").unwrap();
    std::fs::write(&fixed, "TOO LONG").unwrap();
    let fixed_oversized = Command::new(&binary).output().unwrap();
    let fixed_oversized_text = text(&fixed_oversized);
    assert!(
        !fixed_oversized.status.success(),
        "oversized fixed string passed:\n{fixed_oversized_text}"
    );
    assert!(
        fixed_oversized_text.contains("8 characters do not fit a 4-element string"),
        "fixed-string capacity failure was not precise:\n{fixed_oversized_text}"
    );
}

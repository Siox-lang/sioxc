//! Regression for sioxc #499: a hardware ROM initialized with a qualified
//! `fs::read` or `std::fs::read` loads its file, as the bare `read` does; a
//! qualified one was ignored and left the ROM zero-filled.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn qualified_reads_load_the_rom() {
    let dir = std::env::temp_dir().join(format!("siox_i499_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rom = dir.join("rom.bin");
    std::fs::write(&rom, b"ABCD").unwrap();
    let file = dir.join("rom.siox");
    std::fs::write(&file, SOURCE.replace("ROMFILE", rom.to_str().unwrap())).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const SOURCE: &str = r#"module test_rom;

use std::fs;

entity RomFull {
    data: unsigned[8][4] out,
}
impl RomFull {
    let rom: unsigned[8][4] = std::fs::read<unsigned[8]>("ROMFILE");
    data = rom;
}

entity RomQualified {
    data: unsigned[8][4] out,
}
impl RomQualified {
    let rom: unsigned[8][4] = fs::read<unsigned[8]>("ROMFILE");
    data = rom;
}

entity RomBare {
    data: unsigned[8][4] out,
}
impl RomBare {
    let rom: unsigned[8][4] = read<unsigned[8]>("ROMFILE");
    data = rom;
}

#[test]
entity TestBench {}
impl TestBench {
    let q_data: unsigned[8][4];
    let b_data: unsigned[8][4];
    let rq: RomQualified = { .data = q_data };
    let rb: RomBare = { .data = b_data };
    let f_data: unsigned[8][4];
    let rf: RomFull = { .data = f_data };

    check: process {
        print!("RomBare:      [{}, {}, {}, {}]\n", b_data[0], b_data[1], b_data[2], b_data[3]);
        print!("RomQualified: [{}, {}, {}, {}]\n", q_data[0], q_data[1], q_data[2], q_data[3]);
        assert!(b_data[0] == 0x41);
        assert!(q_data[0] == 0x41 and q_data[3] == 0x44, "qualified");
        assert!(f_data[0] == 0x41 and f_data[3] == 0x44, "full path");
    }
}
"#;

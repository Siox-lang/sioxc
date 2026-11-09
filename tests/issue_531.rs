//! Regression for sioxc #531: `ULogic` (unresolved `std_ulogic`) has
//! `Logic`'s nine-value operators, encoding, matching and clock edges; it
//! had none, so `a and b` or `clk.rising()` on one was rejected. `Resolve`
//! stays `Logic`'s.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn ulogic_has_the_logic_operations() {
    let dir = std::env::temp_dir().join(format!("siox_i531_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ulogic.siox");
    std::fs::write(&file, SOURCE).unwrap();
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
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const SOURCE: &str = r#"module p531;

use std::bits::unsigned;

use std::logic::ULogic;

entity Counter {
    clk: ULogic in,
    count: unsigned[4] out,
}
impl Counter {
    tick: process {
        if clk.rising() { count = count + 1; }
    }
}

#[test]
entity TB {}
impl TB {
    let clk: ULogic = '0';
    let count: unsigned[4];
    let c: Counter = { .clk = clk, .count = count };
    run: process {
        let a: ULogic = '1';
        let b: ULogic = '0';
        let h: ULogic = 'H';
        let u: ULogic = 'U';
        assert!((a and b) == '0' and (a or b) == '1' and (not a) == '0', "and/or/not");
        assert!((a xor b) == '1' and (a nand a) == '0' and (b nor b) == '1' and (a xnor h) == '1', "xor family");
        assert!((u and b) == '0' and (u or a) == '1' and (u and a) == 'U', "unknowns");
        assert!(h.to_bool() and h.to_x01() == '1' and not h.is_binary(), "encoding");
        assert!(a.matches('-') and h.matches('1'), "matches");
        clk = '1'; await 1ns;
        clk = '0'; await 1ns;
        clk = 'H'; await 1ns;
        assert!(count == 2, "rising edges {}", count);
        print!("done\n");
    }
}
"#;

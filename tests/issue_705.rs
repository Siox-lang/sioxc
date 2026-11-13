//! Regression for sioxc #705: the constant environment Process IR folds
//! calls with labels a descending array constant from its left end
//! (`integer[1..0] = [4, 8]` holds `[1] = 4`), and holds impl constants and
//! positional struct constants. A folded call read `WIDTHS[1]` as 8 where
//! the run-time call read 4.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn folded_calls_read_constants_as_run_time_does() {
    let dir = std::env::temp_dir().join(format!("siox_i705_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("constants.siox");
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
    assert!(
        run.status.success() && stdout.contains("type_len=4 const_call=4 rt_call=4 pos=5 5"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p705;

use std::bits::unsigned;

const WIDTHS: integer[1..0] = [4, 8];

pub struct Pair {
    pub a: integer,
    pub b: integer,
}

const P_POS: Pair = { 4, 5 };

entity Holder {
    o: integer out,
}

impl Holder {
    const LIMIT: integer = 6;
    o = LIMIT;
}

fn get_w1(x: integer) -> integer {
    return WIDTHS[1] + x;
}

fn get_b(x: integer) -> integer {
    return P_POS.b + x;
}

#[test]
entity Tb {}
impl Tb {
    let o: unsigned[WIDTHS[1]];
    let h: integer;
    let holder: Holder = { .o = h };
    process {
        let rt: integer = 0;
        await 1ns;
        print!("type_len={} const_call={} rt_call={} pos={} {}\n", o'length, get_w1(0), get_w1(rt), get_b(0), get_b(rt));
        assert!(get_w1(0) == 4 and get_w1(rt) == 4 and get_b(0) == 5 and get_b(rt) == 5);
        print!("done\n");
    }
}
"#;

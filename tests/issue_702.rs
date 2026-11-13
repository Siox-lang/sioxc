//! Regression for sioxc #702: a module constant is folded with the same
//! type tables as an impl constant. `collect` folded module constants
//! before the enum and array-family tables were filled, so an enum field of
//! an aggregate constant was opaque and its `'1'` never resolved (E-P021),
//! where the identical constant inside an impl worked.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn module_and_impl_constants_fold_alike() {
    let dir = std::env::temp_dir().join(format!("siox_i702_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("order.siox");
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

const SOURCE: &str = r#"module test_collect_order;

use std::bits::unsigned;

pub enum Bit2 {
    '0',
    '1',
}

pub struct Holder {
    pub b: Bit2,
    pub v: unsigned[4],
}

fn make_holder() -> Holder {
    return { .b = '1', .v = 5 };
}

const H_MOD: Holder = make_holder();

entity Top {
    b_mod: Bit2 out,
    b_imp: Bit2 out,
    v_mod: unsigned[4] out,
    v_imp: unsigned[4] out,
}
impl Top {
    const H_IMP: Holder = make_holder();
    let s_mod: Holder = H_MOD;
    let s_imp: Holder = H_IMP;
    b_mod = s_mod.b;
    b_imp = s_imp.b;
    v_mod = s_mod.v;
    v_imp = s_imp.v;
}

#[test]
entity Tb {}
impl Tb {
    let bm: Bit2;
    let bi: Bit2;
    let vm: unsigned[4];
    let vi: unsigned[4];
    let top: Top = { .b_mod = bm, .b_imp = bi, .v_mod = vm, .v_imp = vi };
    process {
        await 1ns;
        print!("{} {} {} {}\n", bm, bi, vm, vi);
        assert!(bm == '1' and bi == '1' and vm == 5 and vi == 5);
        print!("done\n");
    }
}
"#;

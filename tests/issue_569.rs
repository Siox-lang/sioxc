//! Regression for sioxc #569: a struct's own `impl Default` is its default
//! everywhere (spec 3.29), for an uninitialized signal or local and for
//! the nullary constructor `Pt()`, in hardware and in a testbench. Only an
//! explicit `Pt::default()` called it; the rest built all zeros.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn impl_default_is_the_default_everywhere() {
    let dir = std::env::temp_dir().join(format!("siox_i569_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("default.siox");
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
    assert!(run.status.success(), "{stdout}");
    assert_eq!(stdout.matches("x=7, y=9").count(), 5, "{stdout}");
}

const SOURCE: &str = r#"module test_default_struct;
use std::bits::unsigned;

struct Pt { pub x: unsigned[8], pub y: unsigned[8] }
impl Default for Pt {
    static fn default() -> Pt {
        return Pt { .x = 7, .y = 9 };
    }
}

entity Top {
    out_sig_x: unsigned[8] out,
    out_sig_y: unsigned[8] out,
    out_call_x: unsigned[8] out,
    out_call_y: unsigned[8] out,
}

impl Top {
    let uninit_sig: Pt; // Uninitialized signal
    process {
        let call_pt: Pt = Pt(); // Short nullary constructor Pt()
        out_sig_x = uninit_sig.x;
        out_sig_y = uninit_sig.y;
        out_call_x = call_pt.x;
        out_call_y = call_pt.y;
    }
}

#[test] entity TB {}
impl TB {
    let sx: unsigned[8];
    let sy: unsigned[8];
    let cx: unsigned[8];
    let cy: unsigned[8];
    let dut: Top = { .out_sig_x = sx, .out_sig_y = sy, .out_call_x = cx, .out_call_y = cy };

    process {
        await 1ns;
        print!("HW uninit_sig: x={}, y={}\n", sx, sy);
        print!("HW Pt(): x={}, y={}\n", cx, cy);

        let local_uninit: Pt;
        print!("TB local_uninit: x={}, y={}\n", local_uninit.x, local_uninit.y);

        let local_call: Pt = Pt();
        print!("TB Pt(): x={}, y={}\n", local_call.x, local_call.y);

        let local_def: Pt = Pt::default();
        print!("TB Pt::default(): x={}, y={}\n", local_def.x, local_def.y);
    }
}
"#;

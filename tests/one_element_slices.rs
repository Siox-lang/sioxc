//! #199: a range slices at every width. `v[i..i]` is a one-element vector,
//! not the element `v[i]`, so code generic over a width also works at 1.

use std::process::Command;

#[test]
fn issue_199_a_one_element_slice_is_a_vector() {
    let dir = std::env::temp_dir().join(format!("siox_one_element_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("slices.siox");
    std::fs::write(
        &file,
        r#"module m;
use std::bits::unsigned;
entity Low<W: integer> { v: unsigned[8] in, y: unsigned[W] out, }
impl<W: integer> Low<W> { y = v[W - 1..0]; }
#[test] entity T {}
impl T {
    let v: unsigned[8] = 0b1010_0101;
    let one: unsigned[1];
    let two: unsigned[2];
    let narrow: Low<1> = { .v = v, .y = one };
    let wide: Low<2> = { .v = v, .y = two };
    process {
        await 1ns;
        let arr: integer[3] = [7, 8, 9];
        let first: integer[1] = arr[0..0];
        let rows: integer[2][2] = [[1, 2], [3, 4]];
        let row: integer[2][1] = rows[1..1];
        let bit: unsigned[1] = v[2..2];
        print!("{} {} {} {} {} {}\n", one, two, first[0], row[0][1], bit, v[2]);
    }
}
"#,
    )
    .unwrap();
    let binary = dir.join("slices");
    let built = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "-o",
        ])
        .arg(&binary)
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let ran = Command::new(&binary).output().unwrap();
    let out =
        String::from_utf8_lossy(&ran.stdout).to_string() + &String::from_utf8_lossy(&ran.stderr);
    assert!(
        out.contains("1 1 7 4 1 '1'") && out.contains("1 passed"),
        "{out}"
    );
}

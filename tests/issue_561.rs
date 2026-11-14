//! Regression for sioxc #561: a packed vector over a plain user enum
//! (`struct States(State[3])`, no `impl LogicEncoding`) is packed as `Logic`
//! is, its first two variants on the value bit and every other one in the
//! companion plane. It could be declared but not constructed, indexed or
//! passed to hardware.

use std::process::Command;

fn prints(name: &str, source: &str, expect: &[&str]) {
    let dir = std::env::temp_dir().join(format!("siox_i561_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
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
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{name}: {stdout}");
    for line in expect {
        assert!(stdout.contains(line), "{name}: {stdout}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn plain_enum_vectors_pack() {
    prints("issue", ISSUE, &[]);
    prints(
        "full",
        FULL,
        &[
            // derived `==`, then element reads (`Done` lives in the companion)
            "true false true true",
            // a hardware port reads each variant, `Done` included
            "hw_run=true",
            "hw_idle=true",
            "hw_done=true idle=false run=false",
        ],
    );
}

const ISSUE: &str = r#"module enum_array_newtype;

enum State { Idle, Run, Done }

struct States(State[2]);

#[test]
entity Tb {}
impl Tb {
    process {
        let s: States = States([State::Idle, State::Run]);
        assert!(s[1] == State::Run, "index");
    }
}
"#;

const FULL: &str = r#"module f561;

enum State { Idle, Run, Done }

#[derive(Eq)]
struct States(State[3]);

entity Pick {
    s: States in,
    o: State out,
}
impl Pick {
    o = s[2];
}

#[test]
entity Tb {}
impl Tb {
    let s: States;
    let o: State;
    let dut: Pick = { .s = s, .o = o };
    process {
        let a: States = States([State::Done, State::Run, State::Idle]);
        let b: States = States([State::Done, State::Run, State::Idle]);
        let c: States = States([State::Done, State::Run, State::Done]);
        print!("{} {} {} {}\n", a == b, a == c, a[0] == State::Done, c[2] == State::Done);
        s = States([State::Idle, State::Idle, State::Run]);
        await 1ns;
        print!("hw_run={} ", o == State::Run);
        s = States([State::Idle, State::Idle, State::Idle]);
        await 1ns;
        print!("hw_idle={} ", o == State::Idle);
        s = c;
        await 1ns;
        print!("hw_done={} idle={} run={}\n", o == State::Done, o == State::Idle, o == State::Run);
        c[2] = State::Run;
        print!("{}\n", c[2] == State::Run and c[0] == State::Done);
    }
}
"#;

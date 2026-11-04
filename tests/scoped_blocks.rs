//! Scoped blocks in implementations (docs/proposals/scoped-blocks.md):
//! `label: { … }` and `{ … }` group members under a scope of their own,
//! flattened right after macro expansion. A labelled block's members are
//! named by its path (`counter.n`), which is also their waveform scope.

use std::process::Command;

fn sioxc(name: &str, source: &str, args: &[&str]) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_blocks_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, dir)
}

/// The proposal's example: a register private to its block, a `pub` member
/// through the label, a nested block, and an unlabelled block's export.
#[cfg(feature = "llvm")]
#[test]
fn scoped_blocks_flatten_into_their_implementation() {
    let (ok, text, dir) = sioxc("run", GOOD, &["--test", "-o", "blocks.bin"]);
    assert!(ok, "{text}");
    assert!(!text.contains("warning"), "{text}");
    let run = Command::new(dir.join("blocks.bin"))
        .args(["-o", "blocks.vcd"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("scoped blocks done"), "{text}");
    // The label is a waveform scope.
    let vcd = std::fs::read_to_string(dir.join("blocks.vcd")).unwrap();
    assert!(vcd.contains("$scope module counter $end"), "{vcd}");
    assert!(vcd.contains("$scope module inner $end"), "{vcd}");
}

/// Shadowing, a type-level member, a collision between unlabelled blocks,
/// and private access by bare name and through the label, each once.
#[test]
fn block_scoping_is_checked() {
    let (ok, text, _) = sioxc("bad", BAD, &["--emit", "metadata"]);
    assert!(!ok, "{text}");
    for message in [
        "`shared` is already visible from an enclosing scope",
        "`n` is already visible from an enclosing scope",
        "a scoped block holds signals, instances and processes",
        "`x` is declared twice in this implementation",
        "`n` is private to block `counter`",
        "`counter.n` is private to its block",
    ] {
        assert_eq!(text.matches(message).count(), 1, "`{message}`:\n{text}");
    }
    assert!(!text.contains("unknown value"), "reported twice:\n{text}");
    assert!(
        !text.contains("counter.uart"),
        "a pub instance's port is fine:\n{text}"
    );
}

#[test]
fn pub_let_at_the_root_is_redundant() {
    let (ok, text, _) = sioxc(
        "root",
        "module m;\nentity R { y: Bit out }\nimpl R {\n    pub let top: Bit;\n    y = top;\n}\n",
        &["--emit", "metadata"],
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("`pub let` belongs in a scoped block"),
        "{text}"
    );
}

const GOOD: &str = r#"module probe;

entity Cpu { clk: Bit in, count: unsigned[8] out, overflow: Bit out, deep: unsigned[8] out,
             flag: Bit out }

impl Cpu {
    counter: {
        let n: unsigned[8] = 250;          // a register, private to the block
        pub let wrapped: Bit = '0';        // visible to the whole implementation

        step: process {
            if clk.rising() {
                n = n + 1;
                wrapped = if n == 255 { '1' } else { '0' };
            }
        }

        count = n;                         // outer names are visible inside

        inner: {
            pub let twice: unsigned[8];
            twice = n + n;                 // the enclosing block's n
        }
        deep = inner.twice;
    }

    {
        pub let seen: Bit;                 // an unlabelled block's pub member joins the root
        let hold: Bit;
        hold = counter.wrapped;
        seen = hold;
    }

    overflow = counter.wrapped;
    flag = seen;
}

#[test]
entity T {}

impl T {
    let clk: Bit;
    let count: unsigned[8];
    let overflow: Bit;
    let deep: unsigned[8];
    let flag: Bit;
    let cpu: Cpu = { .clk = clk, .count = count, .overflow = overflow, .deep = deep, .flag = flag };
    clock: process { clk = not clk after 5ns; }
    check: process {
        for i in 0..4 { await clk.rising(); }
        await 1ns;
        print!("count={} overflow={} deep={} flag={}", count, overflow, deep, flag);
        assert!(count == 255, "the block's register counted 250 -> 255");
        assert!(deep == 254, "inner.twice is 255 + 255, wrapped to 8 bits");
        await clk.rising(); await 1ns;
        assert!(overflow == '1', "counter.wrapped through the label");
        assert!(flag == '1', "an unlabelled block's pub member, by its bare name");
        print!("scoped blocks done");
    }
}
"#;

const BAD: &str = r#"module probe;

entity Leaf { a: Bit in, y: Bit out }
impl Leaf { y = a; }

entity Bad { clk: Bit in, o: Bit out, p: Bit out, q: Bit out, r: Bit out }

impl Bad {
    let shared: Bit;

    counter: {
        let n: Bit;
        let shared: Bit;                   // shadows the root's `shared`
        pub let uart: Leaf = { .a = clk, .y = r };
        fn helper() -> Bit { return '0'; } // type-level member in a block
        p: process { let n: Bit = '0'; }   // a process local hides `n`
    }

    { let x: Bit; x = '1'; }
    { let x: Bit; x = '0'; }               // collides in the shared namespace

    o = n;                                 // private to `counter`
    p = counter.n;                         // private through the label
    q = counter.uart.y;                    // a pub instance's port: fine
}

"#;

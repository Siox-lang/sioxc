//! Entity methods (owner-approved, 2026-11-03): a `pub fn` with a receiver
//! on an entity is called on a direct child instance. Its writes are
//! combinational strobes active only while it is called, and its result comes
//! back in the same cycle; the state lives in the child, as with a vendor
//! FIFO's `wr_en`/`din`.

use std::process::Command;

fn sioxc(name: &str, source: &str, args: &[&str]) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_methods_{name}_{}", std::process::id()));
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

/// A FIFO driven only through `push`, `pop` and `is_empty`, with no warning.
#[cfg(feature = "llvm")]
#[test]
fn methods_strobe_the_child_and_answer() {
    let (ok, text, dir) = sioxc("run", GOOD, &["--test", "-o", "methods.bin"]);
    assert!(ok, "{text}");
    assert!(!text.contains("warning"), "{text}");
    let run = Command::new(dir.join("methods.bin")).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("entity methods done"), "{text}");
}

/// Two call sites, a call in a loop and a testbench call, each reported once.
#[test]
fn method_calls_are_checked() {
    let (ok, text, _) = sioxc("bad", BAD, &["--emit", "metadata"]);
    assert!(!ok, "{text}");
    for message in [
        "`fifo.push` is already called; an entity method has one call site per instance",
        "`fifo.push` is called inside a loop",
        "calling entity method `fifo.push` from a testbench is not supported yet",
    ] {
        assert_eq!(text.matches(message).count(), 1, "`{message}`:\n{text}");
    }
}

/// A generic wrapper around a black-box vendor FIFO lowers, the parent's
/// generic arguments written into the method ports' types.
#[test]
fn a_vendor_fifo_wrapper_lowers() {
    let (ok, text, _) = sioxc("xpm", XPM, &["--top", "Producer", "--emit", "ir"]);
    assert!(ok, "{text}");
    assert!(!text.contains("error"), "{text}");
}

const GOOD: &str = r#"module probe;

// The child: state in its own clocked process, like a vendor FIFO; the
// methods are strobes and answers.
entity Fifo { clk: Bit in }

impl Fifo {
    let buf: unsigned[8][4];
    let wp: unsigned[2];
    let rp: unsigned[2];
    let count: unsigned[3];
    let wr_en: Bit;
    let din: unsigned[8];
    let rd_en: Bit;

    store: process {
        if clk.rising() {
            if wr_en == '1' and count != 4 {
                buf[wp] = din;
                wp = wp + 1;
            }
            if rd_en == '1' and count != 0 {
                rp = rp + 1;
            }
            if wr_en == '1' and rd_en == '0' and count != 4 { count = count + 1; }
            if rd_en == '1' and wr_en == '0' and count != 0 { count = count - 1; }
        }
    }

    pub fn is_empty() -> Bit { return if count == 0 { '1' } else { '0' }; }
    pub fn push(v: unsigned[8]) { wr_en = '1'; din = v; }
    pub fn pop() -> unsigned[8] { rd_en = '1'; return buf[rp]; }
}

// The parent: calls the methods under ordinary conditions.
entity Pipe { clk: Bit in, valid: Bit in, data: unsigned[8] in, take: Bit in,
              q: unsigned[8] out, empty: Bit out }

impl Pipe {
    let fifo: Fifo = { .clk = clk };

    feed: process {
        if valid == '1' {
            fifo.push(data);
        }
    }

    drain: process {
        empty = fifo.is_empty();
        q = 0;
        if take == '1' {
            q = fifo.pop();
        }
    }
}

#[test]
entity MethodsTest {}

impl MethodsTest {
    let clk: Bit;
    let valid: Bit;
    let data: unsigned[8];
    let take: Bit;
    let q: unsigned[8];
    let empty: Bit;
    let pipe: Pipe = { .clk = clk, .valid = valid, .data = data, .take = take,
                       .q = q, .empty = empty };

    clock: process { clk = not clk after 5ns; }

    check: process {
        await 1ns;
        assert!(empty == '1', "starts empty");
        valid = '1'; data = 11;
        await clk.rising(); await 1ns;
        data = 22;
        await clk.rising(); await 1ns;
        valid = '0';
        assert!(empty == '0', "two pushed");
        take = '1';
        await 1ns;
        assert!(q == 11, "first in, first out");
        await clk.rising(); await 1ns;
        assert!(q == 22, "then the second");
        await clk.rising(); await 1ns;
        take = '0';
        await 1ns;
        assert!(empty == '1', "empty again");
        print!("entity methods done");
    }
}
"#;

const BAD: &str = r#"module probe;

// The child: state in its own clocked process, like a vendor FIFO; the
// methods are strobes and answers.
entity Fifo { clk: Bit in }

impl Fifo {
    let buf: unsigned[8][4];
    let wp: unsigned[2];
    let rp: unsigned[2];
    let count: unsigned[3];
    let wr_en: Bit;
    let din: unsigned[8];
    let rd_en: Bit;

    store: process {
        if clk.rising() {
            if wr_en == '1' and count != 4 {
                buf[wp] = din;
                wp = wp + 1;
            }
            if rd_en == '1' and count != 0 {
                rp = rp + 1;
            }
            if wr_en == '1' and rd_en == '0' and count != 4 { count = count + 1; }
            if rd_en == '1' and wr_en == '0' and count != 0 { count = count - 1; }
        }
    }

    pub fn is_empty() -> Bit { return if count == 0 { '1' } else { '0' }; }
    pub fn push(v: unsigned[8]) { wr_en = '1'; din = v; }
    pub fn pop() -> unsigned[8] { rd_en = '1'; return buf[rp]; }
}


entity Twice { clk: Bit in, a: Bit in, b: Bit in, x: unsigned[8] in }
impl Twice {
    let fifo: Fifo = { .clk = clk };
    one: process { if a == '1' { fifo.push(x); } }
    two: process { if b == '1' { fifo.push(x); } }
}
entity Looped { clk: Bit in, x: unsigned[8] in }
impl Looped {
    let fifo: Fifo = { .clk = clk };
    p: process { for i in 0..1 { fifo.push(x); } }
}
#[test]
entity Tb {}
impl Tb {
    let clk: Bit;
    let fifo: Fifo = { .clk = clk };
    check: process { fifo.push(1); }
}
"#;

const XPM: &str = r#"module probe;

extern entity XpmFifoSync<
    FIFO_WRITE_DEPTH: integer, WRITE_DATA_WIDTH: integer, READ_DATA_WIDTH: integer,
    FIFO_READ_LATENCY: integer,
> {
    dout: Logic[READ_DATA_WIDTH] out,
    empty: Logic out,
    full: Logic out,
    din: Logic[WRITE_DATA_WIDTH] in,
    rd_en: Logic in,
    rst: Logic in,
    sleep: Logic in,
    wr_clk: Logic in,
    wr_en: Logic in,
}
attr library for XpmFifoSync = "xpm";
attr name for XpmFifoSync = "xpm_fifo_sync";

pub entity Fifo<W: integer, DEPTH: integer> { clk: Logic in, rst: Logic in }

impl<W: integer, DEPTH: integer> Fifo<W, DEPTH> {
    let wr_en: Logic;
    let din: Logic[W];
    let rd_en: Logic;
    let dout: Logic[W];
    let full: Logic;
    let empty: Logic;
    let ip: XpmFifoSync<FIFO_WRITE_DEPTH = DEPTH, WRITE_DATA_WIDTH = W, READ_DATA_WIDTH = W,
                        FIFO_READ_LATENCY = 0> = {
        .wr_clk = clk, .rst = rst, .sleep = '0',
        .wr_en = wr_en, .din = din, .rd_en = rd_en, .dout = dout,
        .full = full, .empty = empty,
    };

    pub fn is_full() -> Logic { return full; }
    pub fn push(v: Logic[W]) { wr_en = '1'; din = v; }
    pub fn pop() -> Logic[W] { rd_en = '1'; return dout; }
}

pub entity Producer { clk: Logic in, rst: Logic in, valid: Bit in, data: Logic[8] in, q: Logic[8] out }
impl Producer {
    let fifo: Fifo<W = 8, DEPTH = 32> = { .clk = clk, .rst = rst };
    feed: process { if valid == '1' and fifo.is_full() == '0' { fifo.push(data); } }
    drain: process { q = fifo.pop(); }
}
"#;

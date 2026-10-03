//! The same source must compile to the same IR and diagnostics every time.

use siox::compiler::{Artifact, CompileRequest, Compiler, Emit, SourceInput};

/// One design touching each place that used to follow a `HashMap`'s per-map
/// hash seed:
/// - reading a metavalue-carrying slice into a `Logic` builds a
///   discriminant-membership chain (`m == 0 or m == 1`), in set order;
/// - a struct literal connected to a struct-typed port drives one leaf per
///   field, in port-map order;
/// - an unused struct `let` warns once per field, in local-map order;
/// - two testbench instances sharing a struct local are linked leaf by leaf,
///   in binding-map order;
/// - a struct literal connected to a testbench instance's port is flattened
///   in port-map order.
const DESIGN: &str = "module det;
use std::bits::unsigned;

struct Inner { pub lo: unsigned[8], pub hi: unsigned[8] }
entity Pick { o: Inner in, y: unsigned[8] out }
impl Pick {
    y = o.hi;
    let pair: Inner;
    pair.lo = 1;
    pair.hi = 2;
}
entity Top { mvx: Logic out, y: unsigned[8] out }
impl Top {
    let meta: unsigned[4] = \"1X10\";
    mvx = meta[2..2];
    let inner: Pick = { .o = { .lo = 11, .hi = 22 }, .y = y };
}
struct Link { pub req: unsigned[8], pub ack: unsigned[8] }
view Up for Link { req out, ack in }
view Down for Link { req in, ack out }
entity Src { bus: Link Up }
impl Src { bus.req = 5; }
entity Dst { bus: Link Down, seen: unsigned[8] out }
impl Dst { bus.ack = 6; seen = bus.req; }
#[test]
entity T {}
impl T {
    let l: Link;
    let seen: unsigned[8];
    let s: Src = { .bus = l };
    let d: Dst = { .bus = l, .seen = seen };
    let tb_pick: Pick = { .o = { .lo = 3, .hi = 4 } };
    await 1ns;
}
";

/// IR text plus every diagnostic, as one comparable string.
fn compile_once(entry: &std::path::Path) -> String {
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(SourceInput::path(entry), Emit::Ir));
    let ir = match &compilation.artifact {
        Some(Artifact::Text(text)) => text.clone(),
        other => panic!("no IR: {other:?}\n{}", compilation.render_diagnostics()),
    };
    format!(
        "{ir}\n--- diagnostics ---\n{}",
        compilation.render_diagnostics()
    )
}

/// Every map in the compiler gets a fresh hash seed, so repeated compiles in
/// one process explore different iteration orders. Before this was fixed,
/// the design above compiled to several different IR texts and warning
/// orders; 17 of the 185 corpus programs did too.
#[test]
fn a_design_compiles_to_identical_output_every_time() {
    let dir = std::env::temp_dir().join(format!("siox_determinism_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("det.siox");
    std::fs::write(&entry, DESIGN).unwrap();

    let first = compile_once(&entry);
    // The design must really exercise all three paths.
    assert!(first.contains("mvx"), "{first}");
    assert!(first.contains("inner.o.lo"), "{first}");
    assert!(first.contains("never read"), "{first}");
    assert!(first.contains("driver T.d.bus.req = %v"), "{first}");
    assert!(first.contains("driver T.tb_pick.o.lo = %v"), "{first}");
    assert!(first.contains("Number(Integer([3]))"), "{first}");
    for run in 1..20 {
        let next = compile_once(&entry);
        assert!(
            next == first,
            "compile {run} differed from the first:\n--- first ---\n{first}\n--- run {run} ---\n{next}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

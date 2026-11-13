//! Regression for sioxc #631: an operator impl whose body applies another
//! operator to its operands' fields (`self.start + rhs.start` inside
//! `Line`'s `Add`) lowers in hardware. The inner field had no type there,
//! so `Point`'s impl was never found ("no compatible aggregate value form").

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn nested_operator_impls_lower_in_hardware() {
    let dir = std::env::temp_dir().join(format!("siox_i631_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("nested.siox");
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

const SOURCE: &str = r#"module test_nested_operator;

pub struct Point {
    pub x: integer,
    pub y: integer,
}

impl Add<Point, Point> for Point {
    fn add(rhs: Point) -> Point {
        return Point { .x = self.x + rhs.x, .y = self.y + rhs.y };
    }
}

pub struct Line {
    pub start: Point,
    pub end: Point,
}

impl Add<Line, Line> for Line {
    fn add(rhs: Line) -> Line {
        // Line::add invokes Point::add on self.start + rhs.start:
        return Line {
            .start = self.start + rhs.start,
            .end = self.end + rhs.end,
        };
    }
}

entity Top {
    line_a: Line in,
    line_b: Line in,
    line_c: Line out,
}

impl Top {
    line_c = line_a + line_b;
}

#[test] entity T {}
impl T {
    let a: Line = Line {
        .start = Point { .x = 1, .y = 2 },
        .end = Point { .x = 3, .y = 4 },
    };
    let b: Line = Line {
        .start = Point { .x = 10, .y = 20 },
        .end = Point { .x = 30, .y = 40 },
    };
    let c: Line;
    let top: Top = { .line_a = a, .line_b = b, .line_c = c };
    process {
        await 1ns;
        print!("c.start.x = {}\n", c.start.x);
        print!("c.end.y = {}\n", c.end.y);
        assert!(c.start.x == 11 and c.end.y == 44);
        print!("done\n");
    }
}
"#;

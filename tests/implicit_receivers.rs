//! An implementation or trait member without `static` has an implicit
//! receiver, as C++'s `this`; `static fn` has none. The old written `self`
//! parameter is an error that says how to migrate, and a member that lost
//! its meaning in the change fails loudly instead of silently gaining one.

use std::process::Command;

fn check(name: &str, source: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_receivers_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--emit",
            "metadata",
        ])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

#[test]
fn a_member_uses_its_implicit_receiver() {
    let (ok, text) = check(
        "implicit",
        "module m;\n\
         struct Counter { pub value: integer }\n\
         impl Counter {\n\
             pub fn doubled() -> integer { return self.value * 2; }\n\
             pub fn plus(n: integer) -> integer { return self.value + n; }\n\
             pub static fn at(value: integer) -> Counter { return Counter { .value = value }; }\n\
         }\n\
         entity E { y: integer out }\n\
         impl E { let c: Counter = Counter::at(3); y = c.doubled() + c.plus(1); }\n",
    );
    assert!(ok, "{text}");
}

/// The old spelling still compiles, with a warning that says what to drop.
#[test]
fn a_written_self_says_how_to_migrate() {
    let (ok, text) = check(
        "written",
        "module m;\nstruct P { pub x: integer }\nimpl P { pub fn get(self) -> integer { return self.x; } }\n",
    );
    assert!(ok, "{text}");
    assert!(text.contains("warning"), "{text}");
    assert!(
        text.contains("the receiver is implicit: remove the `self` parameter"),
        "{text}"
    );
}

#[test]
fn an_unmarked_constructor_fails_loudly() {
    let (ok, text) = check(
        "unmarked",
        "module m;\nstruct P { pub x: integer }\n\
         impl P { pub fn at(x: integer) -> P { return P { .x = x }; } }\n\
         entity E { y: integer out }\nimpl E { let p: P = P::at(1); y = p.x; }\n",
    );
    assert!(!ok, "{text}");
    assert!(text.contains("has a receiver"), "{text}");
    assert!(text.contains("`static fn at`"), "{text}");
}

#[test]
fn a_static_trait_member_stays_static() {
    let (ok, text) = check(
        "trait",
        "module m;\nstruct K { pub k: integer }\n\
         impl Default for K { fn default() -> K { return K { .k = 7 }; } }\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("is `static`, and this impl's has a receiver"),
        "{text}"
    );
}

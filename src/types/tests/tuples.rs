//! Tuples: types, values, positions, comparison, patterns and destructuring.

use super::*;

const PAIR: &str = "module m;\n\
    fn split(x: unsigned[8]) -> (unsigned[8], Bit) { return (x, '1'); }\n";

#[test]
/// Tuple values, positions, returns and comparisons are accepted.
fn tuples_check() {
    let src = format!(
        "{PAIR}fn f(x: unsigned[8]) -> Bool {{\n\
         let p: (unsigned[8], Bit) = split(x);\n\
         let q: ((unsigned[8], Bit), Bit) = (p, p.1);\n\
         let (a: unsigned[8], b: Bit) = p;\n\
         (a, _) = q.0;\n\
         return p == (x, '1') and q.0 != p;\n\
         }}\n"
    );
    assert_eq!(check_src(&src), 0);
}

#[test]
/// A position past the end is an error, as is an operator other than
/// `==`/`!=`, and comparing tuples of different sizes.
fn tuple_misuse_is_rejected() {
    let src = format!(
        "{PAIR}fn f(x: unsigned[8]) -> Bool {{\n\
         let p: (unsigned[8], Bit) = split(x);\n\
         let b: Bit = p.2;\n\
         let l: Bool = p < p;\n\
         return p == (x, '1', '0');\n\
         }}\n"
    );
    assert_eq!(check_src(&src), 3);
}

#[test]
/// A tuple assignment is checked position by position, widths included.
fn tuple_assignment_is_checked_per_position() {
    let src = format!(
        "{PAIR}fn f(x: unsigned[8]) {{\n\
         let p: (unsigned[4], Bit) = split(x);\n\
         }}\n"
    );
    assert_eq!(check_src(&src), 1);
}

#[test]
/// Every name a tuple `let` declares needs a type.
fn tuple_let_names_need_types() {
    let src = format!(
        "{PAIR}fn f(x: unsigned[8]) {{\n\
         let (a, b) = split(x);\n\
         }}\n"
    );
    assert_eq!(check_src(&src), 2);
}

#[test]
/// A tuple is matched by tuple patterns of its own size.
fn tuple_patterns_must_fit() {
    let src = format!(
        "{PAIR}fn f(x: unsigned[8]) -> Bit {{\n\
         match split(x) {{\n\
         (_, '1', _) => {{ return '1'; }}\n\
         '0' => {{ return '0'; }}\n\
         _ => {{ return '0'; }}\n\
         }}\n\
         }}\n"
    );
    assert_eq!(check_src(&src), 2);
}

#[test]
/// Exhaustiveness is judged over the enum positions' combinations: a
/// missing one warns, a full set needs no `_`.
fn tuple_match_exhaustiveness() {
    let missing = "module m;\n\
        fn f(a: Bit, b: Bool) -> Bit {\n\
        match (a, b) {\n\
        ('0', _) => { return '0'; }\n\
        ('1', Bool::true) => { return '1'; }\n\
        }\n\
        return '0';\n\
        }\n";
    assert_eq!(warnings(missing, "W-P007"), 1);
    let covered = "module m;\n\
        fn f(a: Bit, b: Bool) -> Bit {\n\
        match (a, b) {\n\
        ('0', _) => { return '0'; }\n\
        ('1', Bool::true) => { return '1'; }\n\
        ('1', Bool::false) => { return '0'; }\n\
        }\n\
        }\n";
    assert_eq!(warnings(covered, "W-P007"), 0);
    assert_eq!(check_src(covered), 0);
}

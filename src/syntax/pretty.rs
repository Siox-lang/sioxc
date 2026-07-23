//! Pretty-printer: AST -> canonical siox source.
//!
//! Spec Stage 2 acceptance: "Pretty-printer round-trips simple examples."
//! Also backs `siox ast` debug output (Stage 12).
//!
//! The output is deterministic and re-parses to an equivalent AST. It is not a
//! byte-for-byte reproduction of the input: comments are dropped, redundant
//! parentheses are normalized away (and precedence-required ones reinserted),
//! and `match` arms / imports use a single canonical form.

use crate::syntax::ast::*;

/// Render a module back to canonical source text.
pub fn print_module(module: &Module) -> String {
    let mut p = Printer { out: String::new(), indent: 0 };
    p.module(module);
    p.out
}

const INDENT: &str = "    ";

struct Printer {
    out: String,
    indent: usize,
}

impl Printer {
    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    // --- items --------------------------------------------------------------

    fn module(&mut self, m: &Module) {
        self.line(&format!("module {};", path(&m.path)));
        for item in &m.items {
            self.blank();
            self.item(item);
        }
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Using(u) => self.using(u),
            Item::Const(c) => self.const_decl(c),
            Item::Fn(f) => self.fn_decl(f),
            Item::ExternBlock { abi, fns, .. } => {
                self.line(&format!("extern \"{abi}\" {{"));
                self.indent += 1;
                for f in fns {
                    self.fn_decl(f);
                }
                self.indent -= 1;
                self.line("}");
            }
            Item::Struct(s) => self.struct_decl(s),
            Item::Enum(e) => self.enum_decl(e),
            Item::Entity(e) => self.entity(e),
            Item::Impl(i) => self.impl_decl(i),
            Item::Trait(t) => self.trait_decl(t),
            Item::AttrDecl(a) => self.attr_decl(a),
        }
    }

    fn using(&mut self, u: &Using) {
        let s = match &u.kind {
            UsingKind::Import { base, names } => {
                let names =
                    names.iter().map(|n| trait_name_str(&n.text)).collect::<Vec<_>>().join(", ");
                if base.segments.is_empty() {
                    format!("using {names};")
                } else {
                    format!("using {}::{{{names}}};", path(base))
                }
            }
            UsingKind::Alias { name, ty } => format!("using {} = {};", name.text, type_str(ty)),
        };
        self.line(&s);
    }

    fn const_decl(&mut self, c: &ConstDecl) {
        let kw = pub_kw(c.is_pub);
        self.line(&format!(
            "{kw}const {}: {} = {};",
            c.name.text,
            type_str(&c.ty),
            expr(&c.value)
        ));
    }

    fn struct_decl(&mut self, s: &StructDecl) {
        let kw = pub_kw(s.is_pub);
        let base = match &s.base {
            Some(t) => format!(" : {}", type_str(t)),
            None => String::new(),
        };
        // Bodyless newtype: `struct B : A;`.
        if s.base.is_some() && s.fields.is_empty() {
            self.line(&format!("{kw}struct {}{}{base};", s.name.text, params(&s.params)));
            return;
        }
        self.line(&format!("{kw}struct {}{}{base} {{", s.name.text, params(&s.params)));
        self.indent += 1;
        for f in &s.fields {
            self.line(&format!("{}: {},", f.name.text, type_str(&f.ty)));
        }
        self.indent -= 1;
        self.line("}");
    }

    fn enum_decl(&mut self, e: &EnumDecl) {
        let kw = pub_kw(e.is_pub);
        let repr = match &e.repr {
            Some(t) => format!(": {}", type_str(t)),
            None => String::new(),
        };
        if e.repr.is_some() && e.variants.is_empty() {
            self.line(&format!("{kw}enum {}{repr};", e.name.text));
            return;
        }
        self.line(&format!("{kw}enum {}{repr} {{", e.name.text));
        self.indent += 1;
        for v in &e.variants {
            match &v.value {
                Some(val) => self.line(&format!("{} = {},", v.name.text, expr(val))),
                None => self.line(&format!("{},", v.name.text)),
            }
        }
        self.indent -= 1;
        self.line("}");
    }

    fn entity(&mut self, e: &EntityDecl) {
        for a in &e.attrs {
            self.line(&attr(a));
        }
        let mut head = String::new();
        head.push_str(pub_kw(e.is_pub));
        if e.is_extern {
            head.push_str("extern ");
        }
        head.push_str(&format!("entity {}{} {{", e.name.text, params(&e.params)));
        self.line(&head);
        self.indent += 1;
        for port in &e.ports {
            let dir = match port.dir {
                Some(d) => format!("{} ", dir_str(d)),
                None => String::new(),
            };
            self.line(&format!("{dir}{}: {};", port.name.text, type_str(&port.ty)));
        }
        self.indent -= 1;
        self.line("}");
    }

    fn impl_decl(&mut self, i: &ImplDecl) {
        for a in &i.attrs {
            let value = a
                .value
                .as_ref()
                .map(|v| format!(" = {}", expr(v)))
                .unwrap_or_default();
            self.line(&format!("#[{}{value}]", path(&a.name)));
        }
        let target = type_str(&i.target);
        let head = match &i.trait_ {
            Some(tr) => {
                let name = match tr.segments.as_slice() {
                    [seg] => trait_name_str(&seg.text),
                    _ => path(tr),
                };
                let args = if i.trait_args.is_empty() {
                    String::new()
                } else {
                    let list =
                        i.trait_args.iter().map(generic_arg).collect::<Vec<_>>().join(", ");
                    format!("<{list}>")
                };
                format!("impl {name}{args}{} for {} {{", params(&i.params), target)
            }
            None => format!("impl {}{} {{", target, params(&i.params)),
        };
        self.line(&head);
        self.indent += 1;
        for item in &i.items {
            self.impl_item(item);
        }
        self.indent -= 1;
        self.line("}");
    }

    fn impl_item(&mut self, item: &ImplItem) {
        match item {
            ImplItem::Const(c) => self.const_decl(c),
            ImplItem::Let(l) => self.line(&format!("{};", let_decl(l))),
            ImplItem::Fn(f) => self.fn_decl(f),
            ImplItem::ModeField { dir, name, .. } => {
                self.line(&format!("{} {};", dir_str(*dir), name.text));
            }
            ImplItem::Stmt(s) => self.stmt(s),
        }
    }

    fn trait_decl(&mut self, t: &TraitDecl) {
        let kw = pub_kw(t.is_pub);
        self.line(&format!("{kw}trait {}{} {{", trait_name_str(&t.name.text), params(&t.params)));
        self.indent += 1;
        for f in &t.items {
            self.fn_decl(f);
        }
        self.indent -= 1;
        self.line("}");
    }

    fn attr_decl(&mut self, a: &AttrDecl) {
        let kw = pub_kw(a.is_pub);
        let targets = a.targets.iter().map(|t| t.text.clone()).collect::<Vec<_>>().join(", ");
        self.line(&format!("{kw}attr {}: {} for {targets};", a.name.text, type_str(&a.ty)));
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        let ps = f.params.iter().map(fn_param).collect::<Vec<_>>().join(", ");
        let ret = match &f.ret {
            Some(t) => format!(" -> {}", type_str(t)),
            None => String::new(),
        };
        match &f.body {
            Some(body) => {
                self.line(&format!("fn {}{}({ps}){ret} {{", f.name.text, params(&f.generics)));
                self.indent += 1;
                for s in &body.stmts {
                    self.stmt(s);
                }
                self.indent -= 1;
                self.line("}");
            }
            None => self.line(&format!("fn {}{}({ps}){ret};", f.name.text, params(&f.generics))),
        }
    }

    // --- statements ---------------------------------------------------------

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l) => self.line(&format!("{};", let_decl(l))),
            Stmt::Assign { target, value, after, .. } => {
                let delay = after.as_ref().map(|d| format!(" after {}", expr(d))).unwrap_or_default();
                self.line(&format!("{} = {}{delay};", expr(target), expr(value)));
            }
            Stmt::If(i) => self.if_stmt(i),
            Stmt::Match(m) => self.match_stmt(m),
            Stmt::For { var, range, body, .. } => {
                self.line(&format!("for {} in {} {{", var.text, expr(range)));
                self.block_body(body);
                self.line("}");
            }
            Stmt::Expr(e) => self.line(&format!("{};", expr(e))),
            Stmt::Return { value, .. } => match value {
                Some(v) => self.line(&format!("return {};", expr(v))),
                None => self.line("return;"),
            },
        }
    }

    fn if_stmt(&mut self, i: &IfStmt) {
        self.if_chain("if", i);
    }

    /// Render an if/else-if chain flat, e.g. `if a { } else if b { } else { }`.
    /// `head` is `"if"` for the first link and `"} else if"` for continuations.
    fn if_chain(&mut self, head: &str, i: &IfStmt) {
        self.line(&format!("{head} {} {{", expr(&i.cond)));
        self.block_body(&i.then);
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                self.line("} else {");
                self.block_body(b);
                self.line("}");
            }
            Some(ElseBranch::If(inner)) => self.if_chain("} else if", inner),
            None => self.line("}"),
        }
    }

    fn match_stmt(&mut self, m: &MatchStmt) {
        self.line(&format!("match {} {{", expr(&m.scrutinee)));
        self.indent += 1;
        for arm in &m.arms {
            self.line(&format!("{} => {{", pattern(&arm.pattern)));
            self.block_body(&arm.body);
            self.line("}");
        }
        self.indent -= 1;
        self.line("}");
    }

    fn block_body(&mut self, b: &Block) {
        self.indent += 1;
        for s in &b.stmts {
            self.stmt(s);
        }
        self.indent -= 1;
    }
}

// --- leaf renderers (pure) --------------------------------------------------

fn pub_kw(is_pub: bool) -> &'static str {
    if is_pub {
        "pub "
    } else {
        ""
    }
}

fn dir_str(d: Direction) -> &'static str {
    match d {
        Direction::In => "in",
        Direction::Out => "out",
        Direction::Inout => "inout",
    }
}

fn path(p: &Path) -> String {
    p.segments.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join("::")
}

/// Operator traits (`trait "+"`) print their name quoted.
fn trait_name_str(name: &str) -> String {
    let is_ident = name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
    if is_ident {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

fn params(p: &Params) -> String {
    if p.params.is_empty() {
        return String::new();
    }
    let inner = p
        .params
        .iter()
        .map(|param| match &param.bound {
            Some(b) => format!("{}: {}", param.name.text, type_str(b)),
            None => param.name.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{inner}>")
}

fn fn_param(p: &FnParam) -> String {
    if p.is_self {
        return "self".to_string();
    }
    let name = p.name.as_ref().map(|n| n.text.clone()).unwrap_or_default();
    match &p.ty {
        Some(t) => format!("{name}: {}", type_str(t)),
        None => name,
    }
}

fn let_decl(l: &LetDecl) -> String {
    let mut s = String::new();
    for a in &l.attrs {
        s.push_str(&attr(a));
        s.push(' ');
    }
    s.push_str(&format!("let {}", l.name.text));
    if let Some(t) = &l.ty {
        s.push_str(&format!(": {}", type_str(t)));
    }
    if let Some(v) = &l.value {
        s.push_str(&format!(" = {}", expr(v)));
    }
    s
}

fn attr(a: &Attr) -> String {
    match &a.value {
        Some(v) => format!("#[{} = {}]", path(&a.name), expr(v)),
        None => format!("#[{}]", path(&a.name)),
    }
}

fn pattern(p: &Pattern) -> String {
    match p {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Path(p) => path(p),
        Pattern::BitPattern { text, .. } => text.clone(),
        Pattern::Or { alts, .. } => {
            alts.iter().map(pattern).collect::<Vec<_>>().join(" | ")
        }
        Pattern::Range { lo, hi, .. } if lo == hi => lo.to_string(),
        Pattern::Range { lo, hi, .. } => format!("{lo}..{hi}"),
    }
}

fn generic_arg(a: &GenericArg) -> String {
    match a {
        GenericArg::Positional(e) => expr(e),
        GenericArg::Named { name, value } => format!("{} = {}", name.text, expr(value)),
    }
}

/// Render a [`Type`] as canonical source. Public so tooling (e.g. the CLI's
/// pipeline trace) can name a type without re-walking the AST.
pub fn type_str(t: &Type) -> String {
    match t {
        Type::Path(p) => path(p),
        Type::Indexed { base, index, .. } => match index {
            Some(i) => format!("{}[{}]", type_str(base), expr(i)),
            None => format!("{}[]", type_str(base)),
        },
        Type::Generic { base, args, .. } => {
            let inner = args.iter().map(generic_arg).collect::<Vec<_>>().join(", ");
            format!("{}<{inner}>", type_str(base))
        }
        Type::Mode { dir, inner, mode, .. } => {
            let m = match mode {
                Some(name) => format!("::{}", name.text),
                None => String::new(),
            };
            format!("{} {}{m}", dir_str(*dir), type_str(inner))
        }
    }
}

fn un_op(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "not ",
    }
}

/// The source string of a binary operator (also the operator-trait name).
pub fn bin_op(op: &BinOp) -> &str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Custom { symbol, .. } => symbol,
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
    }
}

/// Binding power mirroring the parser, used to decide where parentheses are
/// required. Higher binds tighter; atoms/postfix are effectively infinite.
fn bin_prec(op: &BinOp) -> u8 {
    match op {
        BinOp::Mul | BinOp::Div => 90,
        BinOp::Add | BinOp::Sub => 80,
        BinOp::Shl | BinOp::Shr => 70,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 60,
        BinOp::Eq | BinOp::Ne => 50,
        BinOp::And => 40,
        BinOp::Or => 30,
        BinOp::Custom { precedence, .. } => *precedence,
    }
}

const POSTFIX_PREC: u8 = 200;
const UNARY_PREC: u8 = 100;
const RANGE_PREC: u8 = 1;

fn expr(e: &Expr) -> String {
    expr_prec(e, 0)
}

/// Render one expression (for other crates, e.g. attribute values in `tree`).
pub fn expr_string(e: &Expr) -> String {
    expr(e)
}

/// Render `e`, wrapping it in parentheses if its own precedence is below
/// `parent` (so the re-parsed tree keeps the same shape).
fn expr_prec(e: &Expr, parent: u8) -> String {
    let (s, prec) = expr_inner(e);
    if prec < parent {
        format!("({s})")
    } else {
        s
    }
}

fn expr_inner(e: &Expr) -> (String, u8) {
    match e {
        Expr::Int { text, .. } => (text.clone(), u8::MAX),
        Expr::SuffixLit { text, suffix, .. } => (format!("{text}{}", suffix.text), u8::MAX),
        Expr::BitStrLit { base, digits, .. } => (format!("{base}\"{digits}\""), u8::MAX),
        Expr::CharLit { ch, .. } => (format!("'{ch}'"), u8::MAX),
        Expr::StrLit { text, .. } => (format!("\"{text}\""), u8::MAX),
        // `true`/`false` desugar to `Bool::true`/`Bool::false`; print them back
        // in their surface form so source round-trips.
        Expr::Path(p)
            if p.segments.len() == 2
                && p.segments[0].text == "Bool"
                && matches!(p.segments[1].text.as_str(), "true" | "false") =>
        {
            (p.segments[1].text.clone(), u8::MAX)
        }
        Expr::Path(p) => (path(p), u8::MAX),
        Expr::Field { base, field, .. } => {
            (format!("{}.{}", expr_prec(base, POSTFIX_PREC), field.text), POSTFIX_PREC)
        }
        Expr::SysAttr { base, attr, .. } => {
            (format!("{}::{}", expr_prec(base, POSTFIX_PREC), attr.text), POSTFIX_PREC)
        }
        Expr::Index { base, index, .. } => {
            (format!("{}[{}]", expr_prec(base, POSTFIX_PREC), expr(index)), POSTFIX_PREC)
        }
        Expr::Range { lo, hi, .. } => (
            format!("{}..{}", expr_prec(lo, RANGE_PREC + 1), expr_prec(hi, RANGE_PREC + 1)),
            RANGE_PREC,
        ),
        Expr::IfExpr { cond, then, els, .. } => {
            // An IfExpr in `els` prints as an `else if` chain.
            let e = match els.as_ref() {
                Expr::IfExpr { .. } => format!("else {}", expr(els)),
                _ => format!("else {{ {} }}", expr(els)),
            };
            (format!("if {} {{ {} }} {}", expr(cond), expr(then), e), 0)
        }
        Expr::Match { scrutinee, arms, .. } => {
            let arms_str = arms
                .iter()
                .map(|a| {
                    let val = match a.body.stmts.as_slice() {
                        [Stmt::Expr(e)] => expr(e),
                        [Stmt::Return { value: Some(e), .. }] => expr(e),
                        _ => "{ .. }".to_string(),
                    };
                    format!("{} => {}", pattern(&a.pattern), val)
                })
                .collect::<Vec<_>>()
                .join(", ");
            (format!("match {} {{ {} }}", expr(scrutinee), arms_str), 0)
        }
        Expr::Unary { op, rhs, .. } => {
            (format!("{}{}", un_op(*op), expr_prec(rhs, UNARY_PREC)), UNARY_PREC)
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let p = bin_prec(op);
            (
                format!("{} {} {}", expr_prec(lhs, p), bin_op(op), expr_prec(rhs, p + 1)),
                p,
            )
        }
        Expr::Call { callee, args, bang, .. } => {
            let a = args.iter().map(expr).collect::<Vec<_>>().join(", ");
            let b = if *bang { "!" } else { "" };
            (format!("{}{b}({a})", expr_prec(callee, POSTFIX_PREC)), POSTFIX_PREC)
        }
        Expr::Construct { ty, args, spread, .. } => {
            // A leading `..base` spread, then the explicit/positional args.
            let spread_part = spread.iter().map(|b| format!("..{}", expr(b)));
            let a = spread_part
                .chain(args.iter().map(|c| match (&c.field, &c.value) {
                    // Explicit `.field = value`.
                    (Some(f), Some(v)) => format!(".{} = {}", f.text, expr(v)),
                    (Some(f), None) => format!(".{}", f.text),
                    // Positional `value`.
                    (None, Some(v)) => expr(v),
                    (None, None) => String::new(),
                }))
                .collect::<Vec<_>>()
                .join(", ");
            match ty {
                Some(ty) => (format!("{} {{ {a} }}", type_str(ty)), POSTFIX_PREC),
                None => (format!("{{ {a} }}"), POSTFIX_PREC),
            }
        }
        Expr::Concat { parts, .. } => {
            let p = parts.iter().map(expr).collect::<Vec<_>>().join(", ");
            (format!("{{ {p} }}"), POSTFIX_PREC)
        }
        Expr::Array { elems, .. } => {
            let p = elems.iter().map(expr).collect::<Vec<_>>().join(", ");
            (format!("[{p}]"), POSTFIX_PREC)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};

    fn roundtrip(src: &str) {
        let mut sink = DiagnosticSink::new();
        let m1 = crate::syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source did not parse:\n{src}");

        let printed = print_module(&m1);
        let mut sink2 = DiagnosticSink::new();
        let m2 = crate::syntax::parse_module(FileId(0), &printed, &mut sink2);
        assert_eq!(
            sink2.error_count(),
            0,
            "pretty-printed output did not re-parse:\n{printed}"
        );
        assert_eq!(m1.items.len(), m2.items.len(), "item count changed:\n{printed}");

        // Printing must be idempotent: print(parse(print(x))) == print(x).
        let printed2 = print_module(&m2);
        assert_eq!(printed, printed2, "pretty-printing is not idempotent");
    }

    #[test]
    fn roundtrips_generic_fn() {
        roundtrip(
            "module m;\nfn maxi<T: Ord>(a: T, b: T) -> T {\n    if a > b {\n        return a;\n    }\n    return b;\n}\n",
        );
    }

    #[test]
    fn where_clause_desugars_to_inline_bounds() {
        // `where T: Ord` parses to the same AST as `<T: Ord>`; printing is
        // canonical (inline) and re-parses identically.
        let mut sink = DiagnosticSink::new();
        let m = crate::syntax::parse_module(
            FileId(0),
            "module m;\nfn f<T>(a: T) -> T\nwhere\n    T: Ord,\n{\n    return a;\n}\n",
            &mut sink,
        );
        assert_eq!(sink.error_count(), 0);
        let printed = print_module(&m);
        assert!(printed.contains("fn f<T: Ord>(a: T)"), "where should print inline:\n{printed}");
    }

    #[test]
    fn roundtrips_derived_types() {
        roundtrip(
            "module m;\n\
             enum Bit { '0', '1' }\n\
             enum ULogic : Bit { 'Z', 'X' }\n\
             enum Logic : ULogic;\n\
             struct Header { valid: Bit }\n\
             struct Packet : Header { data: uint[8] }\n\
             struct Word : Bit[];\n",
        );
    }

    #[test]
    fn roundtrips_trait_type_args() {
        roundtrip(
            "module m;\nstruct C { re: real }\nimpl Add<integer> for C {\n    fn add(self, rhs: integer) -> C {\n        return self;\n    }\n}\n",
        );
    }

    #[test]
    fn roundtrips_if_expressions() {
        roundtrip(
            "module m;\nimpl E {\n    let y: Bit = if sel { a } else { b };\n    z = if x > 200 { 200 } else if x < 10 { 10 } else { x };\n}\n",
        );
    }

    #[test]
    fn roundtrips_unconstrained_arrays_and_char() {
        roundtrip(
            "module std::text;\n\
             pub using string = Char[];\n\
             entity E {\n\
               in s: string[5];\n\
               in c: Char;\n\
             }\n",
        );
    }

    #[test]
    fn roundtrips_operator_traits() {
        roundtrip(
            "module m;\n\
             pub trait Add2 {\n\
               fn apply(self, rhs: Self) -> Self;\n\
             }\n\
             struct V { x: Bit }\n\
             impl Add2 for V {\n\
               fn apply(self, rhs: V) -> V {\n\
                 return self;\n\
               }\n\
             }\n",
        );
    }

    #[test]
    fn roundtrips_suffix_and_bitstring_literals() {
        roundtrip(
            "module m;\n\
             entity E { out y: uint[8]; }\n\
             impl E {\n\
               let t = 10ns;\n\
               let f = 100MHz;\n\
               let c = 5i;\n\
               y = x\"AB\";\n\
               await 1ns;\n\
             }\n",
        );
    }

    #[test]
    fn roundtrips_logic_literal_enum_variants() {
        roundtrip(
            "module std::logic;\n\
             pub enum Bit {\n    '0',\n    '1',\n}\n\
             pub enum Bool {\n    false,\n    true,\n}\n",
        );
    }

    #[test]
    fn roundtrips_concat_and_nameless_struct_literal() {
        roundtrip(
            "module m;\n\
             entity E { out y: uint[8]; }\n\
             impl E {\n\
               let p: Packet = { .valid = '1', .data = 5 };\n\
               y = {a, b, c};\n\
             }\n",
        );
    }

    #[test]
    fn roundtrips_a_full_program() {
        roundtrip(
            "module demo::counter;\n\
             using std::logic::{Bit, Logic};\n\
             using Word = uint[32];\n\
             const DEFAULT_WIDTH: usize = 8;\n\
             struct Packet<T> { valid: Bit, data: T }\n\
             enum State: uint[2] { Idle = 0, Start = 1, Done = 2 }\n\
             #[top]\n\
             entity Counter<W: integer> {\n\
               in clk: Bit;\n\
               bus: out Stream<uint[32]>::Source;\n\
               out count: uint[W];\n\
             }\n\
             impl Counter<W: integer> {\n\
               const MAX: uint[W] = (1 << W) - 1;\n\
               let value: uint[W] = 0;\n\
               if clk.rising() {\n\
                 if rst == '1' {\n\
                   value = 0;\n\
                 } else {\n\
                   value = value + 1;\n\
                 }\n\
               }\n\
               count = value;\n\
             }\n",
        );
    }

    #[test]
    fn roundtrips_trait_match_and_construct() {
        roundtrip(
            "module m;\n\
             trait ClockLike { fn rising(self); }\n\
             impl ClockLike for Logic {\n\
               fn rising(self) {\n\
                 return self::event and self::old == '0' and self == '1';\n\
               }\n\
             }\n\
             impl M {\n\
               let dut: Counter<W = 8> = { .clk = clk, .count = c };\n\
               match opcode {\n\
                 State::Idle => { next = State::Start; }\n\
                 _ => op = Op::Nop,\n\
               }\n\
             }\n",
        );
    }

    #[test]
    fn precedence_is_preserved() {
        roundtrip("module m;\nimpl M {\n  y = (a + b) * c;\n  z = a + b * c;\n}\n");
    }

    #[test]
    fn textual_logical_operators_roundtrip() {
        // Custom precedence composes with core and/or; `not` is prefix.
        roundtrip(
            "module m;\n\
             trait custom<S, I, O> { fn apply(self, rhs: I) -> O; }\n\
             #[precedence = 35] impl custom<\"xor\", M, M> for M { fn apply(self, rhs: M) -> M { return self; } }\n\
             #[precedence = 40] impl custom<\"nand\", M, M> for M { fn apply(self, rhs: M) -> M { return self; } }\n\
             #[precedence = 30] impl custom<\"nor\", M, M> for M { fn apply(self, rhs: M) -> M { return self; } }\n\
             impl M {\n  y = a and b or c;\n  z = a xor b and not c;\n  w = a nand b nor c;\n}\n",
        );
    }
}

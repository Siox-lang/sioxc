//! `siox build` — compile a design + its `#[test]` stimulus into a standalone
//! native simulator binary (stage B5.1).
//!
//! The DUT lowers to a native object (`sx_*` C ABI) via the LLVM backend; the
//! testbench statements are translated to a C `main` that drives it. clang
//! links the two into an executable that runs the stimulus and reports
//! PASS/FAIL via its exit code. First cut: integer/logic/bool designs;
//! real/char/string testbenches are a follow-on.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use siox_elab::{Hierarchy, InstanceId};
use siox_ir::{Design, SignalId};
use siox_syntax::ast;
use siox_syntax::Module;

/// Build a native simulator binary for the design's first `#[test]` entity.
pub fn build(modules: &[Module], hier: &Hierarchy, design: &Design, out: &Path) -> Result<(), String> {
    if let Some(s) = design.signals.iter().find(|s| s.width > 64) {
        return Err(format!("signal `{}` is {} bits; siox build is 64-bit only", s.path, s.width));
    }
    let issues = design.validate();
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }

    let test_root = hier
        .roots
        .iter()
        .copied()
        .find(|&r| is_test_entity(modules, &hier.instance(r).entity))
        .ok_or("no #[test] entity to build a simulator from")?;
    let tb_entity = hier.instance(test_root).entity.clone();

    let map = build_map(hier, test_root, design);
    let enums = enum_discriminants(modules);
    let items = test_items(modules, &tb_entity);

    let ctx = Ctx { design, map: &map, enums: &enums };
    let c = ctx.gen_c(&items)?;

    // Emit the DUT object and link a native binary with clang.
    let tmp = std::env::temp_dir().join(format!("siox_build_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let obj = tmp.join("design.o");
    let csrc = tmp.join("sim.c");
    siox_llvm::emit_object(design, &obj)?;
    std::fs::write(&csrc, c).map_err(|e| e.to_string())?;
    let status = Command::new("clang")
        .args([csrc.to_str().unwrap(), obj.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .status()
        .map_err(|e| format!("failed to run clang: {e}"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err("clang failed to link the simulator".into());
    }
    Ok(())
}

/// Translation context: the design, the testbench name -> signal map, and enum
/// discriminants.
struct Ctx<'a> {
    design: &'a Design,
    map: &'a HashMap<String, SignalId>,
    enums: &'a HashMap<String, HashMap<String, u64>>,
}

impl Ctx<'_> {
    fn gen_c(&self, items: &[&ast::ImplItem]) -> Result<String, String> {
        let mut b = String::new();
        b.push_str("#include <stdint.h>\n#include <stdio.h>\n");
        b.push_str("extern void sx_reset(void);\n");
        b.push_str("extern void sx_set(uint32_t, uint64_t);\n");
        b.push_str("extern uint64_t sx_read(uint32_t);\n");
        b.push_str("extern void sx_settle(void);\n\n");
        b.push_str("int main(void) {\n    sx_reset();\n");

        // Initial `let` values, then settle (mirrors the interpreter).
        for item in items {
            if let ast::ImplItem::Let(l) = item {
                match &l.value {
                    Some(ast::Expr::Construct { ty: Some(_), .. }) => {} // instance
                    Some(v) => {
                        if let Some(&id) = self.map.get(&l.name.text) {
                            let e = self.expr(v)?;
                            b.push_str(&format!("    sx_set({}, {e});\n", id.0));
                        }
                    }
                    None => {}
                }
            }
        }
        b.push_str("    sx_settle();\n");

        // Stimulus statements.
        for item in items {
            if let ast::ImplItem::Stmt(s) = item {
                self.stmt(s, &mut b, 1)?;
            }
        }

        b.push_str("    printf(\"PASS\\n\");\n    return 0;\n}\n");
        Ok(b)
    }

    fn stmt(&self, s: &ast::Stmt, b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        match s {
            ast::Stmt::Assign { target, value, .. } => {
                let name = expr_path(target).ok_or("unsupported assignment target")?;
                let id = *self.map.get(&name).ok_or_else(|| format!("unknown signal `{name}`"))?;
                let e = self.expr(value)?;
                b.push_str(&format!("{ind}sx_set({}, {e});\n{ind}sx_settle();\n", id.0));
            }
            ast::Stmt::Expr(ast::Expr::Call { callee, args, bang, .. }) => {
                self.call(callee, args, *bang, b, depth)?;
            }
            ast::Stmt::For { var, range, body, .. } => {
                let (lo, hi) = match range {
                    ast::Expr::Range { lo, hi, .. } => (self.expr(lo)?, self.expr(hi)?),
                    _ => return Err("`for` needs a range".into()),
                };
                let v = &var.text;
                b.push_str(&format!("{ind}for (uint64_t {v} = {lo}; {v} < {hi}; {v}++) {{\n"));
                for s in &body.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                b.push_str(&format!("{ind}}}\n"));
            }
            ast::Stmt::If(iff) => {
                let c = self.expr(&iff.cond)?;
                b.push_str(&format!("{ind}if ({c}) {{\n"));
                for s in &iff.then.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                b.push_str(&format!("{ind}}}\n"));
                if let Some(ast::ElseBranch::Block(block)) = iff.else_.as_deref() {
                    b.push_str(&format!("{ind}else {{\n"));
                    for s in &block.stmts {
                        self.stmt(s, b, depth + 1)?;
                    }
                    b.push_str(&format!("{ind}}}\n"));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        bang: bool,
        b: &mut String,
        depth: usize,
    ) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        let name = match callee {
            ast::Expr::Path(p) => p.segments.first().map(|s| s.text.as_str()).unwrap_or(""),
            _ => "",
        };
        match name {
            "tick" => {
                let clk = args.first().and_then(expr_path).ok_or("tick needs a signal")?;
                let id = self.map.get(&clk).ok_or_else(|| format!("unknown clock `{clk}`"))?.0;
                b.push_str(&format!(
                    "{ind}sx_set({id}, 1); sx_settle();\n{ind}sx_set({id}, 0); sx_settle();\n"
                ));
            }
            "wait" => {
                // No timed behaviour beyond explicit edges: just settle.
                b.push_str(&format!("{ind}sx_settle();\n"));
            }
            "assert" if bang => {
                let cond = args.first().ok_or("assert needs a condition")?;
                let c = self.expr(cond)?;
                let msg = args.get(1).and_then(str_lit).unwrap_or_else(|| "assertion failed".into());
                let msg = msg.replace('\\', "\\\\").replace('"', "\\\"");
                b.push_str(&format!(
                    "{ind}if (!({c})) {{ printf(\"FAIL: {msg}\\n\"); return 1; }}\n"
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Translate a testbench expression to a C expression string.
    fn expr(&self, e: &ast::Expr) -> Result<String, String> {
        Ok(match e {
            ast::Expr::Int { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::SuffixLit { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::Bool { value, .. } => (*value as u64).to_string(),
            ast::Expr::LogicLit { ch, .. } => logic_value(*ch).to_string(),
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let id =
                    self.map.get(&p.segments[0].text).ok_or_else(|| unsup(&p.segments[0].text))?;
                format!("sx_read({})", id.0)
            }
            ast::Expr::Path(p) if p.segments.len() >= 2 => {
                // Enum::Variant -> discriminant.
                let d = self
                    .enums
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .ok_or_else(|| unsup(&p.segments[1].text))?;
                format!("{d}ULL")
            }
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                let path = expr_path(e).ok_or("unsupported field/index")?;
                let id = self.map.get(&path).ok_or_else(|| unsup(&path))?;
                self.check_scalar(*id)?;
                format!("sx_read({})", id.0)
            }
            ast::Expr::Unary { op, rhs, .. } => {
                let r = self.expr(rhs)?;
                match op {
                    ast::UnOp::Not => format!("(!({r}))"),
                    ast::UnOp::Neg => format!("(-({r}))"),
                }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                let (a, o, c) = (self.expr(lhs)?, c_binop(*op)?, self.expr(rhs)?);
                format!("({a} {o} {c})")
            }
            _ => return Err("unsupported testbench expression".into()),
        })
    }

    /// Reject real/char signals in expressions — the first cut is integer only.
    fn check_scalar(&self, id: SignalId) -> Result<(), String> {
        let s = &self.design.signals[id.0 as usize];
        if s.real || s.char {
            return Err(format!(
                "signal `{}` is {}; siox build does not support real/char/string \
                 testbenches yet (use `siox test`)",
                s.path,
                if s.real { "real" } else { "Char" }
            ));
        }
        Ok(())
    }
}

fn unsup(name: &str) -> String {
    format!("testbench references `{name}`, which siox build cannot translate yet")
}

/// Map a siox binary operator to its C spelling. Word-logical ops become
/// boolean C operators (matching the interpreter's semantics).
fn c_binop(op: ast::BinOp) -> Result<&'static str, String> {
    use ast::BinOp::*;
    Ok(match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Shl => "<<",
        Shr => ">>",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        And => "&&",
        Or => "||",
        _ => return Err("unsupported operator in testbench expression".into()),
    })
}

// --- helpers (small replicas of interpreter internals) ---------------------

fn is_test_entity(modules: &[Module], entity: &str) -> bool {
    for m in modules {
        for it in &m.items {
            if let ast::Item::Entity(e) = it {
                if e.name.text == entity {
                    return e.attrs.iter().any(|a| {
                        a.name.segments.last().map(|s| s.text.as_str()) == Some("test")
                    });
                }
            }
        }
    }
    false
}

fn build_map(hier: &Hierarchy, root: InstanceId, design: &Design) -> HashMap<String, SignalId> {
    let mut map = HashMap::new();
    for &child_id in &hier.instance(root).children {
        let child = hier.instance(child_id);
        for c in &child.connections {
            let prefix = format!("{}.{}", child.entity, c.port);
            for (i, sig) in design.signals.iter().enumerate() {
                let id = SignalId(i as u32);
                if sig.path == prefix {
                    map.insert(c.signal.clone(), id);
                } else if let Some(suffix) = sig.path.strip_prefix(&prefix) {
                    if suffix.starts_with('.') || suffix.starts_with('[') {
                        map.insert(format!("{}{suffix}", c.signal), id);
                    }
                }
            }
        }
    }
    map
}

fn test_items<'a>(modules: &'a [Module], entity: &str) -> Vec<&'a ast::ImplItem> {
    let mut items = Vec::new();
    for m in modules {
        for it in &m.items {
            if let ast::Item::Impl(im) = it {
                if im.trait_.is_none() && type_head_name(&im.target) == Some(entity) {
                    items.extend(im.items.iter());
                }
            }
        }
    }
    items
}

fn enum_discriminants(modules: &[Module]) -> HashMap<String, HashMap<String, u64>> {
    let mut out = HashMap::new();
    for m in modules {
        for it in &m.items {
            if let ast::Item::Enum(e) = it {
                let mut vars = HashMap::new();
                let mut next = 0u64;
                for v in &e.variants {
                    let d = match &v.value {
                        Some(ast::Expr::Int { text, .. }) => parse_u64(text),
                        _ => next,
                    };
                    vars.insert(v.name.text.clone(), d);
                    next = d + 1;
                }
                out.insert(e.name.text.clone(), vars);
            }
        }
    }
    out
}

fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.last().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::Mode { inner, .. } => type_head_name(inner),
    }
}

fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => Some(format!("{}.{}", expr_path(base)?, field.text)),
        ast::Expr::Index { base, index, .. } => match index.as_ref() {
            ast::Expr::Int { text, .. } => Some(format!("{}[{}]", expr_path(base)?, parse_u64(text))),
            _ => None,
        },
        _ => None,
    }
}

fn str_lit(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::StrLit { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn parse_u64(text: &str) -> u64 {
    let t = text.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).unwrap_or(0)
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2).unwrap_or(0)
    } else {
        t.parse().unwrap_or(0)
    }
}

fn logic_value(c: char) -> u64 {
    match c {
        '1' | 'H' => 1,
        'Z' => 2,
        'X' | 'U' | 'W' => 3,
        _ => 0,
    }
}

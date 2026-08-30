//! `siox build` — compile a design + its `#[test]` stimulus into a standalone
//! native simulator binary (stage B5.1).
//!
//! The DUT lowers to a native object (`sx_*` C ABI) via the LLVM backend; the
//! testbench statements are translated to a C `main` that drives it. clang
//! links them into an executable that runs *every* `#[test]` (one C function
//! per test, a libtest-style `main`) and reports results + exit code. All
//! tests share the one lowered Design (one `sx_*` namespace); `sx_reset`
//! zeroes state between them. Testbench locals use the same flattened,
//! arbitrary-width scalar representation as elaborated signals.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;

use siox::elab::{Hierarchy, InstanceId};
use siox::ir::{Design, FunctionIndex, LayoutKind, ScalarDomain, SignalId, SourceLayout};
use siox::resolve::{DefId, Resolved};
use siox::syntax::ast;
use siox::syntax::Module;
use siox::test_ir::Program as TestIr;

type NativeOperatorImpls<'a> = HashMap<(String, String), Vec<(&'a ast::FnDecl, Option<String>)>>;
type StructFieldNames = Vec<String>;
type RawStructFieldNames = HashMap<String, (Option<String>, StructFieldNames)>;

const LIBFST_API_C: &str = include_str!("../../third_party/libfst/src/fstapi.c");
const LIBFST_API_H: &str = include_str!("../../third_party/libfst/src/fstapi.h");
const LIBFST_FASTLZ_C: &str = include_str!("../../third_party/libfst/src/fastlz.c");
const LIBFST_FASTLZ_H: &str = include_str!("../../third_party/libfst/src/fastlz.h");
const LIBFST_LZ4_C: &str = include_str!("../../third_party/libfst/src/lz4.c");
const LIBFST_LZ4_H: &str = include_str!("../../third_party/libfst/src/lz4.h");

/// `file:line:col` for a source span, or `None` when the span has no file.
///
/// Shared by every runtime failure so they all name their source the same way.
fn span_location(sources: &siox::diag::SourceMap, span: siox::diag::Span) -> Option<String> {
    let file = sources.get(span.file)?;
    let (line, column) = sources.line_col(span.file, span.start);
    let head = format!("{}:{line}:{column}", file.name);
    // The snippet is rendered here, at emit time, and embedded: the running
    // executable never reads the source, so it stays correct even if the tree
    // has moved on, and there is no file to find at failure time.
    match sources.snippet(span.file, span.start) {
        Some(snippet) => Some(format!("{head}\n{snippet}")),
        None => Some(head),
    }
}

/// Build a native simulator binary that runs *all* `#[test]` entities, like
/// rustc's test harness. Every test's DUT is in the one lowered `Design` (one
/// `sx_*` namespace); `sx_reset` zeroes all state, so tests run sequentially
/// in the same object.
/// The debug accessors compiled into a `-g` build.
///
/// They report through `stderr` because it is unbuffered: the inferior's
/// `stdout` buffer is not flushed while it sits at a breakpoint, so a `printf`
/// here produces a call that appears to do nothing.
///
/// `sx_dbg_get` exists alongside `sx_dbg_print` so a value can be used, not
/// just read -- `p sx_dbg_get("count") * 2` and conditional breakpoints both
/// need a value rather than a side effect.
const SX_DEBUG_ACCESSORS: &str = concat!(
    "\n/* siox: read a signal by its source path from a debugger.\n",
    " *\n",
    " *   (gdb) call sx_dbg_print(\"c.n\")     one signal, by path or unique tail\n",
    " *   (gdb) call sx_dbg_list(\"f.mem\")    every signal whose path contains it\n",
    " *   (gdb) print sx_dbg_get(\"c.n\")      the value, to use in an expression\n",
    " */\n",
    "static int sx_dbg_find(const char *path) {\n",
    "    unsigned i;\n",
    "    int hit = -1;\n",
    "    if (!path) return -1;\n",
    "    for (i = 0; i < sx_signal_count; i++)\n",
    "        if (strcmp(sx_signal_names[i], path) == 0) return (int)i;\n",
    // A trailing match lets the root be left off (`c.n` for `T.c.n`), but only
    // when it is unique -- otherwise the reader is silently shown one of
    // several signals that share a leaf name.
    "    for (i = 0; i < sx_signal_count; i++) {\n",
    "        size_t n = strlen(sx_signal_names[i]), m = strlen(path);\n",
    "        if (m < n && sx_signal_names[i][n - m - 1] == '.' &&\n",
    "            strcmp(sx_signal_names[i] + n - m, path) == 0)\n",
    "            hit = (hit == -1) ? (int)i : -2;\n",
    "    }\n",
    "    return hit;\n",
    "}\n",
    // A value past one word is shown in hex: assembling the decimal of a
    // 512-bit number in the inferior would mean bignum division, and hex is
    // what a reader compares against a waveform anyway.
    "static void sx_dbg_show(unsigned i) {\n",
    "    unsigned width = sx_signal_widths[i], words, top, k;\n",
    "    if (width <= 64) {\n",
    "        fprintf(stderr, \"%s = %llu\\n\", sx_signal_names[i],\n",
    "                (unsigned long long)sx_read(i));\n",
    "        return;\n",
    "    }\n",
    "    words = (width + 63) / 64;\n",
    "    top = words;\n",
    "    while (top > 1 && sx_read_word(i, top - 1) == 0) top--;\n",
    "    fprintf(stderr, \"%s = 0x%llx\", sx_signal_names[i],\n",
    "            (unsigned long long)sx_read_word(i, top - 1));\n",
    "    for (k = top - 1; k > 0; k--)\n",
    "        fprintf(stderr, \"%016llx\", (unsigned long long)sx_read_word(i, k - 1));\n",
    "    fprintf(stderr, \"\\n\");\n",
    "}\n",
    // `sx_dbg_get` can only hand back one machine word, so on a wider signal it
    // says which word that is. Returning the low bits silently is the same
    // truncation this whole table exists to avoid.
    "__attribute__((used)) uint64_t sx_dbg_get(const char *path) {\n",
    "    int i = sx_dbg_find(path);\n",
    "    if (i < 0) return 0;\n",
    "    if (sx_signal_widths[i] > 64)\n",
    "        fprintf(stderr, \"note: `%s` is %u bits; this is its low word — \"\n",
    "                        \"use sx_dbg_print for the whole value\\n\",\n",
    "                sx_signal_names[i], sx_signal_widths[i]);\n",
    "    return sx_read((uint32_t)i);\n",
    "}\n",
    "__attribute__((used)) int sx_dbg_print(const char *path) {\n",
    "    int i = sx_dbg_find(path);\n",
    "    if (i == -1) { fprintf(stderr, \"no signal matching `%s`\\n\", path); return 0; }\n",
    "    if (i == -2) { fprintf(stderr, \"`%s` matches more than one signal; \"\n",
    "                          \"add enough of the path to be unique\\n\", path); return 0; }\n",
    "    sx_dbg_show((unsigned)i);\n",
    "    return 1;\n",
    "}\n",
    "__attribute__((used)) int sx_dbg_list(const char *prefix) {\n",
    "    unsigned i;\n",
    "    int n = 0;\n",
    "    for (i = 0; i < sx_signal_count; i++)\n",
    "        if (!prefix || !*prefix || strstr(sx_signal_names[i], prefix)) {\n",
    "            sx_dbg_show(i);\n",
    "            n++;\n",
    "        }\n",
    "    if (n == 0) fprintf(stderr, \"no signal matching `%s`\\n\", prefix ? prefix : \"\");\n",
    "    return n;\n",
    "}\n",
);

pub(super) struct BuildRequest<'a> {
    pub modules: &'a [Module],
    pub resolved: &'a Resolved,
    pub hierarchy: &'a Hierarchy,
    pub test_ir: &'a TestIr,
    pub design: &'a Design,
    pub sources: &'a siox::diag::SourceMap,
    /// Attribute generated code back to `.siox` and leave it unoptimized.
    pub debug: bool,
    pub output: &'a Path,
}

pub(super) fn build(request: BuildRequest<'_>) -> Result<(), String> {
    let BuildRequest {
        modules,
        resolved,
        hierarchy: hier,
        test_ir,
        design,
        sources,
        debug,
        output: out,
    } = request;
    let issues = design.validate();
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }

    let mut fns = FunctionIndex::new(resolved);
    let enums = siox::ir::enum_discriminants(modules, &fns);
    let families = siox::ir::array_families(modules, &fns);
    let mut op_impls: NativeOperatorImpls<'_> = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Impl(im) = item {
                let trait_path = im.trait_.as_ref();
                let trait_key = trait_path.and_then(|path| fns.trait_path_key(path));
                if let (Some(tr), Some(ty)) = (trait_key, fns.type_head_key(&im.target)) {
                    let compiler_operator = tr == "Operator";
                    let operator = if compiler_operator {
                        im.trait_args.first().and_then(|a| match a {
                            ast::GenericArg::Positional(ast::Expr::StrLit { text, .. }) => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                    } else {
                        Some(tr)
                    };
                    let Some(operator) = operator else { continue };
                    let input_index = usize::from(compiler_operator);
                    let input = im.trait_args.get(input_index).and_then(|a| match a {
                        ast::GenericArg::Positional(ast::Expr::Path(p)) => fns.type_path_key(p),
                        ast::GenericArg::PositionalType(ty) => fns.type_head_key(ty),
                        _ => None,
                    });
                    for it in &im.items {
                        if let ast::ImplItem::Fn(f) = it {
                            let input = input.clone().or_else(|| {
                                f.params
                                    .iter()
                                    .find(|p| !p.is_self)
                                    .and_then(|p| p.ty.as_ref())
                                    .and_then(|ty| fns.type_head_key(ty))
                            });
                            op_impls
                                .entry((operator.clone(), ty.clone()))
                                .or_default()
                                .push((f, input));
                        }
                    }
                }
            }
        }
    }
    let type_aliases: HashMap<String, ast::Type> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Using(ast::Using {
                kind: ast::UsingKind::Alias { name, ty },
                ..
            }) => Some((fns.type_alias_decl_key(name), ty.clone())),
            _ => None,
        })
        .collect();
    let mut extern_fns = Vec::new();
    let mut trait_decls: HashMap<String, &ast::TraitDecl> = HashMap::new();
    let mut static_defaults: Vec<(String, String)> = Vec::new();
    for m in modules {
        for item in &m.items {
            match item {
                ast::Item::Fn(f) => {
                    fns.insert_free(f);
                }
                ast::Item::ExternBlock {
                    abi, fns: block, ..
                } if abi == "C" => {
                    for f in block {
                        extern_fns.push(f);
                        fns.insert_free(f);
                    }
                }
                // A *static* associated fn (no `self`) is callable as
                // `Type::name(..)`, keyed like a module-level fn.
                ast::Item::Impl(im) => {
                    let Some(ty) = fns.type_head_key(&im.target) else {
                        continue;
                    };
                    for it in &im.items {
                        if let ast::ImplItem::Fn(f) = it {
                            if !f.params.iter().any(|p| p.is_self) {
                                fns.insert_associated(format!("{ty}::{}", f.name.text), f);
                            }
                        }
                    }
                    if let Some(tr) = im.trait_.as_ref().and_then(|path| fns.trait_path_key(path)) {
                        static_defaults.push((ty, tr));
                    }
                }
                ast::Item::Trait(t) => {
                    trait_decls.insert(fns.trait_decl_key(&t.name), t);
                }
                _ => {}
            }
        }
    }
    // A trait's `self`-less defaults are associated functions the implementing
    // type inherits (`Thing::tag()`). This waits until every trait is known,
    // since one may be declared after the impl; `or_insert` keeps the impl's
    // own static, so an override wins.
    for (ty, tr) in static_defaults {
        let Some(t) = trait_decls.get(&tr) else {
            continue;
        };
        for f in t
            .items
            .iter()
            .filter(|f| f.body.is_some() && !f.params.iter().any(|p| p.is_self))
        {
            fns.insert_associated_default(format!("{ty}::{}", f.name.text), f);
        }
    }
    // Module consts (LOW/HIGH, user consts), to a fixpoint so order-independent.
    let const_decls: Vec<(String, &ast::ConstDecl)> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|it| match it {
            ast::Item::Const(c) => Some((fns.constant_decl_key(c), c)),
            _ => None,
        })
        .collect();
    // The tables are folded per testbench, below, so each sees its own
    // constants as well as the module's.
    // Nominal field order remains useful for syntax that has no concrete value
    // path (module constants and synthetic inlined literals). Concrete storage
    // shape comes only from `Design::source_layouts`.
    let struct_field_names = collect_struct_field_names(modules, &fns);
    let methods = collect_methods(modules, &fns);
    let derived_widths = siox::ir::derived_widths(modules, &fns);

    // Header, one `signed test_<name>(void)` per test, then a libtest-style main.
    let mut prog = String::new();
    prog.push_str(
        "#include <errno.h>\n#include <stdint.h>\n#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n#include \"fstapi.h\"\n",
    );
    for f in &extern_fns {
        let name = &f.name.text;
        let ret = f
            .ret
            .as_ref()
            .map_or("void", |ty| extern_c_type(Some(ty), &type_aliases, &fns));
        let params = f
            .params
            .iter()
            .filter(|param| !param.is_self)
            .map(|param| extern_c_type(param.ty.as_ref(), &type_aliases, &fns))
            .collect::<Vec<_>>()
            .join(", ");
        prog.push_str(&format!(
            "extern {ret} {name}({});\n",
            if params.is_empty() { "void" } else { &params }
        ));
    }
    prog.push_str("extern void sx_reset(void);\n");
    prog.push_str("extern void sx_set_word(uint32_t, uint32_t, uint64_t);\n");
    prog.push_str("extern uint64_t sx_read_word(uint32_t, uint32_t);\n");
    prog.push_str("extern uint32_t sx_range_error(void);\n");
    prog.push_str("extern uint32_t sx_range_site(void);\n");
    prog.push_str("extern int64_t sx_range_value(void);\n");
    prog.push_str("extern uint32_t sx_index_error(void);\n");
    prog.push_str("extern int64_t sx_index_value(void);\n");
    prog.push_str("static signed sx_check_ranges(void);\n");
    let abi_words = design
        .signals
        .iter()
        .map(|signal| siox::llvm::words_for(signal.width).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let value_bits = design
        .signals
        .iter()
        .map(|signal| signal.width)
        .max()
        .unwrap_or(1)
        .max(max_literal_type_width(modules, &derived_widths, &fns))
        .max(siox::llvm::ABI_WORD_BITS);
    // ceil(bits * log10(2)) + sign/NUL slack, using a conservative integer
    // approximation so formatting storage scales with the actual value type.
    let decimal_capacity = u64::from(value_bits)
        .saturating_mul(30_103)
        .div_ceil(100_000)
        .saturating_add(2);
    prog.push_str(&format!(
        "typedef unsigned _BitInt({value_bits}) sx_value;\n\
         #define SX_DECIMAL_CAP {decimal_capacity}\n\
         static const uint32_t sx_nwords[] = {{{abi_words}}};\n\
         static void sx_set(uint32_t s, sx_value v) {{\n\
         \x20   for (uint32_t i = 0; i < sx_nwords[s]; ++i)\n\
         \x20       sx_set_word(s, i, (uint64_t)(v >> (i * 64)));\n\
         }}\n\
         static sx_value sx_read(uint32_t s) {{\n\
         \x20   sx_value v = 0;\n\
         \x20   for (uint32_t i = 0; i < sx_nwords[s]; ++i)\n\
         \x20       v |= (sx_value)sx_read_word(s, i) << (i * 64);\n\
         \x20   return v;\n\
         }}\n\
         static sx_value sx_udiv(sx_value lhs, sx_value rhs) {{\n\
         \x20   return rhs == 0 ? 0 : lhs / rhs;\n\
         }}\n\
         static sx_value sx_idiv(int64_t lhs, int64_t rhs) {{\n\
         \x20   if (rhs == 0) return 0;\n\
         \x20   if (lhs == INT64_MIN && rhs == -1) return (sx_value)(uint64_t)INT64_MIN;\n\
         \x20   return (sx_value)(uint64_t)(lhs / rhs);\n\
         }}\n\
         static sx_value sx_shl(sx_value lhs, sx_value rhs) {{\n\
         \x20   return rhs >= {value_bits} ? 0 : lhs << rhs;\n\
         }}\n\
         static sx_value sx_shr(sx_value lhs, sx_value rhs) {{\n\
         \x20   return rhs >= {value_bits} ? 0 : lhs >> rhs;\n\
         }}\n\
         static sx_value sx_ishr(int64_t lhs, sx_value rhs) {{\n\
         \x20   return rhs >= 64 ? (lhs < 0 ? (sx_value)-1 : 0) \
         : (sx_value)(uint64_t)(lhs >> rhs);\n\
         }}\n\
         static int64_t sx_i64(sx_value value, uint32_t width) {{\n\
         \x20   uint64_t low = (uint64_t)value;\n\
         \x20   if (width == 0 || width >= 64) return (int64_t)low;\n\
         \x20   uint64_t mask = (UINT64_C(1) << width) - 1, sign = UINT64_C(1) << (width - 1);\n\
         \x20   low &= mask;\n\
         \x20   return (int64_t)((low ^ sign) - sign);\n\
         }}\n\
         static sx_value sx_mask(sx_value value, uint32_t width) {{\n\
         \x20   return width >= {value_bits} ? value : value & (((sx_value)1 << width) - 1);\n\
         }}\n\
         static char *sx_decimal(sx_value value, char *buffer) {{\n\
         \x20   char *end = buffer + SX_DECIMAL_CAP, *cursor = end;\n\
         \x20   *--cursor = '\\0';\n\
         \x20   do {{ *--cursor = (char)('0' + value % 10); value /= 10; }} while (value);\n\
         \x20   memmove(buffer, cursor, (size_t)(end - cursor));\n\
         \x20   return buffer;\n\
         }}\n\
         static char *sx_utf8(sx_value raw, char buffer[5]) {{\n\
         \x20   uint32_t value = (uint32_t)raw;\n\
         \x20   if (value <= 0x7f) {{ buffer[0] = (char)value; buffer[1] = 0; }}\n\
         \x20   else if (value <= 0x7ff) {{ buffer[0] = (char)(0xc0 | value >> 6); \
         buffer[1] = (char)(0x80 | (value & 0x3f)); buffer[2] = 0; }}\n\
         \x20   else if (value <= 0xffff && !(value >= 0xd800 && value <= 0xdfff)) {{ \
         buffer[0] = (char)(0xe0 | value >> 12); buffer[1] = (char)(0x80 | ((value >> 6) & 0x3f)); \
         buffer[2] = (char)(0x80 | (value & 0x3f)); buffer[3] = 0; }}\n\
         \x20   else if (value <= 0x10ffff) {{ buffer[0] = (char)(0xf0 | value >> 18); \
         buffer[1] = (char)(0x80 | ((value >> 12) & 0x3f)); \
         buffer[2] = (char)(0x80 | ((value >> 6) & 0x3f)); \
         buffer[3] = (char)(0x80 | (value & 0x3f)); buffer[4] = 0; }}\n\
         \x20   else {{ buffer[0] = '?'; buffer[1] = 0; }}\n\
         \x20   return buffer;\n\
         }}\n\
         static char *sx_chars(const sx_value *values, size_t length, char *buffer) {{\n\
         \x20   char *cursor = buffer;\n\
         \x20   for (size_t i = 0; i < length; ++i) {{\n\
         \x20       char encoded[5];\n\
         \x20       sx_utf8(values[i], encoded);\n\
         \x20       size_t bytes = strlen(encoded);\n\
         \x20       memcpy(cursor, encoded, bytes);\n\
         \x20       cursor += bytes;\n\
         \x20   }}\n\
         \x20   *cursor = '\\0';\n\
         \x20   return buffer;\n\
         }}\n"
    ));
    prog.push_str(
        r#"typedef struct sx_io_block {
    void *pointer;
    struct sx_io_block *next;
} sx_io_block;
typedef struct {
    unsigned char *data;
    size_t length;
} sx_bytes;
typedef struct {
    sx_value *values;
    size_t length;
    const char *text;
    size_t text_length;
} sx_dyn_array;
static sx_io_block *g_io_blocks;
static signed g_io_failed;
static char g_io_message[512];
static void sx_io_reset(void) {
    while (g_io_blocks) {
        sx_io_block *next = g_io_blocks->next;
        free(g_io_blocks->pointer);
        free(g_io_blocks);
        g_io_blocks = next;
    }
    g_io_failed = 0;
}
static void sx_io_fail(const char *operation, const char *path, const char *detail) {
    if (!g_io_failed)
        snprintf(g_io_message, sizeof g_io_message, "%s(\"%s\"): %s", operation, path, detail);
    g_io_failed = 1;
}
static void *sx_io_alloc(size_t size) {
    if (size == 0) size = 1;
    void *pointer = calloc(1, size);
    sx_io_block *block = pointer ? malloc(sizeof *block) : 0;
    if (!pointer || !block) {
        free(pointer);
        free(block);
        sx_io_fail("file I/O", "<runtime>", "out of memory");
        return 0;
    }
    block->pointer = pointer;
    block->next = g_io_blocks;
    g_io_blocks = block;
    return pointer;
}
static sx_bytes sx_read_file(const char *operation, const char *path) {
    sx_bytes result = {0};
    FILE *file = fopen(path, "rb");
    if (!file) {
        sx_io_fail(operation, path, strerror(errno));
        return result;
    }
    if (fseek(file, 0, SEEK_END) || ftell(file) < 0) {
        sx_io_fail(operation, path, "cannot determine file length");
        fclose(file);
        return result;
    }
    long end = ftell(file);
    if (end < 0 || fseek(file, 0, SEEK_SET)) {
        sx_io_fail(operation, path, "cannot seek file");
        fclose(file);
        return result;
    }
    result.length = (size_t)end;
    if (result.length == SIZE_MAX) {
        sx_io_fail(operation, path, "file is too large");
        fclose(file);
        result.length = 0;
        return result;
    }
    result.data = sx_io_alloc(result.length + 1);
    if (!result.data) {
        fclose(file);
        result.length = 0;
        return result;
    }
    size_t read = fread(result.data, 1, result.length, file);
    if (read != result.length) {
        sx_io_fail(operation, path, ferror(file) ? strerror(errno) : "short read");
        result.length = read;
    }
    result.data[result.length] = 0;
    fclose(file);
    return result;
}
static signed sx_utf8_next(const unsigned char *data, size_t length,
                           size_t *cursor, uint32_t *value) {
    size_t at = *cursor;
    if (at >= length) return 0;
    unsigned b0 = data[at++];
    if (b0 <= 0x7f) *value = b0;
    else if (b0 >= 0xc2 && b0 <= 0xdf && at < length
             && (data[at] & 0xc0) == 0x80) {
        *value = ((b0 & 0x1f) << 6) | (data[at++] & 0x3f);
    } else if (b0 >= 0xe0 && b0 <= 0xef && at + 1 < length
               && (data[at] & 0xc0) == 0x80 && (data[at + 1] & 0xc0) == 0x80
               && !(b0 == 0xe0 && data[at] < 0xa0)
               && !(b0 == 0xed && data[at] >= 0xa0)) {
        *value = ((b0 & 0x0f) << 12) | ((data[at] & 0x3f) << 6)
                 | (data[at + 1] & 0x3f);
        at += 2;
    } else if (b0 >= 0xf0 && b0 <= 0xf4 && at + 2 < length
               && (data[at] & 0xc0) == 0x80 && (data[at + 1] & 0xc0) == 0x80
               && (data[at + 2] & 0xc0) == 0x80
               && !(b0 == 0xf0 && data[at] < 0x90)
               && !(b0 == 0xf4 && data[at] >= 0x90)) {
        *value = ((b0 & 0x07) << 18) | ((data[at] & 0x3f) << 12)
                 | ((data[at + 1] & 0x3f) << 6) | (data[at + 2] & 0x3f);
        at += 3;
    } else return -1;
    *cursor = at;
    return 1;
}
static sx_dyn_array sx_read_text(const char *path) {
    sx_dyn_array result = {0};
    sx_bytes bytes = sx_read_file("read<string>", path);
    if (g_io_failed) return result;
    size_t capacity = bytes.length ? bytes.length : 1;
    if (capacity > SIZE_MAX / sizeof(sx_value)) {
        sx_io_fail("read<string>", path, "file is too large");
        return result;
    }
    result.values = sx_io_alloc(capacity * sizeof(sx_value));
    if (!result.values) return result;
    size_t cursor = 0;
    while (cursor < bytes.length) {
        uint32_t value;
        if (sx_utf8_next(bytes.data, bytes.length, &cursor, &value) < 0) {
            sx_io_fail("read<string>", path, "file is not valid UTF-8");
            return result;
        }
        result.values[result.length++] = (sx_value)value;
    }
    result.text = (const char *)bytes.data;
    result.text_length = bytes.length;
    return result;
}
static signed sx_read_values(const char *operation, const char *path,
                             sx_value *out, size_t count,
                             size_t element_bytes, uint32_t element_bits) {
    sx_bytes bytes = sx_read_file(operation, path);
    if (g_io_failed) return 0;
    if (element_bytes && count > SIZE_MAX / element_bytes) {
        sx_io_fail(operation, path, "declared target is too large");
        return 0;
    }
    size_t capacity = count * element_bytes;
    if (bytes.length > capacity) {
        snprintf(g_io_message, sizeof g_io_message,
                 "%s(\"%s\"): %zu bytes do not fit (%zu elements x %zu bytes)",
                 operation, path, bytes.length, count, element_bytes);
        g_io_failed = 1;
        return 0;
    }
    for (size_t element = 0; element < count; ++element) {
        sx_value value = 0;
        for (size_t byte = 0; byte < element_bytes; ++byte) {
            size_t offset = element * element_bytes + byte;
            if (offset < bytes.length)
                value |= (sx_value)bytes.data[offset] << (byte * 8);
        }
        out[element] = sx_mask(value, element_bits);
    }
    return 1;
}
static sx_value sx_dyn_get(const sx_dyn_array *array, sx_value raw_index) {
    if (!array->length) return 0;
    size_t index = (size_t)raw_index;
    return index < array->length ? array->values[index]
                                 : array->values[array->length - 1];
}
static signed sx_dyn_equal_values(const sx_dyn_array *array,
                                  const sx_value *values, size_t length) {
    if (array->length != length) return 0;
    for (size_t i = 0; i < length; ++i)
        if (array->values[i] != values[i]) return 0;
    return 1;
}
"#,
    );
    prog.push_str("extern void sx_settle(void);\n");
    prog.push_str(
        "static const char *g_msg;\n\
         static const char *g_loc;\n\
         static signed g_range_failed;\n",
    );
    prog.push_str(
        "static char g_index_message[160];\n\
         static int64_t sx_checked_index(int64_t value, int64_t left, int64_t right,\n\
         \x20                               const char *location) {\n\
         \x20   int64_t low = left < right ? left : right;\n\
         \x20   int64_t high = left > right ? left : right;\n\
         \x20   if ((value < low || value > high) && !g_range_failed) {\n\
         \x20       snprintf(g_index_message, sizeof g_index_message,\n\
         \x20                \"index %lld is outside declared range %lld..%lld\",\n\
         \x20                (long long)value, (long long)left, (long long)right);\n\
         \x20       g_msg = g_index_message; g_loc = location; g_range_failed = 1;\n\
         \x20   }\n\
         \x20   return value;\n\
         }\n\
         static sx_value sx_dyn_get_checked(const sx_dyn_array *array, sx_value raw_index,\n\
         \x20                                  const char *location) {\n\
         \x20   if (!array->length) {\n\
         \x20       if (!g_range_failed) {\n\
         \x20           snprintf(g_index_message, sizeof g_index_message,\n\
         \x20                    \"index %lld is outside an empty runtime array\",\n\
         \x20                    (long long)(int64_t)raw_index);\n\
         \x20           g_msg = g_index_message; g_loc = location; g_range_failed = 1;\n\
         \x20       }\n\
         \x20   } else {\n\
         \x20       (void)sx_checked_index((int64_t)raw_index, 0,\n\
         \x20                              (int64_t)(array->length - 1), location);\n\
         \x20   }\n\
         \x20   return sx_dyn_get(array, raw_index);\n\
         }\n",
    );
    prog.push_str("static signed g_warnings;\n");
    prog.push_str("static double sx_f64(uint64_t b) { double d; memcpy(&d, &b, 8); return d; }\n");
    prog.push_str(
        "static uint64_t sx_b64(double d) { uint64_t b; memcpy(&b, &d, 8); return b; }\n",
    );
    // xorshift64* with the runner's constants: identical random sequences.
    prog.push_str(
        "static uint64_t g_rand = 0x9E3779B97F4A7C15ULL;\n\
         static uint64_t sx_rand(void) {\n\
         \x20   g_rand ^= g_rand >> 12; g_rand ^= g_rand << 25; g_rand ^= g_rand >> 27;\n\
         \x20   return g_rand * 0x2545F4914F6CDD1DULL;\n}\n",
    );
    prog.push_str(&format!(
        "static sx_value sx_random_value(void) {{\n\
         \x20   sx_value value = 0;\n\
         \x20   for (unsigned bit = 0; bit < {value_bits}; bit += 64)\n\
         \x20       value |= (sx_value)sx_rand() << bit;\n\
         \x20   return value;\n\
         }}\n\
         static sx_value sx_randint(sx_value left, sx_value right) {{\n\
         \x20   if (right < left) {{ sx_value swap = left; left = right; right = swap; }}\n\
         \x20   sx_value span = right - left;\n\
         \x20   if (right <= (sx_value)UINT64_MAX) {{\n\
         \x20       if (span == (sx_value)UINT64_MAX) return left + (sx_value)sx_rand();\n\
         \x20       uint64_t range = (uint64_t)span + 1, threshold = (0 - range) % range, draw;\n\
         \x20       do {{ draw = sx_rand(); }} while (draw < threshold);\n\
         \x20       return left + (sx_value)(draw % range);\n\
         \x20   }}\n\
         \x20   if (span == ~(sx_value)0) return sx_random_value();\n\
         \x20   sx_value range = span + 1, threshold = (0 - range) % range, draw;\n\
         \x20   do {{ draw = sx_random_value(); }} while (draw < threshold);\n\
         \x20   return left + draw % range;\n\
         }}\n"
    ));
    // `uniform()` — the runner's exact expression, so both engines agree.
    prog.push_str(
        "static uint64_t sx_uniform(void) {\n\
         \x20   double d = (double)(sx_rand() >> 11) / (double)(1ULL << 53);\n\
         \x20   return sx_b64(d);\n}\n",
    );
    prog.push_str(&gen_wave_runtime(design));
    // The event wheel: earliest pending clock edge, and one step of the
    // scheduler (advance to that edge, toggle the due clocks, settle).
    prog.push_str(
        "static uint64_t sx_next_edge(const uint64_t *next, signed n) {\n\
         \x20   uint64_t t = UINT64_MAX;\n\
         \x20   for (signed i = 0; i < n; i++) if (next[i] < t) t = next[i];\n\
         \x20   return t;\n}\n\
         static signed sx_step_clock(uint64_t *now, uint64_t *next, const uint32_t *cid,\n\
         \x20                        const uint64_t *half, signed n) {\n\
         \x20   uint64_t t = sx_next_edge(next, n);\n\
         \x20   if (t == UINT64_MAX) return 0;\n\
         \x20   if (t > *now) *now = t;\n\
         \x20   for (signed i = 0; i < n; i++)\n\
         \x20       if (next[i] == t) { sx_set(cid[i], !sx_read(cid[i])); \
         next[i] = UINT64_MAX - next[i] < half[i] ? UINT64_MAX : next[i] + half[i]; }\n\
         \x20   sx_run_settle(*now);\n\
         \x20   return 1;\n}\n\n",
    );

    // Runtime index failures are latched by the LLVM engine before a fallback
    // mux value is stored. The generated harness owns the source table and the
    // human-readable report, just as it does for constrained numerics below.
    let index_sites = design.index_sites();
    if !index_sites.is_empty() {
        prog.push_str("static const int64_t sx_index_left[] = {");
        for site in &index_sites {
            prog.push_str(&format!("{}LL,", site.left));
        }
        prog.push_str("};\nstatic const int64_t sx_index_right[] = {");
        for site in &index_sites {
            prog.push_str(&format!("{}LL,", site.right));
        }
        prog.push_str("};\nstatic const char *const sx_index_sites[] = {\n");
        for site in &index_sites {
            match span_location(sources, site.span) {
                Some(at) => prog.push_str(&format!("    \"{}\",\n", c_escape(&at))),
                None => prog.push_str("    0,\n"),
            }
        }
        prog.push_str("};\nstatic char sx_index_message[160];\n");
    }
    let index_check = if index_sites.is_empty() {
        String::new()
    } else {
        format!(
            "    {{ uint32_t i = sx_index_error(); if (i && i <= {n}u) {{\n\
             \x20     int64_t v = sx_index_value(); uint32_t s = i - 1;\n\
             \x20     snprintf(sx_index_message, sizeof sx_index_message,\n\
             \x20              \"index %lld is outside declared range %lld..%lld\",\n\
             \x20              (long long)v, (long long)sx_index_left[s],\n\
             \x20              (long long)sx_index_right[s]);\n\
             \x20     g_msg = sx_index_message; g_loc = sx_index_sites[s];\n\
             \x20     g_range_failed = 1; return 1; }} }}\n",
            n = index_sites.len()
        )
    };

    // The dynamic range assert (spec 3.26): after settles, ranged numerics
    // must lie in their domain.
    let ranged: Vec<(u32, &siox::ir::Signal)> = design
        .signals
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.range.map(|_| (i as u32, s)))
        .collect();
    if !ranged.is_empty() {
        // Where each assignment that can break a domain lives, indexed the way
        // the engine latches it. The strings are rendered here, at compile
        // time: the executable holds no source and must stay right if the tree
        // moves on.
        let sites = design.range_sites();
        if !sites.is_empty() {
            prog.push_str("static const char *const sx_range_sites[] = {\n");
            for span in &sites {
                match span_location(sources, *span) {
                    Some(at) => prog.push_str(&format!("    \"{}\",\n", c_escape(&at))),
                    None => prog.push_str("    0,\n"),
                }
            }
            prog.push_str("};\n");
        }
        prog.push_str(
            "static signed sx_check_ranges(void) {\n    int64_t v;\n    uint32_t e;\n\
             \x20   if (g_range_failed) return 1;\n\
",
        );
        prog.push_str(&index_check);
        prog.push_str("    e = sx_range_error();\n");
        // Read a ranged signal into `v`, sign-extending when its domain goes
        // below zero. Both the engine-flagged path and the post-settle scan
        // need the value, and the message is written once for both: naming the
        // signal and its bounds while dropping the number that broke them left
        // the reader to go and find it.
        let decode = |id: u32, sig: &siox::ir::Signal, lo: i64| {
            if lo < 0 && sig.width > 0 && sig.width < 64 {
                format!(
                    "v = (int64_t)sx_read({id}); if (v & {s}LL) v -= {m}LL;",
                    s = 1u64 << (sig.width - 1),
                    m = 1u64 << sig.width
                )
            } else {
                format!("v = (int64_t)sx_read({id});")
            }
        };
        let report = |id: u32, sig: &siox::ir::Signal, lo: i64, hi: i64| {
            let buffer = format!("_sx_range_{id}");
            let capacity = sig.path.len() + 64;
            format!(
                "static char {buffer}[{capacity}]; \
                 snprintf({buffer}, sizeof {buffer}, \
                 \"`{}` left its range {lo}..{hi} (it was %lld)\", (long long)v); \
                 g_msg = {buffer};{at}",
                sig.path,
                at = span_location(sources, sig.declaration_span)
                    .map(|at| format!(" g_loc = \"{}\";", c_escape(&at)))
                    .unwrap_or_default(),
            )
        };

        // The engine flagged this *before* the value was narrowed to the
        // destination, so take the value it kept. Decoding the signal here
        // reported the truncated one, which can be back inside the domain the
        // message says was left.
        prog.push_str("    if (e) { v = sx_range_value(); switch (e) {\n");
        for (id, sig) in &ranged {
            let (lo, hi) = sig.range.unwrap();
            prog.push_str(&format!(
                "    case {}: {{ {} }} break;\n",
                id + 1,
                report(*id, sig, lo, hi)
            ));
        }
        prog.push_str(
            "    default: g_msg = \"a ranged signal left its domain\"; break;\n\
             \x20   }\n",
        );
        // The declaration is the fallback, not the answer: it says which
        // domain was left, while the reader wants the line that left it. A
        // synthesized driver (a port connection) latches site 0 and keeps the
        // declaration.
        if !sites.is_empty() {
            prog.push_str(&format!(
                "    {{ uint32_t s = sx_range_site();\n\
                 \x20     if (s && s <= {n}u && sx_range_sites[s - 1]) g_loc = sx_range_sites[s - 1]; }}\n",
                n = sites.len(),
            ));
        }
        prog.push_str("    g_range_failed = 1; return 1; }\n");
        for (id, sig) in &ranged {
            let (lo, hi) = sig.range.unwrap();
            prog.push_str(&format!(
                "    {}\n    if (v < {lo}LL || v > {hi}LL) {{ {} g_range_failed = 1; return 1; }}\n",
                decode(*id, sig, lo),
                report(*id, sig, lo, hi)
            ));
        }
        prog.push_str("    return 0;\n}\n\n");
    } else {
        prog.push_str("static signed sx_check_ranges(void) {\n    if (g_range_failed) return 1;\n");
        prog.push_str(&index_check);
        prog.push_str("    return 0;\n}\n\n");
    }

    let mut names = Vec::new();
    for test in &test_ir.tests {
        let root = test.root;
        let instance = hier.instance(root);
        let name = hier.root_path(root);
        let qualified = test.qualified_name.clone();
        let symbol = format!("r{}", root.0);
        let (map, aliases) = build_map(hier, root, design);
        let items = siox::testbench::implementation_items(modules, resolved, instance.entity_id);
        // A testbench's own constants. Folded per test so two entities that
        // each declare `LIMIT` cannot collide in one table, and through the
        // same routine as the module-level ones so the kinds cannot diverge.
        let mut scoped_decls = const_decls.clone();
        scoped_decls.extend(items.iter().filter_map(|item| match item {
            ast::ImplItem::Const(c) => Some((c.name.text.clone(), c)),
            _ => None,
        }));
        let ConstTables {
            real_consts,
            integer_consts,
            const_ranges,
            consts,
            const_exprs,
            const_array_exprs,
        } = const_tables(
            &scoped_decls,
            &enums,
            &fns,
            &type_aliases,
            &struct_field_names,
        );
        let clocks = scan_clocks(&items, &aliases)?;
        let instance_names: std::collections::HashSet<String> = hier
            .instance(root)
            .children
            .iter()
            .map(|&c| hier.instance(c).name.clone())
            .collect();
        let entity_ports: HashMap<DefId, Vec<(String, ast::Type)>> = modules
            .iter()
            .flat_map(|m| &m.items)
            .filter_map(|it| match it {
                ast::Item::Entity(e) => resolved.declared(e.name.span).map(|id| {
                    (
                        id,
                        e.ports
                            .iter()
                            .map(|p| (p.name.text.clone(), p.ty.clone()))
                            .collect(),
                    )
                }),
                _ => None,
            })
            .collect();
        let ctx = Ctx {
            design,
            sources,
            debug,
            map: &map,
            enums: &enums,
            families: &families,
            name: &name,
            symbol: &symbol,
            clocks,
            locals: Default::default(),
            local_widths: Default::default(),
            local_indices: Default::default(),
            local_ranges: Default::default(),
            dynamic_strings: Default::default(),
            local_families: Default::default(),
            local_types: Default::default(),
            op_impls: &op_impls,
            methods: &methods,
            struct_field_names: &struct_field_names,
            derived_widths: &derived_widths,
            tmp_seq: std::cell::Cell::new(0),
            const_exprs: &const_exprs,
            const_array_exprs: &const_array_exprs,
            consts: &consts,
            real_consts: &real_consts,
            integer_consts: &integer_consts,
            const_ranges: &const_ranges,
            aliases: &aliases,
            tmp: Default::default(),
            message_id: Default::default(),
            fns: &fns,
            resolved,
            type_aliases: &type_aliases,
            fn_env: Default::default(),
            fn_type_env: Default::default(),
            instance_names,
            entity_ports,
            value_bits,
        };
        prog.push_str(&ctx.gen_test_fn(&items)?);
        names.push((symbol, qualified));
    }
    prog.push_str(&gen_main(&names));

    // Emit the DUT object (all tests' logic) and link with clang.
    let tmp = std::env::temp_dir().join(format!("siox_build_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let obj = tmp.join("design.o");
    let csrc = tmp.join("sim.c");
    siox::llvm::emit_object(design, &obj)?;
    // A debug build carries the signal-name table and the three accessors that
    // read it. Hardware signals are not variables -- they live behind
    // `sx_read`, indexed by `SignalId` -- so DWARF has nothing natural to
    // describe, and printing one by its siox path needs a lookup.
    //
    // The lookup is compiled *into* the binary rather than shipped as a gdb
    // Python script. An embedded script is possible (`.debug_gdb_scripts`) but
    // gdb declines to auto-load one unless the binary's directory is on
    // `auto-load safe-path`, so it trades an explicit `source` for a security
    // warning and an edit to the user's gdbinit. Plain functions need none of
    // that, and work in any debugger that can call into the inferior.
    if debug {
        let mut table = String::from(
            "\n/* siox: hierarchical signal names, indexed by SignalId. */\n\
             const char *const sx_signal_names[] = {\n",
        );
        for signal in &design.signals {
            table.push_str(&format!("    \"{}\",\n", c_escape(&signal.path)));
        }
        table.push_str("};\n");
        // Widths too: `sx_read` hands back one word, so without them a value
        // wider than 64 bits prints as its low word and looks like a small
        // number rather than a truncation.
        table.push_str("const unsigned sx_signal_widths[] = {\n");
        for signal in &design.signals {
            table.push_str(&format!("    {},\n", signal.width));
        }
        table.push_str("};\n");
        table.push_str(&format!(
            "const unsigned sx_signal_count = {};\n",
            design.signals.len()
        ));
        table.push_str(SX_DEBUG_ACCESSORS);
        prog.push_str(&table);
    }
    std::fs::write(&csrc, &prog).map_err(|e| e.to_string())?;
    for (name, contents) in [
        ("fstapi.c", LIBFST_API_C),
        ("fstapi.h", LIBFST_API_H),
        ("fastlz.c", LIBFST_FASTLZ_C),
        ("fastlz.h", LIBFST_FASTLZ_H),
        ("lz4.c", LIBFST_LZ4_C),
        ("lz4.h", LIBFST_LZ4_H),
    ] {
        std::fs::write(tmp.join(name), contents).map_err(|error| error.to_string())?;
    }
    if std::env::var("SIOX_DEBUG_C").is_ok() {
        let _ = std::fs::write("/tmp/siox_debug.c", &prog);
    }
    // A debug build keeps the `#line` mapping usable: optimization reorders and
    // merges the code those directives point at, so stepping degrades exactly
    // where it is most wanted. The default stays optimized, since simulation
    // throughput matters for long runs.
    let optimization = if debug { "-O0" } else { "-O2" };
    let mut clang = Command::new("clang");
    clang
        .arg(&csrc)
        .arg(&obj)
        .arg(tmp.join("fstapi.c"))
        .arg(tmp.join("fastlz.c"))
        .arg(tmp.join("lz4.c"))
        .args([optimization, "-lm", "-lz"]);
    if debug {
        // `-grecord-command-line` puts the flags in DWARF, so a binary can be
        // asked how it was built rather than taken on trust -- which is also
        // what lets the unoptimized choice be verified.
        clang.args(["-g", "-grecord-command-line"]);
    }
    let status = clang
        .arg("-o")
        .arg(out)
        .status()
        .map_err(|e| format!("failed to run clang: {e}"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err("clang failed to link the simulator".into());
    }
    Ok(())
}

#[derive(Default)]
struct WaveScope {
    children: BTreeMap<String, WaveScope>,
    signals: Vec<(usize, String)>,
}

fn logic_vcd_symbols(design: &Design, signal: &siox::ir::Signal) -> Option<HashMap<u64, char>> {
    logic_vcd_symbols_for_type(design, signal.enum_type.as_deref()?)
}

fn logic_vcd_symbols_for_type(design: &Design, type_name: &str) -> Option<HashMap<u64, char>> {
    let symbols = design.enum_syms.get(type_name)?;
    let encoding = design.logic_encodings.get(type_name)?;
    let mut out = HashMap::new();
    for &disc in symbols.keys() {
        let value = if encoding.high_impedance.contains(&disc) {
            'z'
        } else if encoding.unknown.contains(&disc) {
            'x'
        } else if encoding.value_bits.get(&disc).copied()? {
            '1'
        } else {
            '0'
        };
        out.insert(disc, value);
    }
    Some(out)
}

fn emit_vcd_scope_header(out: &mut String, name: &str, scope: &WaveScope, design: &Design) {
    out.push_str(&format!("$scope module {name} $end\n"));
    for &(id, ref signal_name) in &scope.signals {
        let signal = &design.signals[id];
        let kind = if signal.real {
            "real"
        } else if signal.enum_type.as_ref().is_some_and(|ty| {
            logic_vcd_symbols(design, signal).is_none() && design.enum_syms.contains_key(ty)
        }) {
            "string"
        } else {
            "wire"
        };
        let width = if kind == "string" || logic_vcd_symbols(design, signal).is_some() {
            1
        } else {
            signal.width.max(1)
        };
        out.push_str(&format!("$var {kind} {width} v{id} {signal_name} $end\n"));
    }
    for (child_name, child) in &scope.children {
        emit_vcd_scope_header(out, child_name, child, design);
    }
    out.push_str("$upscope $end\n");
}

/// Generate the VCD writer into the native executable. Values are sampled
/// directly from the design ABI after each settle; no trace is returned to the
/// compiler process.
fn gen_wave_runtime(design: &Design) -> String {
    let companions: std::collections::HashSet<usize> =
        design.meta_of.values().map(|id| *id as usize).collect();
    let mut root = WaveScope::default();
    for (id, signal) in design.signals.iter().enumerate() {
        if companions.contains(&id) {
            continue;
        }
        let mut parts = signal.path.split('.').collect::<Vec<_>>();
        let signal_name = parts.pop().unwrap_or("signal").to_string();
        let mut scope = &mut root;
        if parts.is_empty() {
            parts.push("top");
        }
        for part in parts {
            scope = scope.children.entry(part.to_string()).or_default();
        }
        scope.signals.push((id, signal_name));
    }
    let mut header =
        String::from("$version siox native test executable $end\n$timescale 1fs $end\n");
    for (name, scope) in &root.children {
        emit_vcd_scope_header(&mut header, name, scope, design);
    }
    header.push_str("$enddefinitions $end\n");

    let n = design.signals.len().max(1);
    let mut c = format!(
        "static FILE *g_vcd;\n\
         static sx_value g_vcd_last[{n}];\n\
         static unsigned char g_vcd_seen[{n}];\n\
         static uint64_t g_vcd_base, g_vcd_last_time;\n\
         static signed g_vcd_started;\n\
         static signed sx_vcd_open(const char *path) {{\n\
         \x20   g_vcd = fopen(path, \"wb\");\n\
         \x20   if (!g_vcd) {{ fprintf(stderr, \"cannot open VCD output %s\\n\", path); return 0; }}\n\
         \x20   fputs(\"{}\", g_vcd);\n\
         \x20   return 1;\n\
         }}\n\
         static void sx_vcd_close(void) {{ if (g_vcd) {{ fclose(g_vcd); g_vcd = 0; }} }}\n\
         static void sx_vcd_begin_test(void) {{\n\
         \x20   if (!g_vcd) return;\n\
         \x20   g_vcd_base = g_vcd_started && g_vcd_last_time != UINT64_MAX \
         ? g_vcd_last_time + 1 : (g_vcd_started ? UINT64_MAX : 0);\n\
         \x20   memset(g_vcd_seen, 0, sizeof(g_vcd_seen));\n\
         }}\n\
         static void sx_vcd_sample(uint64_t now) {{\n\
         \x20   if (!g_vcd) return;\n\
         \x20   uint64_t _time = UINT64_MAX - g_vcd_base < now \
         ? UINT64_MAX : g_vcd_base + now;\n\
         \x20   signed _wrote = 0, _initial = !g_vcd_started;\n",
        c_escape(&header)
    );

    let timestamp = "if (!_wrote) { if (_initial || _time != g_vcd_last_time) fprintf(g_vcd, \"#%llu\\n\", (unsigned long long)_time); if (_initial) fputs(\"$dumpvars\\n\", g_vcd); _wrote = 1; }";
    for (id, signal) in design.signals.iter().enumerate() {
        if companions.contains(&id) {
            continue;
        }
        let meta = design
            .meta_of
            .get(&(id as u32))
            .copied()
            .map(|v| v as usize);
        c.push_str(&format!("    {{ sx_value _v = sx_read({id});"));
        if let Some(meta_id) = meta {
            c.push_str(&format!(
                " sx_value _m = sx_read({meta_id}); if (!g_vcd_seen[{id}] || _v != g_vcd_last[{id}] || !g_vcd_seen[{meta_id}] || _m != g_vcd_last[{meta_id}]) {{ {timestamp} "
            ));
            let table = design
                .array_element_enums
                .get(&(id as u32))
                .map(String::as_str)
                .and_then(|name| logic_vcd_symbols_for_type(design, name))
                .unwrap_or_default();
            c.push_str("fputc('b', g_vcd); for (signed _b = ");
            c.push_str(&signal.width.saturating_sub(1).to_string());
            c.push_str("; _b >= 0; _b--) { unsigned _d = (unsigned)((_m >> (_b * 4)) & 15); signed _ch = 'x'; switch (_d) {");
            let mut entries = table.into_iter().collect::<Vec<_>>();
            entries.sort_by_key(|(disc, _)| *disc);
            for (disc, ch) in entries {
                c.push_str(&format!("case {disc}: _ch = '{ch}'; break;"));
            }
            c.push_str("} if (_ch != 'x' && _ch != 'z') _ch = ((_v >> _b) & 1) ? '1' : '0'; fputc(_ch, g_vcd); }");
            c.push_str(&format!(
                " fprintf(g_vcd, \" v{id}\\n\"); g_vcd_last[{meta_id}] = _m; g_vcd_seen[{meta_id}] = 1;"
            ));
        } else {
            c.push_str(&format!(
                " if (!g_vcd_seen[{id}] || _v != g_vcd_last[{id}]) {{ {timestamp} "
            ));
            if signal.real {
                c.push_str(&format!(
                    "fprintf(g_vcd, \"r%.17g v{id}\\n\", sx_f64((uint64_t)_v));"
                ));
            } else if let Some(table) = logic_vcd_symbols(design, signal) {
                c.push_str("signed _ch = 'x'; switch ((uint64_t)_v) {");
                let mut entries = table.into_iter().collect::<Vec<_>>();
                entries.sort_by_key(|(disc, _)| *disc);
                for (disc, ch) in entries {
                    c.push_str(&format!("case {disc}ULL: _ch = '{ch}'; break;"));
                }
                c.push_str(&format!("}} fprintf(g_vcd, \"%cv{id}\\n\", _ch);"));
            } else if let Some(symbols) = signal
                .enum_type
                .as_ref()
                .and_then(|ty| design.enum_syms.get(ty))
            {
                c.push_str("switch ((uint64_t)_v) {");
                let mut entries = symbols.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(disc, _)| **disc);
                for (&disc, symbol) in entries {
                    c.push_str(&format!(
                        "case {disc}ULL: fputs(\"s{} v{id}\\n\", g_vcd); break;",
                        c_escape(symbol)
                    ));
                }
                c.push_str(&format!(
                    "default: fprintf(g_vcd, \"s%llu v{id}\\n\", (unsigned long long)_v); break; }}"
                ));
            } else if signal.width <= 1 {
                c.push_str(&format!(
                    "fprintf(g_vcd, \"%cv{id}\\n\", (_v & 1) ? '1' : '0');"
                ));
            } else {
                c.push_str("fputc('b', g_vcd); for (signed _b = ");
                c.push_str(&signal.width.saturating_sub(1).to_string());
                c.push_str("; _b >= 0; _b--) fputc(((_v >> _b) & 1) ? '1' : '0', g_vcd);");
                c.push_str(&format!(" fputs(\" v{id}\\n\", g_vcd);"));
            }
        }
        c.push_str(&format!(
            " g_vcd_last[{id}] = _v; g_vcd_seen[{id}] = 1; }} }}\n"
        ));
    }
    c.push_str(
        "    if (_wrote) { if (_initial) fputs(\"$end\\n\", g_vcd); g_vcd_started = 1; g_vcd_last_time = _time; }\n\
         }\n",
    );
    c.push_str(&gen_fst_runtime(design, &root, &companions));
    c.push_str(
        "static void sx_wave_begin_test(void) { sx_vcd_begin_test(); sx_fst_begin_test(); }\n\
         static void sx_run_settle(uint64_t now) {\n\
         \x20   sx_settle();\n\
         \x20   sx_vcd_sample(now);\n\
         \x20   sx_fst_sample(now);\n\
         \x20   (void)sx_check_ranges();\n\
         }\n\n",
    );
    c
}

fn emit_fst_scope_registration(out: &mut String, name: &str, scope: &WaveScope, design: &Design) {
    out.push_str(&format!(
        "    fstWriterSetScope(g_fst, FST_ST_VCD_MODULE, \"{}\", 0);\n",
        c_escape(name)
    ));
    for &(id, ref signal_name) in &scope.signals {
        let signal = &design.signals[id];
        let (kind, width) = if signal.real {
            ("FST_VT_VCD_REAL", 1)
        } else if signal.enum_type.as_ref().is_some_and(|ty| {
            logic_vcd_symbols(design, signal).is_none() && design.enum_syms.contains_key(ty)
        }) {
            ("FST_VT_GEN_STRING", 0)
        } else if logic_vcd_symbols(design, signal).is_some() {
            ("FST_VT_VCD_WIRE", 1)
        } else {
            ("FST_VT_VCD_WIRE", signal.width.max(1))
        };
        out.push_str(&format!(
            "    g_fst_handle[{id}] = fstWriterCreateVar(g_fst, {kind}, FST_VD_IMPLICIT, {width}, \"{}\", 0);\n\
             \x20   if (!g_fst_handle[{id}]) {{ sx_fst_close(); return 0; }}\n",
            c_escape(signal_name)
        ));
    }
    for (child_name, child) in &scope.children {
        emit_fst_scope_registration(out, child_name, child, design);
    }
    out.push_str("    fstWriterSetUpscope(g_fst);\n");
}

fn gen_fst_runtime(
    design: &Design,
    root: &WaveScope,
    companions: &std::collections::HashSet<usize>,
) -> String {
    let n = design.signals.len().max(1);
    let max_width = design
        .signals
        .iter()
        .map(|signal| signal.width)
        .max()
        .unwrap_or(1)
        .max(64);
    let mut registration = String::new();
    for (name, scope) in &root.children {
        emit_fst_scope_registration(&mut registration, name, scope, design);
    }
    let mut c = format!(
        "static fstWriterContext *g_fst;\n\
         static fstHandle g_fst_handle[{n}];\n\
         static sx_value g_fst_last[{n}];\n\
         static unsigned char g_fst_seen[{n}];\n\
         static uint64_t g_fst_base, g_fst_last_time;\n\
         static signed g_fst_started;\n\
         static char g_fst_value[{}];\n\
         static void sx_fst_close(void) {{\n\
         \x20   if (g_fst) {{ fstWriterClose(g_fst); g_fst = 0; }}\n\
         }}\n\
         static signed sx_fst_open(const char *path) {{\n\
         \x20   g_fst = fstWriterCreate(path, 1);\n\
         \x20   if (!g_fst) {{ fprintf(stderr, \"cannot open FST output %s\\n\", path); return 0; }}\n\
         \x20   fstWriterSetPackType(g_fst, FST_WR_PT_LZ4);\n\
         \x20   fstWriterSetTimescale(g_fst, -15);\n\
         \x20   fstWriterSetVersion(g_fst, \"siox native test executable\");\n\
         {registration}\
         \x20   return 1;\n\
         }}\n\
         static void sx_fst_begin_test(void) {{\n\
         \x20   if (!g_fst) return;\n\
         \x20   g_fst_base = g_fst_started && g_fst_last_time != UINT64_MAX \
         ? g_fst_last_time + 1 : (g_fst_started ? UINT64_MAX : 0);\n\
         \x20   memset(g_fst_seen, 0, sizeof(g_fst_seen));\n\
         }}\n\
         static void sx_fst_sample(uint64_t now) {{\n\
         \x20   if (!g_fst) return;\n\
         \x20   uint64_t _time = UINT64_MAX - g_fst_base < now \
         ? UINT64_MAX : g_fst_base + now;\n\
         \x20   signed _wrote = 0;\n",
        u64::from(max_width) + 1
    );
    let timestamp = "if (!_wrote) { fstWriterEmitTimeChange(g_fst, _time); _wrote = 1; }";
    for (id, signal) in design.signals.iter().enumerate() {
        if companions.contains(&id) {
            continue;
        }
        let width = signal.width.max(1);
        let meta = design
            .meta_of
            .get(&(id as u32))
            .copied()
            .map(|value| value as usize);
        c.push_str(&format!("    {{ sx_value _v = sx_read({id});"));
        if let Some(meta_id) = meta {
            c.push_str(&format!(
                " sx_value _m = sx_read({meta_id}); if (!g_fst_seen[{id}] || _v != g_fst_last[{id}] || !g_fst_seen[{meta_id}] || _m != g_fst_last[{meta_id}]) {{ {timestamp} "
            ));
            let table = design
                .array_element_enums
                .get(&(id as u32))
                .map(String::as_str)
                .and_then(|name| logic_vcd_symbols_for_type(design, name))
                .unwrap_or_default();
            c.push_str(&format!(
                "for (signed _b = {}; _b >= 0; --_b) {{ unsigned _d = (unsigned)((_m >> (_b * 4)) & 15); signed _ch = 'x'; switch (_d) {{",
                width - 1
            ));
            let mut entries = table.into_iter().collect::<Vec<_>>();
            entries.sort_by_key(|(disc, _)| *disc);
            for (disc, ch) in entries {
                c.push_str(&format!("case {disc}: _ch = '{ch}'; break;"));
            }
            c.push_str(
                "} if (_ch != 'x' && _ch != 'z') _ch = ((_v >> _b) & 1) ? '1' : '0'; g_fst_value[",
            );
            c.push_str(&(width - 1).to_string());
            c.push_str(" - _b] = (char)_ch; } g_fst_value[");
            c.push_str(&width.to_string());
            c.push_str("] = 0; fstWriterEmitValueChange(g_fst, g_fst_handle[");
            c.push_str(&id.to_string());
            c.push_str("], g_fst_value); g_fst_last[");
            c.push_str(&meta_id.to_string());
            c.push_str("] = _m; g_fst_seen[");
            c.push_str(&meta_id.to_string());
            c.push_str("] = 1;");
        } else {
            c.push_str(&format!(
                " if (!g_fst_seen[{id}] || _v != g_fst_last[{id}]) {{ {timestamp} "
            ));
            if signal.real {
                c.push_str(&format!(
                    "double _d = sx_f64((uint64_t)_v); fstWriterEmitValueChange(g_fst, g_fst_handle[{id}], &_d);"
                ));
            } else if let Some(table) = logic_vcd_symbols(design, signal) {
                c.push_str("signed _ch = 'x'; switch ((uint64_t)_v) {");
                let mut entries = table.into_iter().collect::<Vec<_>>();
                entries.sort_by_key(|(disc, _)| *disc);
                for (disc, ch) in entries {
                    c.push_str(&format!("case {disc}ULL: _ch = '{ch}'; break;"));
                }
                c.push_str(&format!(
                    "}} g_fst_value[0] = (char)_ch; g_fst_value[1] = 0; fstWriterEmitValueChange(g_fst, g_fst_handle[{id}], g_fst_value);"
                ));
            } else if let Some(symbols) = signal
                .enum_type
                .as_ref()
                .and_then(|name| design.enum_syms.get(name))
            {
                c.push_str("const char *_s; switch ((uint64_t)_v) {");
                let mut entries = symbols.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(disc, _)| **disc);
                for (&disc, symbol) in entries {
                    c.push_str(&format!(
                        "case {disc}ULL: _s = \"{}\"; break;",
                        c_escape(symbol)
                    ));
                }
                c.push_str("default: snprintf(g_fst_value, sizeof g_fst_value, \"%llu\", (unsigned long long)_v); _s = g_fst_value; break; } fstWriterEmitVariableLengthValueChange(g_fst, g_fst_handle[");
                c.push_str(&id.to_string());
                c.push_str("], _s, (uint32_t)strlen(_s));");
            } else {
                c.push_str(&format!(
                    "for (signed _b = {}; _b >= 0; --_b) g_fst_value[{} - _b] = ((_v >> _b) & 1) ? '1' : '0'; g_fst_value[{}] = 0; fstWriterEmitValueChange(g_fst, g_fst_handle[{id}], g_fst_value);",
                    width - 1,
                    width - 1,
                    width
                ));
            }
        }
        c.push_str(&format!(
            " g_fst_last[{id}] = _v; g_fst_seen[{id}] = 1; }} }}\n"
        ));
    }
    c.push_str(
        "    if (_wrote) { g_fst_started = 1; g_fst_last_time = _time; }\n\
         }\n",
    );
    c
}

/// The libtest-style `main` that runs each `test_<name>` and reports results.
///
/// Accepts an optional name-substring filter plus `-o`/`--output <path>` for a
/// waveform. The format follows the path's extension: `.vcd` writes VCD and
/// anything else writes FST, so FST is the default without having to name it.
/// Passing `-o` twice with one of each extension writes both.
fn gen_main(names: &[(String, String)]) -> String {
    let mut m = String::new();
    // The waveform format is chosen by the path's extension rather than by a
    // separate flag: `.vcd` writes VCD, anything else writes FST. FST is the
    // richer format and the better default, and `-o wave.vcd` is a shorter way
    // to ask for the other one than remembering two flags.
    m.push_str(
        "static signed sx_is_vcd(const char *path) {\n\
         \x20   const char *dot = 0;\n\
         \x20   for (const char *p = path; *p; p++) if (*p == '.') dot = p;\n\
         \x20   if (!dot) return 0;\n\
         \x20   return (dot[1] == 'v' || dot[1] == 'V') && (dot[2] == 'c' || dot[2] == 'C')\n\
         \x20       && (dot[3] == 'd' || dot[3] == 'D') && !dot[4];\n\
         }\n",
    );
    m.push_str("signed main(signed argc, char **argv) {\n");
    m.push_str(
        "    const char *filter = 0, *vcd_path = 0, *fst_path = 0;\n\
         \x20   for (signed i = 1; i < argc; i++) {\n\
         \x20       const char *path = 0;\n\
         \x20       if (!strcmp(argv[i], \"-o\") || !strcmp(argv[i], \"--output\")) {\n\
         \x20           if (++i == argc) { fprintf(stderr, \"%s requires a path\\n\", argv[i - 1]); return 2; }\n\
         \x20           path = argv[i];\n\
         \x20       } else if (!strncmp(argv[i], \"--output=\", 9)) path = argv[i] + 9;\n\
         \x20       else if (!strncmp(argv[i], \"-o\", 2) && argv[i][2]) path = argv[i] + 2;\n\
         \x20       else if (!filter) { filter = argv[i]; continue; }\n\
         \x20       else { fprintf(stderr, \"unexpected argument: %s\\n\", argv[i]); return 2; }\n\
         \x20       if (sx_is_vcd(path)) vcd_path = path; else fst_path = path;\n\
         \x20   }\n\
         \x20   if (vcd_path && !sx_vcd_open(vcd_path)) return 2;\n\
         \x20   if (fst_path && !sx_fst_open(fst_path)) { sx_vcd_close(); return 2; }\n",
    );
    m.push_str("    signed failed = 0, ran = 0, filtered = 0;\n");
    // Count how many tests match, so the "running N tests" line is post-filter.
    for (_, display) in names {
        m.push_str(&format!(
            "    if (!filter || strstr(\"{display}\", filter)) ran++; else filtered++;\n"
        ));
    }
    m.push_str("    printf(\"\\nrunning %d test%s\\n\", ran, ran == 1 ? \"\" : \"s\");\n");
    for (symbol, display) in names {
        m.push_str(&format!(
            "    if (!filter || strstr(\"{display}\", filter)) {{ \
             if (test_{symbol}()) {{ printf(\"test {display} ... FAILED\\n    %s\\n\", g_msg); \
             if (g_loc) printf(\"  --> %s\\n\", g_loc); failed++; }} \
             else printf(\"test {display} ... ok\\n\"); }}\n"
        ));
    }
    m.push_str(
        "    printf(\"\\ntest result: %s. %d passed; %d failed; %d filtered out\",\n\
         \x20          failed ? \"FAILED\" : \"ok\", ran - failed, failed, filtered);\n\
         \x20   if (g_warnings) printf(\"; %d warning%s\", g_warnings, g_warnings == 1 ? \"\" : \"s\");\n\
         \x20   printf(\"\\n\");\n",
    );
    m.push_str(
        "    sx_io_reset();\n    sx_fst_close();\n    sx_vcd_close();\n    return failed ? 1 : 0;\n}\n",
    );
    m
}

/// Translation context: the design, this test's name -> signal map, and enum
/// discriminants.
struct Ctx<'a> {
    design: &'a Design,
    /// Source text, so a runtime failure can name the line that caused it
    /// rather than only what went wrong.
    sources: &'a siox::diag::SourceMap,
    /// Emit `#line` directives so a debugger sees `.siox` rather than the
    /// intermediate C.
    debug: bool,
    map: &'a HashMap<String, SignalId>,
    enums: &'a HashMap<String, HashMap<String, u64>>,
    families: &'a std::collections::HashSet<String>,
    /// Collision-safe root storage path (qualified only when needed).
    name: &'a str,
    /// Injective C identifier suffix for this test function.
    symbol: &'a str,
    /// `clock(clk, ..)`-registered background clocks: (signal id, half period fs).
    clocks: Vec<(u32, u64)>,
    /// Names currently bound as C locals (unconnected `let`s, loop variables).
    locals: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Declared bit width of a C local (`let c: unsigned[8]` -> 8): writes mask to
    /// it so arithmetic wraps exactly like the equivalent hardware signal.
    local_widths: std::cell::RefCell<HashMap<String, u32>>,
    /// Array/string name -> logical indices in declaration order. Width-only
    /// arrays are `0..N-1`; explicit ranges preserve either direction.
    local_indices: std::cell::RefCell<HashMap<String, Vec<i64>>>,
    /// Declared `(left, right)` bounds for range attributes, preserving
    /// direction independently from flattened storage order.
    local_ranges: std::cell::RefCell<HashMap<String, (i64, i64)>>,
    /// Runtime-owned `string` locals produced by `read<string>`. Their
    /// element count is not known while C is generated, so they use one
    /// `sx_dyn_array` rather than flattened per-character locals.
    dynamic_strings: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Declared nominal array family (`let a: signed[8]` -> "signed"),
    /// connected or local — operators on it inline the family's impls.
    local_families: std::cell::RefCell<HashMap<String, String>>,
    /// Operator-trait impls `(trait, type) -> fn`, mirroring the runner.
    op_impls: &'a NativeOperatorImpls<'a>,
    /// Impl methods `(type head, method) -> fn`, for `recv.method(args)`.
    methods: &'a HashMap<(String, String), &'a ast::FnDecl>,
    /// Base-first nominal field order for syntax-only values that have no
    /// concrete IR path. This intentionally carries no widths or nested shape.
    struct_field_names: &'a HashMap<String, StructFieldNames>,
    /// Derived-type inherited widths (`struct Byte : Logic[8]` -> 8), so a bare
    /// derived-vector local masks to the right width.
    derived_widths: &'a HashMap<String, u32>,
    /// Serial for the temporaries a composite assignment stages its values in.
    /// Writing a composite element by element lets a later element read one
    /// already written, so `a = [a[2], a[0], a[1]]` rotated every slot to the
    /// same value. Every right-hand side is evaluated first, then assigned.
    tmp_seq: std::cell::Cell<u32>,
    /// Declared type head of a testbench local (`let p: Pkt` -> "Pkt"), for
    /// resolving a method call's receiver type.
    local_types: std::cell::RefCell<HashMap<String, String>>,
    /// Module-level `const` values keyed by resolved qualified identity, plus
    /// implementation-local constants keyed by their local leaf.
    const_exprs: &'a HashMap<String, String>,
    /// Module-level `const` lookup tables, one expression per element.
    const_array_exprs: &'a HashMap<String, Vec<String>>,
    /// Integer constants, also usable as local type widths.
    consts: &'a HashMap<String, u128>,
    /// Module constants declared as `real`; their stored C expressions are f64
    /// bit patterns and must be decoded when used as operands.
    real_consts: &'a std::collections::HashSet<String>,
    /// Module constants declared as kernel `integer`.
    integer_consts: &'a std::collections::HashSet<String>,
    /// Named range constants used by local type indices.
    const_ranges: &'a HashMap<String, (i64, i64)>,
    /// Testbench name -> EVERY connected port's signal id (a write drives all).
    aliases: &'a HashMap<String, Vec<SignalId>>,
    /// Unique-suffix counter for generated C identifiers.
    tmp: std::cell::Cell<usize>,
    /// Unique local-static buffer suffix for formatted assertion/warning
    /// messages. Each call owns storage sized from its actual format arity.
    message_id: std::cell::Cell<usize>,
    /// Module-level functions (testbench-callable; translated to C ternaries)
    /// indexed by resolved identity, plus static associated functions.
    fns: &'a FunctionIndex<'a>,
    /// Resolver identity used for nominal entity tables in the native harness.
    resolved: &'a Resolved,
    /// Module type aliases, used when deciding native and foreign ABI kinds.
    type_aliases: &'a HashMap<String, ast::Type>,
    /// Parameter-substitution stack while translating a fn body.
    fn_env: std::cell::RefCell<Vec<HashMap<String, String>>>,
    /// Declared parameter types parallel to `fn_env`, retained while inlining
    /// user functions so named real parameters keep floating-point semantics.
    fn_type_env: std::cell::RefCell<Vec<HashMap<String, String>>>,
    /// Names elaboration turned into DUT instances (`let dut: Sub = {..}` /
    /// `let dut: Sub [= {..}]`) — their `let`s are wired by elaboration and
    /// emit no testbench code.
    instance_names: std::collections::HashSet<String>,
    /// Entity identity -> its ports in declaration order, so a *positional*
    /// connection (`{ x, 4 }`) resolves to the port it fills and an
    /// instance's ports can be given the family their type declares.
    entity_ports: HashMap<DefId, Vec<(String, ast::Type)>>,
    /// Harness-wide `_BitInt` width, also the upper bound for decimal
    /// formatting capacity.
    value_bits: u32,
}

/// One field/index component of a flattened native aggregate access. The IR
/// lowerer has its own typed equivalent; this one operates on emitted C
/// storage paths.
enum NativeAccessStep<'e> {
    Field(&'e str),
    Index(&'e ast::Expr),
}

struct DynamicTargetVariant {
    expression: ast::Expr,
    condition: Option<String>,
}

/// Base-first nominal field order (`struct B : A` prepends A's fields). This
/// supports positional source syntax only; it is not a type/storage layout.
fn collect_struct_field_names(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, StructFieldNames> {
    let mut raw: RawStructFieldNames = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Struct(s) = item {
                let base = s.base.as_ref().and_then(|ty| fns.type_head_key(ty));
                let own = s.fields.iter().map(|f| f.name.text.clone()).collect();
                raw.insert(fns.struct_decl_key(&s.name), (base, own));
            }
        }
    }
    fn flat(
        name: &str,
        raw: &RawStructFieldNames,
        seen: &mut std::collections::HashSet<String>,
    ) -> StructFieldNames {
        if !seen.insert(name.to_string()) {
            return Vec::new();
        }
        let Some((base, own)) = raw.get(name) else {
            return Vec::new();
        };
        let mut out = match base {
            Some(b) => flat(b, raw, seen),
            None => Vec::new(),
        };
        out.extend(own.iter().cloned());
        seen.remove(name);
        out
    }
    raw.keys()
        .map(|k| {
            (
                k.clone(),
                flat(k, &raw, &mut std::collections::HashSet::new()),
            )
        })
        .collect()
}

fn max_literal_type_width(
    modules: &[Module],
    derived: &HashMap<String, u32>,
    fns: &FunctionIndex<'_>,
) -> u32 {
    fn literal_width(text: &str) -> u32 {
        if text.contains('.') {
            return 64;
        }
        let words = parse_word_literal(text);
        let high = words.last().copied().unwrap_or(0);
        ((words.len().saturating_sub(1) as u32) * 64 + (64 - high.leading_zeros())).max(1)
    }

    fn expr_width(expression: &ast::Expr) -> u32 {
        use ast::Expr;
        match expression {
            Expr::Int { text, .. } | Expr::SuffixLit { text, .. } => literal_width(text),
            Expr::BitStrLit { base, digits, .. } => {
                let bits = siox::syntax::bits_per_digit(base.to_ascii_lowercase());
                u32::try_from(siox::syntax::radix_digits(digits).count())
                    .ok()
                    .and_then(|count| count.checked_mul(bits))
                    .unwrap_or(u32::MAX)
                    .max(1)
            }
            Expr::Field { base, .. }
            | Expr::SysAttr { base, .. }
            | Expr::Unary { rhs: base, .. } => expr_width(base),
            Expr::Index { base, index, .. } => {
                let mut width = expr_width(base).max(expr_width(index));
                // A conversion-shaped `unsigned[128](...)` needs a 128-bit C
                // value even when no design signal has that width. Treating a
                // plain constant index the same way is conservative and keeps
                // this syntax-only scan independent of type resolution.
                if matches!(base.as_ref(), Expr::Path(_)) {
                    if let Expr::Int { text, .. } = index.as_ref() {
                        width = width.max(text.replace('_', "").parse().unwrap_or(0));
                    }
                }
                width
            }
            Expr::Range { lo, hi, .. } => expr_width(lo).max(expr_width(hi)),
            Expr::PartialRange { lo, hi, .. } => lo
                .as_deref()
                .map(expr_width)
                .unwrap_or(0)
                .max(hi.as_deref().map(expr_width).unwrap_or(0)),
            Expr::Binary { lhs, rhs, .. } => expr_width(lhs).max(expr_width(rhs)),
            Expr::IfExpr {
                cond, then, els, ..
            } => expr_width(cond).max(expr_width(then)).max(expr_width(els)),
            Expr::Match {
                scrutinee, arms, ..
            } => arms.iter().fold(expr_width(scrutinee), |width, arm| {
                width.max(block_width(&arm.body))
            }),
            Expr::Call { callee, args, .. } => args
                .iter()
                .fold(expr_width(callee), |width, arg| width.max(expr_width(arg))),
            Expr::Construct { args, spread, .. } => {
                let width = args
                    .iter()
                    .filter_map(|argument| argument.value.as_ref())
                    .fold(0, |width, value| width.max(expr_width(value)));
                width.max(spread.as_deref().map(expr_width).unwrap_or(0))
            }
            Expr::Concat { parts, .. } => parts
                .iter()
                .map(expr_width)
                .try_fold(0u32, u32::checked_add)
                .unwrap_or(u32::MAX),
            Expr::Array { elems, .. } => elems.iter().map(expr_width).max().unwrap_or(0),
            Expr::CharLit { .. } | Expr::StrLit { .. } | Expr::Path(_) => 0,
        }
    }

    fn stmt_width(statement: &ast::Stmt) -> u32 {
        match statement {
            ast::Stmt::Let(declaration) => declaration.value.as_ref().map(expr_width).unwrap_or(0),
            ast::Stmt::Assign {
                target,
                value,
                after,
                ..
            } => expr_width(target)
                .max(expr_width(value))
                .max(after.as_ref().map(expr_width).unwrap_or(0)),
            ast::Stmt::If(statement) => {
                let else_width = match statement.else_.as_deref() {
                    Some(ast::ElseBranch::Block(block)) => block_width(block),
                    Some(ast::ElseBranch::If(statement)) => if_width(statement),
                    None => 0,
                };
                expr_width(&statement.cond)
                    .max(block_width(&statement.then))
                    .max(else_width)
            }
            ast::Stmt::Match(statement) => statement
                .arms
                .iter()
                .fold(expr_width(&statement.scrutinee), |width, arm| {
                    width.max(block_width(&arm.body))
                }),
            ast::Stmt::For { range, body, .. } => expr_width(range).max(block_width(body)),
            ast::Stmt::Expr(expression) => expr_width(expression),
            ast::Stmt::Return { value, .. } => value.as_ref().map(expr_width).unwrap_or(0),
        }
    }

    fn if_width(statement: &ast::IfStmt) -> u32 {
        let else_width = match statement.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => block_width(block),
            Some(ast::ElseBranch::If(statement)) => if_width(statement),
            None => 0,
        };
        expr_width(&statement.cond)
            .max(block_width(&statement.then))
            .max(else_width)
    }

    fn block_width(block: &ast::Block) -> u32 {
        block.stmts.iter().map(stmt_width).max().unwrap_or(0)
    }

    fn width(ty: &ast::Type, derived: &HashMap<String, u32>, fns: &FunctionIndex<'_>) -> u32 {
        match ty {
            ast::Type::Path(p) => fns
                .struct_path_key(p)
                .and_then(|key| derived.get(&key))
                .copied()
                .unwrap_or(0),
            ast::Type::Indexed { base, index, .. } => {
                let own = index
                    .as_deref()
                    .and_then(|e| match e {
                        ast::Expr::Int { text, .. } => text.replace('_', "").parse().ok(),
                        ast::Expr::Range { lo, hi, .. } => {
                            let ast::Expr::Int { text: lo, .. } = lo.as_ref() else {
                                return None;
                            };
                            let ast::Expr::Int { text: hi, .. } = hi.as_ref() else {
                                return None;
                            };
                            let lo = lo.replace('_', "").parse::<i64>().ok()? as i128;
                            let hi = hi.replace('_', "").parse::<i64>().ok()? as i128;
                            u32::try_from((lo - hi).unsigned_abs().saturating_add(1)).ok()
                        }
                        _ => None,
                    })
                    .unwrap_or(0);
                own.max(width(base, derived, fns))
            }
            ast::Type::Generic { base, .. } | ast::Type::View { target: base, .. } => {
                width(base, derived, fns)
            }
        }
    }
    let mut widths = Vec::new();
    for module in modules {
        for item in &module.items {
            match item {
                ast::Item::Entity(e) => {
                    widths.extend(e.ports.iter().map(|p| width(&p.ty, derived, fns)))
                }
                ast::Item::Struct(s) => {
                    widths.extend(s.fields.iter().map(|f| width(&f.ty, derived, fns)))
                }
                ast::Item::Fn(f) => {
                    widths.extend(
                        f.params
                            .iter()
                            .filter_map(|p| p.ty.as_ref())
                            .map(|t| width(t, derived, fns)),
                    );
                    widths.extend(f.ret.as_ref().map(|t| width(t, derived, fns)));
                    widths.extend(f.body.as_ref().map(block_width));
                }
                ast::Item::Const(constant) => {
                    widths.push(width(&constant.ty, derived, fns));
                    widths.push(expr_width(&constant.value));
                }
                ast::Item::Enum(en) => {
                    widths.extend(
                        en.variants
                            .iter()
                            .filter_map(|variant| variant.value.as_ref())
                            .map(expr_width),
                    );
                }
                ast::Item::Impl(im) => {
                    for item in &im.items {
                        match item {
                            ast::ImplItem::Let(l) => {
                                widths.extend(l.ty.as_ref().map(|t| width(t, derived, fns)));
                                widths.extend(l.value.as_ref().map(expr_width));
                            }
                            ast::ImplItem::Const(constant) => {
                                widths.push(width(&constant.ty, derived, fns));
                                widths.push(expr_width(&constant.value));
                            }
                            ast::ImplItem::Fn(f) => {
                                widths.extend(
                                    f.params
                                        .iter()
                                        .filter_map(|p| p.ty.as_ref())
                                        .map(|t| width(t, derived, fns)),
                                );
                                widths.extend(f.ret.as_ref().map(|t| width(t, derived, fns)));
                                widths.extend(f.body.as_ref().map(block_width));
                            }
                            ast::ImplItem::Process(process) => {
                                widths.push(block_width(&process.body))
                            }
                            ast::ImplItem::Stmt(statement) => widths.push(stmt_width(statement)),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    widths.into_iter().max().unwrap_or(0)
}

/// Every impl method by `(type head, method name)`, inherent and trait impls,
/// mirroring the runner's `collect_methods`.
fn collect_methods<'a>(
    modules: &'a [Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<(String, String), &'a ast::FnDecl> {
    let mut out = HashMap::new();
    let mut traits: HashMap<String, &ast::TraitDecl> = HashMap::new();
    // (type head, trait name) for each `impl Trait for Type`.
    let mut implemented: Vec<(String, String)> = Vec::new();
    for m in modules {
        for item in &m.items {
            match item {
                ast::Item::Trait(t) => {
                    traits.insert(fns.trait_decl_key(&t.name), t);
                }
                ast::Item::Impl(im) => {
                    if let Some(ty) = fns.type_head_key(&im.target) {
                        for it in &im.items {
                            if let ast::ImplItem::Fn(f) = it {
                                out.entry((ty.clone(), f.name.text.clone())).or_insert(f);
                            }
                        }
                        if let Some(tr) =
                            im.trait_.as_ref().and_then(|path| fns.trait_path_key(path))
                        {
                            implemented.push((ty, tr));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // A trait method with a body is a default the impl may omit (spec 3.20),
    // so the implementing type inherits it. The impl's own methods went in
    // above, and `or_insert` keeps them, so an override always wins.
    for (ty, tr) in implemented {
        let Some(t) = traits.get(&tr) else { continue };
        for f in t.items.iter().filter(|f| f.body.is_some()) {
            out.entry((ty.clone(), f.name.text.clone())).or_insert(f);
        }
    }
    out
}

/// The constructed type and literal path of a `read<T>(path)` call.
fn fs_read_call(e: &ast::Expr) -> Option<(&ast::Type, String)> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        ..
    } = e
    else {
        return None;
    };
    let ast::Expr::Path(p) = callee.as_ref() else {
        return None;
    };
    if p.segments.len() != 1 || p.segments[0].text != "read" {
        return None;
    }
    match (type_args.as_slice(), args.as_slice()) {
        ([requested], [ast::Expr::StrLit { text, .. }]) => Some((requested, text.clone())),
        _ => None,
    }
}

/// An injective C identifier for a testbench-local source path.
///
/// Every byte is encoded, including bytes that C would accept unchanged. This
/// keeps user names out of the harness namespace and prevents flattened paths
/// such as `a_b.c` and `a.b_c` from collapsing onto the same identifier.
fn c_local_ident(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(3 + name.len() * 3);
    encoded.push_str("sxl");
    for byte in name.bytes() {
        encoded.push('_');
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

/// Escape text for embedding inside a C string literal: backslash first,
/// then quote, newline, tab, CR (a raw newline would split the literal).
fn c_escape(t: &str) -> String {
    t.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// Evaluate a module `const` initializer for the native testbench: literals,
/// logic chars, enum variants, other consts, and const-fn arithmetic.
fn eval_c_const(
    e: &ast::Expr,
    consts: &HashMap<String, u128>,
    enums: &HashMap<String, HashMap<String, u64>>,
    fns: &FunctionIndex<'_>,
) -> Option<u128> {
    match e {
        ast::Expr::Int { text, .. } => {
            let words = parse_word_literal(text);
            (words.len() <= 2).then(|| {
                words.first().copied().unwrap_or(0) as u128
                    | ((words.get(1).copied().unwrap_or(0) as u128) << 64)
            })
        }
        ast::Expr::CharLit { ch, .. } => Some(logic_lit_value(*ch, enums) as u128),
        ast::Expr::Path(path) => fns
            .constant_path_key(path)
            .and_then(|key| consts.get(&key).copied())
            .or_else(|| enum_variant_value(path, enums, fns).map(u128::from)),
        ast::Expr::Unary { op, rhs, .. } => {
            let value = eval_c_const(rhs, consts, enums, fns)?;
            Some(match op {
                ast::UnOp::Neg => 0u128.wrapping_sub(value),
                ast::UnOp::Not => u128::from(value == 0),
            })
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let left = eval_c_const(lhs, consts, enums, fns)?;
            let right = eval_c_const(rhs, consts, enums, fns)?;
            Some(match op {
                ast::BinOp::Add => left.wrapping_add(right),
                ast::BinOp::Sub => left.wrapping_sub(right),
                ast::BinOp::Mul => left.wrapping_mul(right),
                ast::BinOp::Div if right != 0 => left / right,
                ast::BinOp::Shl => left.checked_shl(right.try_into().ok()?).unwrap_or(0),
                ast::BinOp::Shr => left.checked_shr(right.try_into().ok()?).unwrap_or(0),
                ast::BinOp::Eq => u128::from(left == right),
                ast::BinOp::Ne => u128::from(left != right),
                ast::BinOp::Lt => u128::from(left < right),
                ast::BinOp::Le => u128::from(left <= right),
                ast::BinOp::Gt => u128::from(left > right),
                ast::BinOp::Ge => u128::from(left >= right),
                ast::BinOp::And => left & right,
                ast::BinOp::Or => left | right,
                _ => return None,
            })
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_c_const(cond, consts, enums, fns)? != 0 {
                eval_c_const(then, consts, enums, fns)
            } else {
                eval_c_const(els, consts, enums, fns)
            }
        }
        _ => {
            let env: HashMap<String, i64> =
                consts.iter().map(|(k, &v)| (k.clone(), v as i64)).collect();
            siox::ir::eval_const_fns(e, &env, fns, 0).map(|v| v as u128)
        }
    }
}

fn enum_variant_value(
    path: &ast::Path,
    enums: &HashMap<String, HashMap<String, u64>>,
    fns: &FunctionIndex<'_>,
) -> Option<u64> {
    let (enumeration, variant) = fns.enum_variant_key(path)?;
    enums
        .get(&enumeration)
        .and_then(|variants| variants.get(&variant))
        .copied()
}

fn emit_c_const(
    expression: &ast::Expr,
    constants: &HashMap<String, String>,
    enums: &HashMap<String, HashMap<String, u64>>,
    fns: &FunctionIndex<'_>,
) -> Option<String> {
    match expression {
        ast::Expr::Int { text, .. } => Some(c_word_literal(&parse_word_literal(text))),
        ast::Expr::CharLit { ch, .. } => {
            Some(format!("((sx_value){})", logic_lit_value(*ch, enums)))
        }
        ast::Expr::Path(path) => fns
            .constant_path_key(path)
            .and_then(|key| constants.get(&key).cloned())
            .or_else(|| {
                enum_variant_value(path, enums, fns).map(|value| format!("((sx_value){value}ULL)"))
            }),
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => {
            let rhs = emit_c_const(rhs, constants, enums, fns)?;
            Some(format!("(-({rhs}))"))
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let lhs = emit_c_const(lhs, constants, enums, fns)?;
            let rhs = emit_c_const(rhs, constants, enums, fns)?;
            match op {
                ast::BinOp::Div => Some(format!("sx_udiv(({lhs}), ({rhs}))")),
                ast::BinOp::Shl => Some(format!("sx_shl(({lhs}), ({rhs}))")),
                ast::BinOp::Shr => Some(format!("sx_shr(({lhs}), ({rhs}))")),
                _ => Some(format!("(({lhs}) {} ({rhs}))", c_binop(op).ok()?)),
            }
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => Some(format!(
            "(({}) ? ({}) : ({}))",
            emit_c_const(cond, constants, enums, fns)?,
            emit_c_const(then, constants, enums, fns)?,
            emit_c_const(els, constants, enums, fns)?
        )),
        ast::Expr::SuffixLit { text, suffix, .. } => {
            let value = c_word_literal(&parse_word_literal(text));
            let scale = ast::suffix_scale(&suffix.text).unwrap_or(1);
            Some(format!("(({value}) * {scale}ULL)"))
        }
        _ => None,
    }
}

/// Emit a module `real` constant as a C `double` expression. Named real
/// dependencies are stored as f64 bit patterns in `consts`, so decode them;
/// integer-shaped dependencies promote normally.
fn emit_c_real_const(
    e: &ast::Expr,
    consts: &HashMap<String, String>,
    real_consts: &std::collections::HashSet<String>,
    enums: &HashMap<String, HashMap<String, u64>>,
    fns: &FunctionIndex<'_>,
) -> Option<String> {
    match e {
        ast::Expr::Int { text, .. } => {
            let normalized = text.replace('_', "");
            normalized
                .parse::<f64>()
                .ok()
                .map(|_| format!("((double)({normalized}))"))
        }
        ast::Expr::Path(path) => {
            let key = fns.constant_path_key(path)?;
            let value = consts.get(&key)?;
            Some(if real_consts.contains(&key) {
                format!("sx_f64({value})")
            } else {
                format!("((double)({value}))")
            })
        }
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => Some(format!(
            "(-({}))",
            emit_c_real_const(rhs, consts, real_consts, enums, fns)?
        )),
        ast::Expr::Binary { op, lhs, rhs, .. }
            if matches!(
                op,
                ast::BinOp::Add | ast::BinOp::Sub | ast::BinOp::Mul | ast::BinOp::Div
            ) =>
        {
            Some(format!(
                "(({}) {} ({}))",
                emit_c_real_const(lhs, consts, real_consts, enums, fns)?,
                c_binop(op).ok()?,
                emit_c_real_const(rhs, consts, real_consts, enums, fns)?
            ))
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => Some(format!(
            "(({}) ? ({}) : ({}))",
            emit_c_const(cond, consts, enums, fns)?,
            emit_c_real_const(then, consts, real_consts, enums, fns)?,
            emit_c_real_const(els, consts, real_consts, enums, fns)?
        )),
        ast::Expr::SuffixLit { text, suffix, .. }
            if suffix.text == "Hz" || suffix.text.ends_with("Hz") =>
        {
            let scale = ast::suffix_scale(&suffix.text)?;
            Some(format!("((double)({}) * {scale}.0)", text.replace('_', "")))
        }
        _ => None,
    }
}

/// Wrap a C expression so it masks to `w` bits (wrap at 2^w).
/// Whether an integer literal's value fits a 64-bit word, so it can take the
/// kernel-integer rendering rather than the arbitrary-width one.
fn literal_fits_word(text: &str) -> bool {
    let clean = text.replace('_', "");
    let parsed = if let Some(hex) = clean.strip_prefix("0x").or(clean.strip_prefix("0X")) {
        u128::from_str_radix(hex, 16)
    } else if let Some(bin) = clean.strip_prefix("0b").or(clean.strip_prefix("0B")) {
        u128::from_str_radix(bin, 2)
    } else {
        clean.parse::<u128>()
    };
    parsed.is_ok_and(|v| v <= u128::from(u64::MAX))
}

fn mask_c(e: &str, w: u32) -> String {
    if w > 0 {
        format!("sx_mask(({e}), {w})")
    } else {
        e.to_string()
    }
}

fn c_word_literal(words: &[u64]) -> String {
    let parts = words
        .iter()
        .enumerate()
        .filter(|(_, word)| **word != 0)
        .map(|(i, word)| {
            if i == 0 {
                format!("((sx_value){word}ULL)")
            } else {
                format!("(((sx_value){word}ULL) << {})", i * 64)
            }
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "((sx_value)0)".to_string()
    } else {
        format!("({})", parts.join(" | "))
    }
}

fn parse_word_literal(text: &str) -> Vec<u64> {
    let text = text.trim().replace('_', "");
    if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        parse_digits_words(digits, 16)
    } else if let Some(digits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        parse_digits_words(digits, 2)
    } else {
        parse_digits_words(&text, 10)
    }
}

fn parse_digits_words(digits: &str, radix: u32) -> Vec<u64> {
    let mut words = Vec::<u64>::new();
    for digit in siox::syntax::radix_digits(digits) {
        let Some(digit) = digit.to_digit(radix) else {
            return vec![0];
        };
        let mut carry = digit as u128;
        for word in &mut words {
            let next = (*word as u128) * radix as u128 + carry;
            *word = next as u64;
            carry = next >> 64;
        }
        if carry != 0 || words.is_empty() {
            words.push(carry as u64);
        }
    }
    words
}

impl Ctx<'_> {
    fn checked_index_c(
        &self,
        expression: String,
        index: &ast::Expr,
        left: i64,
        right: i64,
    ) -> String {
        let location = span_location(self.sources, ast::expr_span(index))
            .map(|at| format!("\"{}\"", c_escape(&at)))
            .unwrap_or_else(|| "0".to_string());
        format!("sx_checked_index((int64_t)({expression}), {left}LL, {right}LL, {location})")
    }

    fn type_is(&self, ty: &ast::Type, expected: &str) -> bool {
        resolved_type_name(ty, self.type_aliases, self.fns).as_deref() == Some(expected)
    }

    fn type_name_is(&self, name: &str, expected: &str) -> bool {
        name == expected
            || self
                .type_aliases
                .get(name)
                .is_some_and(|ty| self.type_is(ty, expected))
    }

    /// Whether this spelling is `string` or an alias chain that reaches it.
    /// Stop at the public string name before following its `Char[]` alias;
    /// `type_is` intentionally resolves through that alias for scalar ABI
    /// classification and therefore cannot answer this surface-type question.
    fn type_is_string(&self, ty: &ast::Type) -> bool {
        let mut current = ty;
        let mut seen = std::collections::HashSet::new();
        loop {
            if is_single_string_type(current) {
                return true;
            }
            let ast::Type::Path(path) = current else {
                return false;
            };
            let Some(name) = self.fns.type_alias_path_key(path) else {
                return false;
            };
            if !seen.insert(name.clone()) {
                return false;
            }
            let Some(alias) = self.type_aliases.get(&name) else {
                return false;
            };
            current = alias;
        }
    }

    fn resolved_sized_string_indices(&self, ty: &ast::Type) -> Option<Vec<i64>> {
        let mut current = ty;
        let mut seen = std::collections::HashSet::new();
        loop {
            if let Some(indices) =
                sized_string_indices(current, self.const_ranges, self.consts, self.fns)
            {
                return Some(indices);
            }
            let ast::Type::Path(path) = current else {
                return None;
            };
            let name = self.fns.type_alias_path_key(path)?;
            if !seen.insert(name.clone()) {
                return None;
            }
            current = self.type_aliases.get(&name)?;
        }
    }

    fn emit_runtime_storage_write(
        &self,
        path: &str,
        value: &str,
        b: &mut String,
        indent: &str,
    ) -> Result<(), String> {
        if let Some(id) = self.map.get(path) {
            b.push_str(&format!("{indent}sx_set({}, {value});\n", id.0));
            for extra in self.alias_ids_beyond(path, &id.0.to_string()) {
                b.push_str(&format!("{indent}sx_set({extra}, {value});\n"));
            }
            return Ok(());
        }
        if self.locals.borrow().contains(path) {
            b.push_str(&format!("{indent}{} = {value};\n", c_local_ident(path)));
            return Ok(());
        }
        Err(format!(
            "runtime file target `{path}` has no native storage"
        ))
    }

    /// A testbench local initialized from a runtime `std::fs` read. Fixed raw
    /// arrays retain their declared labels/element widths; an unconstrained
    /// string owns a dynamic code-point buffer. Hardware/top initializers are
    /// still folded by IR as ROM images — this path is only generated for a
    /// `#[test]` body.
    fn try_declare_fs_read_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        // A runtime file failure names the `let` that asked for the file, the
        // way an assertion names its own statement. Set once here rather than
        // at each of the failure paths below, which are all this declaration's.
        let at = self
            .set_location(l.span)
            .map(|s| format!(" {s}"))
            .unwrap_or_default();
        let at = at.as_str();
        let Some(value) = &l.value else {
            return Ok(false);
        };
        let name = &l.name.text;
        let Some(ty) = &l.ty else {
            return Err(format!("runtime file local `{name}` needs a declared type"));
        };
        let serial = self.tmp.get();
        self.tmp.set(serial + 1);

        let Some((requested, path)) = fs_read_call(value) else {
            return Ok(false);
        };

        if self.type_is_string(requested) {
            if !self.type_is_string(ty) {
                return Err(format!("read<string>(\"{path}\") needs a `string` target"));
            }
            let full = c_escape(&self.design.base_dir.join(&path).to_string_lossy());
            if let Some(indices) = self.resolved_sized_string_indices(ty) {
                self.declare_typed_storage(name, ty, b)?;
                b.push_str(&format!(
                    "    sx_dyn_array _fst{serial} = sx_read_text(\"{full}\");\n\
                         if (g_io_failed) {{ g_msg = g_io_message;{at} return 1; }}\n\
                         if (_fst{serial}.length > {}) {{\n\
                             snprintf(g_io_message, sizeof g_io_message, \
                     \"read<string>(\\\"{}\\\"): %zu characters do not fit a {}-element string\", \
                     _fst{serial}.length); g_msg = g_io_message;{at} return 1;\n\
                         }}\n",
                    indices.len(),
                    c_escape(&path),
                    indices.len()
                ));
                for (position, index) in indices.into_iter().enumerate() {
                    let key = format!("{name}[{index}]");
                    self.emit_runtime_storage_write(
                        &key,
                        &format!(
                            "({position} < _fst{serial}.length ? _fst{serial}.values[{position}] : 0)"
                        ),
                        b,
                        "    ",
                    )?;
                }
                return Ok(true);
            }
            self.local_types
                .borrow_mut()
                .insert(name.clone(), "string".into());
            self.dynamic_strings.borrow_mut().insert(name.clone());
            b.push_str(&format!(
                "    sx_dyn_array {} = sx_read_text(\"{full}\");\n\
                     if (g_io_failed) {{ g_msg = g_io_message;{at} return 1; }}\n",
                c_local_ident(name)
            ));
            return Ok(true);
        }

        self.declare_typed_storage(name, ty, b)?;
        let targets = self
            .persisted_layout(name)
            .and_then(|layout| match &layout.kind {
                LayoutKind::Array {
                    range: Some(range), ..
                } => Some(*range),
                _ => None,
            })
            .map(|range| {
                directional_indices(range.left, range.right)
                    .into_iter()
                    .map(|index| format!("{name}[{index}]"))
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                self.array_parts(ty).map(|(_, indices)| {
                    indices
                        .into_iter()
                        .map(|index| format!("{name}[{index}]"))
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_else(|| vec![name.clone()]);
        let first_key = targets.first().cloned().unwrap_or_else(|| name.clone());
        if !self.locals.borrow().contains(&first_key) && !self.map.contains_key(&first_key) {
            return Err(format!(
                "read<{}>(\"{path}\") needs scalar or packed-vector storage",
                siox::syntax::pretty::type_str(requested)
            ));
        }
        let width = self
            .name_width(&first_key)
            .unwrap_or_else(|| if self.type_is(ty, "integer") { 64 } else { 8 })
            .max(1);
        let bytes = width.div_ceil(8);
        let full = c_escape(&self.design.base_dir.join(&path).to_string_lossy());
        let storage = targets.len().max(1);
        let operation = c_escape(&format!(
            "read<{}>",
            siox::syntax::pretty::type_str(requested)
        ));
        b.push_str(&format!(
            "    sx_value _fsr{serial}[{storage}];\n\
                 if (!sx_read_values(\"{operation}\", \"{full}\", _fsr{serial}, {}, {bytes}, {width})) \
             {{ g_msg = g_io_message;{at} return 1; }}\n",
            targets.len()
        ));
        for (position, key) in targets.into_iter().enumerate() {
            self.emit_runtime_storage_write(&key, &format!("_fsr{serial}[{position}]"), b, "    ")?;
        }
        Ok(true)
    }

    fn try_declare_string_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        let Some(ty) = &l.ty else { return Ok(false) };
        if !is_single_string_type(ty) {
            return Ok(false);
        }
        let literal = match &l.value {
            Some(ast::Expr::StrLit { text, .. }) => Some(text.as_str()),
            _ => None,
        };
        let source = l
            .value
            .as_ref()
            .and_then(|value| self.c_string_elems(value));
        let declared_indices = match ty {
            ast::Type::Indexed {
                index: Some(index), ..
            } => index_values(index, self.const_ranges, self.consts, self.fns),
            _ => None,
        };
        let indices = declared_indices
            .or_else(|| literal.map(|text| (0..text.chars().count() as i64).collect()))
            .or_else(|| {
                source
                    .as_ref()
                    .map(|values| (0..values.len() as i64).collect())
            })
            .unwrap_or_default();
        self.local_types
            .borrow_mut()
            .insert(l.name.text.clone(), "string".into());
        self.local_indices
            .borrow_mut()
            .insert(l.name.text.clone(), indices.clone());
        if let (Some(left), Some(right)) = (indices.first(), indices.last()) {
            self.local_ranges
                .borrow_mut()
                .insert(l.name.text.clone(), (*left, *right));
        }
        for (position, index) in indices.into_iter().enumerate() {
            let key = format!("{}[{index}]", l.name.text);
            self.local_types
                .borrow_mut()
                .insert(key.clone(), "Char".into());
            let value = literal
                .and_then(|text| text.chars().nth(position))
                .map(|ch| format!("{}ULL", ch as u32))
                .or_else(|| {
                    source
                        .as_ref()
                        .and_then(|values| values.get(position))
                        .cloned()
                })
                .unwrap_or_else(|| "0ULL".into());
            // Elaboration materializes a connected string as one signal per
            // character. Seed those signals directly so the DUT connection
            // sees the initializer; only genuinely unconnected strings need C
            // locals.
            if let Some(&id) = self.map.get(&key) {
                b.push_str(&format!("    sx_set({}, {value});\n", id.0));
                for extra in self.alias_ids_beyond(&key, &id.0.to_string()) {
                    b.push_str(&format!("    sx_set({extra}, {value});\n"));
                }
            } else {
                b.push_str(&format!(
                    "    uint64_t {} = {value};\n",
                    c_local_ident(&key)
                ));
                self.locals.borrow_mut().insert(key);
            }
        }
        Ok(true)
    }

    /// Materialize a one-dimensional array whose element is a scalar or packed
    /// vector. Packed vectors (`unsigned[128]`) are leaves; an additional
    /// index (`unsigned[128][2]`) is the array dimension represented here.
    fn try_declare_array_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        let Some(ty) = &l.ty else { return Ok(false) };
        let persisted_array = self
            .persisted_layout(&l.name.text)
            .is_some_and(|layout| matches!(&layout.kind, LayoutKind::Array { .. }));
        if !persisted_array && self.array_parts(ty).is_none() {
            return Ok(false);
        }
        self.declare_typed_storage(&l.name.text, ty, b)?;
        if let Some(value) = &l.value {
            if !self.write_composite(&l.name.text, value, b, "    ")? {
                return Err(format!(
                    "unsupported array initializer for `{}`",
                    l.name.text
                ));
            }
        }
        Ok(true)
    }

    /// Recursively flatten an array/field aggregate into typed scalar leaves.
    /// Connected leaves already exist in `map`; only unconnected leaves need C
    /// storage, but both receive metadata for formatting and target coercion.
    fn declare_typed_storage(
        &self,
        prefix: &str,
        ty: &ast::Type,
        b: &mut String,
    ) -> Result<(), String> {
        if let Some((left, right)) = type_index_bounds(ty, self.const_ranges, self.consts, self.fns)
        {
            self.local_ranges
                .borrow_mut()
                .insert(prefix.to_string(), (left, right));
        }
        if let Some(indices) = sized_string_indices(ty, self.const_ranges, self.consts, self.fns) {
            self.local_types
                .borrow_mut()
                .insert(prefix.to_string(), "string".into());
            self.local_indices
                .borrow_mut()
                .insert(prefix.to_string(), indices.clone());
            for index in indices {
                let element = format!("{prefix}[{index}]");
                self.local_types
                    .borrow_mut()
                    .insert(element.clone(), "Char".into());
                if !self.map.contains_key(&element) {
                    b.push_str(&format!("    uint64_t {} = 0;\n", c_local_ident(&element)));
                    self.locals.borrow_mut().insert(element);
                }
            }
            return Ok(());
        }
        // Concrete testbench declarations were already specialized by IR.
        // Consume that authoritative tree so generic field substitutions,
        // views, and nested range direction are not independently
        // reconstructed in the generated-C backend.
        if let Some(layout) = self.persisted_layout(prefix).cloned() {
            return self.declare_layout_storage(prefix, &layout, b);
        }
        // Unconstrained/dynamically sized strings are handled above their
        // callers and deliberately have no static layout. Every other typed
        // testbench declaration must have been persisted by IR; silently
        // rebuilding it from AST here would restore a second layout authority.
        let head = type_head_name(ty);
        Err(format!(
            "IR did not retain a concrete layout for testbench local `{prefix}`{}",
            head.map(|name| format!(" of type `{name}`"))
                .unwrap_or_default()
        ))
    }

    fn persisted_layout(&self, local_path: &str) -> Option<&SourceLayout> {
        self.design
            .source_layouts
            .get(&format!("{}.{}", self.name, local_path))
    }

    fn concrete_field_names(&self, local_path: &str) -> Option<Vec<String>> {
        let LayoutKind::Struct { fields, .. } = &self.persisted_layout(local_path)?.kind else {
            return None;
        };
        Some(fields.iter().map(|field| field.name.clone()).collect())
    }

    /// Field names for expression-only values that do not have a concrete
    /// storage path (for example, an inlined function parameter). Prefer a
    /// concrete, non-view IR specialization; the AST table is only a fallback
    /// for a nominal type that never occurs as a declared design value.
    fn nominal_field_names(&self, type_name: &str) -> Option<Vec<String>> {
        let mut candidates = self.design.source_layouts.values().filter_map(|layout| {
            let LayoutKind::Struct { name, view, fields } = &layout.kind else {
                return None;
            };
            (name == type_name).then(|| {
                (
                    view.is_some(),
                    fields
                        .iter()
                        .map(|field| field.name.clone())
                        .collect::<Vec<_>>(),
                )
            })
        });
        let first = candidates.next();
        let from_ir = candidates.fold(first, |best, candidate| match best {
            Some(current) if current.0 <= candidate.0 => Some(current),
            _ => Some(candidate),
        });
        from_ir
            .map(|(_, fields)| fields)
            .or_else(|| self.struct_field_names.get(type_name).cloned())
    }

    fn layout_is_composite(layout: &SourceLayout) -> bool {
        matches!(
            layout.kind,
            LayoutKind::Struct { .. } | LayoutKind::Array { .. }
        )
    }

    /// Materialize a concrete IR layout as flattened C storage. Connected
    /// leaves already live in the design object; unconnected leaves receive a
    /// native local. Both paths record the same scalar metadata used by
    /// formatting, conversions, range attributes, and operator dispatch.
    fn declare_layout_storage(
        &self,
        prefix: &str,
        layout: &SourceLayout,
        b: &mut String,
    ) -> Result<(), String> {
        match &layout.kind {
            LayoutKind::Struct { name, fields, .. } => {
                self.local_types
                    .borrow_mut()
                    .insert(prefix.to_string(), name.clone());
                for field in fields {
                    self.declare_layout_storage(
                        &format!("{prefix}.{}", field.name),
                        &field.layout,
                        b,
                    )?;
                }
                Ok(())
            }
            LayoutKind::Array { range, element } => {
                let range = range.ok_or_else(|| {
                    format!("concrete testbench array `{prefix}` has no index range")
                })?;
                if matches!(
                    &element.kind,
                    LayoutKind::Scalar {
                        domain: ScalarDomain::Character,
                        ..
                    }
                ) {
                    self.local_types
                        .borrow_mut()
                        .insert(prefix.to_string(), "string".to_string());
                }
                let indices = directional_indices(range.left, range.right);
                self.local_ranges
                    .borrow_mut()
                    .insert(prefix.to_string(), (range.left, range.right));
                self.local_indices
                    .borrow_mut()
                    .insert(prefix.to_string(), indices.clone());
                for index in indices {
                    self.declare_layout_storage(&format!("{prefix}[{index}]"), element, b)?;
                }
                Ok(())
            }
            LayoutKind::Packed {
                width,
                family,
                range,
                ..
            } => {
                if let Some(range) = range {
                    self.local_ranges
                        .borrow_mut()
                        .insert(prefix.to_string(), (range.left, range.right));
                }
                self.local_families
                    .borrow_mut()
                    .insert(prefix.to_string(), family.clone());
                self.local_types
                    .borrow_mut()
                    .insert(prefix.to_string(), family.clone());
                self.declare_layout_leaf(prefix, *width, 0, b)
            }
            LayoutKind::Scalar {
                width,
                domain,
                nominal,
                value_range,
            } => {
                if let Some(range) = value_range {
                    self.local_ranges
                        .borrow_mut()
                        .insert(prefix.to_string(), *range);
                }
                let name = nominal.clone().unwrap_or_else(|| match domain {
                    ScalarDomain::Bits => "bits".to_string(),
                    ScalarDomain::Integer => "integer".to_string(),
                    ScalarDomain::Real => "real".to_string(),
                    ScalarDomain::Character => "Char".to_string(),
                    ScalarDomain::Enum(name) => name.clone(),
                });
                self.local_types
                    .borrow_mut()
                    .insert(prefix.to_string(), name.clone());
                let default = self.design.new_defaults.get(&name).copied().unwrap_or(0);
                self.declare_layout_leaf(prefix, *width, default, b)
            }
            LayoutKind::Opaque { name, width } => {
                self.local_types
                    .borrow_mut()
                    .insert(prefix.to_string(), name.clone());
                self.declare_layout_leaf(prefix, width.unwrap_or(64), 0, b)
            }
        }
    }

    fn declare_layout_leaf(
        &self,
        prefix: &str,
        width: u32,
        default: u64,
        b: &mut String,
    ) -> Result<(), String> {
        if width == 0 {
            return Err(format!(
                "testbench local `{prefix}` has an unresolved width"
            ));
        }
        self.local_widths
            .borrow_mut()
            .insert(prefix.to_string(), width);
        if !self.map.contains_key(prefix) {
            let c_ty = if width > 64 { "sx_value" } else { "uint64_t" };
            b.push_str(&format!(
                "    {c_ty} {} = {default}ULL;\n",
                c_local_ident(prefix)
            ));
            self.locals.borrow_mut().insert(prefix.to_string());
        }
        Ok(())
    }

    /// Emit writes for a composite value assigned to a connected name — a
    /// string literal (`s = "hi"` -> one `Char` element per index) or a struct
    /// literal (`a = { .re = 3 }` -> one field signal each). Returns `true`
    /// when it handled `value`, `false` to fall through to scalar assignment.
    /// Materialize an *unconnected* struct-typed testbench local as one C local
    /// per field (`let p: Pkt;` -> `uint64_t sxl_p_a = 0, sxl_p_b = 0;`),
    /// recording each field's width/family and the receiver type. A struct
    /// literal initializer (`let p: Pkt = { .a = 1 };`) writes the fields. A
    /// *connected* struct port (fields in the signal map) returns `false` so the
    /// existing signal path handles it. Returns `true` when handled.
    fn try_declare_struct_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        if let Some(layout) = self.persisted_layout(&l.name.text).cloned() {
            if matches!(layout.kind, LayoutKind::Struct { .. }) {
                let prefix = &l.name.text;
                let connected = self.map.keys().any(|name| {
                    name == prefix
                        || name
                            .strip_prefix(prefix)
                            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
                });
                self.declare_layout_storage(prefix, &layout, b)?;
                if connected {
                    // The general let path writes a connected initializer, but
                    // it now sees layout-derived leaf metadata registered above.
                    return Ok(false);
                }
                if let Some(value) = &l.value {
                    let wrote = self.write_composite(prefix, value, b, "    ")?;
                    let defaulted = matches!(value,
                        ast::Expr::Call { callee, args, .. }
                            if args.is_empty() && self.c_default_construction(callee).is_some());
                    if !wrote && !defaulted {
                        return Err(format!("unsupported aggregate initializer for `{prefix}`"));
                    }
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Expand a name that holds a struct into a literal reading its fields:
    /// `x` becomes `{ .a = x.a, .b = x.b }`. That is what lets an arm be a
    /// plain name rather than a literal.
    fn struct_name_literal(&self, e: &ast::Expr) -> Option<ast::Expr> {
        let path = expr_path(e)?;
        let fields = self.concrete_field_names(&path).or_else(|| {
            let ty = self.arg_type_name(e)?;
            self.nominal_field_names(&ty)
        })?;
        let span = siox::syntax::ast::expr_span(e);
        let ident = |t: &str| ast::Ident {
            text: t.to_string(),
            span,
        };
        let args = fields
            .into_iter()
            .map(|field| ast::ConnectArg {
                field: Some(ident(&field)),
                value: Some(ast::Expr::Field {
                    base: Box::new(ast::Expr::Path(ast::Path {
                        segments: vec![ident(&path)],
                        span,
                    })),
                    field: ident(&field),
                    span,
                }),
                span,
            })
            .collect();
        Some(ast::Expr::Construct {
            ty: None,
            args,
            spread: None,
            span,
        })
    }

    /// Turn a struct-valued `match`/`if` expression into a struct literal
    /// whose fields are each a scalar `match`/`if`.
    ///
    /// `match s { p => P { .a = x, .b = y }, .. }` becomes
    /// `P { .a = match s { p => x, .. }, .b = match s { p => y, .. } }`.
    /// Every branch has to be a literal of the same struct for this to be
    /// meaningful; a branch that is a name or a call is left alone, and the
    /// caller reports it rather than emitting something wrong.
    fn distribute_struct_choice(&self, value: &ast::Expr) -> Option<ast::Expr> {
        use ast::Expr as E;
        // The branches, in order, and a way to rebuild the choice around new
        // branch expressions.
        let raw: Vec<&E> = match value {
            E::Match { arms, .. } => arms.iter().map(|a| a.value_expr()).collect::<Option<_>>()?,
            E::IfExpr { then, els, .. } => vec![then.as_ref(), els.as_ref()],
            _ => return None,
        };
        // An arm may be a *call* returning a struct (`_ => mk(6)`) or simply a
        // *name* (`_ => x`) rather than a literal, so reduce each branch to a
        // literal first. `reduced` owns the rewrites that `branches` borrows.
        let reduced: Vec<Option<ast::Expr>> = raw
            .iter()
            .map(|br| {
                self.struct_call_rewrite(br, 0)
                    .or_else(|| self.struct_name_literal(br))
            })
            .collect();
        let branches: Vec<&E> = raw
            .iter()
            .zip(&reduced)
            .map(|(br, red)| red.as_ref().unwrap_or(br))
            .collect();
        // Each branch must be a struct literal over the same set of fields.
        // The *order* is not part of that: fields are matched by name below,
        // so an arm may list them differently from its neighbour.
        let mut field_order: Vec<String> = Vec::new();
        for (i, br) in branches.iter().enumerate() {
            let E::Construct {
                args, spread: None, ..
            } = br
            else {
                return None;
            };
            let names: Vec<String> = args
                .iter()
                .map(|a| a.field.as_ref().map(|f| f.text.clone()))
                .collect::<Option<_>>()?;
            if i == 0 {
                field_order = names;
            } else {
                let (mut here, mut first) = (names, field_order.clone());
                here.sort();
                first.sort();
                if here != first {
                    return None;
                }
            }
        }
        if field_order.is_empty() {
            return None;
        }
        let field_of = |br: &E, field: &str| -> Option<ast::Expr> {
            let E::Construct { args, .. } = br else {
                return None;
            };
            args.iter()
                .find(|a| a.field.as_ref().is_some_and(|f| f.text == field))
                .and_then(|a| a.value.clone())
        };
        let ty = match branches[0] {
            E::Construct { ty, .. } => ty.clone(),
            _ => None,
        };
        let span = siox::syntax::ast::expr_span(value);
        let mut args = Vec::with_capacity(field_order.len());
        for field in &field_order {
            let per_field = match value {
                E::Match {
                    scrutinee, arms, ..
                } => E::Match {
                    scrutinee: scrutinee.clone(),
                    arms: arms
                        .iter()
                        .zip(&branches)
                        .map(|(arm, br)| {
                            let v = field_of(br, field)?;
                            Some(ast::MatchArm {
                                pattern: arm.pattern.clone(),
                                body: ast::Block {
                                    stmts: vec![ast::Stmt::Return {
                                        value: Some(v),
                                        span: arm.span,
                                    }],
                                    span: arm.span,
                                },
                                span: arm.span,
                            })
                        })
                        .collect::<Option<Vec<_>>>()?,
                    span,
                },
                E::IfExpr { cond, .. } => E::IfExpr {
                    cond: cond.clone(),
                    then: Box::new(field_of(branches[0], field)?),
                    els: Box::new(field_of(branches[1], field)?),
                    span,
                },
                _ => return None,
            };
            args.push(ast::ConnectArg {
                field: Some(ast::Ident {
                    text: field.clone(),
                    span,
                }),
                value: Some(per_field),
                span,
            });
        }
        Some(E::Construct {
            ty,
            args,
            spread: None,
            span,
        })
    }

    /// The single expression a body returns, or `None` when the body is
    /// anything more than one `return`.
    fn sole_return<'b>(&self, f: &'b ast::FnDecl) -> Option<&'b ast::Expr> {
        let body = f.body.as_ref()?;
        match body.stmts.as_slice() {
            [ast::Stmt::Return { value: Some(e), .. }] => Some(e),
            _ => None,
        }
    }

    /// Rewrite a struct-valued *call* into the literal it returns, expressed in
    /// the caller's terms.
    ///
    /// The testbench writes a composite from a name, a literal or a spread. A
    /// computed struct — `a + a`, `a.doubled()`, `twice(a)` — matched none of
    /// those, fell through to the scalar path, and was reported as
    /// "unknown signal `s`": a struct has no single signal to look up, so the
    /// destination took the blame for a value the emitter could not build.
    /// Substituting the arguments into the returned literal turns all three
    /// into the `Construct` case that already works, and composes, so
    /// `twice(a) + a` resolves in two steps.
    fn struct_call_rewrite(&self, value: &ast::Expr, depth: u32) -> Option<ast::Expr> {
        if depth > 8 {
            return None; // recursive body; the frontend owns that diagnostic
        }
        let mut binds: HashMap<String, ast::Expr> = HashMap::new();
        let f = match value {
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                let op_str = siox::syntax::pretty::bin_op(op);
                let lhs_ty = self.arg_type_name(lhs)?;
                let fns = self.op_impls.get(&(op_str.to_string(), lhs_ty.clone()))?;
                let rhs_ty = self.arg_type_name(rhs);
                let declared = |f: &ast::FnDecl, a: &Option<String>| -> Option<String> {
                    let d = a.clone().or_else(|| {
                        f.params
                            .iter()
                            .find(|p| !p.is_self)
                            .and_then(|p| p.ty.as_ref())
                            .and_then(|ty| self.fns.type_head_key(ty))
                    })?;
                    Some(if d == "Self" { lhs_ty.clone() } else { d })
                };
                let (f, _) = match &rhs_ty {
                    Some(r) => fns
                        .iter()
                        .find(|(f, a)| declared(f, a).as_deref() == Some(r))?,
                    None => (fns.len() == 1).then(|| &fns[0])?,
                };
                binds.insert("self".to_string(), (**lhs).clone());
                if let Some(p) = f.params.iter().find(|p| !p.is_self) {
                    if let Some(n) = &p.name {
                        binds.insert(n.text.clone(), (**rhs).clone());
                    }
                }
                *f
            }
            ast::Expr::Call { callee, args, .. } => match callee.as_ref() {
                // A method: bind `self` to the receiver.
                ast::Expr::Field { base, field, .. } => {
                    let ty = self.arg_type_name(base)?;
                    let f = *self.methods.get(&(ty, field.text.clone()))?;
                    binds.insert("self".to_string(), (**base).clone());
                    for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
                        if let Some(n) = &p.name {
                            binds.insert(n.text.clone(), a.clone());
                        }
                    }
                    f
                }
                // A module-level function.
                _ => {
                    let f = self.fns.get(callee)?;
                    for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
                        if let Some(n) = &p.name {
                            binds.insert(n.text.clone(), a.clone());
                        }
                    }
                    f
                }
            },
            _ => return None,
        };
        // A *composite*-returning body is this function's business; a scalar
        // one is already handled by the ordinary expression path.
        //
        // An array return counts. This used to ask only whether the return
        // named a field-aggregate struct, and `unsigned[8][2]` heads on
        // `unsigned` — a newtype with no named fields — so an array-returning
        // call was declined and `x = constant2()` came back as "unknown signal
        // `x`", a struct having no single signal to look up. The caller writes
        // composites of either kind.
        let returned = f.ret.as_ref()?;
        let returns_struct = self
            .fns
            .type_head_key(returned)
            .and_then(|head| self.nominal_field_names(&head))
            .is_some_and(|fields| !fields.is_empty());
        // `array_parts` is `None` for a packed vector and `Some` for a real
        // array, which is exactly the distinction wanted here.
        if !returns_struct && self.array_parts(returned).is_none() {
            return None;
        }
        let body = self.sole_return(f)?;
        let rewritten = siox::ir::subst_expr_paths(body, &binds);
        // Substituting a *struct-valued* argument leaves field reads of it
        // behind — `self.x` with `self = twice(a)` becomes `twice(a).x`, which
        // has no more hardware form here than it does in an entity. Reducing
        // the base to its literal and taking the field named turns it back
        // into ordinary scalar arithmetic.
        // A result that is itself a call (`fn outer(v) { return twice(v); }`)
        // needs no recursion here: `write_composite` re-enters this function
        // with whatever comes back.
        Some(self.fold_struct_fields(&rewritten, depth + 1))
    }

    /// Replace `<struct expression>.field` with the field's own value.
    fn fold_struct_fields(&self, e: &ast::Expr, depth: u32) -> ast::Expr {
        use ast::Expr as E;
        if depth > 8 {
            return e.clone();
        }
        let go = |x: &E| self.fold_struct_fields(x, depth);
        match e {
            E::Field { base, field, span } => {
                let base = self.fold_struct_fields(base, depth);
                // A call in base position becomes the literal it returns.
                let base = match self.struct_call_rewrite(&base, depth) {
                    Some(lit) => lit,
                    None => base,
                };
                if let E::Construct { args, .. } = &base {
                    for a in args {
                        let named = a.field.as_ref().is_some_and(|n| n.text == field.text);
                        if named {
                            if let Some(v) = &a.value {
                                return self.fold_struct_fields(v, depth + 1);
                            }
                        }
                    }
                }
                E::Field {
                    base: Box::new(base),
                    field: field.clone(),
                    span: *span,
                }
            }
            E::Binary { op, lhs, rhs, span } => E::Binary {
                op: op.clone(),
                lhs: Box::new(go(lhs)),
                rhs: Box::new(go(rhs)),
                span: *span,
            },
            E::Unary { op, rhs, span } => E::Unary {
                op: *op,
                rhs: Box::new(go(rhs)),
                span: *span,
            },
            E::Call {
                callee,
                type_args,
                args,
                bang,
                span,
            } => E::Call {
                callee: Box::new(go(callee)),
                type_args: type_args.clone(),
                args: args.iter().map(go).collect(),
                bang: *bang,
                span: *span,
            },
            E::Construct {
                ty,
                args,
                spread,
                span,
            } => E::Construct {
                ty: ty.clone(),
                args: args
                    .iter()
                    .map(|a| ast::ConnectArg {
                        field: a.field.clone(),
                        value: a.value.as_ref().map(&go),
                        span: a.span,
                    })
                    .collect(),
                spread: spread.as_ref().map(|s| Box::new(go(s))),
                span: *span,
            },
            other => other.clone(),
        }
    }

    /// Write a composite into `b`. Thin wrapper over
    /// [`Self::write_composite_into`] that owns the staging phase.
    fn write_composite(
        &self,
        name: &str,
        value: &ast::Expr,
        b: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        let (mut decls, mut writes) = (String::new(), String::new());
        let wrote = self.write_composite_into(name, value, &mut decls, &mut writes, ind)?;
        b.push_str(&decls);
        b.push_str(&writes);
        Ok(wrote)
    }

    /// Stage and then write a complete flattened composite source. Keeping
    /// this in one place makes path copies, runtime-selected copies, and
    /// struct spreads share both alias fan-out and assignment ordering.
    fn stage_composite_source(
        &self,
        name: &str,
        source: BTreeMap<String, String>,
        decls: &mut String,
        writes: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        let target = self.composite_targets(name);
        if source.keys().ne(target.keys()) {
            return Err(format!("composite assignment shape mismatch at `{name}`"));
        }
        for (suffix, expression) in source {
            let (signal, destination) = &target[&suffix];
            // Every leaf is read before any destination is changed. This is
            // observable for swaps and overlapping aggregate copies.
            let expression = self.stage_value(&expression, decls, ind);
            if *signal {
                writes.push_str(&format!("{ind}sx_set({destination}, {expression});\n"));
                for extra in self.alias_ids_beyond(&format!("{name}{suffix}"), destination) {
                    writes.push_str(&format!("{ind}sx_set({extra}, {expression});\n"));
                }
            } else {
                writes.push_str(&format!("{ind}{destination} = {expression};\n"));
            }
        }
        Ok(true)
    }

    /// Write a composite, staging every value before any of it lands.
    ///
    /// `decls` collects the temporaries and `writes` the assignments, and both
    /// are threaded through the recursion so a *nested* composite stages into
    /// the same phase as its parent. Without that, `arr = [arr[1], arr[0]]`
    /// over an array of structs copied element 1 into element 0 and then read
    /// element 0 back for element 1.
    fn write_composite_into(
        &self,
        name: &str,
        value: &ast::Expr,
        decls: &mut String,
        writes: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        // A struct-valued conditional distributes over the fields: one
        // scalar conditional per field, which the emitter already handles.
        // `p = match s { .. => P { .a = x, .b = y } }` cannot reduce to a
        // single literal, since which arm applies is a runtime question.
        if let Some(spread_out) = self.distribute_struct_choice(value) {
            return self.write_composite_into(name, &spread_out, decls, writes, ind);
        }
        // A computed struct value reduces to the literal its body returns,
        // which the `Construct` arm below already knows how to write.
        if let Some(rewritten) = self.struct_call_rewrite(value, 0) {
            return self.write_composite_into(name, &rewritten, decls, writes, ind);
        }
        if let Some(source_name) = expr_path(value) {
            let source = self.composite_reads(&source_name);
            if !source.is_empty() {
                return self
                    .stage_composite_source(name, source, decls, writes, ind)
                    .map_err(|error| {
                        if error.starts_with("composite assignment shape mismatch") {
                            format!(
                                "composite assignment shape mismatch: `{name}` and `{source_name}`"
                            )
                        } else {
                            error
                        }
                    });
            }
        } else {
            // A whole composite selected at runtime has no `expr_path`, but
            // each target suffix can still be read through the same dynamic
            // access mux as a scalar field. This covers `dst[i] = src[j]` and
            // gives every leaf the usual before-any-write staging.
            let target = self.composite_targets(name);
            if !target.is_empty() {
                let mut source = BTreeMap::new();
                for suffix in target.keys() {
                    let Some(expression) = self.dynamic_array_read(value, suffix) else {
                        source.clear();
                        break;
                    };
                    source.insert(suffix.clone(), expression?);
                }
                if !source.is_empty() {
                    return self.stage_composite_source(name, source, decls, writes, ind);
                }
            }
        }
        if !matches!(value, ast::Expr::StrLit { .. }) {
            if let Some(source) = self.c_string_elems(value) {
                let mut target = Vec::new();
                while let Some(destination) = {
                    let element = format!("{name}[{}]", target.len());
                    self.map
                        .get(&element)
                        .map(|id| (true, id.0.to_string()))
                        .or_else(|| {
                            self.locals
                                .borrow()
                                .contains(&element)
                                .then(|| (false, c_local_ident(&element)))
                        })
                } {
                    target.push(destination);
                }
                let target_is_empty_string = target.is_empty()
                    && self.local_types.borrow().get(name).map(String::as_str) == Some("string");
                if !target.is_empty() || target_is_empty_string {
                    if target.len() != source.len() {
                        return Err(format!(
                            "array assignment length mismatch: `{name}` has {} element(s), source has {}",
                            target.len(),
                            source.len()
                        ));
                    }
                    for (index, ((signal, destination), expression)) in
                        target.iter().zip(source).enumerate()
                    {
                        if *signal {
                            writes
                                .push_str(&format!("{ind}sx_set({destination}, {expression});\n"));
                            // Each element fans out to every instance it feeds,
                            // as the scalar and field paths do.
                            let element = format!("{name}[{index}]");
                            for extra in self.alias_ids_beyond(&element, destination) {
                                writes.push_str(&format!("{ind}sx_set({extra}, {expression});\n"));
                            }
                        } else {
                            writes.push_str(&format!("{ind}{destination} = {expression};\n"));
                        }
                    }
                    return Ok(true);
                }
            }
        }
        match value {
            ast::Expr::Array { elems, .. } => {
                let indices = self
                    .local_indices
                    .borrow()
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| {
                        let mut indices = Vec::new();
                        while self.has_storage_prefix(&format!("{name}[{}]", indices.len())) {
                            indices.push(indices.len() as i64);
                        }
                        indices
                    });
                if indices.len() != elems.len() {
                    return Err(format!(
                        "array assignment length mismatch: `{name}` has {} element(s), source has {}",
                        indices.len(),
                        elems.len()
                    ));
                }
                for (index, value) in indices.into_iter().zip(elems) {
                    let element = format!("{name}[{index}]");
                    if self.write_composite_into(&element, value, decls, writes, ind)? {
                        continue;
                    }
                    if let Some(&id) = self.map.get(&element) {
                        let expression = self.value_for(id, value)?;
                        let expression = self.stage_value(&expression, decls, ind);
                        writes.push_str(&format!("{ind}sx_set({}, {expression});\n", id.0));
                        for extra in self.alias_ids_beyond(&element, &id.0.to_string()) {
                            writes.push_str(&format!("{ind}sx_set({extra}, {expression});\n"));
                        }
                    } else {
                        let expression = self.value_for_local(&element, value)?;
                        let expression = match self.local_widths.borrow().get(&element) {
                            Some(&width) => mask_c(&expression, width),
                            None => expression,
                        };
                        let expression = self.stage_value(&expression, decls, ind);
                        writes.push_str(&format!(
                            "{ind}{} = {expression};\n",
                            c_local_ident(&element)
                        ));
                    }
                }
                Ok(true)
            }
            ast::Expr::StrLit { text, .. } => {
                let source_len = text.chars().count();
                let indices = self
                    .local_indices
                    .borrow()
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| (0..source_len as i64).collect());
                // Only a per-element target (a `Char[]`, or a `Logic[]`
                // elaborated one signal per character) is written character by
                // character. A packed vector has no `name[i]` storage, so the
                // loop below wrote nothing and still reported the value
                // handled: the caller skipped the scalar path and never
                // declared the local, and `let v: unsigned[8] = "10000011";`
                // failed with `no value named v`.
                let per_element = self.local_types.borrow().get(name).map(String::as_str)
                    == Some("string")
                    || indices.iter().any(|index| {
                        let element = format!("{name}[{index}]");
                        self.map.contains_key(&element) || self.locals.borrow().contains(&element)
                    });
                if !per_element {
                    return Ok(false);
                }
                if indices.len() != source_len {
                    return Err(format!(
                        "string assignment length mismatch: `{name}` has {} element(s), source has {source_len}",
                        indices.len()
                    ));
                }
                for (index, ch) in indices.into_iter().zip(text.chars()) {
                    let element = format!("{name}[{index}]");
                    let value = self.char_value_for_target(&element, ch);
                    if let Some(&id) = self.map.get(&element) {
                        writes.push_str(&format!("{ind}sx_set({}, {value}ULL);\n", id.0));
                        for extra in self.alias_ids_beyond(&element, &id.0.to_string()) {
                            writes.push_str(&format!("{ind}sx_set({extra}, {value}ULL);\n"));
                        }
                    } else if self.locals.borrow().contains(&element) {
                        let expression = match self.local_widths.borrow().get(&element) {
                            Some(&width) => mask_c(&format!("{value}ULL"), width),
                            None => format!("{value}ULL"),
                        };
                        writes.push_str(&format!(
                            "{ind}{} = {expression};\n",
                            c_local_ident(&element)
                        ));
                    }
                }
                Ok(true)
            }
            ast::Expr::Construct {
                ty, args, spread, ..
            } => {
                let fields = self.concrete_field_names(name).or_else(|| {
                    let type_name = ty
                        .as_ref()
                        .and_then(type_head_name)
                        .map(str::to_string)
                        .or_else(|| self.local_types.borrow().get(name).cloned());
                    type_name.and_then(|head| self.nominal_field_names(&head))
                });
                let mut wrote = false;

                if let Some(spread) = spread.as_deref() {
                    let source = if let Some(source_name) = expr_path(spread) {
                        self.composite_reads(&source_name)
                    } else {
                        let target = self.composite_targets(name);
                        let mut source = BTreeMap::new();
                        for suffix in target.keys() {
                            let Some(expression) = self.dynamic_array_read(spread, suffix) else {
                                source.clear();
                                break;
                            };
                            source.insert(suffix.clone(), expression?);
                        }
                        source
                    };
                    if !source.is_empty() {
                        wrote |= self.stage_composite_source(name, source, decls, writes, ind)?;
                    }
                }

                for (position, arg) in args.iter().enumerate() {
                    let field_name =
                        arg.field
                            .as_ref()
                            .map(|field| field.text.as_str())
                            .or_else(|| {
                                fields
                                    .as_ref()
                                    .and_then(|fields| fields.get(position))
                                    .map(String::as_str)
                            });
                    let Some(field_name) = field_name else {
                        return Err(format!(
                            "cannot bind positional field {} while assigning `{name}`",
                            position + 1
                        ));
                    };
                    let Some(value) = &arg.value else { continue };
                    wrote |= self.write_composite_field_staged(
                        &format!("{name}.{field_name}"),
                        value,
                        decls,
                        writes,
                        ind,
                    )?;
                }
                Ok(wrote)
            }
            ast::Expr::Concat { parts, .. } => {
                let fields = self.concrete_field_names(name).or_else(|| {
                    self.local_types
                        .borrow()
                        .get(name)
                        .and_then(|head| self.nominal_field_names(head))
                });
                let Some(fields) = fields else {
                    return Ok(false);
                };
                if fields.is_empty() {
                    return Ok(false);
                }
                if fields.len() != parts.len() {
                    return Err(format!(
                        "struct assignment field count mismatch: `{name}` has {} field(s), source has {}",
                        fields.len(),
                        parts.len()
                    ));
                }
                for (field, value) in fields.iter().zip(parts) {
                    self.write_composite_field_staged(
                        &format!("{name}.{field}"),
                        value,
                        decls,
                        writes,
                        ind,
                    )?;
                }
                Ok(true)
            }
            // An elementwise operator over arrays (`let y: Logic[3] = not a;`).
            // std declares these as blanket impls over `T[]`; lowering lifts
            // them per element and this is the same lift, so the two engines
            // agree instead of one refusing what the other computes.
            other => {
                let Some(indices) = self.local_indices.borrow().get(name).cloned() else {
                    return Ok(false);
                };
                let mut lifted = Vec::with_capacity(indices.len());
                for k in 0..indices.len() {
                    let Some(element) = self.elementwise_at(other, k, indices.len()) else {
                        return Ok(false);
                    };
                    lifted.push((format!("{name}[{}]", indices[k]), element));
                }
                for (target, element) in lifted {
                    self.write_composite_field_staged(&target, &element, decls, writes, ind)?;
                }
                Ok(true)
            }
        }
    }

    /// One position of an elementwise array expression, mirroring lowering's
    /// rule: every path naming an array of the same length becomes that
    /// array's `k`-th element, paired by position so a descending range keeps
    /// its own indices.
    fn elementwise_at(&self, e: &ast::Expr, k: usize, len: usize) -> Option<ast::Expr> {
        match e {
            ast::Expr::Path(_) => {
                let path = expr_path(e)?;
                let borrowed = self.local_indices.borrow();
                let indices = borrowed.get(&path)?;
                let index = *indices.get(k).filter(|_| indices.len() == len)?;
                let span = ast::expr_span(e);
                Some(ast::Expr::Index {
                    base: Box::new(e.clone()),
                    index: Box::new(ast::Expr::Int {
                        text: index.to_string(),
                        span,
                    }),
                    span,
                })
            }
            ast::Expr::Binary { op, lhs, rhs, span } => Some(ast::Expr::Binary {
                op: op.clone(),
                lhs: Box::new(self.elementwise_at(lhs, k, len)?),
                rhs: Box::new(self.elementwise_at(rhs, k, len)?),
                span: *span,
            }),
            // The condition is scalar and shared; only the branches are
            // per-element. Mirrors lowering so the engines agree.
            ast::Expr::IfExpr {
                cond,
                then,
                els,
                span,
            } => Some(ast::Expr::IfExpr {
                cond: cond.clone(),
                then: Box::new(self.elementwise_at(then, k, len)?),
                els: Box::new(self.elementwise_at(els, k, len)?),
                span: *span,
            }),
            // `match` selects a whole branch the way `if` does; the two
            // share `MatchArm` and have drifted apart before, so they are
            // lifted together here.
            ast::Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                let mut lifted = Vec::with_capacity(arms.len());
                for a in arms {
                    let value = self.elementwise_at(a.value_expr()?, k, len)?;
                    lifted.push(ast::MatchArm {
                        pattern: a.pattern.clone(),
                        body: ast::Block {
                            stmts: vec![ast::Stmt::Expr(value)],
                            span: a.body.span,
                        },
                        span: a.span,
                    });
                }
                Some(ast::Expr::Match {
                    scrutinee: scrutinee.clone(),
                    arms: lifted,
                    span: *span,
                })
            }
            ast::Expr::Unary { op, rhs, span } => Some(ast::Expr::Unary {
                op: *op,
                rhs: Box::new(self.elementwise_at(rhs, k, len)?),
                span: *span,
            }),
            _ => None,
        }
    }

    /// Evaluate `expr` into a fresh temporary and return its name.
    fn stage_value(&self, expr: &str, decls: &mut String, ind: &str) -> String {
        let n = self.tmp_seq.get();
        self.tmp_seq.set(n + 1);
        let tmp = format!("_sxc{n}");
        decls.push_str(&format!("{ind}sx_value {tmp} = {expr};\n"));
        tmp
    }

    /// [`Self::write_composite_field`] with the value staged: the expression
    /// goes into `decls` and the assignment into `writes`, so a caller can
    /// emit every field's value before any of them lands. Without that split,
    /// `t = P { .a = t.b, .b = t.a }` wrote `a` and then read it back for `b`.
    fn write_composite_field_staged(
        &self,
        field: &str,
        value: &ast::Expr,
        decls: &mut String,
        writes: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        // A nested composite manages its own ordering.
        if self.write_composite(field, value, writes, ind)? {
            return Ok(true);
        }
        if let Some(&id) = self.map.get(field) {
            let expression = self.value_for(id, value)?;
            let tmp = self.stage_value(&expression, decls, ind);
            writes.push_str(&format!("{ind}sx_set({}, {tmp});\n", id.0));
            for extra in self.alias_ids_beyond(field, &id.0.to_string()) {
                writes.push_str(&format!("{ind}sx_set({extra}, {tmp});\n"));
            }
            return Ok(true);
        }
        if self.locals.borrow().contains(field) {
            let expression = self.value_for_local(field, value)?;
            let expression = match self.local_widths.borrow().get(field) {
                Some(&width) => mask_c(&expression, width),
                None => expression,
            };
            let tmp = self.stage_value(&expression, decls, ind);
            writes.push_str(&format!("{ind}{} = {tmp};\n", c_local_ident(field)));
            return Ok(true);
        }
        Ok(false)
    }

    fn has_storage_prefix(&self, prefix: &str) -> bool {
        let descendant = |name: &str| {
            name == prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('[') || rest.starts_with('.'))
        };
        self.map.keys().any(|name| descendant(name))
            || self.locals.borrow().iter().any(|name| descendant(name))
    }

    /// Scalar descendant reads keyed by their suffix relative to `prefix`.
    fn composite_reads(&self, prefix: &str) -> BTreeMap<String, String> {
        let mut reads = BTreeMap::new();
        for (name, id) in self.map {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                reads.insert(suffix.to_string(), format!("sx_read({})", id.0));
            }
        }
        for name in self.locals.borrow().iter() {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                reads.insert(suffix.to_string(), c_local_ident(name));
            }
        }
        reads
    }

    /// Scalar descendant write destinations keyed like `composite_reads`.
    /// The other signals a connected name feeds. `map` records one; a name
    /// wired to several instance ports has them all in `aliases`.
    fn alias_ids_beyond(&self, path: &str, primary: &str) -> Vec<String> {
        self.aliases
            .get(path)
            .map(|ids| {
                ids.iter()
                    .map(|id| id.0.to_string())
                    .filter(|id| id != primary)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn composite_targets(&self, prefix: &str) -> BTreeMap<String, (bool, String)> {
        let mut targets = BTreeMap::new();
        for (name, id) in self.map {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                targets.insert(suffix.to_string(), (true, id.0.to_string()));
            }
        }
        for name in self.locals.borrow().iter() {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                targets.insert(suffix.to_string(), (false, c_local_ident(name)));
            }
        }
        targets
    }

    /// The declared `(family, width)` of a vector-family type (`signed[8]` ->
    /// ("signed", 8)). Mirrors the runner's rule.
    fn declared_family(&self, ty: &ast::Type) -> Option<(String, u32)> {
        if let ast::Type::Indexed {
            base,
            index: Some(i),
            ..
        } = ty
        {
            if matches!(base.as_ref(), ast::Type::Indexed { .. }) {
                return self.declared_family(base);
            }
            let head = self.fns.type_head_key(base)?;
            if !self.families.contains(&head) {
                return None;
            }
            return Some((
                head,
                u32::try_from(index_values(i, self.const_ranges, self.consts, self.fns)?.len())
                    .ok()?,
            ));
        }
        None
    }

    /// The outer array element type and count, excluding a packed vector's own
    /// width index (`unsigned[128]` is one value; `unsigned[128][2]` is two).
    fn array_parts<'b>(&self, ty: &'b ast::Type) -> Option<(&'b ast::Type, Vec<i64>)> {
        let ast::Type::Indexed {
            base,
            index: Some(index),
            ..
        } = ty
        else {
            return None;
        };
        if let ast::Type::Path(path) = base.as_ref() {
            let head = self
                .fns
                .struct_path_key(path)
                .or_else(|| path.segments.last().map(|segment| segment.text.clone()))?;
            if self.families.contains(&head) {
                return None;
            }
        }
        Some((
            base,
            index_values(index, self.const_ranges, self.consts, self.fns)?,
        ))
    }

    /// The bit width a testbench name carries: a local's declared width or the
    /// connected signal's.
    fn name_width(&self, name: &str) -> Option<u32> {
        if let Some(&w) = self.local_widths.borrow().get(name) {
            return Some(w);
        }
        self.map
            .get(name)
            .map(|&id| self.design.signals[id.0 as usize].width)
    }

    /// Translate `lhs op rhs` through the lhs family's operator impl as an
    /// inline C expression, when one exists — the native mirror of the
    /// runner's `dispatch_binop`. Comparisons derive from `Ord::cmp`.
    /// The `(family, width)` an operand dispatches on, for the shapes that
    /// carry one indirectly. Kept separate from `c_dispatch_binop`'s own match
    /// so a branch can be asked the same question its parent was.
    fn dispatch_operand_family(&self, e: &ast::Expr) -> Option<(String, Option<u32>)> {
        if let Some((fam, w)) = self.conversion_target(e) {
            return Some((fam, Some(w)));
        }
        match e {
            ast::Expr::IfExpr { then, els, .. } => self
                .dispatch_operand_family(then)
                .or_else(|| self.dispatch_operand_family(els)),
            ast::Expr::Match { arms, .. } => arms
                .iter()
                .filter_map(|a| a.value_expr())
                .find_map(|v| self.dispatch_operand_family(v)),
            // Arithmetic keeps its operands' family, so `(a - b) < 0` has to
            // dispatch the same `Ord` that `d < 0` does after `let d = a - b`.
            // Without this the inline form fell through to an unsigned compare
            // and a negative result tested as positive.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => self
                .dispatch_operand_family(lhs)
                .or_else(|| self.dispatch_operand_family(rhs)),
            // `not x` is whatever `x` is. Without this `(not x) and y` lost
            // the family and fell back to a bitwise `and` over discriminants:
            // `'X'` came out as `'1'`, where the same value bound to a name
            // first gave `'X'`.
            ast::Expr::Unary { rhs, .. } => self.dispatch_operand_family(rhs),
            // A *bit* of a packed vector is not a named local, so it has no
            // family of its own; it reads as the vector's element type, which
            // is what an operator impl on that element is keyed by. Mirrors
            // lowering, where `v[7] xor v[5]` had the same gap: `and` worked
            // on the same operands because it is a built-in needing no impl.
            ast::Expr::Index { base, .. } => {
                if let Some(name) = expr_path(e) {
                    if let Some(family) = self
                        .local_families
                        .borrow()
                        .get(&name)
                        .cloned()
                        .or_else(|| self.local_types.borrow().get(&name).cloned())
                    {
                        return Some((family, self.name_width(&name)));
                    }
                }
                let base_name = expr_path(base)?;
                // A connected signal knows its element directly; a pure local
                // has no signal, so go through its declared family.
                let element = self
                    .map
                    .get(&base_name)
                    .and_then(|id| self.design.array_element_enums.get(&id.0))
                    .or_else(|| {
                        let family = self.local_families.borrow().get(&base_name).cloned()?;
                        self.design.array_element_of_family.get(&family)
                    })?;
                Some((element.clone(), None))
            }
            _ => {
                // Any named value: a plain local, or a struct leaf like `p.x`
                // (an `Expr::Field`, which a shape match misses — a connected
                // `signed` field then compared unsigned).
                if let Some(name) = expr_path(e) {
                    let family = self
                        .local_families
                        .borrow()
                        .get(&name)
                        .cloned()
                        .or_else(|| self.local_types.borrow().get(&name).cloned())?;
                    return Some((family, self.name_width(&name)));
                }
                // A call has no name to look up, so its declared return type
                // supplies both family and width; without it a call never
                // dispatched an impl and `neg(x) < 0` compared unsigned.
                let ret = self.call_return_type(e)?;
                let head = resolved_type_name(&ret, self.type_aliases, self.fns)?;
                Some((head, self.declared_width(&ret)))
            }
        }
    }

    fn c_dispatch_binop(
        &self,
        op: &ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Result<Option<String>, String> {
        // The family decides which operator impl runs; the width is only ever
        // needed to bind `self'length` inside it. Both come from the one
        // place that knows how each expression shape carries a family —
        // asking the question here a second time is how `Expr::Field` and
        // then `Expr::Binary` ended up answered in one and not the other.
        let Some((fam, lwidth)) = self.dispatch_operand_family(lhs) else {
            return Ok(None);
        };
        let op_str = siox::syntax::pretty::bin_op(op);
        // `==`/`!=`: bit equality at the type's width (mask both sides).
        if matches!(op_str, "==" | "!=") {
            let Some(w) = lwidth else {
                return Ok(None);
            };
            if w == 0 || w >= 64 {
                return Ok(None);
            }
            let a = mask_c(&self.expr(lhs)?, w);
            let b = mask_c(&self.expr(rhs)?, w);
            return Ok(Some(format!(
                "(({a}) {} ({b}))",
                if op_str == "==" { "==" } else { "!=" }
            )));
        }
        // `Ordering` discriminants from std's enum, not a baked-in 0/2.
        let ord = |v: &str, fallback: u64| {
            self.enums
                .get("Ordering")
                .and_then(|m| m.get(v))
                .copied()
                .unwrap_or(fallback)
        };
        let cmp = match op_str {
            "<" => Some((ord("Less", 0), false)),
            ">" => Some((ord("Greater", 2), false)),
            ">=" => Some((ord("Less", 0), true)),
            "<=" => Some((ord("Greater", 2), true)),
            _ => None,
        };
        let tr = match cmp {
            Some(_) => "<=>",
            None => op_str,
        };
        let Some(candidates) = self.op_impls.get(&(tr.to_string(), fam.clone())) else {
            return Ok(None);
        };
        let rhs_type = match rhs {
            ast::Expr::Path(p) if p.segments.len() == 1 => self
                .local_families
                .borrow()
                .get(&p.segments[0].text)
                .cloned()
                .or_else(|| self.local_types.borrow().get(&p.segments[0].text).cloned()),
            ast::Expr::Int { .. } => Some("integer".to_string()),
            _ => None,
        };
        let selected = rhs_type
            .as_deref()
            .and_then(|rhs| {
                candidates.iter().find(|(_, input)| {
                    input.as_deref() == Some(rhs)
                        || (input.as_deref() == Some("Self") && rhs == fam)
                })
            })
            .or_else(|| (candidates.len() == 1).then(|| &candidates[0]));
        let Some((f, _)) = selected else {
            return Ok(None);
        };
        let Some(body) = f.body.as_ref() else {
            return Ok(None);
        };

        let w = lwidth.unwrap_or(0);
        let mut env = HashMap::new();
        // Narrow each operand to its own width before the body sees it. A
        // signal already holds exactly that many bits, but an *expression*
        // does not: `0 - s` is computed full-width, so signed division read
        // -200 rather than the 56 the same expression gives in hardware.
        let narrow = |v: String, width: u32| {
            if width > 0 && width < 64 {
                mask_c(&v, width)
            } else {
                v
            }
        };
        let lhs_c = narrow(format!("({})", self.expr(lhs)?), w);
        env.insert("self".to_string(), lhs_c);
        env.insert("self::length".to_string(), format!("{w}ULL"));
        if let Some(pdecl) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &pdecl.name {
                let rw = match rhs {
                    ast::Expr::Path(p) if p.segments.len() == 1 => {
                        self.name_width(&p.segments[0].text).unwrap_or(w)
                    }
                    _ => w,
                };
                let rhs_c = narrow(format!("({})", self.expr(rhs)?), rw);
                env.insert(n.text.clone(), rhs_c);
                env.insert(format!("{}::length", n.text), format!("{rw}ULL"));
            }
        }
        self.fn_env.borrow_mut().push(env);
        self.fn_type_env.borrow_mut().push(HashMap::new());
        let out = self.c_fn_stmts(&body.stmts, false);
        self.fn_type_env.borrow_mut().pop();
        self.fn_env.borrow_mut().pop();
        let r = out?;
        Ok(Some(match cmp {
            Some((want, ne)) => {
                format!("(({r}) {} {want}ULL)", if ne { "!=" } else { "==" })
            }
            None => mask_c(&r, w),
        }))
    }

    fn c_dispatch_not(&self, rhs: &ast::Expr) -> Result<Option<String>, String> {
        // Any operand that carries a family, not only a bare name: asking for
        // a `Path` here meant `not (a and b)` skipped `Logic`'s table and
        // negated the discriminant, giving '0' where hardware said 'X'.
        let Some((family, operand_width)) = self.dispatch_operand_family(rhs) else {
            return Ok(None);
        };
        let width = operand_width.unwrap_or(0);
        // A packed nominal array forwards the blanket `T[]` implementation of core
        // `not`; the native harness performs that element-wise operation
        // directly at the concrete width.
        if self.families.contains(&family) && width > 0 {
            return Ok(Some(mask_c(&format!("~({})", self.expr(rhs)?), width)));
        }
        let Some((f, _)) = self
            .op_impls
            .get(&("not".to_string(), family))
            .and_then(|candidates| candidates.first())
        else {
            return Ok(None);
        };
        let Some(body) = f.body.as_ref() else {
            return Ok(None);
        };
        let mut env = HashMap::new();
        env.insert("self".to_string(), format!("({})", self.expr(rhs)?));
        env.insert("self::length".to_string(), format!("{width}ULL"));
        self.fn_env.borrow_mut().push(env);
        self.fn_type_env.borrow_mut().push(HashMap::new());
        let out = self.c_fn_stmts(&body.stmts, false);
        self.fn_type_env.borrow_mut().pop();
        self.fn_env.borrow_mut().pop();
        let value = out?;
        Ok(Some(if width > 0 {
            mask_c(&value, width)
        } else {
            value
        }))
    }

    /// The width of a call argument, for binding `param'length` at an inline.
    /// A named value carries its declared width; anything else has none to
    /// give and the body falls back to its own rules.
    fn arg_width(&self, a: &ast::Expr) -> Option<u32> {
        if let Some(w) = expr_path(a).and_then(|p| self.name_width(&p)) {
            return Some(w);
        }
        if let Some(w) = self.slice_width(a) {
            return Some(w);
        }
        // Only a *named* argument had a width, so `x'length` inside an inlined
        // body had nothing to consult when the argument was an expression:
        // `sext(a)` worked and `sext(a + 0)` did not, though both are the same
        // eight bits. A conversion states its width, arithmetic keeps its
        // operands', a branch takes its branches' — the shapes operator
        // dispatch already walks, so ask the same question there.
        self.dispatch_operand_family(a).and_then(|(_, w)| w)
    }

    /// The declared bit width of a vector-family type: `unsigned[8]` -> 8 (and the
    /// element width of an array of one). Mirrors the runner's rule.
    fn declared_width(&self, ty: &ast::Type) -> Option<u32> {
        // A bare derived-vector type (`struct Byte : Logic[8]`) inherits its
        // base array's width.
        if let ast::Type::Path(p) = ty {
            if let Some(key) = self.fns.struct_path_key(p) {
                if let Some(&w) = self.derived_widths.get(&key) {
                    return Some(w);
                }
            }
        }
        if let ast::Type::Indexed {
            base,
            index: Some(i),
            ..
        } = ty
        {
            if matches!(base.as_ref(), ast::Type::Indexed { .. }) {
                return self.declared_width(base);
            }
            let head = self.fns.type_head_key(base)?;
            if !self.families.contains(&head) {
                return None;
            }
            return u32::try_from(index_values(i, self.const_ranges, self.consts, self.fns)?.len())
                .ok();
        }
        None
    }

    /// `signed test_<name>(void) { ... }` — 0 on pass, 1 on the first failed
    /// assert (printing its message first, like a panic).
    fn gen_test_fn(&self, items: &[&ast::ImplItem]) -> Result<String, String> {
        let mut b = String::new();
        b.push_str(&format!(
            "signed test_{}(void) {{\n    g_range_failed = 0;\n    g_loc = 0;\n    sx_io_reset();\n    sx_reset();\n    sx_wave_begin_test();\n",
            self.symbol
        ));

        // The test's event wheel: sim time + per-clock next-edge state. Arrays
        // are sized >=1 so clock-less tests still compile; `_nclk` grows as
        // `clock()` statements register (source order matches scan order).
        let n = self.clocks.len().max(1);
        let cid: Vec<String> = self.clocks.iter().map(|(c, _)| c.to_string()).collect();
        let half: Vec<String> = self.clocks.iter().map(|(_, h)| format!("{h}ULL")).collect();
        let next = if half.is_empty() {
            "0".to_string()
        } else {
            half.join(", ")
        };
        b.push_str(&format!(
            "    uint64_t _now = 0; (void)_now;\n             \x20   uint64_t _next[{n}] = {{{next}}}; (void)_next;\n             \x20   static const uint32_t _cid[{n}] = {{{}}};\n             \x20   static const uint64_t _half[{n}] = {{{}}};\n             \x20   signed _nclk = {}; (void)_nclk;\n",
            if cid.is_empty() { "0".to_string() } else { cid.join(", ") },
            if half.is_empty() { "0".to_string() } else { half.join(", ") },
            self.clocks.len(),
        ));

        // One pass in source order (sequential `let` semantics, mirroring
        // the runner): connected lets write signals, unconnected scalars
        // become C locals, and a settle precedes the first statement.
        let mut started = false;
        for item in items {
            match item {
                // Canonical clock processes are concurrent services, not
                // source-ordered stimulus. Their `_next` entries were seeded
                // in the prologue so declaration order cannot delay them.
                ast::ImplItem::Stmt(statement)
                    if siox::testbench::is_clock_statement(statement) => {}
                ast::ImplItem::Process(process)
                    if siox::testbench::is_clock_process(&process.body.stmts) => {}
                // A DUT instance is wired by elaboration, which binds each
                // port to the *name* it is connected to. A connection given a
                // value instead — `{ .n = 7 }` — has no name to bind, so the
                // port kept its default and the testbench read 0 while
                // hardware lowered the same connection to a constant driver.
                ast::ImplItem::Let(l) if self.instance_names.contains(&l.name.text) => {
                    self.record_instance_port_families(l);
                    self.seed_valued_connections(l, &mut b)?;
                }
                ast::ImplItem::Let(l) if self.try_declare_fs_read_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) if self.try_declare_string_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) if self.try_declare_array_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) if self.try_declare_struct_local(l, &mut b)? => {}
                // Instances are claimed by name above, so everything that
                // reaches here is a value. This arm used to open by skipping
                // any typed construction as "an instance", which also caught a
                // struct literal: a *connected* struct local is handed here
                // deliberately, because its storage is the DUT's port signals,
                // so both halves believed the other wrote the initializer and
                // `let p: Packet = Packet { .id = 1 }` powered on at zero the
                // moment `p` was wired to a port -- silently, since the
                // testbench and the design both read the same zero.
                ast::ImplItem::Let(l) => {
                    let value = &l.value;
                    // Record the nominal array family for every declared name
                    // (connected ports too): operators dispatch on it.
                    if let Some((fam, _)) = l.ty.as_ref().and_then(|t| self.declared_family(t)) {
                        self.local_families
                            .borrow_mut()
                            .insert(l.name.text.clone(), fam);
                    }
                    if let Some((left, right)) = l.ty.as_ref().and_then(|ty| {
                        type_index_bounds(ty, self.const_ranges, self.consts, self.fns)
                    }) {
                        self.local_ranges
                            .borrow_mut()
                            .insert(l.name.text.clone(), (left, right));
                    }
                    if let Some(head) = l.ty.as_ref().and_then(|ty| self.fns.type_head_key(ty)) {
                        self.local_types
                            .borrow_mut()
                            .insert(l.name.text.clone(), head);
                    }
                    // A string/struct-literal initializer on a connected
                    // name writes each element/field.
                    if let Some(v) = value {
                        if self.write_composite(&l.name.text, v, &mut b, "    ")? {
                            continue;
                        }
                    }
                    if let Some(&id) = self.map.get(&l.name.text) {
                        if let Some(v) = value {
                            let e = self.value_for(id, v)?;
                            b.push_str(&format!("    sx_set({}, {e});\n", id.0));
                            // A name connected to several instance ports
                            // has one entry in `map` and all of them in
                            // `aliases`. Seeding only the first left every
                            // other instance reading its default: three
                            // `Inc`s fed by one local gave 1, 1, 11. The
                            // assignment path already drives them all.
                            for a in self
                                .aliases
                                .get(&l.name.text)
                                .map(|v| v.as_slice())
                                .unwrap_or(&[])
                            {
                                if *a != id {
                                    b.push_str(&format!("    sx_set({}, {e});\n", a.0));
                                }
                            }
                        }
                    } else {
                        let e = match value {
                            // A char literal on an enum-typed local resolves
                            // by position in that enum (data-driven), like
                            // the native harness + hardware paths.
                            Some(v) => self.value_for_local(&l.name.text, v)?,
                            // Uninitialized: the type's `new()` default
                            // (`Logic` -> `'U'`), matching native + hardware.
                            None => {
                                l.ty.as_ref()
                                    .and_then(|ty| self.fns.type_head_key(ty))
                                    .and_then(|head| self.design.new_defaults.get(&head))
                                    .map(|v| format!("{v}ULL"))
                                    .unwrap_or_else(|| "0".to_string())
                            }
                        };
                        // A vector-family local wraps at its declared
                        // width, like the equivalent hardware signal.
                        let declared_width = l.ty.as_ref().and_then(|t| self.declared_width(t));
                        let e = match declared_width {
                            Some(w) => {
                                self.local_widths
                                    .borrow_mut()
                                    .insert(l.name.text.clone(), w);
                                mask_c(&e, w)
                            }
                            None => e,
                        };
                        let c_ty = if declared_width.is_some_and(|w| w > 64) {
                            "sx_value"
                        } else {
                            "uint64_t"
                        };
                        b.push_str(&format!(
                            "    {c_ty} {} = {e};\n",
                            c_local_ident(&l.name.text)
                        ));
                        self.locals.borrow_mut().insert(l.name.text.clone());
                    }
                }
                ast::ImplItem::Stmt(st) => {
                    if !started {
                        b.push_str("    sx_settle();\n");
                        started = true;
                    }
                    self.stmt(st, &mut b, 1)?;
                }
                ast::ImplItem::Process(process) => {
                    if !started {
                        b.push_str("    sx_settle();\n");
                        started = true;
                    }
                    for statement in &process.body.stmts {
                        self.stmt(statement, &mut b, 1)?;
                    }
                }
                _ => {}
            }
        }
        if !started {
            b.push_str("    sx_settle();\n");
        }
        self.locals.borrow_mut().clear();

        b.push_str("    if (sx_check_ranges()) return 1;\n    return 0;\n}\n\n");
        // Post-settle range asserts (values persist, so checking at the next
        // settle also catches violations that occur inside await loops).
        let b = b.replace(
            "sx_settle();",
            "sx_run_settle(_now); if (sx_check_ranges()) return 1;",
        );
        Ok(b)
    }

    /// The width of a scalar enum part of a concatenation. Vector locals
    /// record a declared width; an enum local does not, so mirror the rule
    /// lowering uses — enough bits to hold the largest discriminant, which
    /// makes `Bit` one bit and nine-valued `Logic` four.
    fn enum_part_width(&self, e: &ast::Expr) -> Option<u32> {
        let head = match e {
            ast::Expr::Path(p) if p.segments.len() == 1 => self
                .local_types
                .borrow()
                .get(&p.segments[0].text)
                .cloned()?,
            _ => return None,
        };
        let max = *self.enums.get(&head)?.values().max()?;
        Some((u64::BITS - max.leading_zeros()).max(1))
    }

    /// Give each `<instance>.<port>` the nominal array family its declared type
    /// carries. A local records this from its own declaration; a port read
    /// through the instance had no entry, so `d.y` on a `signed[8]` port
    /// compared and printed as unsigned — `d.y == -100` was false while
    /// `d.y == tb` against an identical local was true.
    fn record_instance_port_families(&self, l: &ast::LetDecl) {
        let Some(ports) =
            l.ty.as_ref()
                .and_then(|ty| resolved_type_def_id(ty, self.resolved))
                .and_then(|id| self.entity_ports.get(&id))
        else {
            return;
        };
        for (port, ty) in ports {
            if let Some((family, _)) = self.declared_family(ty) {
                self.local_families
                    .borrow_mut()
                    .insert(format!("{}.{}", l.name.text, port), family);
            }
        }
    }

    /// Seed the ports of an instance whose connection is a value rather than
    /// a name. Elaboration binds `{ .n = sig }` by name and has nothing to
    /// bind for `{ .n = 7 }`; the `<inst>.<port>` key reaches the port signal
    /// either way.
    fn seed_valued_connections(&self, l: &ast::LetDecl, b: &mut String) -> Result<(), String> {
        // A positional connection fills ports in declaration order. It also
        // *parses* as a concatenation — a brace list with no leading `.` is
        // ambiguous by shape and only the declared type tells them apart —
        // so both forms arrive here.
        let ports =
            l.ty.as_ref()
                .and_then(|ty| resolved_type_def_id(ty, self.resolved))
                .and_then(|id| self.entity_ports.get(&id));
        let by_position = |i: usize| ports.and_then(|ps| ps.get(i)).map(|(n, _)| n.clone());
        let pairs: Vec<(String, &ast::Expr)> = match &l.value {
            Some(ast::Expr::Construct { args, .. }) => args
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let value = c.value.as_ref()?;
                    let port = match &c.field {
                        Some(f) => f.text.clone(),
                        None => by_position(i)?,
                    };
                    Some((port, value))
                })
                .collect(),
            Some(ast::Expr::Concat { parts, .. }) => parts
                .iter()
                .enumerate()
                .filter_map(|(i, p)| Some((by_position(i)?, p)))
                .collect(),
            _ => return Ok(()),
        };
        for (port, value) in pairs {
            // A plain name is already bound, and re-driving it here would
            // fight the binding.
            if expr_path(value).is_some() {
                continue;
            }
            let key = format!("{}.{}", l.name.text, port);
            // A struct-typed port is one signal per leaf field, so a struct
            // literal has no single key to drive and the scalar path below
            // skipped it in silence.
            if self.write_composite(&key, value, b, "    ")? {
                b.push_str("    sx_settle();\n");
                continue;
            }
            if !self.map.contains_key(&key) {
                continue;
            }
            let e = self.expr(value)?;
            self.drive_signal(&key, &e, b, "    ")?;
        }
        Ok(())
    }

    /// Drive one name and every port it connects to (`sx_set` masks to each
    /// signal's own width). Extracted so a multi-target write cannot grow a
    /// second copy of the fan-out.
    fn drive_signal(&self, name: &str, e: &str, b: &mut String, ind: &str) -> Result<(), String> {
        if !self.map.contains_key(name) {
            return Err(format!("unknown signal `{name}`"));
        }
        b.push_str(&format!("{ind}{{ sx_value _v = {e};"));
        for a in self.aliases.get(name).map(|v| v.as_slice()).unwrap_or(&[]) {
            b.push_str(&format!(" sx_set({}, _v);", a.0));
        }
        b.push_str(&format!(" }}\n{ind}sx_settle();\n"));
        Ok(())
    }

    /// Write one packed bit selected by a constant or runtime declared label.
    /// The update is guarded and the checked index latches a failure before an
    /// out-of-range runtime label can reach the recovery path. A
    /// connected Logic-family signal's companion nibble is updated in lockstep
    /// when the finished design has one.
    fn write_packed_bit(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        b: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        let ast::Expr::Index { base, index, .. } = target else {
            return Ok(false);
        };
        if matches!(
            index.as_ref(),
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return Ok(false);
        }
        let Some(path) = expr_path(base) else {
            return Ok(false);
        };
        if !self.array_elements(&path).is_empty()
            || (!self.locals.borrow().contains(&path) && !self.map.contains_key(&path))
        {
            return Ok(false);
        }
        let Some(width) = self.name_width(&path) else {
            return Ok(false);
        };
        let (left, right) = self
            .local_ranges
            .borrow()
            .get(&path)
            .copied()
            .unwrap_or((0, i64::from(width).saturating_sub(1)));
        let (low, high) = (left.min(right), left.max(right));
        let index = self.checked_index_c(self.expr(index)?, index, left, right);
        let value = self.expr(value)?;
        let serial = self.tmp.get();
        self.tmp.set(serial + 1);
        b.push_str(&format!(
            "{ind}{{ int64_t _bi{serial} = (int64_t)({index}); \
             sx_value _bv{serial} = {value};\n\
             {ind}  if (_bi{serial} >= {low}LL && _bi{serial} <= {high}LL) {{\n\
             {ind}    uint64_t _bp{serial} = (uint64_t)(_bi{serial} - {low}LL);\n"
        ));
        if self.locals.borrow().contains(&path) {
            let local = c_local_ident(&path);
            let encoding = self
                .local_types
                .borrow()
                .get(&path)
                .and_then(|family| self.design.array_element_of_family.get(family))
                .and_then(|element| self.design.logic_encodings.get(element));
            let value_bit = encoding
                .map(|encoding| c_logic_value_bit(&format!("_bv{serial}"), encoding))
                .unwrap_or_else(|| format!("(_bv{serial} != 0)"));
            b.push_str(&format!(
                "{ind}    sx_value _bm{serial} = ((sx_value)1) << _bp{serial};\n\
                 {ind}    {local} = ({local} & ~_bm{serial}) | \
                 (({value_bit}) << _bp{serial});\n"
            ));
        } else {
            let mut targets = self.aliases.get(&path).cloned().unwrap_or_default();
            if targets.is_empty() {
                if let Some(&id) = self.map.get(&path) {
                    targets.push(id);
                }
            }
            targets.sort_by_key(|id| id.0);
            targets.dedup_by_key(|id| id.0);
            for id in targets {
                let encoding = self
                    .design
                    .array_element_enums
                    .get(&id.0)
                    .and_then(|element| self.design.logic_encodings.get(element));
                let value_bit = encoding
                    .map(|encoding| c_logic_value_bit(&format!("_bv{serial}"), encoding))
                    .unwrap_or_else(|| format!("(_bv{serial} != 0)"));
                b.push_str(&format!(
                    "{ind}    {{ sx_value _bm = ((sx_value)1) << _bp{serial}; \
                     sx_value _old = sx_read({}); \
                     sx_set({}, (_old & ~_bm) | (({value_bit}) << _bp{serial})); }}\n",
                    id.0, id.0,
                ));
                if let Some(&meta) = self.design.meta_of.get(&id.0) {
                    let is_binary = encoding
                        .map(|encoding| c_disc_in(&format!("_bv{serial}"), &encoding.binary))
                        .unwrap_or_else(|| "0".to_string());
                    b.push_str(&format!(
                        "{ind}    {{ uint64_t _ms = _bp{serial} * 4; \
                         sx_value _mm = ((sx_value)15) << _ms; \
                         sx_value _md = {is_binary} ? 0 : _bv{serial}; \
                         sx_value _old = sx_read({meta}); \
                         sx_set({meta}, (_old & ~_mm) | ((_md & 15) << _ms)); }}\n"
                    ));
                }
            }
        }
        b.push_str(&format!("{ind}  }}\n{ind}}}\n{ind}sx_settle();\n"));
        Ok(true)
    }

    /// `w[7..4] = v` on a testbench local or a connected signal.
    ///
    /// Hardware has lowered constant slice writes since ranges landed
    /// (`merge_slice`), and the testbench emitter had only the single-bit
    /// form. Building a stimulus word a nibble at a time — the ordinary way to
    /// write one — came back as "unsupported assignment target", with no code
    /// and no span, after parse, resolve and typecheck had all reported
    /// success.
    ///
    /// The bounds are normalized exactly as `merge_slice` normalizes them, so
    /// both engines place the same bits for the same source. Like hardware's
    /// slice write (and unlike its single-bit write) this touches the value
    /// plane only: neither engine carries a Logic metavalue through a slice.
    fn write_packed_slice(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        b: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        let ast::Expr::Index { base, index, .. } = target else {
            return Ok(false);
        };
        if !matches!(
            index.as_ref(),
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return Ok(false);
        }
        let Some(path) = expr_path(base) else {
            return Ok(false);
        };
        // An array's elements are separate signals; only a packed scalar is
        // written by bit position.
        if !self.array_elements(&path).is_empty()
            || (!self.locals.borrow().contains(&path) && !self.map.contains_key(&path))
        {
            return Ok(false);
        }
        let Some((a, c)) = self.slice_bounds(&path, index) else {
            return Ok(false);
        };
        // Declared labels map onto compact storage: `unsigned[15..8]` puts
        // label 8 at storage bit 0, the same mapping `c_bit_slice_of` reads by.
        let declared_low = self
            .local_ranges
            .borrow()
            .get(&path)
            .map(|&(left, right)| left.min(right))
            .unwrap_or(0);
        let (a, c) = (a - declared_low, c - declared_low);
        let (hi, lo) = (a.max(c), a.min(c));
        if lo < 0 {
            return Err(format!("`{path}` sliced with a negative bound"));
        }
        let width = (hi - lo + 1) as u32;
        let serial = self.tmp.get();
        self.tmp.set(serial + 1);
        let v = self.expr(value)?;
        // `sx_mask` rather than a `u64` literal: a slice may be wider than one
        // ABI word, and the mask has to be built at the value's full width.
        b.push_str(&format!(
            "{ind}sx_value _sv{serial} = sx_mask(({v}), {width}) << {lo};\n\
             {ind}sx_value _sk{serial} = ~(sx_mask(~(sx_value)0, {width}) << {lo});\n"
        ));
        if self.locals.borrow().contains(&path) {
            let local = c_local_ident(&path);
            b.push_str(&format!(
                "{ind}{local} = ({local} & _sk{serial}) | _sv{serial};\n"
            ));
            return Ok(true);
        }
        let mut targets = self.aliases.get(&path).cloned().unwrap_or_default();
        if targets.is_empty() {
            if let Some(&id) = self.map.get(&path) {
                targets.push(id);
            }
        }
        targets.sort_by_key(|id| id.0);
        targets.dedup_by_key(|id| id.0);
        for id in targets {
            b.push_str(&format!(
                "{ind}sx_set({}, (sx_read({}) & _sk{serial}) | _sv{serial});\n",
                id.0, id.0
            ));
        }
        b.push_str(&format!("{ind}sx_settle();\n"));
        Ok(true)
    }

    /// Expand every runtime *array* index in an assignment target into the
    /// concrete flattened targets used by the native harness.
    ///
    /// A packed vector index is deliberately left in the AST: after an outer
    /// array dimension has been made concrete, [`Self::write_packed_bit`] can
    /// update that selected word with its existing declared-bit mapping. Each
    /// runtime index expression is evaluated once, even when a nested array
    /// produces many candidate leaves.
    fn dynamic_array_target_variants(
        &self,
        target: &ast::Expr,
        declarations: &mut String,
        ind: &str,
    ) -> Result<Option<Vec<DynamicTargetVariant>>, String> {
        fn combine(left: Option<String>, right: String) -> String {
            match left {
                Some(left) => format!("({left}) && ({right})"),
                None => right,
            }
        }

        fn expand(
            cx: &Ctx<'_>,
            expression: &ast::Expr,
            declarations: &mut String,
            ind: &str,
        ) -> Result<(Vec<DynamicTargetVariant>, bool), String> {
            match expression {
                ast::Expr::Path(_) => Ok((
                    vec![DynamicTargetVariant {
                        expression: expression.clone(),
                        condition: None,
                    }],
                    false,
                )),
                ast::Expr::Field { base, field, span } => {
                    let (bases, expanded) = expand(cx, base, declarations, ind)?;
                    Ok((
                        bases
                            .into_iter()
                            .map(|variant| DynamicTargetVariant {
                                expression: ast::Expr::Field {
                                    base: Box::new(variant.expression),
                                    field: field.clone(),
                                    span: *span,
                                },
                                condition: variant.condition,
                            })
                            .collect(),
                        expanded,
                    ))
                }
                ast::Expr::Index { base, index, span } => {
                    let (bases, mut expanded) = expand(cx, base, declarations, ind)?;
                    let is_range = matches!(
                        index.as_ref(),
                        ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                    );
                    let is_literal = signed_index_bound(index).is_some();
                    let is_array = !is_range
                        && !is_literal
                        && bases.iter().any(|variant| {
                            expr_path(&variant.expression)
                                .is_some_and(|path| cx.local_indices.borrow().contains_key(&path))
                        });
                    if !is_array {
                        return Ok((
                            bases
                                .into_iter()
                                .map(|variant| DynamicTargetVariant {
                                    expression: ast::Expr::Index {
                                        base: Box::new(variant.expression),
                                        index: index.clone(),
                                        span: *span,
                                    },
                                    condition: variant.condition,
                                })
                                .collect(),
                            expanded,
                        ));
                    }

                    expanded = true;
                    let serial = cx.tmp.get();
                    cx.tmp.set(serial + 1);
                    let raw_index = cx.expr(index)?;
                    let first_path = bases
                        .iter()
                        .find_map(|variant| expr_path(&variant.expression));
                    let bounds = first_path
                        .as_deref()
                        .and_then(|path| cx.local_indices.borrow().get(path).cloned())
                        .and_then(|indices| Some((*indices.first()?, *indices.last()?)))
                        .ok_or_else(|| {
                            "the declared bounds of a runtime array assignment are not known here"
                                .to_string()
                        })?;
                    let runtime_index = cx.checked_index_c(raw_index, index, bounds.0, bounds.1);
                    declarations.push_str(&format!(
                        "{ind}int64_t _ai{serial} = (int64_t)({runtime_index});\n"
                    ));
                    let mut variants = Vec::new();
                    for variant in bases {
                        let path = expr_path(&variant.expression).ok_or_else(|| {
                            "cannot resolve the base of a runtime array assignment".to_string()
                        })?;
                        let indices =
                            cx.local_indices
                                .borrow()
                                .get(&path)
                                .cloned()
                                .ok_or_else(|| {
                                    format!("the declared indices of `{path}` are not known here")
                                })?;
                        for logical in indices {
                            let condition = combine(
                                variant.condition.clone(),
                                format!("_ai{serial} == {logical}LL"),
                            );
                            variants.push(DynamicTargetVariant {
                                expression: ast::Expr::Index {
                                    base: Box::new(variant.expression.clone()),
                                    index: Box::new(ast::Expr::Int {
                                        text: logical.to_string(),
                                        span: ast::expr_span(index),
                                    }),
                                    span: *span,
                                },
                                condition: Some(condition),
                            });
                        }
                    }
                    Ok((variants, expanded))
                }
                _ => Ok((
                    vec![DynamicTargetVariant {
                        expression: expression.clone(),
                        condition: None,
                    }],
                    false,
                )),
            }
        }

        let (variants, expanded) = expand(self, target, declarations, ind)?;
        Ok(expanded.then_some(variants))
    }

    /// Write a target containing one or more runtime array indices. Native
    /// aggregates are flattened, so this is C control flow over concrete
    /// leaves. A checked index latches a failure first; entering no concrete
    /// branch afterwards is only internal recovery behavior.
    fn write_dynamic_array_target(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        b: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        let mut declarations = String::new();
        let Some(variants) = self.dynamic_array_target_variants(target, &mut declarations, ind)?
        else {
            return Ok(false);
        };
        let mut writes = String::new();
        for variant in variants {
            let target = variant.expression;
            let condition = variant.condition;
            let branch_ind = format!("{ind}  ");
            let conditional = condition.is_some();
            if let Some(condition) = condition.as_deref() {
                writes.push_str(&format!("{ind}if ({condition}) {{\n"));
            }
            let inner = if conditional { &branch_ind } else { ind };
            let mut handled = self.write_packed_bit(&target, value, &mut writes, inner)?;
            if !handled {
                handled = self.write_packed_slice(&target, value, &mut writes, inner)?;
            }
            if !handled {
                let name =
                    expr_path(&target).ok_or("unsupported runtime-indexed assignment target")?;
                if self.write_composite(&name, value, &mut writes, inner)? {
                    writes.push_str(&format!("{inner}sx_settle();\n"));
                    handled = true;
                } else if self.locals.borrow().contains(&name) {
                    let expression = self.value_for_local(&name, value)?;
                    let expression = match self.local_widths.borrow().get(&name) {
                        Some(&width) => mask_c(&expression, width),
                        None => expression,
                    };
                    writes.push_str(&format!(
                        "{inner}{} = {expression};\n",
                        c_local_ident(&name)
                    ));
                    handled = true;
                } else if let Some(&id) = self.map.get(&name) {
                    let expression = self.value_for(id, value)?;
                    self.drive_signal(&name, &expression, &mut writes, inner)?;
                    handled = true;
                }
            }
            if !handled {
                return Err(format!(
                    "cannot write runtime-selected target `{}`",
                    siox::syntax::pretty::expr_string(&target)
                ));
            }
            if conditional {
                writes.push_str(&format!("{ind}}}\n"));
            }
        }
        b.push_str(&declarations);
        b.push_str(&writes);
        Ok(true)
    }

    fn stmt(&self, s: &ast::Stmt, b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        // Attribute what follows to the statement that produced it, so a
        // debugger stops on `.siox` rather than on the generated C. Everything
        // emitted until the next directive belongs to this statement, which is
        // exactly the granularity stepping wants.
        if self.debug {
            if let Some(line) = self.line_directive(ast::stmt_span(s)) {
                b.push_str(&line);
            }
        }
        match s {
            ast::Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                if after.is_some() {
                    // `clk = !clk after d;` registers on the event wheel; other
                    // delayed writes aren't compiled yet.
                    let (path, _) = after_toggle(target, value, after)?
                        .ok_or("only the `clk = not clk after d` form of `after` is supported in a native test executable")?;
                    if !self.map.contains_key(&path) {
                        return Err(format!("unknown signal `{path}`"));
                    }
                    for id in self.aliases.get(&path).map(|v| v.as_slice()).unwrap_or(&[]) {
                        if let Some(i) = self.clocks.iter().position(|(c, _)| *c == id.0) {
                            b.push_str(&format!(
                                "{ind}_next[{i}] = _now + {}ULL; _nclk = {}; sx_settle();\n",
                                self.clocks[i].1,
                                i + 1
                            ));
                        }
                    }
                    return Ok(());
                }
                if self.write_packed_bit(target, value, b, &ind)? {
                    return Ok(());
                }
                if self.write_packed_slice(target, value, b, &ind)? {
                    return Ok(());
                }
                if self.write_dynamic_array_target(target, value, b, &ind)? {
                    return Ok(());
                }
                // `{ hi, lo } = src` — hardware has always lowered this and
                // the testbench refused it. Split into one write per part so
                // the alias fan-out stays in `drive_signal` rather than
                // growing a second copy, which is where this emitter has
                // been wrong before.
                if let ast::Expr::Concat { parts, .. } = target {
                    let mut targets = Vec::with_capacity(parts.len());
                    for part in parts {
                        let name = expr_path(part).ok_or("unsupported assignment target")?;
                        let width = self
                            .name_width(&name)
                            .ok_or_else(|| format!("the width of `{name}` is not known here"))?;
                        targets.push((name, width));
                    }
                    let whole = self.expr(value)?;
                    let k = self.tmp.get();
                    self.tmp.set(k + 1);
                    b.push_str(&format!("{ind}sx_value _c{k} = {whole};\n"));
                    // First part is most significant, so it shifts down by the
                    // total width of everything after it.
                    for (i, (name, width)) in targets.iter().enumerate() {
                        let shift: u32 = targets[i + 1..].iter().map(|(_, w)| *w).sum();
                        let part = if shift == 0 {
                            format!("_c{k}")
                        } else {
                            format!("(_c{k} >> {shift})")
                        };
                        let e = format!("sx_mask({part}, {width})");
                        if self.locals.borrow().contains(name) {
                            b.push_str(&format!("{ind}{} = {e};\n", c_local_ident(name)));
                        } else {
                            self.drive_signal(name, &e, b, &ind)?;
                        }
                    }
                    return Ok(());
                }
                let name = expr_path(target).ok_or("unsupported assignment target")?;
                // A string or struct literal writes several element/field
                // signals (`s = "hi"`, `a = { .re = 3 }`).
                if self.write_composite(&name, value, b, &ind)? {
                    b.push_str(&format!("{ind}sx_settle();\n"));
                    return Ok(());
                }
                if self.locals.borrow().contains(&name) {
                    let e = self.value_for_local(&name, value)?;
                    let e = match self.local_widths.borrow().get(&name) {
                        Some(&w) => mask_c(&e, w),
                        None => e,
                    };
                    b.push_str(&format!("{ind}{} = {e};\n", c_local_ident(&name)));
                    return Ok(());
                }
                let id = *self
                    .map
                    .get(&name)
                    .ok_or_else(|| format!("unknown signal `{name}`"))?;
                let e = self.value_for(id, value)?;
                self.drive_signal(&name, &e, b, &ind)?;
            }
            // A `let` inside a block (spec 3.13). Testbench statements are
            // sequential C, so this is a C local in the same scope and C's
            // own braces give it the right lifetime. It passed `check` and
            // then failed at build claiming the name was unknown.
            ast::Stmt::Let(l) => {
                let name = &l.name.text;
                let family = l.ty.as_ref().and_then(|t| self.declared_family(t));
                let width = family
                    .as_ref()
                    .map(|&(_, w)| w)
                    .or_else(|| l.ty.as_ref().and_then(|t| self.declared_width(t)));
                // A composite local is one C variable per leaf, which the
                // top-level declarators build; inside a block only the scalar
                // case is handled, and saying so beats emitting one variable
                // for something that needs several.
                let composite = l.ty.as_ref().is_some_and(|t| {
                    // A nominal array family is itself a newtype struct (`struct
                    // unsigned(Logic[])`), so "is a struct" is not the
                    // question — "has fields to spread" is.
                    self.fns
                        .type_head_key(t)
                        .and_then(|h| self.nominal_field_names(&h))
                        .is_some_and(|fields| !fields.is_empty())
                        || is_single_string_type(t)
                        // `array_parts` is None for a packed vector and Some
                        // for a real array.
                        || self.array_parts(t).is_some()
                });
                if composite {
                    return Err(format!(
                        "`let {name}` inside a block is only supported for scalar \
                         values; declare a struct, array or string local at the top \
                         of the testbench"
                    ));
                }
                self.locals.borrow_mut().insert(name.clone());
                if let Some((fam, _)) = family {
                    self.local_families.borrow_mut().insert(name.clone(), fam);
                }
                if let Some(w) = width {
                    self.local_widths.borrow_mut().insert(name.clone(), w);
                }
                if let Some((left, right)) = l
                    .ty
                    .as_ref()
                    .and_then(|ty| type_index_bounds(ty, self.const_ranges, self.consts, self.fns))
                {
                    self.local_ranges
                        .borrow_mut()
                        .insert(name.clone(), (left, right));
                }
                if let Some(head) = l.ty.as_ref().and_then(|ty| self.fns.type_head_key(ty)) {
                    self.local_types.borrow_mut().insert(name.clone(), head);
                }
                let init = match &l.value {
                    Some(v) => self.value_for_local(name, v)?,
                    // Uninitialized: the type's `new()` default, as a
                    // top-level local gets.
                    None => {
                        l.ty.as_ref()
                            .and_then(|ty| self.fns.type_head_key(ty))
                            .and_then(|head| self.design.new_defaults.get(&head))
                            .map(|v| format!("{v}ULL"))
                            .unwrap_or_else(|| "0".to_string())
                    }
                };
                let init = match self.local_widths.borrow().get(name) {
                    Some(&w) => mask_c(&init, w),
                    None => init,
                };
                b.push_str(&format!(
                    "{ind}sx_value {} = {init};\n",
                    c_local_ident(name)
                ));
            }
            ast::Stmt::Expr(ast::Expr::Call {
                callee, args, bang, ..
            }) => {
                self.call(callee, args, *bang, b, depth)?;
            }
            ast::Stmt::For {
                var, range, body, ..
            } => {
                let v = &var.text;
                if let Some(path) =
                    expr_path(range).filter(|path| self.dynamic_strings.borrow().contains(path))
                {
                    let k = self.tmp.get();
                    self.tmp.set(k + 1);
                    let source = c_local_ident(&path);
                    let variable = c_local_ident(v);
                    b.push_str(&format!(
                        "{ind}{{ sx_dyn_array *_a{k} = &{source};\n\
                         {ind}for (size_t _i{k} = 0; _i{k} < _a{k}->length; ++_i{k}) {{\n\
                         {ind}sx_value {variable} = _a{k}->values[_i{k}];\n"
                    ));
                    let fresh = self.locals.borrow_mut().insert(v.clone());
                    let previous_type = self
                        .local_types
                        .borrow_mut()
                        .insert(v.clone(), "Char".to_string());
                    let previous_family = self.local_families.borrow_mut().remove(v);
                    let previous_width = self.local_widths.borrow_mut().insert(v.clone(), 32);
                    for statement in &body.stmts {
                        self.stmt(statement, b, depth + 1)?;
                    }
                    self.local_types.borrow_mut().remove(v);
                    self.local_widths.borrow_mut().remove(v);
                    if let Some(previous) = previous_type {
                        self.local_types.borrow_mut().insert(v.clone(), previous);
                    }
                    if let Some(previous) = previous_family {
                        self.local_families.borrow_mut().insert(v.clone(), previous);
                    }
                    if let Some(previous) = previous_width {
                        self.local_widths.borrow_mut().insert(v.clone(), previous);
                    }
                    if fresh {
                        self.locals.borrow_mut().remove(v);
                    }
                    b.push_str(&format!("{ind}}} }}\n"));
                    return Ok(());
                }
                // `for x in xs`: iterate a DUT-connected array via an id table.
                if let Some((path, n)) =
                    expr_path(range).and_then(|p| self.array_len(&p).map(|n| (p, n)))
                {
                    let k = self.tmp.get();
                    self.tmp.set(k + 1);
                    let indices = self
                        .local_indices
                        .borrow()
                        .get(&path)
                        .cloned()
                        .unwrap_or_else(|| (0..n as i64).collect());
                    let values: Vec<String> = indices
                        .into_iter()
                        .map(|index| {
                            let element = format!("{path}[{index}]");
                            if let Some(id) = self.map.get(&element) {
                                format!("sx_read({})", id.0)
                            } else {
                                c_local_ident(&element)
                            }
                        })
                        .collect();
                    let variable = c_local_ident(v);
                    b.push_str(&format!(
                        "{ind}{{ sx_value _a{k}[] = {{{}}};\n\
                         {ind}for (signed _i{k} = 0; _i{k} < {n}; _i{k}++) {{ \
                         sx_value {variable} = _a{k}[_i{k}];\n",
                        values.join(", ")
                    ));
                    let fresh = self.locals.borrow_mut().insert(v.clone());
                    let first_element = self
                        .local_indices
                        .borrow()
                        .get(&path)
                        .and_then(|indices| indices.first().copied())
                        .unwrap_or(0);
                    let element = format!("{path}[{first_element}]");
                    let loop_type = self.local_types.borrow().get(&element).cloned();
                    let loop_family = self.local_families.borrow().get(&element).cloned();
                    let loop_width = self.local_widths.borrow().get(&element).copied();
                    let previous_type = self.local_types.borrow_mut().remove(v);
                    let previous_family = self.local_families.borrow_mut().remove(v);
                    let previous_width = self.local_widths.borrow_mut().remove(v);
                    if let Some(ty) = loop_type {
                        self.local_types.borrow_mut().insert(v.clone(), ty);
                    }
                    if let Some(family) = loop_family {
                        self.local_families.borrow_mut().insert(v.clone(), family);
                    }
                    if let Some(width) = loop_width {
                        self.local_widths.borrow_mut().insert(v.clone(), width);
                    }
                    for s in &body.stmts {
                        self.stmt(s, b, depth + 1)?;
                    }
                    self.local_types.borrow_mut().remove(v);
                    self.local_families.borrow_mut().remove(v);
                    self.local_widths.borrow_mut().remove(v);
                    if let Some(previous) = previous_type {
                        self.local_types.borrow_mut().insert(v.clone(), previous);
                    }
                    if let Some(previous) = previous_family {
                        self.local_families.borrow_mut().insert(v.clone(), previous);
                    }
                    if let Some(previous) = previous_width {
                        self.local_widths.borrow_mut().insert(v.clone(), previous);
                    }
                    if fresh {
                        self.locals.borrow_mut().remove(v);
                    }
                    b.push_str(&format!("{ind}}} }}\n"));
                    return Ok(());
                }
                let (lo, hi) = match range {
                    ast::Expr::Range { lo, hi, .. } => (self.expr(lo)?, self.expr(hi)?),
                    _ => return Err("`for` needs a range or an array".into()),
                };
                // Inclusive, directional range (`0..2` -> 0,1,2; `2..0` -> 2,1,0):
                // step by the sign of hi-lo and break *after* running the body at
                // `hi`. The counter is signed so a descending loop to 0 doesn't
                // wrap; `{v}` is exposed as uint64_t to match index/value use.
                let k = self.tmp.get();
                self.tmp.set(k + 1);
                let variable = c_local_ident(v);
                b.push_str(&format!(
                    "{ind}{{ int64_t _lo{k} = (int64_t)({lo}), _hi{k} = (int64_t)({hi});\n\
                     {ind}signed _st{k} = _lo{k} <= _hi{k} ? 1 : -1;\n\
                     {ind}for (int64_t _c{k} = _lo{k}; ; _c{k} += _st{k}) {{\n\
                     {ind}uint64_t {variable} = (uint64_t)_c{k};\n"
                ));
                let fresh = self.locals.borrow_mut().insert(v.clone());
                let previous_type = self
                    .local_types
                    .borrow_mut()
                    .insert(v.clone(), "integer".to_string());
                let previous_family = self.local_families.borrow_mut().remove(v);
                let previous_width = self.local_widths.borrow_mut().insert(v.clone(), 64);
                for s in &body.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                self.local_types.borrow_mut().remove(v);
                self.local_widths.borrow_mut().remove(v);
                if let Some(previous) = previous_type {
                    self.local_types.borrow_mut().insert(v.clone(), previous);
                }
                if let Some(previous) = previous_family {
                    self.local_families.borrow_mut().insert(v.clone(), previous);
                }
                if let Some(previous) = previous_width {
                    self.local_widths.borrow_mut().insert(v.clone(), previous);
                }
                if fresh {
                    self.locals.borrow_mut().remove(v);
                }
                b.push_str(&format!("{ind}if (_c{k} == _hi{k}) break;\n{ind}}} }}\n"));
            }
            ast::Stmt::If(iff) => self.c_if(iff, b, depth)?,
            // A testbench-level match: first arm whose pattern hits, as a C
            // if/else-if chain over the evaluated scrutinee.
            ast::Stmt::Match(m) => {
                let scrut = self.expr(&m.scrutinee)?;
                let k = self.tmp.get();
                self.tmp.set(k + 1);
                b.push_str(&format!("{ind}{{ sx_value _m{k} = {scrut};\n"));
                let mut first = true;
                for arm in &m.arms {
                    let cond = self.pattern_cond(&arm.pattern, &m.scrutinee, &format!("_m{k}"))?;
                    let kw = if first { "if" } else { "else if" };
                    match cond {
                        Some(c) => b.push_str(&format!("{ind}{kw} {} {{\n", c_condition(&c))),
                        None => b.push_str(&format!(
                            "{ind}{} {{\n",
                            if first { "if (1)" } else { "else" }
                        )),
                    }
                    for s in &arm.body.stmts {
                        self.stmt(s, b, depth + 1)?;
                    }
                    b.push_str(&format!("{ind}}}\n"));
                    first = false;
                }
                b.push_str(&format!("{ind}}}\n"));
            }
            _ => {}
        }
        Ok(())
    }

    /// `if`/`else if`/`else` chains, recursing through the else branch.
    fn c_if(&self, iff: &ast::IfStmt, b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        let c = self.expr(&iff.cond)?;
        // Binary/comparison lowering already encloses its expression. Avoid
        // producing `if ((a == b))`, which Clang diagnoses as suspicious,
        // while still parenthesizing a bare name or literal as C requires.
        let condition = c_condition(&c);
        b.push_str(&format!("{ind}if {condition} {{\n"));
        for s in &iff.then.stmts {
            self.stmt(s, b, depth + 1)?;
        }
        b.push_str(&format!("{ind}}}\n"));
        match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => {
                b.push_str(&format!("{ind}else {{\n"));
                for s in &block.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                b.push_str(&format!("{ind}}}\n"));
            }
            Some(ast::ElseBranch::If(inner)) => {
                b.push_str(&format!("{ind}else {{\n"));
                self.c_if(inner, b, depth + 1)?;
                b.push_str(&format!("{ind}}}\n"));
            }
            None => {}
        }
        Ok(())
    }

    /// Expand a format string into a printf format plus argument expressions,
    /// honouring `{{`/`}}` escapes and rendering enum/real/integer operands the
    /// way `print!` does. Shared by `print!` and the `assert!`/`warn!` messages.
    /// A stand-in for `e` when deciding how to *render* it. A branch-valued
    /// expression has no name or type of its own, but its branches do, and the
    /// type checker has already made them agree — so the first branch answers
    /// "is this a Char / an enum / a signed vector" for the whole expression.
    /// The value still comes from `e` itself.
    fn type_witness(e: &ast::Expr) -> &ast::Expr {
        match e {
            ast::Expr::IfExpr { then, .. } => Self::type_witness(then),
            ast::Expr::Match { arms, .. } => match arms.iter().find_map(|a| a.value_expr()) {
                Some(v) => Self::type_witness(v),
                None => e,
            },
            // An operator that keeps its operands' family renders as they do,
            // so `a and b` on `Logic` prints a symbol rather than the raw
            // discriminant. A literal operand carries no name to look a type
            // up by, so prefer the side that does: `x and '1'` reads as `x`.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                let l = Self::type_witness(lhs);
                if expr_path(l).is_some() {
                    l
                } else {
                    Self::type_witness(rhs)
                }
            }
            ast::Expr::Unary {
                op: ast::UnOp::Not,
                rhs,
                ..
            } => Self::type_witness(rhs),
            _ => e,
        }
    }

    fn c_format(&self, text: &str, args: &[ast::Expr]) -> Result<(String, Vec<String>), String> {
        let mut cfmt = String::new();
        let mut cargs = Vec::new();
        let mut vals = args.iter();
        for part in siox::syntax::format::parts(text) {
            let a = match part {
                siox::syntax::format::FormatPart::Text(t) => {
                    cfmt.push_str(&c_escape(&t).replace('%', "%%"));
                    continue;
                }
                siox::syntax::format::FormatPart::Placeholder => vals.next(),
            };
            let Some(a) = a else { continue };
            // Type questions go through a witness so a branch-valued argument
            // is rendered as whatever its branches are.
            let w = Self::type_witness(a);
            let sig = expr_path(w)
                .and_then(|p| self.map.get(&p))
                .map(|id| &self.design.signals[id.0 as usize]);
            let local_type = expr_path(w)
                .and_then(|path| self.local_types.borrow().get(&path).cloned())
                .or_else(|| {
                    // A dynamic index (`s[i]`) has no path, but every element
                    // of an array shares a type, so element 0 answers for the
                    // whole of it. Without this `s[i]` on a string printed the
                    // code point where `s[0]` printed the character.
                    let ast::Expr::Index { base, .. } = w else {
                        return None;
                    };
                    let base = expr_path(base)?;
                    if self.dynamic_strings.borrow().contains(&base) {
                        return Some("Char".to_string());
                    }
                    self.local_types
                        .borrow()
                        .get(&format!("{base}[0]"))
                        .cloned()
                });
            let string_elems = self.formatted_string_elems(a);
            let is_real = self.is_real_operand(a);
            let is_char = sig.is_some_and(|signal| signal.char)
                || local_type.as_deref() == Some("Char")
                || self.call_return_head(w).as_deref() == Some("Char");
            let ety: Option<String> = sig
                .and_then(|s| s.enum_type.clone())
                .or(local_type)
                .or_else(|| {
                    // A call returning an enum renders its symbol, like a
                    // signal or local of that type does.
                    let head = self.call_return_head(w)?;
                    self.design.enum_syms.contains_key(&head).then_some(head)
                })
                .or_else(|| {
                    // `x'ascending` is a `Bool`, and the only range attribute
                    // that is not a number — the others are covered by
                    // `is_integer_operand`. Without this it printed its
                    // discriminant, 0 or 1, where hardware said false/true.
                    let ast::Expr::SysAttr { attr, .. } = w else {
                        return None;
                    };
                    (attr.text == "ascending" && self.design.enum_syms.contains_key("Bool"))
                        .then(|| "Bool".to_string())
                })
                .or_else(|| {
                    // The variant itself names its enum: `print!("{}",
                    // State::Done)`. Everything reached through a name
                    // rendered as `Done` while the literal variant — the one
                    // form that says its type outright — printed `2`.
                    let ast::Expr::Path(p) = w else { return None };
                    self.fns
                        .enum_variant_key(p)
                        .map(|(enumeration, _)| enumeration)
                        .filter(|enumeration| self.design.enum_syms.contains_key(enumeration))
                });
            let enum_syms = ety.as_ref().and_then(|e| self.design.enum_syms.get(e));
            if let ast::Expr::StrLit { text, .. } = a {
                cfmt.push_str("%s");
                cargs.push(format!("\"{}\"", c_escape(text)));
            } else if let Some(path) = self.dynamic_string_path(a) {
                cfmt.push_str("%s");
                cargs.push(format!("{}.text", c_local_ident(&path)));
            } else if let Some(elems) = string_elems {
                cfmt.push_str("%s");
                if elems.is_empty() {
                    cargs.push("\"\"".into());
                } else {
                    let capacity = elems.len().saturating_mul(4).saturating_add(1);
                    cargs.push(format!(
                        "sx_chars((sx_value[]){{{}}}, {}, (char[{capacity}]){{0}})",
                        elems.join(", "),
                        elems.len()
                    ));
                }
            } else if let Some(syms) = enum_syms {
                let mut tern = String::from("\"?\"");
                for (disc, sym) in syms {
                    let esc = c_escape(sym);
                    tern = format!("(_v=={disc}?\"{esc}\":{tern})");
                }
                cfmt.push_str("%s");
                cargs.push(format!(
                    "({{ long long _v = (long long)({}); {tern}; }})",
                    self.expr(a)?
                ));
            } else if is_real {
                cfmt.push_str("%g");
                cargs.push(format!("sx_f64({})", self.value_for_print(a)?));
            } else if is_char {
                cfmt.push_str("%s");
                cargs.push(format!("sx_utf8(({}), (char[5]){{0}})", self.expr(a)?));
            } else if Self::is_literal_integer_expr(a) || self.is_integer_operand(a) {
                cfmt.push_str("%lld");
                let rendered = self.expr(a)?;
                cargs.push(format!(
                    "(long long)({})",
                    self.c_integer_operand(a, &rendered)
                ));
            } else if let Some(width) = self.signed_vector_width(a) {
                cfmt.push_str("%lld");
                let rendered = self.expr(a)?;
                cargs.push(format!("(long long)sx_i64(({rendered}), {width})"));
            } else {
                cfmt.push_str("%s");
                cargs.push(format!(
                    "sx_decimal(({}), (char[SX_DECIMAL_CAP]){{0}})",
                    self.expr(a)?
                ));
            }
        }
        Ok((cfmt, cargs))
    }

    /// A `#line` directive pointing at a span's source line.
    ///
    /// Clang turns these into DWARF that names the `.siox` file, so
    /// `break counter.siox:34`, stepping, and source display all work without
    /// the debugger knowing anything about siox. `None` when the span has no
    /// file, or when the path cannot be written as a C string.
    fn line_directive(&self, span: siox::diag::Span) -> Option<String> {
        let file = self.sources.get(span.file)?;
        let (line, _) = self.sources.line_col(span.file, span.start);
        // An absolute path lets a debugger find the source from any working
        // directory, which is how a generated binary is usually run.
        let path = std::fs::canonicalize(&file.name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file.name.clone());
        Some(format!("#line {line} \"{}\"\n", c_escape(&path)))
    }

    /// `file:line:col` for a source span, as a C string literal assignment.
    ///
    /// A failing assertion used to report only its message, so finding the
    /// statement in a large testbench meant reading it. The span is known at
    /// emit time; nothing needs to be looked up when the test runs.
    fn set_location(&self, span: siox::diag::Span) -> Option<String> {
        let at = span_location(self.sources, span)?;
        Some(format!("g_loc = \"{}\";", c_escape(&at)))
    }

    /// The C expression that sets `g_msg` for an `assert!`/`warn!` message at
    /// `args[at]`. With no format arguments this is a plain string literal;
    /// with them it formats into a static buffer first.
    fn c_message(&self, args: &[ast::Expr], at: usize, fallback: &str) -> Result<String, String> {
        let Some(text) = args.get(at).and_then(str_lit) else {
            return Ok(format!("g_msg = \"{}\";", c_escape(fallback)));
        };
        let rest = args.get(at + 1..).unwrap_or(&[]);
        if rest.is_empty() {
            return Ok(format!("g_msg = \"{}\";", c_escape(&text)));
        }
        let (cfmt, cargs) = self.c_format(&text, rest)?;
        let list = if cargs.is_empty() {
            String::new()
        } else {
            format!(", {}", cargs.join(", "))
        };
        let decimal_capacity = u64::from(self.value_bits)
            .saturating_mul(30_103)
            .div_ceil(100_000)
            .saturating_add(2);
        let enum_capacity = self
            .design
            .enum_syms
            .values()
            .flat_map(|symbols| symbols.values().map(|symbol| symbol.len() as u64 + 1))
            .max()
            .unwrap_or(0);
        let string_capacity = rest
            .iter()
            .filter_map(|arg| match arg {
                ast::Expr::StrLit { text, .. } => Some(text.len() as u64 + 1),
                _ => self
                    .formatted_string_elems(arg)
                    .map(|elems| (elems.len() as u64).saturating_mul(4).saturating_add(1)),
            })
            .max()
            .unwrap_or(0);
        let argument_capacity = decimal_capacity
            .max(enum_capacity)
            .max(string_capacity)
            .max(32);
        let capacity = u64::try_from(text.len())
            .unwrap_or(u64::MAX)
            .saturating_add(
                u64::try_from(rest.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(argument_capacity),
            )
            .saturating_add(1);
        let id = self.message_id.get();
        self.message_id.set(id.saturating_add(1));
        Ok(format!(
            "static char _sx_msg_{id}[{capacity}]; \
             snprintf(_sx_msg_{id}, sizeof _sx_msg_{id}, \"{cfmt}\"{list}); \
             g_msg = _sx_msg_{id};"
        ))
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
            // `tick`/`wait` were removed: `await` is the one timing primitive
            // (`wait` errors at parse; tick() returns to std later as source).
            "tick" => {
                return Err(
                    "`tick()` was removed (it returns as a std function later); \
                            write `clk = '1'; await 5ns; clk = '0';` or start a clock \
                            generator (`clk = not clk after 5ns;`)"
                        .into(),
                );
            }
            // clock(clk, period): register a background clock on the wheel
            // (init low; first toggle one half period from now).
            // clock() was sugar; the canonical generator is the after-form.
            "clock" => {
                return Err(
                    "`clock()` was removed; write `clk = not clk after <half-period>;`".into(),
                );
            }
            // await <duration> | <edge> | <condition>.
            "await" => self.emit_await(args, b, depth)?,
            // print!: expand the format at compile time into a printf.
            "print" if bang => {
                let Some(ast::Expr::StrLit { text, .. }) = args.first() else {
                    return Err("print! needs a format string".into());
                };
                let (cfmt, cargs) = self.c_format(text, &args[1..])?;
                let call_args = if cargs.is_empty() {
                    String::new()
                } else {
                    format!(", {}", cargs.join(", "))
                };
                b.push_str(&format!("{ind}printf(\"{cfmt}\\n\"{call_args});\n"));
            }
            // seed!(n): reseed the deterministic RNG.
            "seed" => {
                let n = self.expr(args.first().ok_or("seed! needs a value")?)?;
                b.push_str(&format!("{ind}g_rand = ({n}) ? ({n}) : 1;\n"));
            }
            // stop!/finish!: end the test cleanly (passing).
            "stop" | "finish" => {
                b.push_str(&format!(
                    "{ind}if (g_range_failed) return 1; \
                     printf(\"{name} at %llu fs\\n\", (unsigned long long)_now); return 0;\n"
                ));
            }
            "assert" if bang => {
                let cond = args.first().ok_or("assert needs a condition")?;
                let c = self.expr(cond)?;
                let set = self.c_message(args, 1, "assertion failed")?;
                // Record the failure message and where it was written, then
                // fail this test; `main` prints the `test <name> ... FAILED`
                // line, the message, and the location.
                let at = self
                    .set_location(ast::expr_span(callee))
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default();
                b.push_str(&format!(
                    "{ind}{{ signed _ok = !!({c}); if (g_range_failed) return 1; \
                     if (!_ok) {{ {set}{at} return 1; }} }}\n"
                ));
            }
            // warn!(cond, msg): non-fatal — report to stderr, keep running.
            "warn" if bang => {
                let cond = args.first().ok_or("warn needs a condition")?;
                let c = self.expr(cond)?;
                let set = self.c_message(args, 1, "warning")?;
                b.push_str(&format!(
                    "{ind}{{ signed _ok = !!({c}); if (g_range_failed) return 1; \
                     if (!_ok) {{ {set} fprintf(stderr, \"warning: %s\\n\", g_msg); g_warnings++; }} }}\n"
                ));
            }
            // A method call in statement position (`r.set(7)`): the callee is
            // a field, so it never matched a name above and fell out of this
            // dispatch without a word. Hardware inlines it as drivers; the
            // testbench has to inline it as statements, or a mutating method
            // does nothing at all here while the identical call works in an
            // entity body.
            _ => {
                if let ast::Expr::Field { base, field, .. } = callee {
                    if let Some(ty) = self.receiver_type(base) {
                        let (stmts, _) = self.method_body(&ty, &field.text, base, args)?;
                        for stmt in &stmts {
                            self.stmt(stmt, b, depth)?;
                        }
                    }
                // A user free function in statement position (`store(r, 7)`)
                // reaches here too: its callee is a path that matches none of
                // the builtin names above, so it fell out of the dispatch
                // silently, exactly as the method form did. Hardware inlines
                // it (`lower_free_stmt`); so does this now.
                } else if let Some(stmts) = self.free_fn_body(callee, args) {
                    for stmt in &stmts? {
                        self.stmt(stmt, b, depth)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Element count of a DUT-connected array in the signal map.
    fn array_len(&self, path: &str) -> Option<u64> {
        if let Some(indices) = self.local_indices.borrow().get(path) {
            return u64::try_from(indices.len())
                .ok()
                .filter(|length| *length > 0);
        }
        let mut n = 0;
        while self.map.contains_key(&format!("{path}[{n}]")) {
            n += 1;
        }
        (n > 0).then_some(n)
    }

    fn dynamic_string_path(&self, expression: &ast::Expr) -> Option<String> {
        let path = expr_path(expression)?;
        self.dynamic_strings
            .borrow()
            .contains(&path)
            .then_some(path)
    }

    fn dynamic_string_index(&self, expression: &ast::Expr) -> Option<Result<String, String>> {
        let ast::Expr::Index { base, index, .. } = expression else {
            return None;
        };
        if matches!(
            index.as_ref(),
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return None;
        }
        let path = self.dynamic_string_path(base)?;
        let location = span_location(self.sources, ast::expr_span(index))
            .map(|at| format!("\"{}\"", c_escape(&at)))
            .unwrap_or_else(|| "0".to_string());
        Some(self.expr(index).map(|index| {
            format!(
                "sx_dyn_get_checked(&{}, ({index}), {location})",
                c_local_ident(&path)
            )
        }))
    }

    /// Whether an operand reads a `Char` signal (so a `'x'` literal counterpart
    /// is a code point, not a logic code).
    fn is_char_operand(&self, e: &ast::Expr) -> bool {
        // A branch-valued operand is a character if its branches are.
        let e = Self::type_witness(e);
        // A call has no path to look up, so its declared return type is the
        // only thing that says it yields a character.
        if self.call_return_head(e).as_deref() == Some("Char") {
            return true;
        }
        if let ast::Expr::Index { base, .. } = e {
            if self.dynamic_string_path(base).is_some() {
                return true;
            }
        }
        let Some(path) = expr_path(e) else {
            return false;
        };
        self.map
            .get(&path)
            .map(|&id| self.design.signals[id.0 as usize].char)
            .unwrap_or(false)
            || self.local_types.borrow().get(&path).map(String::as_str) == Some("Char")
    }

    /// An operand in a `Char` comparison: a `'x'` literal is its Unicode code
    /// point; anything else translates normally.
    fn c_char_operand(&self, e: &ast::Expr) -> Result<String, String> {
        match e {
            ast::Expr::CharLit { ch, .. } => Ok(format!("{}ULL", *ch as u32)),
            _ => self.expr(e),
        }
    }

    /// Whether an operand is a bare character/logic literal (`'g'`).
    fn is_char_lit(&self, e: &ast::Expr) -> bool {
        matches!(e, ast::Expr::CharLit { .. })
    }

    /// A char literal's position in enum `en` (VHDL `T'pos`), data-driven from
    /// the enum's declaration — `None` if `e` is not a char literal.
    fn enum_char_lit(&self, en: &str, e: &ast::Expr) -> Option<u64> {
        if let ast::Expr::CharLit { ch, .. } = e {
            return self
                .enums
                .get(en)
                .and_then(|m| m.get(&format!("'{ch}'")))
                .copied();
        }
        None
    }

    fn char_value_for_target(&self, name: &str, ch: char) -> u64 {
        if let Some(id) = self.map.get(name) {
            let signal = &self.design.signals[id.0 as usize];
            if signal.char {
                return ch as u32 as u64;
            }
            if let Some(enum_name) = &signal.enum_type {
                if let Some(value) = self
                    .enums
                    .get(enum_name)
                    .and_then(|symbols| symbols.get(&format!("'{ch}'")))
                {
                    return *value;
                }
            }
        }
        if let Some(ty) = self.local_types.borrow().get(name) {
            if ty == "Char" {
                return ch as u32 as u64;
            }
            if let Some(value) = self
                .enums
                .get(ty)
                .and_then(|symbols| symbols.get(&format!("'{ch}'")))
            {
                return *value;
            }
        }
        ch as u32 as u64
    }

    /// The enum type of an operand that is an enum-typed signal or testbench
    /// local — so a char-literal counterpart resolves by position in that enum.
    fn enum_operand_type(&self, e: &ast::Expr) -> Option<String> {
        let p = expr_path(e)?;
        if let Some(&id) = self.map.get(&p) {
            if let Some(en) = &self.design.signals[id.0 as usize].enum_type {
                return Some(en.clone());
            }
        }
        self.local_types
            .borrow()
            .get(&p)
            .filter(|ty| self.enums.contains_key(*ty))
            .cloned()
    }

    /// An operand in an enum comparison: a char literal takes its position in
    /// `en`; anything else translates normally.
    fn enum_operand_c(&self, en: &str, e: &ast::Expr) -> Result<String, String> {
        match self.enum_char_lit(en, e) {
            Some(d) => Ok(format!("{d}ULL")),
            None => self.expr(e),
        }
    }

    /// Whether an operand reads a `real` signal.
    /// The type name of a conversion argument, when the emitter knows it.
    /// `None` means "not known here", and the caller stays permissive rather
    /// than rejecting something it merely cannot see.
    fn arg_type_name(&self, e: &ast::Expr) -> Option<String> {
        if let ast::Expr::Path(path) = e {
            if let Some((enumeration, _)) = self.fns.enum_variant_key(path) {
                return Some(enumeration);
            }
        }
        // A call reads as its declared return type, so an operator whose left
        // operand is itself a call (`twice(a) + a`) can still find its impl.
        if let ast::Expr::Call { callee, args, .. } = e {
            let ret = match callee.as_ref() {
                ast::Expr::Field { base, field, .. } => self
                    .arg_type_name(base)
                    .and_then(|ty| self.methods.get(&(ty, field.text.clone())).copied()),
                _ => self.fns.get(callee),
            }
            .and_then(|f| f.ret.as_ref())
            .and_then(|ty| resolved_type_name(ty, self.type_aliases, self.fns));
            if let Some(ret) = ret {
                return Some(ret);
            }
            let _ = args;
        }
        let path = expr_path(e)?;
        if let Some(t) = self.local_types.borrow().get(&path) {
            return Some(t.clone());
        }
        // A bit of a packed vector carries its family's element enum.
        if let ast::Expr::Index { base, .. } = e {
            let family = self
                .local_families
                .borrow()
                .get(&expr_path(base)?)
                .cloned()?;
            return self.design.array_element_of_family.get(&family).cloned();
        }
        None
    }

    /// Whether `src` converts to `target`, by the same rule lowering uses: an
    /// explicit `impl From<src> for target`, or a derivation chain along which
    /// every source variant exists in the target (representation-identity).
    fn enum_conversion_exists(&self, src: &str, target: &str) -> bool {
        if src == target {
            return true;
        }
        if let Some(fns) = self.op_impls.get(&("From".to_string(), target.to_string())) {
            let declared = |f: &ast::FnDecl, a: &Option<String>| -> Option<String> {
                a.clone().or_else(|| {
                    f.params
                        .iter()
                        .find(|p| !p.is_self)
                        .and_then(|p| p.ty.as_ref())
                        .and_then(|ty| self.fns.type_head_key(ty))
                })
            };
            if fns
                .iter()
                .any(|(f, a)| declared(f, a).as_deref() == Some(src))
            {
                return true;
            }
        }
        let ancestor = |from: &str, to: &str| {
            let mut cur = from.to_string();
            let mut seen = 0;
            while let Some(b) = self.design.enum_bases.get(&cur) {
                if b == to {
                    return true;
                }
                cur = b.clone();
                seen += 1;
                if seen > 64 {
                    break; // a cycle; the frontend reports it
                }
            }
            false
        };
        // Chain-connected is enough: the newtype form takes no body, so a
        // derived enum cannot add variants and the conversion is total in both
        // directions. (Comparing variant sets here would be wrong anyway —
        // this map holds each enum's *own* variants, and a newtype's are
        // empty.)
        ancestor(src, target) || ancestor(target, src)
    }

    /// The family and width a conversion names: `signed[8](x)` -> ("signed", 8).
    /// A conversion is a `Call` whose callee is the indexed family, so it has
    /// no declared return type to consult like an ordinary call does.
    fn conversion_target(&self, e: &ast::Expr) -> Option<(String, u32)> {
        let ast::Expr::Call { callee, .. } = e else {
            return None;
        };
        let ast::Expr::Index { base, index, .. } = callee.as_ref() else {
            return None;
        };
        let head = expr_path(base)?;
        let w = siox::ir::eval_const_fns(index, &HashMap::new(), self.fns, 0)? as u32;
        (w > 0 && w <= 64).then_some((head, w))
    }

    /// The declared return type of a call, for recovering everything the call
    /// site would otherwise throw away: a vector's width and family, an enum's
    /// symbols. See `call_return_head` for the name alone.
    fn call_return_type(&self, e: &ast::Expr) -> Option<ast::Type> {
        let ast::Expr::Call { callee, .. } = e else {
            return None;
        };
        if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
            let receiver = self.receiver_type(base)?;
            return self
                .methods
                .get(&(receiver, field.text.clone()))
                .and_then(|f| f.ret.clone());
        }
        self.fns.get(callee).and_then(|f| f.ret.clone())
    }

    /// The head name of a call's declared return type. `is_real_operand`
    /// already consulted this for `real`; nothing else did, so a function
    /// returning `Char` produced a value that displayed and compared as a
    /// plain number at the call site while the same value bound to a typed
    /// local behaved correctly.
    fn call_return_head(&self, e: &ast::Expr) -> Option<String> {
        let ast::Expr::Call { callee, .. } = e else {
            return None;
        };
        if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
            let receiver = self.receiver_type(base)?;
            return self
                .methods
                .get(&(receiver, field.text.clone()))
                .and_then(|f| f.ret.as_ref())
                .and_then(|ty| resolved_type_name(ty, self.type_aliases, self.fns));
        }
        self.fns
            .get(callee)
            .and_then(|f| f.ret.as_ref())
            .and_then(|ty| resolved_type_name(ty, self.type_aliases, self.fns))
    }

    fn is_real_operand(&self, e: &ast::Expr) -> bool {
        match e {
            ast::Expr::Int { text, .. } if text.contains('.') => return true,
            ast::Expr::SuffixLit { suffix, .. }
                if suffix.text == "Hz" || suffix.text.ends_with("Hz") =>
            {
                return true;
            }
            ast::Expr::Unary {
                op: ast::UnOp::Neg,
                rhs,
                ..
            } => return self.is_real_operand(rhs),
            ast::Expr::Binary { op, lhs, rhs, .. }
                if !matches!(
                    op,
                    ast::BinOp::Eq
                        | ast::BinOp::Ne
                        | ast::BinOp::Lt
                        | ast::BinOp::Le
                        | ast::BinOp::Gt
                        | ast::BinOp::Ge
                ) =>
            {
                return self.is_real_operand(lhs) || self.is_real_operand(rhs);
            }
            ast::Expr::IfExpr { then, els, .. } => {
                return self.is_real_operand(then) || self.is_real_operand(els);
            }
            ast::Expr::Match { arms, .. } => {
                return arms
                    .iter()
                    .filter_map(ast::MatchArm::value_expr)
                    .any(|value| self.is_real_operand(value));
            }
            ast::Expr::Call { callee, .. } => {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    if let Some(receiver) = self.receiver_type(base) {
                        return self
                            .methods
                            .get(&(receiver, field.text.clone()))
                            .and_then(|function| function.ret.as_ref())
                            .is_some_and(|ty| self.type_is(ty, "real"));
                    }
                }
                if matches!(callee.as_ref(), ast::Expr::Path(path)
                    if path.segments.len() == 1 && path.segments[0].text == "uniform")
                {
                    return true;
                }
                if siox::ir::call_fn_key(callee).is_some() {
                    return self
                        .fns
                        .get(callee)
                        .and_then(|function| function.ret.as_ref())
                        .is_some_and(|ty| self.type_is(ty, "real"));
                }
            }
            _ => {}
        }
        if self
            .fns
            .constant_expr_key(e)
            .is_some_and(|key| self.real_consts.contains(&key))
        {
            return true;
        }
        let Some(path) = expr_path(e) else {
            return false;
        };
        if self
            .fn_type_env
            .borrow()
            .last()
            .and_then(|types| types.get(&path))
            .map(String::as_str)
            == Some("real")
        {
            return true;
        }
        self.map
            .get(&path)
            .map(|&id| self.design.signals[id.0 as usize].real)
            .unwrap_or(false)
            || self.local_types.borrow().get(&path).map(String::as_str) == Some("real")
    }

    /// Whether an operand has the kernel signed `integer` type. Integer
    /// literals alone stay contextual (so `unsigned_value < 5` still dispatches
    /// as unsigned); an explicitly typed path/call/attribute selects signed
    /// comparisons, division, and right shift.
    /// The width of a `signed`-family value whose declared type this stage can
    /// see, for display. The compiler tracks no signedness — `signed` is just
    /// the family whose std impls give it a signed `Ord`, an arithmetic `Shr`
    /// and a signed `Div` — so printing has to recover it from the declared
    /// type, the same way this stage already recovers `Char` and enum symbols.
    ///
    /// Without it a `signed[8]` holding -5 prints as 251 while `s < 0` is
    /// simultaneously true, which reads as a contradiction.
    fn signed_vector_width(&self, e: &ast::Expr) -> Option<u32> {
        // A branch-valued expression is as signed as its branches; the type
        // checker has already made them agree. `is_real_operand` looks through
        // these shapes for `real`, and nothing did for `signed`, so
        // `if c { a } else { b }` on signed values read back as a bit pattern.
        match e {
            ast::Expr::IfExpr { then, els, .. } => {
                return self
                    .signed_vector_width(then)
                    .or_else(|| self.signed_vector_width(els));
            }
            ast::Expr::Match { arms, .. } => {
                return arms
                    .iter()
                    .filter_map(|a| a.value_expr())
                    .find_map(|v| self.signed_vector_width(v));
            }
            // Arithmetic keeps its operands' family and width, so an unbound
            // `a - b` renders as the signed value it is. Binding it first
            // (`let d: signed[8] = a - b;`) printed correctly while the inline
            // form showed the raw pattern — -9 came out as 247.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                return self
                    .signed_vector_width(lhs)
                    .or_else(|| self.signed_vector_width(rhs));
            }
            ast::Expr::Unary { rhs, .. } => return self.signed_vector_width(rhs),
            _ => {}
        }
        // A conversion names its target directly: `signed[8](x)` is a Call
        // whose callee is the indexed family. Hardware read this as signed and
        // the testbench did not, so the two disagreed on `signed[8](200)`.
        if let Some((fam, w)) = self.conversion_target(e) {
            if self.type_name_is(&fam, "signed") {
                return Some(w);
            }
        }
        // A call carries no name; its declared return type is what says the
        // result is signed and how wide it is.
        if let Some(ret) = self.call_return_type(e) {
            if self.type_is(&ret, "signed") {
                if let Some(w) = self.declared_width(&ret) {
                    if w > 0 && w <= 64 {
                        return Some(w);
                    }
                }
            }
        }
        let path = expr_path(e)?;
        // `local_families` is the declared nominal array family; it is recorded for
        // connected locals and struct leaves too, which `local_types` is not.
        let family = self
            .local_families
            .borrow()
            .get(&path)
            .cloned()
            .or_else(|| self.local_types.borrow().get(&path).cloned())?;
        if !self.type_name_is(&family, "signed") {
            return None;
        }
        // A connected name records no local width — the signal it is wired to
        // carries it — so fall back to that rather than losing the family.
        let width = self.local_widths.borrow().get(&path).copied().or_else(|| {
            self.map
                .get(&path)
                .map(|id| self.design.signals[id.0 as usize].width)
        })?;
        (width > 0 && width <= 64).then_some(width)
    }

    /// An expression whose every leaf is an integer literal that fits a word.
    /// It has no declared type to consult, so nothing marked it as a kernel
    /// integer and `print!("{}", 0 - 2)` went through the unsigned decimal
    /// path and printed 2^64 - 2, while the same value bound to a `signed[8]`
    /// printed -2.
    ///
    /// Asked only at the rendering decision, never inside
    /// `is_integer_operand`'s recursion: that walks a `Binary` with `or`, so a
    /// narrow literal beside a wide one would drag the pair onto the 64-bit
    /// path and truncate it.
    fn is_literal_integer_expr(e: &ast::Expr) -> bool {
        match e {
            // A literal too wide for the native word keeps the wide path: the
            // integer rendering casts to `long long`, which would truncate
            // `18446744073709551616 + 1` to 1.
            ast::Expr::Int { text, .. } => !text.contains('.') && literal_fits_word(text),
            ast::Expr::Unary { rhs, .. } => Self::is_literal_integer_expr(rhs),
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                Self::is_literal_integer_expr(lhs) && Self::is_literal_integer_expr(rhs)
            }
            _ => false,
        }
    }

    fn is_integer_operand(&self, e: &ast::Expr) -> bool {
        match e {
            ast::Expr::Unary {
                op: ast::UnOp::Neg,
                rhs,
                ..
            } => {
                return matches!(rhs.as_ref(), ast::Expr::Int { text, .. } if !text.contains('.'))
                    || self.is_integer_operand(rhs);
            }
            ast::Expr::Unary { rhs, .. } => return self.is_integer_operand(rhs),
            ast::Expr::Binary { lhs, rhs, .. } => {
                return self.is_integer_operand(lhs) || self.is_integer_operand(rhs);
            }
            ast::Expr::IfExpr { then, els, .. } => {
                return self.is_integer_operand(then) || self.is_integer_operand(els);
            }
            ast::Expr::Match { arms, .. } => {
                return arms
                    .iter()
                    .filter_map(ast::MatchArm::value_expr)
                    .any(|value| self.is_integer_operand(value));
            }
            ast::Expr::SysAttr { attr, .. }
                if matches!(
                    attr.text.as_str(),
                    "left" | "right" | "high" | "low" | "length"
                ) =>
            {
                return true;
            }
            ast::Expr::Call { callee, .. } => {
                // The kernel conversion `integer(x)` produces a signed kernel
                // value (truncating a `real`, crossing out of a vector). Without
                // this a direct `integer(r) < 0` compared unsigned and was false
                // for every negative `r`, though the same value stored in an
                // `integer` local first compared correctly.
                if let ast::Expr::Path(p) = callee.as_ref() {
                    if p.segments.len() == 1 && p.segments[0].text == "integer" {
                        return true;
                    }
                }
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    if let Some(receiver) = self.receiver_type(base) {
                        return self
                            .methods
                            .get(&(receiver, field.text.clone()))
                            .and_then(|function| function.ret.as_ref())
                            .is_some_and(|ty| self.type_is(ty, "integer"));
                    }
                }
                if siox::ir::call_fn_key(callee).is_some() {
                    return self
                        .fns
                        .get(callee)
                        .and_then(|function| function.ret.as_ref())
                        .is_some_and(|ty| self.type_is(ty, "integer"));
                }
            }
            _ => {}
        }
        if self
            .fns
            .constant_expr_key(e)
            .is_some_and(|key| self.integer_consts.contains(&key))
        {
            return true;
        }
        let Some(path) = expr_path(e) else {
            return false;
        };
        self.fn_type_env
            .borrow()
            .last()
            .and_then(|types| types.get(&path))
            .map(String::as_str)
            == Some("integer")
            || self
                .local_types
                .borrow()
                .get(&path)
                .is_some_and(|name| self.type_name_is(name, "integer"))
            || self
                .map
                .get(&path)
                .is_some_and(|id| self.design.signals[id.0 as usize].integer)
    }

    fn c_integer_operand(&self, e: &ast::Expr, rendered: &str) -> String {
        let signed_width = expr_path(e).and_then(|path| {
            if let Some(id) = self
                .map
                .get(&path)
                .filter(|id| self.design.signals[id.0 as usize].integer)
            {
                let signal = &self.design.signals[id.0 as usize];
                return signal
                    .range
                    .map(|(lo, _)| lo < 0)
                    .unwrap_or(true)
                    .then_some(signal.width);
            }
            let integer_local = self
                .local_types
                .borrow()
                .get(&path)
                .is_some_and(|name| self.type_name_is(name, "integer"));
            if !integer_local {
                return None;
            }
            let negative_capable = self
                .local_ranges
                .borrow()
                .get(&path)
                .map(|(lo, _)| *lo < 0)
                .unwrap_or(true);
            negative_capable
                .then(|| self.local_widths.borrow().get(&path).copied())
                .flatten()
        });
        match signed_width {
            Some(width) => format!("sx_i64(({rendered}), {width})"),
            None => format!("((int64_t)({rendered}))"),
        }
    }

    /// An operand in a `real` comparison, as a C `double`: a real signal reads
    /// its bits, an integer/decimal literal is a float constant, anything else
    /// is cast.
    fn c_real_operand(&self, e: &ast::Expr) -> Result<String, String> {
        match e {
            ast::Expr::Int { text, .. } => Ok(format!("((double){})", text.replace('_', ""))),
            ast::Expr::SuffixLit { text, suffix, .. }
                if suffix.text == "Hz" || suffix.text.ends_with("Hz") =>
            {
                let scale = ast::suffix_scale(&suffix.text).unwrap_or(1);
                Ok(format!("((double)({}) * {scale}.0)", text.replace('_', "")))
            }
            _ if self.is_real_operand(e) => {
                if let Some(path) = expr_path(e) {
                    if self
                        .fn_env
                        .borrow()
                        .last()
                        .is_some_and(|environment| environment.contains_key(&path))
                    {
                        return Ok(format!("sx_f64({})", self.expr(e)?));
                    }
                    if self.locals.borrow().contains(&path) {
                        return Ok(format!("sx_f64({})", c_local_ident(&path)));
                    }
                    if let Some(id) = self.map.get(&path) {
                        return Ok(format!("sx_f64(sx_read({}))", id.0));
                    }
                }
                Ok(format!("sx_f64({})", self.expr(e)?))
            }
            _ => Ok(format!("((double)({}))", self.expr(e)?)),
        }
    }

    /// The per-element C read-expressions of a `Char` array (a string) operand:
    /// a connected string reads each element signal, a local reads its C
    /// element locals. `None` if `e` isn't an array-shaped name.
    fn c_string_elems(&self, e: &ast::Expr) -> Option<Vec<String>> {
        let path = expr_path(e)?;
        if let Some(indices) = self.local_indices.borrow().get(&path).cloned() {
            let values = indices
                .into_iter()
                .filter_map(|index| {
                    let element = format!("{path}[{index}]");
                    if let Some(id) = self.map.get(&element) {
                        Some(format!("sx_read({})", id.0))
                    } else if self.locals.borrow().contains(&element) {
                        Some(c_local_ident(&element))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            return Some(values);
        }
        // A connected string: element signals `path[i]` in the map.
        if let Some(n) = self.array_len(&path) {
            return Some(
                (0..n)
                    .map(|i| format!("sx_read({})", self.map[&format!("{path}[{i}]")].0))
                    .collect(),
            );
        }
        // A testbench-local string: one C local per element (`sxl_path_i`).
        let mut elems = Vec::new();
        while self
            .locals
            .borrow()
            .contains(&format!("{path}[{}]", elems.len()))
        {
            elems.push(c_local_ident(&format!("{path}[{}]", elems.len())));
        }
        if !elems.is_empty()
            || self.local_types.borrow().get(&path).map(String::as_str) == Some("string")
        {
            Some(elems)
        } else {
            None
        }
    }

    fn c_string_value_slice(&self, expression: &ast::Expr) -> Option<(String, String)> {
        if let Some(path) = self.dynamic_string_path(expression) {
            let ident = c_local_ident(&path);
            return Some((format!("{ident}.values"), format!("{ident}.length")));
        }
        let values = match expression {
            ast::Expr::StrLit { text, .. } => text
                .chars()
                .map(|character| format!("((sx_value){})", character as u32))
                .collect::<Vec<_>>(),
            _ => self.c_string_elems(expression)?,
        };
        let length = values.len();
        let storage = if values.is_empty() {
            "((sx_value[]){0})".to_string()
        } else {
            format!("((sx_value[]){{{}}})", values.join(", "))
        };
        Some((storage, length.to_string()))
    }

    /// A formatted string argument as its per-character reads. Empty
    /// testbench-local strings deliberately produce an empty vector; they
    /// otherwise have no element local through which `c_string_elems` could
    /// recognize them.
    fn formatted_string_elems(&self, e: &ast::Expr) -> Option<Vec<String>> {
        let path = expr_path(e)?;
        let ty = self.local_types.borrow().get(&path).cloned();
        let elems = self.c_string_elems(e);
        match ty.as_deref() {
            Some("string") => Some(elems.unwrap_or_default()),
            Some("Char") if elems.is_some() => elems,
            _ => None,
        }
    }

    /// Whole-string `==` / `!=` as a C boolean, when one operand is a string
    /// literal (a string is a `Char` array). `None` if neither side is a string
    /// literal (fall through to scalar handling).
    fn c_string_cmp(
        &self,
        op: &ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Result<Option<String>, String> {
        let left_dynamic = self.dynamic_string_path(lhs);
        let right_dynamic = self.dynamic_string_path(rhs);
        if left_dynamic.is_some() || right_dynamic.is_some() {
            let (dynamic, other) = match (left_dynamic, right_dynamic) {
                (Some(left), _) => (left, rhs),
                (None, Some(right)) => (right, lhs),
                (None, None) => unreachable!(),
            };
            let Some((values, length)) = self.c_string_value_slice(other) else {
                return Ok(None);
            };
            let equal = format!(
                "sx_dyn_equal_values(&{}, {values}, {length})",
                c_local_ident(&dynamic)
            );
            return Ok(Some(if matches!(op, ast::BinOp::Eq) {
                equal
            } else {
                format!("(!({equal}))")
            }));
        }
        let lit = |e: &ast::Expr| match e {
            ast::Expr::StrLit { text, .. } => Some(text.chars().collect::<Vec<char>>()),
            _ => None,
        };
        let eq = matches!(op, ast::BinOp::Eq);
        let left_literal = lit(lhs);
        let right_literal = lit(rhs);
        if let (Some(left), Some(right)) = (&left_literal, &right_literal) {
            let equal = left == right;
            return Ok(Some(if equal == eq { "1".into() } else { "0".into() }));
        }
        if left_literal.is_none() && right_literal.is_none() {
            let (Some(left), Some(right)) = (self.c_string_elems(lhs), self.c_string_elems(rhs))
            else {
                return Ok(None);
            };
            if left.len() != right.len() {
                return Ok(Some(if eq { "0".into() } else { "1".into() }));
            }
            let terms = left
                .iter()
                .zip(&right)
                .map(|(a, b)| format!("({a} == {b})"))
                .collect::<Vec<_>>();
            let all = if terms.is_empty() {
                "1".into()
            } else {
                terms.join(" && ")
            };
            return Ok(Some(if eq {
                format!("({all})")
            } else {
                format!("(!({all}))")
            }));
        }
        let (elems, chars) = match (left_literal, right_literal) {
            (Some(_), Some(_)) => unreachable!(),
            (None, Some(c)) => (self.c_string_elems(lhs), c),
            (Some(c), None) => (self.c_string_elems(rhs), c),
            (None, None) => unreachable!(),
        };
        let Some(elems) = elems else { return Ok(None) };
        // A length mismatch is unequal — a constant either way.
        if elems.len() != chars.len() {
            return Ok(Some(if eq { "0".into() } else { "1".into() }));
        }
        let terms: Vec<String> = elems
            .iter()
            .zip(&chars)
            .map(|(e, &c)| format!("({e} == {}ULL)", c as u32))
            .collect();
        let all = if terms.is_empty() {
            "1".to_string()
        } else {
            terms.join(" && ")
        };
        Ok(Some(if eq {
            format!("({all})")
        } else {
            format!("(!({all}))")
        }))
    }

    /// `await <duration> | <edge> | <condition>` in the native harness, on the
    /// generated event wheel: a duration runs the clocks up to `now + dur`; an
    /// edge or condition steps clock edges until it fires (bounded, mirroring
    /// the runner's scheduler).
    /// Emit the edge-wait loop for `await <clk>::<kind>` / `await clk.kind()`.
    fn emit_await_edge(
        &self,
        base: &ast::Expr,
        kind: &str,
        ind: &str,
        b: &mut String,
    ) -> Result<(), String> {
        let id = expr_path(base)
            .and_then(|p| self.map.get(&p))
            .ok_or("await: unknown edge signal")?
            .0;
        let hit = match kind {
            "rising" => "!_p && _c",
            "falling" => "_p && !_c",
            _ => "_p != _c",
        };
        b.push_str(&format!(
            "{ind}{{ uint64_t _p = sx_read({id}); \
             for (signed _g = 0; _g < 1000000; _g++) {{ \
             if (!sx_step_clock(&_now, _next, _cid, _half, _nclk)) break; \
             uint64_t _c = sx_read({id}); if ({hit}) break; _p = _c; }} }}\n"
        ));
        Ok(())
    }

    fn emit_await(&self, args: &[ast::Expr], b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        match args.first() {
            Some(ast::Expr::SuffixLit { .. }) | Some(ast::Expr::Field { .. }) => {
                let dur = duration_fs(args)?;
                b.push_str(&format!(
                    "{ind}{{ uint64_t _tgt = UINT64_MAX - _now < {dur}ULL \
                     ? UINT64_MAX : _now + {dur}ULL; \
                     while (sx_next_edge(_next, _nclk) != UINT64_MAX && \
                     sx_next_edge(_next, _nclk) <= _tgt) \
                     sx_step_clock(&_now, _next, _cid, _half, _nclk); \
                     _now = _tgt; sx_settle(); }}\n"
                ));
            }
            Some(ast::Expr::SysAttr { base, attr, .. }) => {
                self.emit_await_edge(base, &attr.text, &ind, b)?;
            }
            // `await clk.rising()` — a `ClockLike` edge method waits on the same
            // edge machinery as the `::rising` sysattr.
            Some(ast::Expr::Call { callee, .. })
                if matches!(callee.as_ref(), ast::Expr::Field { field, .. }
                    if matches!(field.text.as_str(), "rising" | "falling" | "edge")) =>
            {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    self.emit_await_edge(base, &field.text, &ind, b)?;
                }
            }
            Some(cond) => {
                let c = self.expr(cond)?;
                b.push_str(&format!(
                    "{ind}for (signed _g = 0; _g < 1000000 && !({c}); _g++) {{ \
                     if (!sx_step_clock(&_now, _next, _cid, _half, _nclk)) break; }}\n"
                ));
            }
            None => {}
        }
        Ok(())
    }

    /// The C value for writing `e` to signal `id`: a real-typed target takes
    /// a float literal's f64 bit pattern (matching the runner's eval_for).
    fn value_for(&self, id: SignalId, e: &ast::Expr) -> Result<String, String> {
        let sig = &self.design.signals[id.0 as usize];
        if sig.real {
            if let ast::Expr::Int { text, .. } = e {
                if let Ok(f) = text.replace('_', "").parse::<f64>() {
                    return Ok(format!("{}ULL", f.to_bits()));
                }
            }
        }
        // A char-typed target reads a character literal as its code point.
        if sig.char {
            if let ast::Expr::CharLit { ch, .. } = e {
                return Ok(format!("{}ULL", *ch as u32));
            }
        }
        // A char literal written to an enum signal takes its position in that
        // enum (data-driven), matching the IR's `coerce_to_target`.
        if let Some(en) = &sig.enum_type {
            if let Some(d) = self.enum_char_lit(en, e) {
                return Ok(format!("{d}ULL"));
            }
        }
        self.expr(e)
    }

    /// Translate a value using an unconnected local's declared symbol type.
    /// A character literal is a Unicode code point for `Char`, an ordinal for
    /// an enum, and only otherwise a context-free logic literal.
    fn value_for_local(&self, name: &str, e: &ast::Expr) -> Result<String, String> {
        let ty = self.local_types.borrow().get(name).cloned();
        match (ty.as_deref(), e) {
            (Some("real"), ast::Expr::Int { text, .. }) => text
                .replace('_', "")
                .parse::<f64>()
                .map(|value| format!("{}ULL", value.to_bits()))
                .map_err(|_| format!("invalid real literal `{text}`")),
            (Some("Char"), ast::Expr::CharLit { ch, .. }) => Ok(format!("{}ULL", *ch as u32)),
            (Some(en), _) => match self.enum_char_lit(en, e) {
                Some(discriminant) => Ok(format!("{discriminant}ULL")),
                None => self.expr(e),
            },
            (None, _) => self.expr(e),
        }
    }

    /// A `print!` real argument as a u64-bit-pattern C expression.
    fn value_for_print(&self, a: &ast::Expr) -> Result<String, String> {
        if let ast::Expr::Int { text, .. } = a {
            if let Ok(f) = text.replace('_', "").parse::<f64>() {
                return Ok(format!("{}ULL", f.to_bits()));
            }
        }
        self.expr(a)
    }

    /// The declared type head of a method-call receiver (`p` in `p.sum()`): a
    /// struct/enum local, or a numeric family (`n.cmp(m)`).
    fn receiver_type(&self, recv: &ast::Expr) -> Option<String> {
        let p = expr_path(recv)?;
        if let Some(t) = self.local_types.borrow().get(&p) {
            return Some(t.clone());
        }
        self.local_families.borrow().get(&p).cloned()
    }

    /// A method call `recv.method(args)` as a C expression: substitute `self`
    /// with the receiver and each parameter with its argument into the body,
    /// then flatten it (like a module fn). `self.a` becomes `<recv>.a`, which
    /// reads the receiver's struct-local field.
    fn c_method_call(
        &self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
    ) -> Result<String, String> {
        let ty = self.receiver_type(recv).ok_or_else(|| {
            // A method body reads `self.field` by the receiver's mangled
            // local name, so the receiver has to *be* a name here. The
            // hardware path substitutes values and accepts an expression,
            // so say which shape is missing and what to do about it.
            if expr_path(recv).is_none() {
                format!(
                    "a method on an expression receiver is not lowered in a \
                         testbench yet: bind it first (`let r: T = ...; r.{method}()`)"
                )
            } else {
                format!("cannot resolve the receiver type of `.{method}()`")
            }
        })?;
        let (stmts, real) = self.method_body(&ty, method, recv, args)?;
        self.c_fn_stmts(&stmts, real)
    }

    /// A free function's body with its parameters substituted, for a call in
    /// statement position (`store(r, 7)`). The value path (`c_fn_call`) folds
    /// constants and flattens returns into an expression, which a procedure
    /// has none of; this is the same substitution `method_body` does.
    fn free_fn_body(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> Option<Result<Vec<ast::Stmt>, String>> {
        let f = self.fns.get(callee)?;
        let body = f.body.as_ref()?;
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                map.insert(n.text.clone(), a.clone());
            }
        }
        Some(Ok(body
            .stmts
            .iter()
            .map(|s| siox::ir::subst_stmt_paths(s, &map))
            .collect()))
    }

    /// A method's body with `self` and the parameters substituted, plus
    /// whether it returns a `real`. Shared by the value path (`p.sum()`) and
    /// the statement path (`r.set(7)`), so a method means the same thing in
    /// both — the statement path used to mean nothing at all.
    fn method_body(
        &self,
        ty: &str,
        method: &str,
        recv: &ast::Expr,
        args: &[ast::Expr],
    ) -> Result<(Vec<ast::Stmt>, bool), String> {
        let f = self
            .methods
            .get(&(ty.to_string(), method.to_string()))
            .ok_or_else(|| format!("unknown method `{ty}::{method}`"))?;
        let body = f.body.as_ref().ok_or("method has no body")?;
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        map.insert("self".to_string(), recv.clone());
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                map.insert(n.text.clone(), a.clone());
            }
        }
        let stmts = body
            .stmts
            .iter()
            .map(|s| siox::ir::subst_stmt_paths(s, &map))
            .collect();
        let real = f.ret.as_ref().is_some_and(|ty| self.type_is(ty, "real"));
        Ok((stmts, real))
    }

    /// A module-fn call as a C expression: bind the arguments, then flatten
    /// the `return`/`if` body into nested conditionals.
    /// The C constant for a zero-argument construction of a named type: the
    /// `impl New` default where one is declared, an enum's first variant, and
    /// zero for a nominal array family. Returns `None` when the callee is not a type,
    /// so an ordinary call falls through.
    fn c_default_construction(&self, callee: &ast::Expr) -> Option<String> {
        // `unsigned[8]()` — the width is irrelevant to the value.
        let name = match callee {
            ast::Expr::Index { base, .. } => expr_path(base)?,
            _ => match callee {
                ast::Expr::Path(p) => self
                    .fns
                    .type_owner_key(p)
                    .or_else(|| (p.segments.len() == 1).then(|| p.segments[0].text.clone()))?,
                _ => return None,
            },
        };
        if let Some(&v) = self.design.new_defaults.get(&name) {
            return Some(format!("{v}ULL"));
        }
        if let Some(syms) = self.design.enum_syms.get(&name) {
            // The declared-first variant: `enum Phase { Idle, Run }` -> Idle.
            let first = syms.keys().copied().min()?;
            return Some(format!("{first}ULL"));
        }
        (self.nominal_field_names(&name).is_some()
            || self.type_name_is(&name, "unsigned")
            || self.type_name_is(&name, "signed"))
        .then(|| "0ULL".to_string())
    }

    fn c_fn_call(&self, callee: &ast::Expr, args: &[ast::Expr]) -> Result<String, String> {
        // `T()` / `unsigned[8]()`: the type's default (spec 3.29). Hardware
        // built these; the testbench had no case for a zero-argument type
        // call, so `Phase()` and `unsigned[8]()` failed as "unsupported call"
        // although `let e: Phase;` — the same default, implicitly — worked.
        if args.is_empty() {
            if let Some(v) = self.c_default_construction(callee) {
                return Ok(v);
            }
        }
        // `Byte(200)` — a newtype constructor is value-transparent, narrowed
        // to the width its base fixes. Hardware treats it as a conversion;
        // here it fell through to "unsupported call `Byte`".
        if args.len() == 1 {
            if let Some(name) = expr_path(callee) {
                if let Some(&width) = self.derived_widths.get(&name) {
                    let v = self.expr(&args[0])?;
                    return Ok(if width > 0 && width < 64 {
                        mask_c(&v, width)
                    } else {
                        v
                    });
                }
            }
        }
        // A bare name, or `Type::name` for a static associated fn.
        let Some(key) = siox::ir::call_fn_key(callee) else {
            return Err("unsupported call in testbench expression".into());
        };
        let name = key.as_str();
        let Some(f) = self.fns.get(callee) else {
            // Runtime-provided functions (std::rand).
            return match name {
                "exists" => {
                    let path = match args.first() {
                        Some(ast::Expr::StrLit { text, .. }) => text.clone(),
                        _ => return Err("exists() needs a literal path".into()),
                    };
                    // Resolve relative to the design's source directory, then
                    // escape for the C string literal.
                    let full = self
                        .design
                        .base_dir
                        .join(&path)
                        .to_string_lossy()
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"");
                    Ok(format!(
                        "({{ FILE *_f = fopen(\"{full}\", \"rb\"); signed _e = _f != 0; if (_f) fclose(_f); _e; }})"
                    ))
                }
                "read" => Err(
                    "runtime `read<T>()` returns owned aggregate storage and is only valid \
                     as a typed #[test] local initializer (`let x: T = read<T>(..);`)"
                        .to_string(),
                ),
                "rand" => Ok("sx_rand()".to_string()),
                "uniform" => Ok("sx_uniform()".to_string()),
                "randint" => {
                    let lo = self.expr(args.first().ok_or("randint needs bounds")?)?;
                    let hi = self.expr(args.get(1).ok_or("randint needs bounds")?)?;
                    Ok(format!("sx_randint(({lo}), ({hi}))"))
                }
                _ => Err(format!("unsupported call `{name}` in testbench expression")),
            };
        };
        if f.body.is_none() {
            let mut converted = Vec::new();
            for (param, arg) in f.params.iter().filter(|param| !param.is_self).zip(args) {
                if param.ty.as_ref().is_some_and(|ty| self.type_is(ty, "real")) {
                    converted.push(self.c_real_operand(arg)?);
                } else if param
                    .ty
                    .as_ref()
                    .is_some_and(|ty| self.type_is(ty, "integer"))
                {
                    let rendered = self.expr(arg)?;
                    converted.push(self.c_integer_operand(arg, &rendered));
                } else {
                    converted.push(self.expr(arg)?);
                }
            }
            let call = format!("{}({})", f.name.text, converted.join(", "));
            return Ok(
                if f.ret.as_ref().is_some_and(|ty| self.type_is(ty, "real")) {
                    format!("sx_b64({call})")
                } else {
                    call
                },
            );
        }
        let body = f.body.as_ref().ok_or("fn has no body")?;
        let real_return = f.ret.as_ref().is_some_and(|ty| self.type_is(ty, "real"));
        // Constant arguments fold statically (also the only way a recursive
        // fn like clog2 compiles here). The evaluator is integer-only, so a
        // declared real result must retain the typed native path below.
        if !real_return {
            let consts: Option<Vec<i64>> = args
                .iter()
                .map(|a| siox::ir::eval_const_fns(a, &HashMap::new(), self.fns, 0))
                .collect();
            if let Some(cs) = consts {
                let mut cenv = HashMap::new();
                for (p, v) in f.params.iter().filter(|p| !p.is_self).zip(cs) {
                    if let Some(n) = &p.name {
                        cenv.insert(n.text.clone(), v);
                    }
                }
                if let Some(v) = siox::ir::eval_const_stmts(&body.stmts, &cenv, self.fns, 0) {
                    return Ok(format!("{}ULL", v as u64));
                }
            }
        }
        if self.tmp.get() > 4096 {
            return Err(format!("fn `{name}` recurses without constant arguments"));
        }
        self.tmp.set(self.tmp.get() + 64);
        let mut env = HashMap::new();
        let mut types = HashMap::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                if let Some(head) =
                    p.ty.as_ref()
                        .and_then(|ty| resolved_type_name(ty, self.type_aliases, self.fns))
                {
                    types.insert(n.text.clone(), head.clone());
                    if head == "real" {
                        env.insert(
                            n.text.clone(),
                            format!("sx_b64({})", self.c_real_operand(a)?),
                        );
                        continue;
                    }
                    // A `signed` vector reaching an `integer` parameter is a
                    // value, not a bit pattern: `compatible` allows the
                    // coercion, so it has to be value-preserving. Passing the
                    // raw bits made `abs(-5)` see 251 and return it unchanged.
                    if head == "integer" {
                        if let Some(w) = self.signed_vector_width(a) {
                            let v = self.expr(a)?;
                            env.insert(n.text.clone(), format!("((sx_value)sx_i64(({v}), {w}))"));
                            continue;
                        }
                    }
                }
                // A struct argument has no single C expression — a struct
                // local is one variable per leaf field — so bind the leaves
                // instead: the body's `a.x` resolves through the same
                // flattened name the environment is keyed by. Without this,
                // `manhattan(p, q)` reported `p` as a name not in scope,
                // though a *method* on the same struct inlined fine.
                // An array parameter is composite for the same reason a struct
                // one is: it has no single C value, so `self.expr` below
                // reported the argument as a name that is not in scope. Its
                // leaves bind under `v[0]`, `v[1]`, which is how the body
                // spells them and how an indexed path resolves here.
                let argument_layout = expr_path(a).and_then(|path| self.persisted_layout(&path));
                let composite = argument_layout.is_some_and(Self::layout_is_composite)
                    || p.ty
                        .as_ref()
                        .and_then(type_head_name)
                        .and_then(|head| self.nominal_field_names(head))
                        .is_some_and(|fields| !fields.is_empty())
                    || p.ty.as_ref().is_some_and(|t| self.array_parts(t).is_some());
                if composite {
                    if let Some(path) = expr_path(a) {
                        let leaves = self.composite_reads(&path);
                        if !leaves.is_empty() {
                            for (suffix, value) in leaves {
                                env.insert(format!("{}{suffix}", n.text), value);
                            }
                            continue;
                        }
                    }
                    // An array *literal* argument has no path to read leaves
                    // from; each element binds directly.
                    if let ast::Expr::Array { elems, .. } = a {
                        for (index, element) in elems.iter().enumerate() {
                            let value = format!("({})", self.expr(element)?);
                            env.insert(format!("{}[{index}]", n.text), value);
                        }
                        continue;
                    }
                    // A struct *literal* argument, likewise: it has no path
                    // either, so no spelling of one reached a struct parameter
                    // here — named, named-with-its-type, or positional all came
                    // back as an unsupported expression, the positional one
                    // complaining about the width of a part because braces
                    // with no field names lex as a concatenation. The
                    // parameter's declared type is what says these are fields.
                    if let Some(named) =
                        p.ty.as_ref()
                            .and_then(type_head_name)
                            .and_then(|head| self.nominal_field_names(head))
                            .and_then(|fields| literal_struct_fields(a, &fields))
                    {
                        for (field, value) in named {
                            let value = format!("({})", self.expr(value)?);
                            env.insert(format!("{}.{field}", n.text), value);
                        }
                        continue;
                    }
                }
                env.insert(n.text.clone(), format!("({})", self.expr(a)?));
                // The argument's width travels with it, as at the operator and
                // method inline sites. A nested inline reads `self'length` off
                // this parameter — signed's Ord tests the sign with
                // `self >> (self'length - 1)` — and without a binding that
                // shifted by 0, so `abs(-5)` came back as 251.
                if let Some(w) = self.arg_width(a) {
                    env.insert(format!("{}::length", n.text), format!("{w}ULL"));
                }
            }
        }
        self.fn_env.borrow_mut().push(env);
        self.fn_type_env.borrow_mut().push(types);
        let out = self.c_fn_stmts(&body.stmts, real_return);
        self.fn_type_env.borrow_mut().pop();
        self.fn_env.borrow_mut().pop();
        self.tmp.set(self.tmp.get() - 64);
        let value = out?;
        // The result is a value of the declared return type, so it wraps to
        // that width — the operator inlines beside this one already do. Left
        // unmasked, `neg(5) -> signed[8]` produced a full-width `0 - 5`, and
        // every consumer then read a 64-bit pattern where 8 bits were
        // declared: the sign test in signed's Ord looked at bit 7 of that.
        if real_return {
            return Ok(value);
        }
        match f.ret.as_ref().and_then(|ty| self.declared_width(ty)) {
            Some(w) if w > 0 && w < 64 => Ok(mask_c(&value, w)),
            _ => Ok(value),
        }
    }

    /// `return e;` / `if c { .. } else { .. }` chains as nested C ternaries.
    fn c_fn_stmts(&self, stmts: &[ast::Stmt], real_return: bool) -> Result<String, String> {
        match stmts.first() {
            Some(ast::Stmt::Return { value: Some(v), .. }) if real_return => {
                Ok(format!("sx_b64({})", self.c_real_operand(v)?))
            }
            Some(ast::Stmt::Return { value: Some(v), .. }) => Ok(format!("({})", self.expr(v)?)),
            Some(ast::Stmt::If(iff)) => {
                let c = self.expr(&iff.cond)?;
                let t = self.c_fn_stmts(&iff.then.stmts, real_return)?;
                let e = match iff.else_.as_deref() {
                    Some(ast::ElseBranch::Block(b)) => self.c_fn_stmts(&b.stmts, real_return)?,
                    _ => self.c_fn_stmts(&stmts[1..], real_return)?,
                };
                Ok(format!("(({c}) ? {t} : {e})"))
            }
            // A `match` whose arms return is the same shape as an `if`
            // chain. Folded in reverse so an earlier arm wins, matching how
            // lowering inlines the same body.
            Some(ast::Stmt::Match(m)) => {
                let scrut = self.expr(&m.scrutinee)?;
                // What the body yields when no arm returns.
                let after = self.c_fn_stmts(&stmts[1..], real_return);
                let mut acc: Option<String> = after.as_ref().ok().cloned();
                for arm in m.arms.iter().rev() {
                    let value = match self.c_fn_stmts(&arm.body.stmts, real_return) {
                        Ok(value) => value,
                        // An arm that returns nothing falls through to the
                        // statements after the match.
                        Err(_) => match acc.clone() {
                            Some(_) => after.clone()?,
                            None => return Err("a match arm yields no value".into()),
                        },
                    };
                    acc = Some(
                        match (self.pattern_cond(&arm.pattern, &m.scrutinee, &scrut)?, acc) {
                            // A wildcard covers everything after it.
                            (None, _) => value,
                            // Nothing follows: an exhaustive match ends here.
                            (Some(_), None) => value,
                            (Some(cond), Some(otherwise)) => {
                                format!("(({cond}) ? {value} : {otherwise})")
                            }
                        },
                    );
                }
                acc.ok_or_else(|| "a match with no arms yields no value".to_string())
            }
            // `let t: T = expr;` names a value for the statements that
            // follow. The body compiles to one C expression, so the name is
            // substituted rather than declared — matching how lowering
            // inlines the same body.
            Some(ast::Stmt::Let(l)) => {
                let value = l
                    .value
                    .as_ref()
                    .ok_or("a `let` inside a fn body needs a value")?;
                let rendered = format!("({})", self.expr(value)?);
                let width =
                    l.ty.as_ref()
                        .and_then(|t| self.declared_family(t).map(|(_, w)| w))
                        .or_else(|| l.ty.as_ref().and_then(|t| self.declared_width(t)));
                let bound = match width {
                    Some(w) if w > 0 && w < 64 => mask_c(&rendered, w),
                    _ => rendered,
                };
                let mut scoped = self.fn_env.borrow().last().cloned().unwrap_or_default();
                if let Some(w) = width {
                    scoped.insert(format!("{}::length", l.name.text), format!("{w}ULL"));
                }
                scoped.insert(l.name.text.clone(), bound);
                self.fn_env.borrow_mut().push(scoped);
                let out = self.c_fn_stmts(&stmts[1..], real_return);
                self.fn_env.borrow_mut().pop();
                out
            }
            _ => Err("fn bodies compile as return/if chains only".into()),
        }
    }

    /// Translate a testbench expression to a C expression string.
    /// The C condition for a match pattern over `scrut` (a C expression), or
    /// `None` for a wildcard/always-match (spec 3.22). Or-patterns `||` their
    /// alternatives' conditions.
    fn pattern_cond(
        &self,
        pattern: &ast::Pattern,
        scrutinee: &ast::Expr,
        scrut: &str,
    ) -> Result<Option<String>, String> {
        Ok(match pattern {
            ast::Pattern::Wildcard => None,
            ast::Pattern::Path(p) if p.segments.len() >= 2 => {
                let d = enum_variant_value(p, self.enums, self.fns).ok_or_else(|| {
                    format!(
                        "unknown variant `{}`",
                        p.segments.last().expect("variant path").text
                    )
                })?;
                Some(format!("(({scrut}) == {d}ULL)"))
            }
            ast::Pattern::BitPattern { text, .. } => {
                let (mask, value) = siox::syntax::bit_pattern_mask(text)
                    .ok_or_else(|| format!("bad bit pattern `{text}`"))?;
                Some(format!(
                    "((({scrut}) & {}) == {})",
                    c_word_literal(&mask),
                    c_word_literal(&value)
                ))
            }
            ast::Pattern::Or { alts, .. } => {
                let mut parts = Vec::new();
                for a in alts {
                    match self.pattern_cond(a, scrutinee, scrut)? {
                        None => return Ok(None),
                        Some(c) => parts.push(c),
                    }
                }
                Some(format!("({})", parts.join(" || ")))
            }
            ast::Pattern::Range { lo, hi, .. } => {
                let (low, high) = if lo <= hi { (*lo, *hi) } else { (*hi, *lo) };
                let comparison = |operator: &str, value: i64| {
                    let (lhs, rhs) = if self.is_real_operand(scrutinee) {
                        (
                            format!("sx_f64((uint64_t)({scrut}))"),
                            format!("((double){:e})", value as f64),
                        )
                    } else if let Some(width) = self.signed_vector_width(scrutinee) {
                        (
                            format!("sx_i64(({scrut}), {width})"),
                            format!("((int64_t){}ULL)", value as u64),
                        )
                    } else if self.is_integer_operand(scrutinee) {
                        (
                            self.c_integer_operand(scrutinee, scrut),
                            format!("((int64_t){}ULL)", value as u64),
                        )
                    } else {
                        (
                            format!("({scrut})"),
                            format!("((sx_value){}ULL)", value as u64),
                        )
                    };
                    format!("(({lhs}) {operator} ({rhs}))")
                };
                if low == high {
                    Some(comparison("==", low))
                } else {
                    Some(format!(
                        "({} && {})",
                        comparison(">=", low),
                        comparison("<=", high)
                    ))
                }
            }
            // A character literal names a variant of a char-valued enum, the
            // same discriminant a `== '0'` comparison uses here.
            ast::Pattern::CharLit { ch, .. } => Some(format!(
                "(({scrut}) == {}ULL)",
                logic_lit_value(*ch, self.enums)
            )),
            _ => Some("0".to_string()),
        })
    }

    fn expr(&self, e: &ast::Expr) -> Result<String, String> {
        if let Some(read) = self.dynamic_string_index(e) {
            return read;
        }
        // A field/element reached through a runtime array index has no static
        // path, but every concrete leaf does. Handle it once before the shape
        // match; packed bits return None and continue to their dedicated arm.
        if expr_path(e).is_none() && matches!(e, ast::Expr::Field { .. } | ast::Expr::Index { .. })
        {
            if let Some(read) = self.dynamic_array_read(e, "") {
                return read;
            }
        }
        Ok(match e {
            ast::Expr::IfExpr {
                cond, then, els, ..
            } => {
                if self.is_real_operand(e) {
                    format!(
                        "sx_b64(({}) ? ({}) : ({}))",
                        self.expr(cond)?,
                        self.c_real_operand(then)?,
                        self.c_real_operand(els)?
                    )
                } else {
                    format!(
                        "(({}) ? ({}) : ({}))",
                        self.expr(cond)?,
                        self.expr(then)?,
                        self.expr(els)?
                    )
                }
            }
            // A match-expression: a first-match C ternary chain over the arms.
            ast::Expr::Match {
                scrutinee, arms, ..
            } => {
                let scrut = self.expr(scrutinee)?;
                let real = self.is_real_operand(e);
                // Build from the last arm backward.
                let mut out = String::from("0");
                for arm in arms.iter().rev() {
                    let val = match arm.value_expr() {
                        Some(v) if real => format!("sx_b64({})", self.c_real_operand(v)?),
                        Some(v) => self.expr(v)?,
                        None => "0".to_string(),
                    };
                    match self.pattern_cond(&arm.pattern, scrutinee, &scrut)? {
                        None => out = format!("({val})"), // wildcard: the default
                        Some(cond) => out = format!("({cond} ? ({val}) : {out})"),
                    }
                }
                out
            }
            ast::Expr::Int { text, .. } => c_word_literal(&parse_word_literal(text)),
            ast::Expr::SuffixLit { text, suffix, .. } => {
                let scale = ast::suffix_scale(&suffix.text).unwrap_or(1);
                if suffix.text == "Hz" || suffix.text.ends_with("Hz") {
                    format!("sx_b64((double)({text}) * {scale}.0)")
                } else {
                    format!("((uint64_t)({text}) * {scale}ULL)")
                }
            }
            ast::Expr::BitStrLit { base, digits, .. } => {
                let radix = siox::syntax::radix_of(*base);
                c_word_literal(&parse_digits_words(digits, radix))
            }
            // A plain string on a packed vector is a per-character logic
            // vector, MSB first — the same decode the hardware side does, one
            // value bit per character discriminant. Without this a testbench
            // could not spell a value the design itself accepts.
            ast::Expr::StrLit { text, .. } => {
                let chars: Vec<char> = text.chars().collect();
                let mut words = vec![0u64; chars.len().div_ceil(64).max(1)];
                for (i, ch) in chars.iter().enumerate() {
                    let pos = chars.len() - 1 - i; // first character is the top bit
                    let disc = logic_lit_value(*ch, self.enums);
                    let bit = self
                        .design
                        .logic_encodings
                        .get(siox::ir::DEFAULT_LOGIC_TYPE)
                        .and_then(|encoding| encoding.value_bit(disc))
                        .unwrap_or(0);
                    words[pos / 64] |= bit << (pos % 64);
                }
                c_word_literal(&words)
            }
            ast::Expr::CharLit { ch, .. } => logic_lit_value(*ch, self.enums).to_string(),
            // Conversions mask to the target width (testbench side).
            // A method call `recv.method(args)` (possibly nullary) inlines the
            // impl body as a C expression, before the conversion logic below.
            ast::Expr::Call { callee, args, .. }
                if matches!(callee.as_ref(), ast::Expr::Field { .. }) =>
            {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    return self.c_method_call(base, &field.text, args);
                }
                unreachable!()
            }
            // A free-function call — a user fn or a runtime one (`exists`,
            // `rand`) — rather than a type conversion.
            ast::Expr::Call { callee, args, .. }
                if self.fns.get(callee).is_some()
                    || matches!(callee.as_ref(), ast::Expr::Path(p)
                        if p.segments.len() == 1
                            && matches!(p.segments[0].text.as_str(),
                                "exists" | "rand" | "randint" | "uniform" | "read")) =>
            {
                return self.c_fn_call(callee, args);
            }
            ast::Expr::Call { callee, args, .. } => {
                // A nullary call is a function, not a conversion — hand it to
                // the call path rather than failing the build.
                let Some(arg) = args.first() else {
                    return self.c_fn_call(callee, args);
                };
                let v = self.expr(arg)?;
                let w = match callee.as_ref() {
                    ast::Expr::Index { base, index, .. }
                        if (match base.as_ref() {
                            ast::Expr::Path(path) => {
                                self.fns.struct_path_key(path).or_else(|| expr_path(base))
                            }
                            _ => expr_path(base),
                        })
                        .is_some_and(|head| self.families.contains(&head)) =>
                    {
                        parse_u64(match index.as_ref() {
                            ast::Expr::Int { text, .. } => text,
                            _ => return Err("conversion width must be a constant here".into()),
                        })
                    }
                    ast::Expr::Path(p)
                        if p.segments.len() == 1 && p.segments[0].text == "resize" =>
                    {
                        match args.get(1) {
                            Some(ast::Expr::Int { text, .. }) => parse_u64(text),
                            // `resize(x, self::length)` inside an inlined
                            // operator impl: the bound width is a C literal
                            // like `8ULL` — recover the number.
                            Some(other) => {
                                let c = self.expr(other)?;
                                c.trim_end_matches("ULL").parse().map_err(|_| {
                                    "resize width must be a constant here".to_string()
                                })?
                            }
                            None => return Err("resize width must be a constant here".into()),
                        }
                    }
                    ast::Expr::Path(p)
                        if p.segments.len() == 1
                            && matches!(p.segments[0].text.as_str(), "integer" | "Char") =>
                    {
                        // Crossing out of `real` is a value conversion, not a
                        // pass-through: the operand holds f64 bits, so handing
                        // them on unchanged returned the bit pattern —
                        // `integer(3.5)` read 4615063718147915776. Truncate
                        // toward zero, as the hardware side does.
                        if p.segments[0].text == "integer" && self.is_real_operand(arg) {
                            // `c_real_operand` renders the operand as a C
                            // double; `self.expr` would render a literal like
                            // `2.9` as the integer 2, whose bits are a
                            // denormal.
                            let d = self.c_real_operand(arg)?;
                            return Ok(format!("((sx_value)(int64_t)({d}))"));
                        }
                        return Ok(format!("({v})"));
                    }
                    // An enum-derivation conversion (`Logic(u)`, `ULogic(x)`):
                    // representation-identity along the chain — pass through.
                    //
                    // The chain has to be checked, not assumed. This guard used
                    // to accept any `EnumName(x)` whatever the source was, so
                    // the testbench performed conversions the hardware side
                    // rejects: `Bit(l)` on a `Logic` handed `'X'` straight into
                    // a `Bit`, which has nowhere to put it, and printed `?`.
                    ast::Expr::Path(p)
                        if self
                            .fns
                            .enum_path_key(p)
                            .is_some_and(|key| self.enums.contains_key(&key)) =>
                    {
                        let target = self.fns.enum_path_key(p).expect("guarded enum path");
                        if let Some(src) = self.arg_type_name(arg) {
                            if !self.enum_conversion_exists(&src, &target) {
                                return Err(format!(
                                    "no conversion from `{src}` to `{target}`: `T(x)` needs \
                                     an `impl From<S> for T`, or a derivation chain between \
                                     the two types"
                                ));
                            }
                        }
                        return Ok(format!("({v})"));
                    }
                    _ => return self.c_fn_call(callee, args),
                };
                if w > 64 {
                    format!("((sx_value)({v}))")
                } else if w == 0 || w == 64 {
                    format!("({v})")
                } else {
                    format!("(({v}) & {}ULL)", (1u64 << w) - 1)
                }
            }
            // `::length`: an impl body's bound length (`self::length`), an
            // array's element count, or a name's bit width (they coincide for a
            // flat vector) — VHDL `'length`.
            ast::Expr::SysAttr { base, attr, .. } if attr.text == "length" => {
                let path = expr_path(base).ok_or("`'length` needs a named base")?;
                if self.dynamic_strings.borrow().contains(&path) {
                    return Ok(format!("((sx_value){}.length)", c_local_ident(&path)));
                }
                if let Some(v) = self
                    .fn_env
                    .borrow()
                    .last()
                    .and_then(|m| m.get(&format!("{path}::length")))
                {
                    return Ok(v.clone());
                }
                if let Some(n) = self.array_len(&path) {
                    return Ok(format!("{n}ULL"));
                }
                // Neither a declared width nor an element count. The message
                // named neither the value nor the attribute, and spelled the
                // attribute with the internal `::` key rather than the `'`
                // the language uses.
                format!(
                    "{}ULL",
                    self.name_width(&path).ok_or_else(|| format!(
                        "`{path}'length` is not known here: `{path}` has no declared \
                         width and no element count in the testbench"
                    ))?
                )
            }
            // Range bounds (VHDL `'left`/`'right`/`'high`/`'low`/`'ascending`).
            // A name reads as ascending `0..width-1`; hardware bounds are
            // const-folded in the IR, so this covers only bounds in emitted
            // testbench code.
            ast::Expr::SysAttr { base, attr, .. }
                if matches!(
                    attr.text.as_str(),
                    "left" | "right" | "high" | "low" | "ascending"
                ) =>
            {
                let path = expr_path(base);
                if let Some(dynamic) = path
                    .as_ref()
                    .filter(|path| self.dynamic_strings.borrow().contains(*path))
                {
                    let ident = c_local_ident(dynamic);
                    return Ok(match attr.text.as_str() {
                        "left" | "low" => "((sx_value)0)".to_string(),
                        "right" | "high" => {
                            format!("((sx_value)(int64_t)((int64_t){ident}.length - 1))")
                        }
                        "ascending" => "((sx_value)1)".to_string(),
                        _ => unreachable!(),
                    });
                }
                let bounds = path
                    .as_ref()
                    .and_then(|path| self.local_ranges.borrow().get(path).copied());
                let w = path
                    .as_ref()
                    .and_then(|path| self.name_width(path))
                    .unwrap_or(0) as i64;
                let (left, right) = bounds.unwrap_or((0, (w - 1).max(0)));
                let v = match attr.text.as_str() {
                    "left" => left,
                    "right" => right,
                    "low" => left.min(right),
                    "high" => left.max(right),
                    "ascending" => i64::from(left <= right),
                    _ => unreachable!(),
                };
                if attr.text == "ascending" {
                    format!("{v}ULL")
                } else {
                    // Bounds are language `integer` values. Cast through a
                    // signed ABI word so a negative bound sign-extends to the
                    // harness-wide value instead of becoming only 64 one-bits.
                    format!("((sx_value)(int64_t){v}LL)")
                }
            }
            ast::Expr::Path(p)
                if p.segments.len() == 1
                    && self
                        .fn_env
                        .borrow()
                        .last()
                        .is_some_and(|m| m.contains_key(&p.segments[0].text)) =>
            {
                self.fn_env.borrow().last().unwrap()[&p.segments[0].text].clone()
            }
            ast::Expr::Path(p)
                if p.segments.len() == 1 && self.locals.borrow().contains(&p.segments[0].text) =>
            {
                c_local_ident(&p.segments[0].text)
            }
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                if let Some(&id) = self.map.get(&p.segments[0].text) {
                    format!("sx_read({})", id.0)
                } else if let Some(value) = self
                    .fns
                    .constant_path_key(p)
                    .and_then(|key| self.const_exprs.get(&key))
                {
                    value.clone()
                } else {
                    // Every binder has been consulted by now — a function
                    // parameter, a testbench local, a connected signal, a
                    // constant — so the name simply does not exist. Saying
                    // "cannot translate yet" here blamed the compiler for
                    // the reader's typo.
                    return Err(unknown_value(&p.segments[0].text));
                }
            }
            ast::Expr::Path(p) if p.segments.len() >= 2 => {
                if let Some(value) = self
                    .fns
                    .constant_path_key(p)
                    .and_then(|key| self.const_exprs.get(&key))
                {
                    return Ok(value.clone());
                }
                // Enum::Variant -> discriminant.
                let d = enum_variant_value(p, self.enums, self.fns).ok_or_else(|| {
                    format!(
                        "`{}` is not a resolved enum variant",
                        p.segments.last().expect("variant path").text
                    )
                })?;
                format!("{d}ULL")
            }
            // An element of a constant lookup table, at a constant index or a
            // runtime one. Constants are stored one scalar per name, so both
            // forms missed the table entirely and were reported as something
            // the emitter cannot translate.
            ast::Expr::Index { base, index, .. }
                if self
                    .fns
                    .constant_expr_key(base)
                    .is_some_and(|key| self.const_array_exprs.contains_key(&key)) =>
            {
                let key = self.fns.constant_expr_key(base).unwrap();
                let values = &self.const_array_exprs[&key];
                let idx = self.expr(index)?;
                // C folds the chain away when the index is a literal.
                let mut out = String::from("0ULL");
                for (k, value) in values.iter().enumerate().rev() {
                    out = format!("((({idx}) == {k}ULL) ? ({value}) : {out})");
                }
                out
            }
            // A constant bit slice of a packed value, before the generic
            // field/index path: `w[7..4]` has no `expr_path` to look up.
            ast::Expr::Index { base, index, .. } if self.c_bit_slice(base, index).is_some() => {
                self.c_bit_slice(base, index).unwrap()?
            }
            // A runtime bit of a packed value. Arrays were handled above;
            // this selects one storage bit (and its Logic companion nibble,
            // when present) by the vector's declared labels.
            ast::Expr::Index { base, index, .. } if self.c_dynamic_bit(base, index).is_some() => {
                self.c_dynamic_bit(base, index).unwrap()?
            }
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                // A qualified struct constant (`a::record::CURRENT.left`)
                // deliberately has no local/signal path: its base is a
                // multi-segment declaration path.  Consult the resolved
                // constant table before asking for a storage path.
                if let Some(value) = self
                    .fns
                    .constant_expr_key(e)
                    .and_then(|key| self.const_exprs.get(&key))
                {
                    return Ok(value.clone());
                }
                let path = expr_path(e).ok_or_else(|| {
                    // Naming the base is the difference between "the tool has
                    // a gap" and a reader hunting a nameless message.
                    match expr_path_base(e) {
                        Some(b) => unsup(&b),
                        None => "unsupported field/index expression".to_string(),
                    }
                })?;
                // Inside an inlined function body, a struct parameter is
                // bound one leaf at a time (`a.x`), so the environment answers
                // before signals and locals do.
                if let Some(bound) = self
                    .fn_env
                    .borrow()
                    .last()
                    .and_then(|environment| environment.get(&path))
                {
                    return Ok(bound.clone());
                }
                // A struct-field / array-element of a testbench local reads its
                // mangled C local; otherwise it's a connected signal.
                if self.locals.borrow().contains(&path) {
                    c_local_ident(&path)
                } else {
                    let id = self.map.get(&path).ok_or_else(|| unsup(&path))?;
                    self.check_scalar(*id)?;
                    format!("sx_read({})", id.0)
                }
            }
            ast::Expr::Unary { op, rhs, .. } => {
                if *op == ast::UnOp::Not {
                    if let Some(value) = self.c_dispatch_not(rhs)? {
                        return Ok(value);
                    }
                }
                if *op == ast::UnOp::Neg && self.is_real_operand(rhs) {
                    return Ok(format!("sx_b64(-({}))", self.c_real_operand(rhs)?));
                }
                let r = self.expr(rhs)?;
                match op {
                    ast::UnOp::Not => format!("(!({r}))"),
                    ast::UnOp::Neg => format!("(-({r}))"),
                }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // Whole-string equality (`o == "hello"`): a string is a `Char`
                // array, so compare element by element (matches the runner).
                if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
                    if let Some(v) = self.c_string_cmp(op, lhs, rhs)? {
                        return Ok(v);
                    }
                }
                // A `Char` operand reads a code point, so a `'x'` literal
                // counterpart is its Unicode value (not a logic code).
                if self.is_char_operand(lhs) || self.is_char_operand(rhs) {
                    let a = self.c_char_operand(lhs)?;
                    let b = self.c_char_operand(rhs)?;
                    match op {
                        ast::BinOp::Div => return Ok(format!("sx_udiv(({a}), ({b}))")),
                        ast::BinOp::Shl => return Ok(format!("sx_shl(({a}), ({b}))")),
                        ast::BinOp::Shr => return Ok(format!("sx_shr(({a}), ({b}))")),
                        _ => {}
                    }
                    return Ok(format!("({a} {} {b})", c_binop(op)?));
                }
                // A char literal compared for (in)equality against an enum-typed
                // operand reads by its position in that enum (VHDL `T'pos`),
                // data-driven — so `state == 'g'` matches the stored
                // discriminant, not `'g'`'s logic value. Restricted to `==`/`!=`
                // with exactly one char-literal side so custom/arithmetic
                // operators on enums still dispatch normally.
                if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne)
                    && (self.is_char_lit(lhs) ^ self.is_char_lit(rhs))
                {
                    if let Some(en) = self
                        .enum_operand_type(lhs)
                        .or_else(|| self.enum_operand_type(rhs))
                    {
                        let a = self.enum_operand_c(&en, lhs)?;
                        let b = self.enum_operand_c(&en, rhs)?;
                        return Ok(format!("({a} {} {b})", c_binop(op)?));
                    }
                }
                // A `real` operand switches to double semantics: reals read
                // their bits as `double`, integer literals coerce (`z.re == 10`
                // compares 10.0). A comparison yields an signed; arithmetic yields
                // the double's bit pattern (matching the runner).
                if self.is_real_operand(lhs) || self.is_real_operand(rhs) {
                    let a = self.c_real_operand(lhs)?;
                    let b = self.c_real_operand(rhs)?;
                    let e = format!("({a} {} {b})", c_binop(op)?);
                    let is_cmp = matches!(
                        op,
                        ast::BinOp::Eq
                            | ast::BinOp::Ne
                            | ast::BinOp::Lt
                            | ast::BinOp::Le
                            | ast::BinOp::Gt
                            | ast::BinOp::Ge
                    );
                    return Ok(if is_cmp {
                        e
                    } else {
                        format!("sx_b64((double){e})")
                    });
                }
                // A typed operand inlines its family's operator impl (signed's
                // signed Div/Ord), matching the runner.
                if let Some(v) = self.c_dispatch_binop(op, lhs, rhs)? {
                    return Ok(v);
                }
                let (a, o, c) = (self.expr(lhs)?, c_binop(op)?, self.expr(rhs)?);
                // A pure-literal expression is a kernel integer too, so
                // `(0 - 7) / 2` takes the signed division rather than the
                // unsigned one — it read 0 - 7 as a huge unsigned and
                // returned 9223372036854775804. *Both* sides must qualify:
                // `or` here would let the narrow `1` in
                // `18446744073709551616 + 1` drag the wide literal onto the
                // 64-bit path, which is how it truncated to 1 once already.
                if self.is_integer_operand(lhs)
                    || self.is_integer_operand(rhs)
                    || (Self::is_literal_integer_expr(lhs) && Self::is_literal_integer_expr(rhs))
                {
                    let signed_a = self.c_integer_operand(lhs, &a);
                    let signed_c = self.c_integer_operand(rhs, &c);
                    match op {
                        ast::BinOp::Div => {
                            return Ok(format!("sx_idiv({signed_a}, {signed_c})"));
                        }
                        ast::BinOp::Shr => {
                            return Ok(format!("sx_ishr({signed_a}, ({c}))"));
                        }
                        ast::BinOp::Eq
                        | ast::BinOp::Ne
                        | ast::BinOp::Lt
                        | ast::BinOp::Le
                        | ast::BinOp::Gt
                        | ast::BinOp::Ge => {
                            return Ok(format!("({signed_a} {o} {signed_c})"));
                        }
                        _ => {}
                    }
                }
                match op {
                    ast::BinOp::Div => format!("sx_udiv(({a}), ({c}))"),
                    ast::BinOp::Shl => format!("sx_shl(({a}), ({c}))"),
                    ast::BinOp::Shr => format!("sx_shr(({a}), ({c}))"),
                    _ => format!("({a} {o} {c})"),
                }
            }
            // The first element is the most significant, so each part shifts
            // left by the total width of everything after it. Hardware has
            // always lowered this; only the testbench refused it.
            ast::Expr::Concat { parts, .. } => {
                let mut widths = Vec::with_capacity(parts.len());
                for part in parts {
                    // A part of unknown width cannot be placed, and guessing
                    // one would silently misalign every part to its left.
                    match self.arg_width(part).or_else(|| self.enum_part_width(part)) {
                        Some(w) if w > 0 => widths.push(w),
                        _ => {
                            return Err(format!(
                                "unsupported testbench expression: `{}` — the width of \
                                 `{}` is not known here",
                                siox::syntax::pretty::expr_string(e),
                                siox::syntax::pretty::expr_string(part)
                            ))
                        }
                    }
                }
                let mut terms = Vec::with_capacity(parts.len());
                for (i, part) in parts.iter().enumerate() {
                    let value = self.expr(part)?;
                    let masked = format!("sx_mask(({value}), {})", widths[i]);
                    let shift: u32 = widths[i + 1..].iter().sum();
                    terms.push(if shift == 0 {
                        masked
                    } else {
                        format!("(({masked}) << {shift})")
                    });
                }
                format!("({})", terms.join(" | "))
            }
            other => {
                // Say WHICH expression, so the report is actionable.
                return Err(format!(
                    "unsupported testbench expression: `{}`",
                    siox::syntax::pretty::expr_string(other)
                ));
            }
        })
    }

    /// Read a scalar leaf through flattened runtime array dimensions. `suffix`
    /// lets composite spread/copy ask for the same dynamically selected base's
    /// `.field` leaf without manufacturing a second AST.
    fn dynamic_array_read(
        &self,
        expression: &ast::Expr,
        suffix: &str,
    ) -> Option<Result<String, String>> {
        fn walk<'e>(
            expression: &'e ast::Expr,
            steps: &mut Vec<NativeAccessStep<'e>>,
        ) -> Option<String> {
            match expression {
                ast::Expr::Path(path) if path.segments.len() == 1 => {
                    Some(path.segments[0].text.clone())
                }
                ast::Expr::Field { base, field, .. } => {
                    let root = walk(base, steps)?;
                    steps.push(NativeAccessStep::Field(&field.text));
                    Some(root)
                }
                ast::Expr::Index { base, index, .. } => {
                    let root = walk(base, steps)?;
                    steps.push(NativeAccessStep::Index(index));
                    Some(root)
                }
                _ => None,
            }
        }

        fn read_from(
            cx: &Ctx<'_>,
            path: &str,
            steps: &[NativeAccessStep<'_>],
            suffix: &str,
            saw_array: bool,
        ) -> Option<Result<String, String>> {
            let Some((step, rest)) = steps.split_first() else {
                return saw_array.then(|| cx.read_path(&format!("{path}{suffix}")));
            };
            match step {
                NativeAccessStep::Field(field) => {
                    read_from(cx, &format!("{path}.{field}"), rest, suffix, saw_array)
                }
                NativeAccessStep::Index(index) => {
                    if matches!(
                        index,
                        ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                    ) {
                        return None;
                    }
                    let Some(indices) = cx.local_indices.borrow().get(path).cloned() else {
                        // An array selection may be followed by a runtime bit
                        // selection of its packed element (`words[i][bit]`).
                        // Once the outer path is concrete, reuse the packed
                        // vector reader rather than treating the bit as a
                        // second aggregate dimension.
                        return (saw_array && rest.is_empty() && suffix.is_empty())
                            .then(|| cx.c_dynamic_bit_of_path(path, index))?;
                    };
                    let runtime_index = match cx.expr(index) {
                        Ok(index) => index,
                        Err(error) => return Some(Err(error)),
                    };
                    let runtime_index = cx.checked_index_c(
                        runtime_index,
                        index,
                        *indices.first()?,
                        *indices.last()?,
                    );
                    // The last arm remains only as an internal recovery value
                    // after a failure is latched; passing code cannot observe
                    // it as language behavior.
                    let (last, earlier) = indices.split_last()?;
                    let mut result =
                        match read_from(cx, &format!("{path}[{last}]"), rest, suffix, true)? {
                            Ok(value) => value,
                            Err(error) => return Some(Err(error)),
                        };
                    for logical in earlier.iter().rev() {
                        let selected =
                            match read_from(cx, &format!("{path}[{logical}]"), rest, suffix, true)?
                            {
                                Ok(value) => value,
                                Err(error) => return Some(Err(error)),
                            };
                        result = format!(
                            "((((int64_t)({runtime_index})) == {logical}LL) ? ({selected}) : ({result}))"
                        );
                    }
                    Some(Ok(result))
                }
            }
        }

        let mut steps = Vec::new();
        let root = walk(expression, &mut steps)?;
        read_from(self, &root, &steps, suffix, false)
    }

    /// The element paths of an array named `base`, in declaration order.
    /// Explicit ranges use their logical labels (`a[4]`, `a[3]`, `a[2]`), not
    /// the flattened storage positions 0, 1, 2.
    fn array_elements(&self, base: &str) -> Vec<String> {
        if let Some(indices) = self.local_indices.borrow().get(base).cloned() {
            return indices
                .into_iter()
                .map(|index| format!("{base}[{index}]"))
                .filter(|element| self.has_storage_prefix(element))
                .collect();
        }
        let mut out = Vec::new();
        loop {
            let element = format!("{base}[{}]", out.len());
            if !self.locals.borrow().contains(&element) && !self.map.contains_key(&element) {
                return out;
            }
            out.push(element);
        }
    }

    /// Read one already-flattened path: a testbench local reads its mangled C
    /// name, anything else is a connected signal.
    /// A constant bit slice of a packed value (`w[7..4]`, `w[7]`, `w[..4]`).
    /// Hardware has had these since ranges landed and the testbench had none,
    /// so a design could compute a nibble and its test could not check it.
    /// Returns `None` when this is not a slice of a scalar — an array element
    /// resolves through its own signal instead.
    fn c_bit_slice(&self, base: &ast::Expr, index: &ast::Expr) -> Option<Result<String, String>> {
        let path = expr_path(base)?;
        // An array's elements are separate locals; only a packed scalar is
        // sliced by bit position.
        if !self.array_elements(&path).is_empty() {
            return None;
        }
        if !self.locals.borrow().contains(&path) && !self.map.contains_key(&path) {
            return None;
        }
        let (a, b) = self.slice_bounds(&path, index)?;
        Some(self.c_bit_slice_of(&path, a, b))
    }

    fn c_dynamic_bit(&self, base: &ast::Expr, index: &ast::Expr) -> Option<Result<String, String>> {
        let path = expr_path(base)?;
        if !self.array_elements(&path).is_empty() {
            return None;
        }
        self.c_dynamic_bit_of_path(&path, index)
    }

    fn c_dynamic_bit_of_path(
        &self,
        path: &str,
        index: &ast::Expr,
    ) -> Option<Result<String, String>> {
        if matches!(
            index,
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return None;
        }
        if (!self.locals.borrow().contains(path) && !self.map.contains_key(path))
            || self.slice_bounds(path, index).is_some()
        {
            return None;
        }
        let width = self.name_width(path)?;
        let (left, right) = self
            .local_ranges
            .borrow()
            .get(path)
            .copied()
            .unwrap_or((0, i64::from(width).saturating_sub(1)));
        let (low, high) = (left.min(right), left.max(right));
        let value = match self.read_path(path) {
            Ok(value) => value,
            Err(error) => return Some(Err(error)),
        };
        let runtime_index = match self.expr(index) {
            Ok(index) => index,
            Err(error) => return Some(Err(error)),
        };
        let runtime_index = self.checked_index_c(runtime_index, index, left, right);
        let meta = self
            .map
            .get(path)
            .and_then(|id| self.design.meta_of.get(&id.0))
            .map(|id| format!("sx_read({id})"));
        let encoding = self
            .map
            .get(path)
            .and_then(|id| self.design.array_element_enums.get(&id.0))
            .and_then(|element| self.design.logic_encodings.get(element));
        let mut out = String::from("0");
        for logical in (low..=high).rev() {
            let physical = logical - low;
            let bit = format!("((({value}) >> {physical}) & 1)");
            let selected = match &meta {
                Some(meta) => {
                    let shift = physical * 4;
                    let nibble = format!("((({meta}) >> {shift}) & 15)");
                    let is_binary = encoding
                        .map(|encoding| c_disc_in(&nibble, &encoding.binary))
                        .unwrap_or_else(|| "0".to_string());
                    let low = encoding
                        .and_then(|encoding| encoding.binary_value(false))
                        .unwrap_or(0);
                    let high = encoding
                        .and_then(|encoding| encoding.binary_value(true))
                        .unwrap_or(1);
                    format!("({is_binary} ? (({bit}) ? {high}ULL : {low}ULL) : ({nibble}))")
                }
                None => bit,
            };
            out = format!("(((int64_t)({runtime_index}) == {logical}LL) ? ({selected}) : ({out}))");
        }
        Some(Ok(out))
    }

    /// The resolved `(a, b)` bit bounds of `base[index]`: an explicit range, a
    /// partial one completed from the declared range, or a single bit. Shared
    /// with `slice_width`, so the width a concat places a part at and the bits
    /// it actually reads come from one rule.
    fn slice_bounds(&self, path: &str, index: &ast::Expr) -> Option<(i64, i64)> {
        let konst = |e: &ast::Expr| siox::ir::eval_const_fns(e, &HashMap::new(), self.fns, 0);
        let declared = self.local_ranges.borrow().get(path).copied();
        Some(match index {
            ast::Expr::Range { lo, hi, .. } => (konst(lo)?, konst(hi)?),
            ast::Expr::PartialRange { lo, hi, .. } => {
                let (left, right) = declared?;
                (
                    match lo {
                        Some(lo) => konst(lo)?,
                        None => left,
                    },
                    match hi {
                        Some(hi) => konst(hi)?,
                        None => right,
                    },
                )
            }
            _ => {
                let n = konst(index)?;
                (n, n)
            }
        })
    }

    /// The bit width of a slice expression `base[index]`, inclusive of both
    /// bounds and independent of direction (spec 3.13). `arg_width` knew a
    /// named value's width and a conversion's, but not a slice's, so a concat
    /// of slices — which hardware lowers fine — was rejected in a testbench as
    /// "the width of `a[3..0]` is not known here".
    fn slice_width(&self, e: &ast::Expr) -> Option<u32> {
        let ast::Expr::Index { base, index, .. } = e else {
            return None;
        };
        let path = expr_path(base)?;
        if !self.array_elements(&path).is_empty() {
            return None;
        }
        if !self.locals.borrow().contains(&path) && !self.map.contains_key(&path) {
            return None;
        }
        let (a, b) = self.slice_bounds(&path, index)?;
        u32::try_from(a.max(b) - a.min(b) + 1).ok()
    }

    /// The C expression for bits `a..b` of `path`. A descending range is the
    /// natural order and shifts out directly; an ascending one names the same
    /// bits with their significance reversed (spec 3.13), so it is assembled
    /// bit by bit — the width is a constant, so the unrolling is bounded.
    fn c_bit_slice_of(&self, path: &str, a: i64, b: i64) -> Result<String, String> {
        let v = self.read_path(path)?;
        let declared_low = self
            .local_ranges
            .borrow()
            .get(path)
            .map(|&(left, right)| left.min(right))
            .unwrap_or(0);
        let (a, b) = (a - declared_low, b - declared_low);
        let (hi, lo) = (a.max(b), a.min(b));
        if hi < 0 || lo < 0 {
            return Err(format!("`{path}` sliced with a negative bound"));
        }
        let width = (hi - lo + 1) as u32;
        // One element of a Logic-family vector is a `Logic`, not a raw bit:
        // `'X'` and `'Z'` live in the companion plane, and the value plane only
        // says 0 or 1. Reading the bit alone compiled `y[7..7] == 'X'` to
        // `bit == 3`, which one bit can never satisfy, while `== '0'` was true
        // whenever the bit happened to be 0 -- so a testbench could confirm a
        // value the design does not hold, on a design the hardware side reports
        // as poisoned. The reconstruction is the one the IR uses for the same
        // read inside an entity: the companion is used exactly when the std
        // contract says the discriminant is not an ordinary binary value.
        if width == 1 {
            if let Some(nibble) = self.companion_nibble(path, lo) {
                let encoding = self
                    .map
                    .get(path)
                    .and_then(|id| self.design.array_element_enums.get(&id.0))
                    .and_then(|element| self.design.logic_encodings.get(element));
                let is_binary = encoding
                    .map(|encoding| c_disc_in(&nibble, &encoding.binary))
                    .unwrap_or_else(|| "0".to_string());
                let low = encoding
                    .and_then(|encoding| encoding.binary_value(false))
                    .unwrap_or(0);
                let high = encoding
                    .and_then(|encoding| encoding.binary_value(true))
                    .unwrap_or(1);
                return Ok(format!(
                    "({is_binary} ? (((({v}) >> {lo}) & 1ULL) ? {high}ULL : {low}ULL) : ({nibble}))"
                ));
            }
        }
        if a >= b {
            let mask = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            return Ok(format!("((({v}) >> {lo}) & {mask}ULL)"));
        }
        let mut parts = Vec::new();
        for k in 0..width {
            let from = a as u32 + k;
            let to = width - 1 - k;
            parts.push(format!("(((({v}) >> {from}) & 1ULL) << {to})"));
        }
        Ok(format!("({})", parts.join(" | ")))
    }

    /// The companion nibble holding element `element`'s full `Logic`
    /// discriminant, when the finished design gave `path` a metavalue plane.
    /// `None` for a metavalue-free vector, which has no companion and needs no
    /// reconstruction. Read a word at a time so a vector past sixteen elements
    /// -- whose companion is wider than one machine word -- is reached too.
    fn companion_nibble(&self, path: &str, element: i64) -> Option<String> {
        let &id = self.map.get(path)?;
        let &companion = self.design.meta_of.get(&id.0)?;
        let bit = element * 4;
        Some(format!(
            "((sx_read_word({companion}, {}) >> {}) & 0xFULL)",
            bit / 64,
            bit % 64
        ))
    }

    fn read_path(&self, path: &str) -> Result<String, String> {
        if self.locals.borrow().contains(path) {
            return Ok(c_local_ident(path));
        }
        let id = self.map.get(path).ok_or_else(|| unsup(path))?;
        self.check_scalar(*id)?;
        Ok(format!("sx_read({})", id.0))
    }

    /// Reject `real` signals in scalar expressions — native stimulus is
    /// integer-word only for now. A `Char` reads as its code point (a plain
    /// integer), so it is allowed.
    fn check_scalar(&self, id: SignalId) -> Result<(), String> {
        let s = &self.design.signals[id.0 as usize];
        if s.real {
            return Err(format!(
                "signal `{}` is real: a real DUT signal cannot be read from a \
                 testbench yet — real *locals* and arithmetic work, so compute \
                 with them here, or expose the value as an integer port \
                 (`integer(x)`)",
                s.path
            ));
        }
        Ok(())
    }
}

/// A form the emitter has no translation for — a real gap in the tool.
fn unsup(name: &str) -> String {
    format!("testbench references `{name}`, which siox build cannot translate yet")
}

/// A name nothing declares — a mistake in the source, not a gap in the tool.
/// The two used to share `unsup`'s wording, which told a reader with a typo
/// to go wait for a compiler feature.
fn unknown_value(name: &str) -> String {
    format!(
        "no value named `{name}` is in scope: it has to be a testbench local, \
         a connected signal, a constant, or a parameter"
    )
}

/// Map a siox binary operator to its C spelling. Word-logical ops become
/// boolean C operators (matching the interpreter's semantics).
fn c_binop(op: &ast::BinOp) -> Result<&'static str, String> {
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
        And => "&",
        Or => "|",
        _ => {
            return Err(format!(
                "unsupported operator `{}` in testbench expression",
                siox::syntax::pretty::bin_op(op)
            ))
        }
    })
}

// --- helpers (small replicas of interpreter internals) ---------------------

/// A `clk = !clk after d;` self-toggle: `Some((clock path, half period))`.
fn after_toggle(
    target: &ast::Expr,
    value: &ast::Expr,
    after: &Option<ast::Expr>,
) -> Result<Option<(String, u64)>, String> {
    let Some(delay) = after.as_ref() else {
        return Ok(None);
    };
    let Some(path) = expr_path(target) else {
        return Ok(None);
    };
    if let ast::Expr::Unary {
        op: ast::UnOp::Not,
        rhs,
        ..
    } = value
    {
        if expr_path(rhs).as_deref() == Some(path.as_str()) {
            let half = duration_fs(std::slice::from_ref(delay))?.max(1);
            return Ok(Some((path, half)));
        }
    }
    Ok(None)
}

/// Collect the background clocks in a test's body — `clock(clk, period)` calls
/// and the VHDL-style `clk = !clk after half;` idiom: (signal id, half fs).
fn scan_clocks(
    items: &[&ast::ImplItem],
    aliases: &HashMap<String, Vec<SignalId>>,
) -> Result<Vec<(u32, u64)>, String> {
    let mut clocks: Vec<(u32, u64)> = Vec::new();
    let mut add = |id: u32, half: u64| {
        if !clocks.iter().any(|(c, _)| *c == id) {
            clocks.push((id, half));
        }
    };
    let mut scan_statement = |statement: &ast::Stmt| -> Result<(), String> {
        if let ast::Stmt::Assign {
            target,
            value,
            after,
            ..
        } = statement
        {
            if let Some((path, half)) = after_toggle(target, value, after)? {
                // A clock shared by several DUTs toggles every port.
                for id in aliases.get(&path).map(|v| v.as_slice()).unwrap_or(&[]) {
                    add(id.0, half);
                }
            }
        }
        Ok(())
    };
    for item in items {
        match item {
            ast::ImplItem::Stmt(statement) => scan_statement(statement)?,
            ast::ImplItem::Process(process)
                if siox::testbench::is_clock_process(&process.body.stmts) =>
            {
                scan_statement(&process.body.stmts[0])?;
            }
            _ => {}
        }
    }
    Ok(clocks)
}

/// The femtosecond duration of `10ns` / `10.ns`; a missing/unknown form
/// defaults to the runner's half period (5 ns).
fn duration_fs(args: &[ast::Expr]) -> Result<u64, String> {
    let scaled = |text: &str, unit: &str| {
        let value = try_parse_u64(text)
            .ok_or_else(|| format!("duration value `{text}` does not fit the native timeline"))?;
        let scale = u64::try_from(ast::suffix_scale(unit).unwrap_or(1_000_000))
            .map_err(|_| format!("time unit `{unit}` is too large for the native timeline"))?;
        value.checked_mul(scale).ok_or_else(|| {
            format!("duration `{text}{unit}` exceeds the native 64-bit femtosecond timeline")
        })
    };
    match args.first() {
        Some(ast::Expr::SuffixLit { text, suffix, .. }) => scaled(text, &suffix.text),
        Some(ast::Expr::Field { base, field, .. }) => {
            if let ast::Expr::Int { text, .. } = base.as_ref() {
                scaled(text, &field.text)
            } else {
                Ok(5_000_000)
            }
        }
        _ => Ok(5_000_000),
    }
}

/// The constant tables the emitter reads, folded from a set of declarations.
///
/// Extracted so module constants and a testbench's *own* constants go through
/// exactly the same folding. They did not: only `Item::Const` was gathered, so
/// a `const` declared inside `impl SomeTest` reported its own name as unknown
/// at every use, in every kind. Folding per testbench also keeps two entities
/// that each declare `LIMIT` from colliding in one flat table.
struct ConstTables {
    real_consts: std::collections::HashSet<String>,
    integer_consts: std::collections::HashSet<String>,
    const_ranges: HashMap<String, (i64, i64)>,
    consts: HashMap<String, u128>,
    const_exprs: HashMap<String, String>,
    const_array_exprs: HashMap<String, Vec<String>>,
}

/// The `(field, value)` pairs of a struct literal written in place, against the
/// fields it is being read as.
///
/// A named argument binds by name and a positional one by declaration order; a
/// brace list carrying no field names at all lexes as a concatenation, and
/// against a struct that is the positional form. `None` for anything that is
/// not a literal, which the caller handles its own way.
fn literal_struct_fields<'a>(
    value: &'a ast::Expr,
    fields: &[String],
) -> Option<Vec<(String, &'a ast::Expr)>> {
    match value {
        ast::Expr::Construct { args, .. } => args
            .iter()
            .enumerate()
            .map(|(position, arg)| {
                let field = match &arg.field {
                    Some(name) => name.text.clone(),
                    None => fields.get(position)?.clone(),
                };
                Some((field, arg.value.as_ref()?))
            })
            .collect(),
        ast::Expr::Concat { parts, .. } => Some(
            parts
                .iter()
                .zip(fields)
                .map(|(part, field)| (field.clone(), part))
                .collect(),
        ),
        _ => None,
    }
}

/// The `(field, value)` pairs of a struct constant's literal.
///
/// Which reading applies is decided by the constant's declared type, not by
/// the braces: against a struct these are fields, against an array or packed
/// vector the same braces stay a concatenation. Named arguments bind by name
/// and positional ones by declaration order; a brace list carrying no field
/// names at all (`{ 6, 7 }`) lexes as a concat, and against a struct type that
/// is the positional form.
fn struct_const_fields<'a>(
    declaration: &'a ast::ConstDecl,
    struct_field_names: &HashMap<String, StructFieldNames>,
    fns: &FunctionIndex<'_>,
) -> Option<Vec<(String, &'a ast::Expr)>> {
    let declared = fns
        .type_head_key(&declaration.ty)
        .and_then(|head| struct_field_names.get(&head))?;
    match &declaration.value {
        ast::Expr::Construct { args, .. } => args
            .iter()
            .enumerate()
            .map(|(position, arg)| {
                let field = match &arg.field {
                    Some(name) => name.text.clone(),
                    None => declared.get(position)?.clone(),
                };
                Some((field, arg.value.as_ref()?))
            })
            .collect(),
        ast::Expr::Concat { parts, .. } => Some(
            parts
                .iter()
                .zip(declared)
                .map(|(part, field)| (field.clone(), part))
                .collect(),
        ),
        _ => None,
    }
}

fn const_tables(
    const_decls: &[(String, &ast::ConstDecl)],
    enums: &HashMap<String, HashMap<String, u64>>,
    fns: &FunctionIndex<'_>,
    type_aliases: &HashMap<String, ast::Type>,
    struct_field_names: &HashMap<String, StructFieldNames>,
) -> ConstTables {
    let real_consts: std::collections::HashSet<String> = const_decls
        .iter()
        .filter(|(_, declaration)| {
            resolved_type_name(&declaration.ty, type_aliases, fns).as_deref() == Some("real")
        })
        .map(|(key, _)| key.clone())
        .collect();
    let integer_consts: std::collections::HashSet<String> = const_decls
        .iter()
        .filter(|(_, declaration)| {
            resolved_type_name(&declaration.ty, type_aliases, fns).as_deref() == Some("integer")
        })
        .map(|(key, _)| key.clone())
        .collect();
    let const_ranges: HashMap<String, (i64, i64)> = const_decls
        .iter()
        .filter(|(_, declaration)| type_head_name(&declaration.ty) == Some("range"))
        .filter_map(|(key, declaration)| {
            let ast::Expr::Range { lo, hi, .. } = &declaration.value else {
                return None;
            };
            Some((
                key.clone(),
                (signed_index_bound(lo)?, signed_index_bound(hi)?),
            ))
        })
        .collect();
    let mut consts: HashMap<String, u128> = HashMap::new();
    for _ in 0..=const_decls.len() {
        let mut progressed = false;
        for (name, c) in const_decls {
            // A struct constant is one entry per field, keyed by the dotted
            // path a read spells (`K.a`) — the same shape hardware folds it
            // into. Without it the testbench refused `K.a` as something "siox
            // build cannot translate yet", on a declaration stage 4 accepted.
            if let Some(fields) = struct_const_fields(c, struct_field_names, fns) {
                if consts.contains_key(&format!("{name}.")) {
                    continue;
                }
                let folded: Option<Vec<(String, u128)>> = fields
                    .into_iter()
                    .map(|(field, value)| Some((field, eval_c_const(value, &consts, enums, fns)?)))
                    .collect();
                if let Some(folded) = folded {
                    for (field, value) in folded {
                        consts.insert(format!("{name}.{field}"), value);
                    }
                    // A marker so the fixed point does not refold this decl.
                    consts.insert(format!("{name}."), 0);
                    progressed = true;
                }
                continue;
            }
            if consts.contains_key(name) {
                continue;
            }
            if let Some(v) = eval_c_const(&c.value, &consts, enums, fns) {
                consts.insert(name.clone(), v);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    // Preserve the source expression separately from the narrow evaluator.
    // `_BitInt` accepts the resulting arbitrary-width C expression directly,
    // so testbench references never inherit the u128 helper's ceiling.
    let mut const_exprs: HashMap<String, String> = HashMap::new();
    for _ in 0..=const_decls.len() {
        let mut progressed = false;
        for (name, declaration) in const_decls {
            // A struct constant is one emitted expression per field, keyed by
            // the dotted path a read spells. The scalar table holds a single
            // entry per name, so a struct constant put nothing in it and every
            // read of `K.a` was reported as untranslatable.
            if let Some(fields) = struct_const_fields(declaration, struct_field_names, fns) {
                if const_exprs.contains_key(&format!("{name}.")) {
                    continue;
                }
                let folded: Option<Vec<(String, String)>> = fields
                    .into_iter()
                    .map(|(field, value)| {
                        Some((field, emit_c_const(value, &const_exprs, enums, fns)?))
                    })
                    .collect();
                if let Some(folded) = folded {
                    for (field, value) in folded {
                        const_exprs.insert(format!("{name}.{field}"), value);
                    }
                    const_exprs.insert(format!("{name}."), String::new());
                    progressed = true;
                }
                continue;
            }
            if const_exprs.contains_key(name) {
                continue;
            }
            let expression = if real_consts.contains(name) {
                emit_c_real_const(&declaration.value, &const_exprs, &real_consts, enums, fns)
                    .map(|value| format!("sx_b64((double)({value}))"))
            } else {
                emit_c_const(&declaration.value, &const_exprs, enums, fns)
            };
            if let Some(expression) = expression {
                const_exprs.insert(name.clone(), expression);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    // Constant lookup tables, one emitted expression per element. The scalar
    // table above holds a single entry per name, so an indexed read of a
    // `const` array found nothing there and was called untranslatable.
    let mut const_array_exprs: HashMap<String, Vec<String>> = HashMap::new();
    for (name, declaration) in const_decls {
        let ast::Expr::Array { elems, .. } = &declaration.value else {
            continue;
        };
        let values: Option<Vec<String>> = elems
            .iter()
            .map(|e| emit_c_const(e, &const_exprs, enums, fns))
            .collect();
        if let Some(values) = values {
            const_array_exprs.insert(name.clone(), values);
        }
    }

    ConstTables {
        real_consts,
        integer_consts,
        const_ranges,
        consts,
        const_exprs,
        const_array_exprs,
    }
}

fn build_map(
    hier: &Hierarchy,
    root: InstanceId,
    design: &Design,
) -> (HashMap<String, SignalId>, HashMap<String, Vec<SignalId>>) {
    // DUTs lower per-instance under the testbench path (`<test>.<inst>.<port>`),
    // so two instances of one entity stay distinct (matches siox-run's map).
    // `aliases` keeps EVERY binding of a name (one clock into many DUTs), so a
    // write drives all connected ports.
    let tb = hier.root_path(root);
    let mut map = HashMap::new();
    let mut aliases: HashMap<String, Vec<SignalId>> = HashMap::new();
    for &child_id in &hier.instance(root).children {
        let child = hier.instance(child_id);
        // Expose the instance's own signals under `<inst>.<rest>` so
        // post-declaration access (`dut.a = x;`, `dut.y`) resolves directly to
        // the DUT's port signal — the third connection form (spec 3.12).
        let iprefix = format!("{tb}.{}.", child.name);
        for (i, sig) in design.signals.iter().enumerate() {
            if let Some(rest) = sig.path.strip_prefix(&iprefix) {
                let key = format!("{}.{}", child.name, rest);
                let id = SignalId(i as u32);
                map.entry(key.clone()).or_insert(id);
                aliases.entry(key).or_default().push(id);
            }
        }
        for c in &child.connections {
            let prefix = format!("{tb}.{}.{}", child.name, c.port);
            for (i, sig) in design.signals.iter().enumerate() {
                let id = SignalId(i as u32);
                if sig.path == prefix {
                    map.insert(c.signal.clone(), id);
                    aliases.entry(c.signal.clone()).or_default().push(id);
                } else if let Some(suffix) = sig.path.strip_prefix(&prefix) {
                    if suffix.starts_with('.') || suffix.starts_with('[') {
                        let key = format!("{}{suffix}", c.signal);
                        map.insert(key.clone(), id);
                        aliases.entry(key).or_default().push(id);
                    }
                }
            }
        }
    }
    (map, aliases)
}

fn resolved_type_def_id(ty: &ast::Type, resolved: &Resolved) -> Option<crate::resolve::DefId> {
    match ty {
        ast::Type::Path(path) => resolved.resolved(path.span),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
            resolved_type_def_id(base, resolved)
        }
        ast::Type::View { view, .. } => resolved.resolved(view.span),
    }
}

fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.last().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::View { view, .. } => view.segments.last().map(|i| i.text.as_str()),
    }
}

/// A single string value rather than an outer array of strings.
fn is_single_string_type(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::Path(path) => path.segments.last().map(|id| id.text.as_str()) == Some("string"),
        ast::Type::Indexed { base, .. } => matches!(
            base.as_ref(),
            ast::Type::Path(path)
                if path.segments.last().map(|id| id.text.as_str()) == Some("string")
        ),
        _ => false,
    }
}

fn sized_string_indices(
    ty: &ast::Type,
    const_ranges: &HashMap<String, (i64, i64)>,
    consts: &HashMap<String, u128>,
    fns: &FunctionIndex<'_>,
) -> Option<Vec<i64>> {
    let ast::Type::Indexed {
        base,
        index: Some(index),
        ..
    } = ty
    else {
        return None;
    };
    if !matches!(
        base.as_ref(),
        ast::Type::Path(path)
            if path.segments.last().map(|id| id.text.as_str()) == Some("string")
    ) {
        return None;
    }
    index_values(index, const_ranges, consts, fns)
}

fn index_values(
    index: &ast::Expr,
    const_ranges: &HashMap<String, (i64, i64)>,
    consts: &HashMap<String, u128>,
    fns: &FunctionIndex<'_>,
) -> Option<Vec<i64>> {
    match index {
        ast::Expr::Int { text, .. } => {
            let count = i64::try_from(try_parse_u64(text)?).ok()?;
            Some((0..count).collect())
        }
        ast::Expr::Range { lo, hi, .. } => {
            let left = const_index_bound(lo, consts, fns)?;
            let right = const_index_bound(hi, consts, fns)?;
            Some(directional_indices(left, right))
        }
        ast::Expr::Path(path) => {
            let key = fns.constant_path_key(path)?;
            if let Some(&(left, right)) = const_ranges.get(&key) {
                Some(directional_indices(left, right))
            } else {
                let count = i64::try_from(*consts.get(&key)?).ok()?;
                Some((0..count).collect())
            }
        }
        _ => None,
    }
}

fn directional_indices(left: i64, right: i64) -> Vec<i64> {
    if left <= right {
        (left..=right).collect()
    } else {
        (right..=left).rev().collect()
    }
}

fn type_index_bounds(
    ty: &ast::Type,
    const_ranges: &HashMap<String, (i64, i64)>,
    consts: &HashMap<String, u128>,
    fns: &FunctionIndex<'_>,
) -> Option<(i64, i64)> {
    let ast::Type::Indexed {
        index: Some(index), ..
    } = ty
    else {
        return None;
    };
    let indices = index_values(index, const_ranges, consts, fns)?;
    Some((*indices.first()?, *indices.last()?))
}

fn const_index_bound(
    expression: &ast::Expr,
    consts: &HashMap<String, u128>,
    fns: &FunctionIndex<'_>,
) -> Option<i64> {
    let env: HashMap<String, i64> = consts
        .iter()
        .filter_map(|(key, &value)| Some((key.clone(), i64::try_from(value).ok()?)))
        .collect();
    siox::ir::eval_const_fns(expression, &env, fns, 0)
}

fn signed_index_bound(expr: &ast::Expr) -> Option<i64> {
    match expr {
        ast::Expr::Int { text, .. } => i64::try_from(try_parse_u64(text)?).ok(),
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => signed_index_bound(rhs)?.checked_neg(),
        _ => None,
    }
}

/// Phase-1 C ABI mapping for foreign function declarations. `real` is the
/// native C `double`, language `integer` is the signed ABI word, and packed
/// scalar values cross as the corresponding unsigned word.
fn extern_c_type(
    ty: Option<&ast::Type>,
    aliases: &HashMap<String, ast::Type>,
    fns: &FunctionIndex<'_>,
) -> &'static str {
    match ty
        .and_then(|ty| resolved_type_name(ty, aliases, fns))
        .as_deref()
    {
        Some("real") => "double",
        Some("integer") => "int64_t",
        _ => "uint64_t",
    }
}

fn resolved_type_name(
    ty: &ast::Type,
    aliases: &HashMap<String, ast::Type>,
    fns: &FunctionIndex<'_>,
) -> Option<String> {
    let mut ty = ty;
    let mut seen = std::collections::HashSet::new();
    loop {
        let name = fns.type_head_key(ty)?;
        let ast::Type::Path(path) = ty else {
            return Some(name);
        };
        let Some(key) = fns.type_alias_path_key(path) else {
            return Some(name);
        };
        if !seen.insert(key.clone()) {
            return Some(name);
        }
        let Some(alias) = aliases.get(&key) else {
            return Some(name);
        };
        ty = alias;
    }
}

fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", expr_path(base)?, field.text))
        }
        ast::Expr::Index { base, index, .. } => Some(format!(
            "{}[{}]",
            expr_path(base)?,
            signed_index_bound(index)?
        )),
        _ => None,
    }
}

/// The innermost named base of a field/index chain, for diagnostics: `a[i].x`
/// has no path but is still recognisably about `a`.
fn expr_path_base(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => expr_path_base(base),
        _ => None,
    }
}

fn descendant_suffix<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)
        .filter(|rest| rest.starts_with('[') || rest.starts_with('.'))
}

/// Render one C control-flow condition with exactly one syntactic wrapper.
/// Most expression lowering already returns a parenthesized expression; adding
/// another pair makes Clang warn about suspicious double-parenthesized
/// comparisons in every generated match arm.
fn c_condition(expression: &str) -> String {
    if expression.starts_with('(') && expression.ends_with(')') {
        expression.to_string()
    } else {
        format!("({expression})")
    }
}

fn str_lit(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::StrLit { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn parse_u64(text: &str) -> u64 {
    try_parse_u64(text).unwrap_or(0)
}

fn try_parse_u64(text: &str) -> Option<u64> {
    let normalized = text.trim().replace('_', "");
    let t = normalized.as_str();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2).ok()
    } else {
        t.parse().ok()
    }
}

/// A logic character's value: its position in std's default logic type
/// (`ULogic`), read from the parsed enum declaration — the emitter holds no
/// value table of its own. `0` if the char is not one of that type's variants.
fn logic_lit_value(c: char, enums: &HashMap<String, HashMap<String, u64>>) -> u64 {
    enums
        .get(siox::ir::DEFAULT_LOGIC_TYPE)
        .and_then(|m| m.get(&format!("'{c}'")))
        .copied()
        .unwrap_or(0)
}

fn c_disc_in(value: &str, members: &std::collections::HashSet<u64>) -> String {
    let mut members = members.iter().copied().collect::<Vec<_>>();
    members.sort_unstable();
    if members.is_empty() {
        return "0".to_string();
    }
    format!(
        "({})",
        members
            .into_iter()
            .map(|disc| format!("(({value}) == {disc}ULL)"))
            .collect::<Vec<_>>()
            .join(" || ")
    )
}

fn c_logic_value_bit(value: &str, encoding: &siox::ir::LogicEncoding) -> String {
    let highs = encoding
        .value_bits
        .iter()
        .filter_map(|(&disc, &high)| high.then_some(disc))
        .collect();
    c_disc_in(value, &highs)
}

#[cfg(test)]
mod tests {
    use super::c_condition;

    #[test]
    fn c_conditions_are_parenthesized_once() {
        assert_eq!(c_condition("ready"), "(ready)");
        assert_eq!(c_condition("((value) == 0ULL)"), "((value) == 0ULL)");
    }
}

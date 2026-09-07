//! The inkwell emitter.

use std::cell::RefCell;
use std::collections::HashMap;

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::TargetMachine;
use inkwell::values::{AsValueRef, FunctionValue, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use siox::ir::{BinOp, Design, Expr, IndexSite, ProcessKind, SignalId, UnOp};

/// LLVM's `IntegerType::MAX_INT_BITS` (from `llvm/IR/DerivedTypes.h`).
/// This is a backend capability, not a siox language/container limit.
pub(crate) const LLVM_MAX_INT_BITS: u32 = 1 << 23;

/// Bound each LLVM combinational helper so instruction selection never has to
/// hold an entire large design in one function. Calls happen only at process
/// group boundaries, keeping the scheduler overhead small.
const COMB_PROCESSES_PER_HELPER: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum CachedIntOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum CachedCast {
    ZeroExtend,
    SignExtend,
    Truncate,
}

impl CachedIntOp {
    /// Whether the operation's operands may be swapped, so a cached result can
    /// be reused for either argument order.
    fn commutative(self) -> bool {
        !matches!(self, Self::Sub)
    }
}

fn has_checked_index(expr: &Expr) -> bool {
    match expr {
        Expr::CheckedIndex { .. } => true,
        Expr::MetaCmp {
            operands, inner, ..
        } => operands.iter().any(has_checked_index) || has_checked_index(inner),
        Expr::Unary { rhs, .. } | Expr::Slice { base: rhs, .. } => has_checked_index(rhs),
        Expr::Binary { lhs, rhs, .. } => has_checked_index(lhs) || has_checked_index(rhs),
        Expr::TableLookup { index, .. } => has_checked_index(index),
        Expr::Select { cond, then, els } => {
            has_checked_index(cond) || has_checked_index(then) || has_checked_index(els)
        }
        Expr::CCall { args, .. } => args.iter().any(has_checked_index),
        Expr::Const(_)
        | Expr::WideConst(_)
        | Expr::Real(_)
        | Expr::Logic(_)
        | Expr::Current(_)
        | Expr::Old(_)
        | Expr::Event(_)
        | Expr::Unknown => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct StateSliceKey {
    array: u8,
    signal: SignalId,
    hi: u32,
    lo: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ComparisonKey {
    predicate: u8,
    lhs: usize,
    rhs: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct OperationKey {
    opcode: CachedIntOp,
    lhs: usize,
    rhs: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SelectKey {
    condition: usize,
    then_value: usize,
    else_value: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CastKey {
    opcode: CachedCast,
    value: usize,
    width: u32,
}

#[derive(Default)]
struct CombValueCache<'ctx> {
    loads: HashMap<(u8, SignalId), IntValue<'ctx>>,
    slices: HashMap<StateSliceKey, IntValue<'ctx>>,
    comparisons: HashMap<ComparisonKey, IntValue<'ctx>>,
    operations: HashMap<OperationKey, IntValue<'ctx>>,
    selects: HashMap<SelectKey, IntValue<'ctx>>,
    casts: HashMap<CastKey, IntValue<'ctx>>,
}

/// Run LLVM's `-O1` pipeline before codegen. The emitter shares dominating
/// combinational values directly; `-O1` handles the remaining folding,
/// simplification, and dead-code removal without a redundant final GVN pass.
/// The broader `-O2` pipeline added about 28% compile time to the large NVC
/// sweep without improving its measured settle throughput.
pub fn optimize_module(module: &Module, tm: &TargetMachine) -> Result<(), String> {
    // Give the optimizer the target's data layout and triple so it sizes
    // pointers, aligns, and vectorizes for the real machine.
    module.set_triple(&tm.get_triple());
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    module
        .run_passes("default<O1>", tm, PassBuilderOptions::create())
        .map_err(|e| format!("LLVM optimization failed: {e}"))
}

#[cfg(all(test, feature = "bitpack"))]
mod bitpack_tests {
    use super::*;
    use siox::ir::Signal;

    #[test]
    /// The event plane uses one bit per signal, so its size tracks the signal
    /// count rather than a fixed word.
    fn events_use_one_bit_per_signal() {
        let signal = |index| Signal {
            path: format!("s{index}"),
            declaration_span: siox::diag::Span::new(siox::diag::FileId(0), 0..0),
            width: 32,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        };
        let design = Design {
            signals: (0..65).map(signal).collect(),
            drivers: vec![],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let llvm = emit_module_ir(&design).unwrap();
        assert!(
            llvm.contains("@event = internal global [2 x i64]"),
            "65 event flags need exactly two words:\n{llvm}"
        );
    }
}

/// Build the LLVM module for `design` and return its textual IR (`.ll`).
/// This is what `siox build --emit-llvm` prints and what golden tests diff.
pub fn emit_module_ir(design: &Design) -> Result<String, String> {
    let ctx = Context::create();
    let module = build_module(&ctx, design)?;
    Ok(module.print_to_string().to_string())
}

/// Build and verify the LLVM module for `design` in `ctx`.
pub(crate) fn build_module<'ctx>(
    ctx: &'ctx Context,
    design: &Design,
) -> Result<Module<'ctx>, String> {
    // Reject IR a backend can't compile (bad ids, Unknown, unknown widths)
    // with a clear message rather than emitting malformed LLVM (B0).
    let issues = design.validate();
    if !issues.is_empty() {
        return Err(format!(
            "cannot codegen invalid IR:\n  - {}",
            issues.join("\n  - ")
        ));
    }
    if let Some((id, signal, width)) = design.signals.iter().enumerate().find_map(|(id, signal)| {
        let width = design.signal_width(SignalId(id as u32))?;
        (width > LLVM_MAX_INT_BITS).then_some((id, signal, width))
    }) {
        return Err(format!(
            "signal `{}` (id {id}) is {width} bits wide, but this LLVM backend supports integer \
             values up to {LLVM_MAX_INT_BITS} bits",
            signal.path
        ));
    }
    if let Some((id, width)) = design
        .process_ir
        .values
        .iter()
        .enumerate()
        .filter_map(|(id, value)| value.bit_width.map(|width| (id, width)))
        .find(|(_, width)| *width > LLVM_MAX_INT_BITS)
    {
        return Err(format!(
            "process value {id} is {width} bits wide, but this LLVM backend supports integer \
             values up to {LLVM_MAX_INT_BITS} bits"
        ));
    }
    if let Some((id, table)) = design
        .lookup_tables
        .iter()
        .enumerate()
        .find(|(_, table)| table.values.len() > u32::MAX as usize)
    {
        return Err(format!(
            "lookup table {id} has {} elements, but this LLVM backend supports at most {}",
            table.values.len(),
            u32::MAX
        ));
    }
    let cg = Codegen::new(ctx, design);
    cg.build();
    super::process::emit_metadata(ctx, &cg.module, design);
    // LLVM's own verifier — a well-formedness net beyond textual checks.
    if let Err(e) = cg.module.verify() {
        return Err(format!(
            "emitted invalid LLVM module:\n{}\n--- IR ---\n{}",
            e,
            cg.module.print_to_string()
        ));
    }
    Ok(cg.module)
}

struct Codegen<'ctx, 'd> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    design: &'d Design,
    n: u32,
    /// Assignment span -> the site index the runtime latches (one-based, so a
    /// zero in the global still means "no site"). Built once from
    /// `Design::range_sites`, which the harness walks too.
    range_sites: HashMap<siox::diag::Span, u32>,
    /// Checked index domain -> one-based runtime diagnostic id.
    index_sites: HashMap<IndexSite, u32>,
    /// Values computed in the current straight-line combinational helper.
    ///
    /// A helper has one basic block, so remembered state loads, direct slices,
    /// comparisons, and pure integer operations dominate every later use.
    /// Stores invalidate their semantic signal and a foreign call clears the
    /// cache. It stays disabled everywhere else: accessors and `sx_settle`
    /// contain control flow where blindly reusing an SSA value would either
    /// cross a non-dominating block or observe stale simulation state.
    comb_values: RefCell<Option<CombValueCache<'ctx>>>,
    /// The signal-state layout: one field per signal, each an integer sized to
    /// the signal's width (`i8`/`i16`/`i32`/`i64`), packed. A `Bit` or `Logic`
    /// takes one byte, not eight. The `cur`/`old`/`event`/`snap` globals all use
    /// it; compute stays in `i64`, so `load` zero-extends and `store` truncates.
    #[cfg(not(feature = "bitpack"))]
    state_ty: inkwell::types::StructType<'ctx>,
    /// The bit-packed layout (feature `bitpack`): `(word, shift)` per signal —
    /// many small signals share a 64-bit word. State globals are `[words x i64]`.
    #[cfg(feature = "bitpack")]
    slots: Vec<(u32, u32)>,
    #[cfg(feature = "bitpack")]
    words: u32,
    /// Dedicated one-bit-per-signal event storage.
    #[cfg(feature = "bitpack")]
    event_words: u32,
}

/// The smallest machine integer that holds `width` bits. Sub-byte widths (a
/// 1-bit `Bit`, a 4-bit `Logic`) round up to a byte — the addressable floor.
#[cfg(not(feature = "bitpack"))]
fn storage_int(ctx: &Context, width: u32) -> inkwell::types::IntType<'_> {
    match width {
        0..=8 => ctx.i8_type(),
        9..=16 => ctx.i16_type(),
        17..=32 => ctx.i32_type(),
        33..=64 => ctx.i64_type(),
        // Past one machine word LLVM still has a native integer: it legalizes
        // `iN` into word-sized pieces with the right carries and shifts, so a
        // multi-word signal needs no hand-written word juggling here. Keep the
        // exact semantic width; rounding to a power of two both wasted storage
        // and overflowed for large valid `u32` widths.
        w => ctx
            .custom_width_int_type(std::num::NonZeroU32::new(w).expect("non-zero width"))
            .expect("LLVM supports the width"),
    }
}

/// The low-`w`-bits mask (`w >= 64` → all ones).
#[cfg(feature = "bitpack")]
fn width_mask(w: u32) -> u64 {
    if w >= 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}

/// Assign each signal a `(word, shift)`. Sub-word values share a word without
/// straddling it; wider values start at a word boundary and reserve as many
/// consecutive words as their own width requires.
#[cfg(feature = "bitpack")]
fn pack_layout(design: &Design) -> (Vec<(u32, u32)>, u32) {
    let mut slots = Vec::with_capacity(design.signals.len());
    let (mut word, mut bit) = (0u32, 0u32);
    for (id, _) in design.signals.iter().enumerate() {
        let w = design
            .signal_width(SignalId(id as u32))
            .expect("validated signal width")
            .max(1);
        if w > 64 {
            if bit != 0 {
                word += 1;
                bit = 0;
            }
            slots.push((word, 0));
            word += w.div_ceil(64);
            continue;
        }
        if bit + w > 64 {
            word += 1;
            bit = 0;
        }
        slots.push((word, bit));
        bit += w;
    }
    let words = (word + u32::from(bit != 0)).max(1);
    (slots, words)
}

impl<'ctx, 'd> Codegen<'ctx, 'd> {
    /// An emitter for `design` in `ctx`, with empty caches.
    fn new(ctx: &'ctx Context, design: &'d Design) -> Self {
        let module = ctx.create_module("design");
        #[cfg(not(feature = "bitpack"))]
        let state_ty = {
            let fields: Vec<_> = design
                .signals
                .iter()
                .enumerate()
                .map(|(id, _)| {
                    storage_int(
                        ctx,
                        design
                            .signal_width(SignalId(id as u32))
                            .expect("validated signal width"),
                    )
                    .into()
                })
                .collect();
            // A literal struct is printed in full at every GEP. Large designs
            // consequently repeated the complete signal layout hundreds of
            // thousands of times in textual LLVM IR, making an 18 KiB source
            // produce more than 500 MiB of IR and temporarily consume over a
            // GiB while LLVM formatted it. A named type has identical layout
            // and code generation, but each use is only `%sx.state`.
            let state = ctx.opaque_struct_type("sx.state");
            assert!(state.set_body(&fields, true), "fresh state type is opaque");
            state
        };
        #[cfg(feature = "bitpack")]
        let (slots, words) = pack_layout(design);
        Codegen {
            ctx,
            module,
            builder: ctx.create_builder(),
            design,
            n: design.signals.len() as u32,
            range_sites: design
                .range_sites()
                .into_iter()
                .enumerate()
                .map(|(i, span)| (span, i as u32 + 1))
                .collect(),
            index_sites: design
                .index_sites()
                .into_iter()
                .enumerate()
                .map(|(i, site)| (site, i as u32 + 1))
                .collect(),
            comb_values: RefCell::new(None),
            #[cfg(not(feature = "bitpack"))]
            state_ty,
            #[cfg(feature = "bitpack")]
            slots,
            #[cfg(feature = "bitpack")]
            words,
            #[cfg(feature = "bitpack")]
            event_words: (design.signals.len() as u32).div_ceil(64).max(1),
        }
    }

    /// The backend ABI word. This is only for counters, packed storage, and
    /// external word accessors; logical expressions use [`Self::value_ty`].
    fn i64t(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i64_type()
    }

    /// LLVM integer for one logical value. Width is a property of that value's
    /// type, never of the widest signal elsewhere in the design.
    fn value_ty(&self, width: u32) -> inkwell::types::IntType<'ctx> {
        self.ctx
            .custom_width_int_type(
                std::num::NonZeroU32::new(width.max(1)).expect("logical widths are non-zero"),
            )
            .expect("LLVM supports the logical width")
    }

    /// The declared width of a signal.
    fn signal_width(&self, id: SignalId) -> u32 {
        self.design
            .signal_width(id)
            .expect("Design::validate accepted this signal layout")
    }

    /// The storage integer type of signal `id` (a field of [`Codegen::state_ty`]).
    #[cfg(not(feature = "bitpack"))]
    fn slot_ty(&self, id: SignalId) -> inkwell::types::IntType<'ctx> {
        storage_int(self.ctx, self.signal_width(id))
    }

    /// Emit the whole module: state globals, accessors, the settle function and
    /// its combinational helpers.
    fn build(&self) {
        let range_error = self
            .module
            .add_global(self.ctx.i32_type(), None, "range_error");
        range_error.set_initializer(&self.ctx.i32_type().const_zero());
        range_error.set_linkage(Linkage::Internal);
        // The offending value, recorded with the id. Rebuilding the message
        // from the stored signal reports the value *after* truncation to the
        // destination width, which can land back inside the declared domain:
        // `t + step` of 10 into `integer<-8..7>` stores -6 and read
        // "`t` left its range -8..7 (it was -6)".
        let range_value = self
            .module
            .add_global(self.ctx.i64_type(), None, "range_value");
        range_value.set_initializer(&self.ctx.i64_type().const_zero());
        range_value.set_linkage(Linkage::Internal);
        // Which assignment stored it, as an index into `Design::range_sites`
        // plus one. Latched with the id under the same predicate, so the three
        // always describe one event: the signal, the value, and the line.
        let range_site = self
            .module
            .add_global(self.ctx.i32_type(), None, "range_site");
        range_site.set_initializer(&self.ctx.i32_type().const_zero());
        range_site.set_linkage(Linkage::Internal);
        let index_error = self
            .module
            .add_global(self.ctx.i32_type(), None, "index_error");
        index_error.set_initializer(&self.ctx.i32_type().const_zero());
        index_error.set_linkage(Linkage::Internal);
        let index_value = self
            .module
            .add_global(self.ctx.i64_type(), None, "index_value");
        index_value.set_initializer(&self.ctx.i64_type().const_zero());
        index_value.set_linkage(Linkage::Internal);
        self.lookup_globals();
        self.state_globals();
        self.accessors();
        let comb_helpers = self.comb_helpers();
        self.settle(&comb_helpers);
    }

    /// Pointer to the flag recording whether a ranged-numeric check failed.
    fn range_error_ptr(&self) -> PointerValue<'ctx> {
        self.module
            .get_global("range_error")
            .expect("range error global")
            .as_pointer_value()
    }

    /// Pointer to the offending value latched by a ranged-numeric failure.
    fn range_value_ptr(&self) -> PointerValue<'ctx> {
        self.module
            .get_global("range_value")
            .expect("range value global")
            .as_pointer_value()
    }

    /// The latched site index for an assignment's span, or `0` for drivers the
    /// lowering synthesized and for spans on signals with no declared range.
    fn site(&self, span: Option<siox::diag::Span>) -> u32 {
        span.and_then(|span| self.range_sites.get(&span))
            .copied()
            .unwrap_or(0)
    }

    /// Pointer to the site identifier of a ranged-numeric failure, so the report
    /// can name the declaration.
    fn range_site_ptr(&self) -> PointerValue<'ctx> {
        self.module
            .get_global("range_site")
            .expect("range site global")
            .as_pointer_value()
    }

    /// Pointer to the flag recording whether a bounds check failed.
    fn index_error_ptr(&self) -> PointerValue<'ctx> {
        self.module
            .get_global("index_error")
            .expect("index error global")
            .as_pointer_value()
    }

    /// Pointer to the offending index latched by a bounds failure.
    fn index_value_ptr(&self) -> PointerValue<'ctx> {
        self.module
            .get_global("index_value")
            .expect("index value global")
            .as_pointer_value()
    }

    /// The storage width for a lookup table's elements: the smallest convenient
    /// integer type that holds `element_width`.
    fn lookup_storage_width(element_width: u32) -> u32 {
        element_width.next_power_of_two().max(8)
    }

    /// Materialize each design-owned constant table once. Logic tables use
    /// byte storage; the same IR node remains valid for wider future tables by
    /// selecting the next native integer width up to the IR's 64-bit value.
    fn lookup_globals(&self) {
        for (id, table) in self.design.lookup_tables.iter().enumerate() {
            let element = self.value_ty(Self::lookup_storage_width(table.element_width));
            let values = table
                .values
                .iter()
                .map(|value| element.const_int(*value, false))
                .collect::<Vec<_>>();
            let array = element.const_array(&values);
            let global = self
                .module
                .add_global(array.get_type(), None, &format!("sx.lookup.{id}"));
            global.set_initializer(&array);
            global.set_constant(true);
            global.set_linkage(Linkage::Internal);
        }
    }

    // --- state ------------------------------------------------------------

    #[cfg(not(feature = "bitpack"))]
    /// Declare the globals backing the design's signal state.
    fn state_globals(&self) {
        // Each of `cur`/`old`/`event`/`snap` is one width-packed struct (see
        // `state_ty`). `snap` holds each delta's entry values, so `old` can
        // advance to them and internally-generated edges fire in the next delta
        // (cascaded event domains / derived clocks).
        for name in ["cur", "old", "event", "snap"] {
            let g = self.module.add_global(self.state_ty, None, name);
            g.set_initializer(&self.state_ty.const_zero());
            g.set_linkage(Linkage::Internal);
        }
    }

    /// Pointer to one named state array.
    fn array_ptr(&self, name: &str) -> PointerValue<'ctx> {
        self.module.get_global(name).unwrap().as_pointer_value()
    }

    /// A compact key for a state array, used in the load and slice caches.
    fn state_array_key(arr: &str) -> u8 {
        match arr {
            "cur" => 0,
            "old" => 1,
            "event" => 2,
            "snap" => 3,
            _ => unreachable!("unknown state array `{arr}`"),
        }
    }

    /// A previously loaded value for this signal, if one is still valid.
    fn cached_load(&self, arr: &str, id: SignalId) -> Option<IntValue<'ctx>> {
        self.comb_values
            .borrow()
            .as_ref()
            .and_then(|cache| cache.loads.get(&(Self::state_array_key(arr), id)).copied())
    }

    /// Record a loaded value so a later read in the same straight-line region
    /// can reuse it.
    fn remember_load(&self, arr: &str, id: SignalId, value: IntValue<'ctx>) {
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.loads.insert((Self::state_array_key(arr), id), value);
        }
    }

    /// Drop the cached load for a signal, because it has just been written.
    fn invalidate_load(&self, arr: &str, id: SignalId) {
        let array = Self::state_array_key(arr);
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.loads.remove(&(array, id));
            cache
                .slices
                .retain(|key, _| key.array != array || key.signal != id);
        }
    }

    /// Drop every cached value after a foreign call that may mutate state.
    fn clear_comb_cache(&self) {
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.loads.clear();
            cache.slices.clear();
            cache.comparisons.clear();
            cache.operations.clear();
            cache.selects.clear();
            cache.casts.clear();
        }
    }

    /// A previously computed state slice, if one is still valid.
    fn cached_slice(&self, key: StateSliceKey) -> Option<IntValue<'ctx>> {
        self.comb_values
            .borrow()
            .as_ref()
            .and_then(|cache| cache.slices.get(&key).copied())
    }

    /// Record a computed state slice for reuse.
    fn remember_slice(&self, key: StateSliceKey, value: IntValue<'ctx>) {
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.slices.insert(key, value);
        }
    }

    /// Emit an integer comparison, reusing an identical earlier one.
    fn int_compare(
        &self,
        predicate: IntPredicate,
        lhs: IntValue<'ctx>,
        rhs: IntValue<'ctx>,
        name: &str,
    ) -> IntValue<'ctx> {
        let key = ComparisonKey {
            predicate: predicate as u8,
            lhs: lhs.as_value_ref() as usize,
            rhs: rhs.as_value_ref() as usize,
        };
        if let Some(value) = self
            .comb_values
            .borrow()
            .as_ref()
            .and_then(|cache| cache.comparisons.get(&key).copied())
        {
            return value;
        }
        let value = self
            .builder
            .build_int_compare(predicate, lhs, rhs, name)
            .unwrap();
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.comparisons.insert(key, value);
        }
        value
    }

    /// Emit a pure integer operation, reusing an identical earlier one.
    /// Commutative operands are normalized so either order hits the same entry.
    fn int_binary(
        &self,
        opcode: CachedIntOp,
        lhs: IntValue<'ctx>,
        rhs: IntValue<'ctx>,
        name: &str,
    ) -> IntValue<'ctx> {
        let mut operands = (lhs.as_value_ref() as usize, rhs.as_value_ref() as usize);
        if opcode.commutative() && operands.0 > operands.1 {
            operands = (operands.1, operands.0);
        }
        let key = OperationKey {
            opcode,
            lhs: operands.0,
            rhs: operands.1,
        };
        if let Some(value) = self
            .comb_values
            .borrow()
            .as_ref()
            .and_then(|cache| cache.operations.get(&key).copied())
        {
            return value;
        }
        let value = match opcode {
            CachedIntOp::Add => self.builder.build_int_add(lhs, rhs, name),
            CachedIntOp::Sub => self.builder.build_int_sub(lhs, rhs, name),
            CachedIntOp::Mul => self.builder.build_int_mul(lhs, rhs, name),
            CachedIntOp::And => self.builder.build_and(lhs, rhs, name),
            CachedIntOp::Or => self.builder.build_or(lhs, rhs, name),
            CachedIntOp::Xor => self.builder.build_xor(lhs, rhs, name),
        }
        .unwrap();
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.operations.insert(key, value);
        }
        value
    }

    /// Emit a select, reusing an identical earlier one.
    fn int_select(
        &self,
        condition: IntValue<'ctx>,
        then_value: IntValue<'ctx>,
        else_value: IntValue<'ctx>,
        name: &str,
    ) -> IntValue<'ctx> {
        let key = SelectKey {
            condition: condition.as_value_ref() as usize,
            then_value: then_value.as_value_ref() as usize,
            else_value: else_value.as_value_ref() as usize,
        };
        if let Some(value) = self
            .comb_values
            .borrow()
            .as_ref()
            .and_then(|cache| cache.selects.get(&key).copied())
        {
            return value;
        }
        let value = self
            .builder
            .build_select(condition, then_value, else_value, name)
            .unwrap()
            .into_int_value();
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.selects.insert(key, value);
        }
        value
    }

    /// Emit an integer cast, reusing an identical earlier one.
    fn int_cast(
        &self,
        opcode: CachedCast,
        value: IntValue<'ctx>,
        ty: inkwell::types::IntType<'ctx>,
        name: &str,
    ) -> IntValue<'ctx> {
        let key = CastKey {
            opcode,
            value: value.as_value_ref() as usize,
            width: ty.get_bit_width(),
        };
        if let Some(value) = self
            .comb_values
            .borrow()
            .as_ref()
            .and_then(|cache| cache.casts.get(&key).copied())
        {
            return value;
        }
        let cast = match opcode {
            CachedCast::ZeroExtend => self.builder.build_int_z_extend(value, ty, name),
            CachedCast::SignExtend => self.builder.build_int_s_extend(value, ty, name),
            CachedCast::Truncate => self.builder.build_int_truncate(value, ty, name),
        }
        .unwrap();
        if let Some(cache) = self.comb_values.borrow_mut().as_mut() {
            cache.casts.insert(key, cast);
        }
        cast
    }

    /// Pointer to signal `id`'s field in `@<arr>`.
    #[cfg(not(feature = "bitpack"))]
    fn slot_ptr(&self, arr: &str, id: SignalId) -> PointerValue<'ctx> {
        self.builder
            .build_struct_gep(self.state_ty, self.array_ptr(arr), id.0, "slot")
            .unwrap()
    }

    /// Load signal `id` from `@<arr>`, zero-extended to the `i64` the compute
    /// paths use.
    #[cfg(not(feature = "bitpack"))]
    fn load(&self, arr: &str, id: SignalId) -> IntValue<'ctx> {
        if let Some(value) = self.cached_load(arr, id) {
            return value;
        }
        let ty = self.slot_ty(id);
        let v = self
            .builder
            .build_load(ty, self.slot_ptr(arr, id), "v")
            .unwrap()
            .into_int_value();
        let value = self.fit(v, self.value_ty(self.signal_width(id)));
        self.remember_load(arr, id, value);
        value
    }

    /// Zero-extend or truncate `v` to `ty`. Storage, compute and ABI widths all
    /// differ once a design holds a multi-word signal, so every crossing goes
    /// through here rather than comparing against a hardcoded 64.
    fn fit(&self, v: IntValue<'ctx>, ty: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        let (from, to) = (v.get_type().get_bit_width(), ty.get_bit_width());
        match from.cmp(&to) {
            std::cmp::Ordering::Less => self.int_cast(CachedCast::ZeroExtend, v, ty, "zx"),
            std::cmp::Ordering::Greater => self.int_cast(CachedCast::Truncate, v, ty, "tr"),
            std::cmp::Ordering::Equal => v,
        }
    }

    /// Signed counterpart of [`Self::fit`]: widening preserves the source sign
    /// bit, while equal-width and narrowing crossings are representation
    /// identical.
    fn fit_signed(&self, v: IntValue<'ctx>, ty: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        let (from, to) = (v.get_type().get_bit_width(), ty.get_bit_width());
        match from.cmp(&to) {
            std::cmp::Ordering::Less => self.int_cast(CachedCast::SignExtend, v, ty, "sx"),
            std::cmp::Ordering::Greater => self.int_cast(CachedCast::Truncate, v, ty, "tr"),
            std::cmp::Ordering::Equal => v,
        }
    }

    /// Store an `i64` compute value into signal `id`'s width-sized slot in
    /// `@<arr>` (truncating; writers already mask to the signal width).
    #[cfg(not(feature = "bitpack"))]
    fn store(&self, arr: &str, id: SignalId, v: IntValue<'ctx>) {
        self.invalidate_load(arr, id);
        let ty = self.slot_ty(id);
        let v = self.fit(v, ty);
        self.builder.build_store(self.slot_ptr(arr, id), v).unwrap();
    }

    // --- bit-packed state layout (feature `bitpack`) ----------------------

    #[cfg(feature = "bitpack")]
    /// Declare the state globals for the test-harness emitter.
    fn state_globals(&self) {
        // `cur`/`old`/`event`/`snap` are each `[words x i64]`; signals share
        // words (see `pack_layout`). `snap` holds each delta's entry values so
        // `old` can advance and internally-generated edges fire next delta.
        let arr = self.i64t().array_type(self.words);
        for name in ["cur", "old", "snap"] {
            let g = self.module.add_global(arr, None, name);
            g.set_initializer(&arr.const_zero());
            g.set_linkage(Linkage::Internal);
        }
        let events = self.i64t().array_type(self.event_words);
        let g = self.module.add_global(events, None, "event");
        g.set_initializer(&events.const_zero());
        g.set_linkage(Linkage::Internal);
    }

    /// Pointer to `@<arr>`'s `word`-th `i64`.
    #[cfg(feature = "bitpack")]
    fn word_ptr(&self, arr: &str, word: u32) -> PointerValue<'ctx> {
        let i64 = self.i64t();
        let words = if arr == "event" {
            self.event_words
        } else {
            self.words
        };
        unsafe {
            self.builder
                .build_in_bounds_gep(
                    i64.array_type(words),
                    self.array_ptr(arr),
                    &[i64.const_zero(), i64.const_int(word as u64, false)],
                    "wp",
                )
                .unwrap()
        }
    }

    /// Load signal `id`: read its word, shift its field down, mask to width.
    #[cfg(feature = "bitpack")]
    fn load(&self, arr: &str, id: SignalId) -> IntValue<'ctx> {
        if let Some(value) = self.cached_load(arr, id) {
            return value;
        }
        let (word, shift, w) = if arr == "event" {
            (id.0 / 64, id.0 % 64, 1)
        } else {
            let (word, shift) = self.slots[id.0 as usize];
            (word, shift, self.signal_width(id))
        };
        let i64 = self.i64t();
        if w > 64 {
            let ty = self.value_ty(w);
            let mut value = ty.const_zero();
            for chunk in 0..w.div_ceil(64) {
                let part = self
                    .builder
                    .build_load(i64, self.word_ptr(arr, word + chunk), "w")
                    .unwrap()
                    .into_int_value();
                let part = self.fit(part, ty);
                let part = if chunk == 0 {
                    part
                } else {
                    self.builder
                        .build_left_shift(part, ty.const_int(u64::from(chunk) * 64, false), "sh")
                        .unwrap()
                };
                value = self.builder.build_or(value, part, "join").unwrap();
            }
            self.remember_load(arr, id, value);
            return value;
        }
        let word_val = self
            .builder
            .build_load(i64, self.word_ptr(arr, word), "w")
            .unwrap()
            .into_int_value();
        let shifted = if shift > 0 {
            self.builder
                .build_right_shift(word_val, i64.const_int(shift as u64, false), false, "sh")
                .unwrap()
        } else {
            word_val
        };
        let field = self
            .builder
            .build_and(shifted, i64.const_int(width_mask(w), false), "fld")
            .unwrap();
        let value = self.fit(field, self.value_ty(w));
        self.remember_load(arr, id, value);
        value
    }

    /// Store signal `id`: read-modify-write its word — clear the field bits,
    /// OR in the masked, shifted value.
    #[cfg(feature = "bitpack")]
    fn store(&self, arr: &str, id: SignalId, v: IntValue<'ctx>) {
        self.invalidate_load(arr, id);
        let (word, shift, w) = if arr == "event" {
            (id.0 / 64, id.0 % 64, 1)
        } else {
            let (word, shift) = self.slots[id.0 as usize];
            (word, shift, self.signal_width(id))
        };
        let i64 = self.i64t();
        if w > 64 {
            let ty = self.value_ty(w);
            let value = self.fit(v, ty);
            for chunk in 0..w.div_ceil(64) {
                let part = if chunk == 0 {
                    value
                } else {
                    self.builder
                        .build_right_shift(
                            value,
                            ty.const_int(u64::from(chunk) * 64, false),
                            false,
                            "sh",
                        )
                        .unwrap()
                };
                self.builder
                    .build_store(self.word_ptr(arr, word + chunk), self.fit(part, i64))
                    .unwrap();
            }
            return;
        }
        let mask = width_mask(w);
        let v = self.fit(v, i64);
        let field = self
            .builder
            .build_and(v, i64.const_int(mask, false), "m")
            .unwrap();
        let field = if shift > 0 {
            self.builder
                .build_left_shift(field, i64.const_int(shift as u64, false), "fsh")
                .unwrap()
        } else {
            field
        };
        let ptr = self.word_ptr(arr, word);
        let cur = self
            .builder
            .build_load(i64, ptr, "w")
            .unwrap()
            .into_int_value();
        let keep = i64.const_int(!(mask << shift), false);
        let cleared = self.builder.build_and(cur, keep, "clr").unwrap();
        let next = self.builder.build_or(cleared, field, "ins").unwrap();
        self.builder.build_store(ptr, next).unwrap();
    }

    // --- accessors: sx_set / sx_read / sx_reset ---------------------------

    /// Emit the `sx_*` accessor functions that make up the design ABI.
    fn accessors(&self) {
        // The compute type follows the design's widest signal, but the ABI
        // must not: `sx_set`/`sx_read` are declared `u64` on the Rust side.
        // Multi-word values cross the boundary a word at a time through
        // `sx_set_word`/`sx_read_word` instead.
        let i64 = self.ctx.i64_type();
        let i32 = self.ctx.i32_type();
        let void = self.ctx.void_type();

        // void sx_reset(void): signals take their declared initial values
        // (VHDL-style); events clear.
        let f = self
            .module
            .add_function("sx_reset", void.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.ctx.append_basic_block(f, "e"));
        self.builder
            .build_store(self.range_error_ptr(), i32.const_zero())
            .unwrap();
        self.builder
            .build_store(self.range_value_ptr(), self.ctx.i64_type().const_zero())
            .unwrap();
        self.builder
            .build_store(self.range_site_ptr(), i32.const_zero())
            .unwrap();
        self.builder
            .build_store(self.index_error_ptr(), i32.const_zero())
            .unwrap();
        self.builder
            .build_store(self.index_value_ptr(), i64.const_zero())
            .unwrap();
        for id in 0..self.n {
            let signal = &self.design.signals[id as usize];
            let init = self
                .value_ty(self.signal_width(SignalId(id)))
                .const_int_arbitrary_precision(&signal.init);
            self.store("cur", SignalId(id), init);
            self.store("old", SignalId(id), init);
            self.store("event", SignalId(id), i64.const_zero());
        }
        self.builder.build_return(None).unwrap();

        // i32 sx_range_error(void): zero, or one plus the first ranged signal
        // whose pre-truncation value left its declared domain.
        let f = self
            .module
            .add_function("sx_range_error", i32.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.ctx.append_basic_block(f, "e"));
        let error = self
            .builder
            .build_load(i32, self.range_error_ptr(), "range")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&error)).unwrap();

        // i64 sx_range_value(void): the value that broke the domain, as it was
        // before truncation to the destination width.
        let f = self
            .module
            .add_function("sx_range_value", i64.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.ctx.append_basic_block(f, "e"));
        let kept = self
            .builder
            .build_load(i64, self.range_value_ptr(), "rvalue")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&kept)).unwrap();

        // i32 sx_range_site(void): zero, or one plus the index in
        // `Design::range_sites` of the assignment that stored the bad value.
        let f = self
            .module
            .add_function("sx_range_site", i32.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.ctx.append_basic_block(f, "e"));
        let site = self
            .builder
            .build_load(i32, self.range_site_ptr(), "rsite")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&site)).unwrap();

        // i32 sx_index_error(void): zero, or one plus the index in
        // `Design::index_sites` for the first active invalid access.
        let f = self
            .module
            .add_function("sx_index_error", i32.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.ctx.append_basic_block(f, "e"));
        let error = self
            .builder
            .build_load(i32, self.index_error_ptr(), "index")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&error)).unwrap();

        // i64 sx_index_value(void): the offending runtime index.
        let f = self
            .module
            .add_function("sx_index_value", i64.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.ctx.append_basic_block(f, "e"));
        let value = self
            .builder
            .build_load(i64, self.index_value_ptr(), "ivalue")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&value)).unwrap();

        // void sx_set(i32 sig, i64 val): cur[sig] = val  (bounded switch).
        let f = self.module.add_function(
            "sx_set",
            void.fn_type(&[i32.into(), i64.into()], false),
            None,
        );
        let entry = self.ctx.append_basic_block(f, "e");
        self.builder.position_at_end(entry);
        let sig = f.get_nth_param(0).unwrap().into_int_value();
        let val = f.get_nth_param(1).unwrap().into_int_value();
        let done = self.ctx.append_basic_block(f, "done");
        let cases: Vec<_> = (0..self.n)
            .map(|id| {
                let bb = self.ctx.append_basic_block(f, "s");
                (i32.const_int(id as u64, false), bb)
            })
            .collect();
        self.builder.position_at_end(entry);
        self.builder.build_switch(sig, done, &cases).unwrap();
        for (id, (_, bb)) in cases.iter().enumerate() {
            self.builder.position_at_end(*bb);
            self.record_range_value(SignalId(id as u32), val, None, 0);
            // Mask to the signal's width, exactly like the interpreter's
            // `set` — outside writers (runner, native harness, FFI) may hand
            // in a value wider than the signal.
            let w = self.signal_width(SignalId(id as u32));
            let stored = if w > 0 && w < 64 {
                let m = i64.const_int((1u64 << w) - 1, false);
                self.builder.build_and(val, m, "m").unwrap()
            } else {
                val
            };
            self.store("cur", SignalId(id as u32), stored);
            self.builder.build_unconditional_branch(done).unwrap();
        }
        self.builder.position_at_end(done);
        self.builder.build_return(None).unwrap();

        // i64 sx_read(i32 sig).
        let f = self
            .module
            .add_function("sx_read", i64.fn_type(&[i32.into()], false), None);
        let entry = self.ctx.append_basic_block(f, "e");
        self.builder.position_at_end(entry);
        let sig = f.get_nth_param(0).unwrap().into_int_value();
        let ret = self.ctx.append_basic_block(f, "ret");
        let cases: Vec<_> = (0..self.n)
            .map(|id| {
                (
                    i32.const_int(id as u64, false),
                    self.ctx.append_basic_block(f, "r"),
                )
            })
            .collect();
        self.builder.position_at_end(entry);
        self.builder.build_switch(sig, ret, &cases).unwrap();
        // Each case loads and jumps to ret; a phi selects the value.
        let mut incoming: Vec<(IntValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            Vec::new();
        for (id, (_, bb)) in cases.iter().enumerate() {
            self.builder.position_at_end(*bb);
            let v = self.fit(self.load("cur", SignalId(id as u32)), i64);
            incoming.push((v, *bb));
            self.builder.build_unconditional_branch(ret).unwrap();
        }
        self.builder.position_at_end(ret);
        let phi = self.builder.build_phi(i64, "v").unwrap();
        let zero = i64.const_zero();
        // default (unmatched sig) yields 0.
        phi.add_incoming(&[(&zero, entry)]);
        for (v, bb) in &incoming {
            phi.add_incoming(&[(v as &dyn inkwell::values::BasicValue, *bb)]);
        }
        self.builder
            .build_return(Some(&phi.as_basic_value().into_int_value()))
            .unwrap();

        self.word_accessors();
    }

    /// `sx_set_word` / `sx_read_word`: move one machine word of a signal across
    /// the ABI, so a value too wide for `u64` crosses a word at a time.
    ///
    /// Word `k` of a signal is bits `[k*64, (k+1)*64)`. Reading
    /// shifts that field down and truncates; writing clears the field and ORs
    /// the new word in, leaving the other words untouched. Widths within one
    /// word behave exactly like `sx_set`/`sx_read` at word 0.
    fn word_accessors(&self) {
        let i64 = self.ctx.i64_type();
        let i32 = self.ctx.i32_type();
        let void = self.ctx.void_type();
        let bits = super::ABI_WORD_BITS;

        // void sx_set_word(i32 sig, i32 word, i64 val)
        let f = self.module.add_function(
            "sx_set_word",
            void.fn_type(&[i32.into(), i32.into(), i64.into()], false),
            None,
        );
        let entry = self.ctx.append_basic_block(f, "e");
        self.builder.position_at_end(entry);
        let sig = f.get_nth_param(0).unwrap().into_int_value();
        let word = f.get_nth_param(1).unwrap().into_int_value();
        let val = f.get_nth_param(2).unwrap().into_int_value();
        let done = self.ctx.append_basic_block(f, "done");
        let cases: Vec<_> = (0..self.n)
            .map(|id| {
                (
                    i32.const_int(id as u64, false),
                    self.ctx.append_basic_block(f, "s"),
                )
            })
            .collect();
        self.builder.position_at_end(entry);
        self.builder.build_switch(sig, done, &cases).unwrap();
        for (id, (_, bb)) in cases.iter().enumerate() {
            self.builder.position_at_end(*bb);
            let w = self.signal_width(SignalId(id as u32));
            let first_word = self
                .builder
                .build_int_compare(IntPredicate::EQ, word, i32.const_zero(), "word0")
                .unwrap();
            self.record_range_value(SignalId(id as u32), val, Some(first_word), 0);
            let cty = self.value_ty(w);
            // shift = word * ABI_WORD_BITS, in the compute type.
            let shift = self
                .builder
                .build_int_mul(self.fit(word, cty), cty.const_int(bits as u64, false), "sh")
                .unwrap();
            let word_mask = self.fit(i64.const_all_ones(), cty);
            let field = self
                .builder
                .build_left_shift(word_mask, shift, "fm")
                .unwrap();
            let keep = self.builder.build_not(field, "nfm").unwrap();
            let old = self.load("cur", SignalId(id as u32));
            let cleared = self.builder.build_and(old, keep, "cl").unwrap();
            let placed = self
                .builder
                .build_left_shift(self.fit(val, cty), shift, "pl")
                .unwrap();
            let merged = self.builder.build_or(cleared, placed, "mg").unwrap();
            // Keep the signal's own width authoritative, as `sx_set` does.
            let stored = self.mask_to_width(merged, w, cty);
            self.store("cur", SignalId(id as u32), stored);
            self.builder.build_unconditional_branch(done).unwrap();
        }
        self.builder.position_at_end(done);
        self.builder.build_return(None).unwrap();

        // i64 sx_read_word(i32 sig, i32 word)
        let f = self.module.add_function(
            "sx_read_word",
            i64.fn_type(&[i32.into(), i32.into()], false),
            None,
        );
        let entry = self.ctx.append_basic_block(f, "e");
        self.builder.position_at_end(entry);
        let sig = f.get_nth_param(0).unwrap().into_int_value();
        let word = f.get_nth_param(1).unwrap().into_int_value();
        let ret = self.ctx.append_basic_block(f, "ret");
        let cases: Vec<_> = (0..self.n)
            .map(|id| {
                (
                    i32.const_int(id as u64, false),
                    self.ctx.append_basic_block(f, "r"),
                )
            })
            .collect();
        self.builder.position_at_end(entry);
        self.builder.build_switch(sig, ret, &cases).unwrap();
        let mut incoming: Vec<(IntValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            Vec::new();
        for (id, (_, bb)) in cases.iter().enumerate() {
            self.builder.position_at_end(*bb);
            let cty = self.value_ty(self.signal_width(SignalId(id as u32)));
            let shift = self
                .builder
                .build_int_mul(self.fit(word, cty), cty.const_int(bits as u64, false), "sh")
                .unwrap();
            let v = self.load("cur", SignalId(id as u32));
            let down = self
                .builder
                .build_right_shift(v, shift, false, "dn")
                .unwrap();
            incoming.push((self.fit(down, i64), *bb));
            self.builder.build_unconditional_branch(ret).unwrap();
        }
        self.builder.position_at_end(ret);
        let phi = self.builder.build_phi(i64, "v").unwrap();
        let zero = i64.const_zero();
        phi.add_incoming(&[(&zero, entry)]);
        for (v, bb) in &incoming {
            phi.add_incoming(&[(v as &dyn inkwell::values::BasicValue, *bb)]);
        }
        self.builder
            .build_return(Some(&phi.as_basic_value().into_int_value()))
            .unwrap();
    }

    /// Mask `v` to a signal's declared width, in type `ty`. A width equal to
    /// (or wider than) the type needs no mask.
    fn mask_to_width(
        &self,
        v: IntValue<'ctx>,
        width: u32,
        ty: inkwell::types::IntType<'ctx>,
    ) -> IntValue<'ctx> {
        if width == 0 || width >= ty.get_bit_width() {
            return v;
        }
        let ones = ty.const_all_ones();
        let shift = ty.const_int((ty.get_bit_width() - width) as u64, false);
        // (all-ones >> (tybits - width)) is the low-`width` mask at any width.
        let m = self
            .builder
            .build_right_shift(ones, shift, false, "wm")
            .unwrap();
        self.builder.build_and(v, m, "mw").unwrap()
    }

    // --- sx_settle: combinational processes in dependency order -----------

    /// Emit `sx_settle` as a bounded **delta-cycle loop** so internally-generated
    /// edges propagate (derived clocks, clock dividers, ripple counters).
    ///
    /// Each delta: (1) `event[i] = cur[i] != old[i]` — changes since the last
    /// delta — and `snap[i] = cur[i]`; if nothing changed, we're stable and
    /// return. (2) combinational settle; (3+4) event blocks compute next-state
    /// from the *pre-commit* state (so simultaneous updates don't see each
    /// other) and commit; (5) re-settle combinational; (6) advance `old <- snap`
    /// so this delta's changes appear as edges in the *next* delta — and only
    /// then, so each edge fires exactly once. A delta cap bounds the loop
    /// against a zero-delay oscillation.
    fn settle(&self, comb_helpers: &[FunctionValue<'ctx>]) {
        let void = self.ctx.void_type();
        let i64 = self.i64t();
        let i1 = self.ctx.bool_type();
        let f = self
            .module
            .add_function("sx_settle", void.fn_type(&[], false), None);
        let entry = self.ctx.append_basic_block(f, "entry");
        let body = self.ctx.append_basic_block(f, "body");
        let done = self.ctx.append_basic_block(f, "done");

        // entry: a delta counter for the oscillation cap; run the body at least
        // once (so combinational logic always settles even with no events).
        self.builder.position_at_end(entry);
        let dcount = self.builder.build_alloca(i64, "dcount").unwrap();
        self.builder.build_store(dcount, i64.const_zero()).unwrap();
        self.builder.build_unconditional_branch(body).unwrap();

        // body — one delta cycle, looping while it keeps producing changes.
        self.builder.position_at_end(body);
        // 1. combinational settle first, so a comb-driven clock (a port
        // connection, `C.clk <- T.clk`) has its new value in `cur` *before* we
        // detect its edge below.
        self.emit_comb_pass(comb_helpers);
        // 2. event[i] = (cur != old): changes since the previous delta. `snap`
        // captures this delta's (post-comb) values for the `old` advance below.
        let mut any = i1.const_zero();
        for i in 0..self.n {
            let id = SignalId(i);
            let cur = self.load("cur", id);
            let ne = self
                .builder
                .build_int_compare(IntPredicate::NE, cur, self.load("old", id), "ev")
                .unwrap();
            self.store("event", id, self.zext(ne));
            self.store("snap", id, cur);
            any = self.builder.build_or(any, ne, "any").unwrap();
        }
        // 3+4. event blocks: stage guards/values from the pre-commit state (so
        // simultaneous updates don't see each other), then commit.
        let mut staged: Vec<(SignalId, IntValue<'ctx>, IntValue<'ctx>)> = Vec::new();
        for eb in &self.design.event_blocks {
            self.record_index_checks(&eb.condition, None);
            let fired = self.as_i1(&eb.condition);
            for (i, u) in eb.updates.iter().enumerate() {
                // Same reasoning as `emit_comb`: an update that a later
                // unconditional one in this block overwrites never reaches the
                // signal. Both are guarded by this block's `fired`, so wherever
                // the earlier could store, the later stores over it -- and
                // range-checking a value the signal never held reported a
                // failure the design does not have. Only within a block: two
                // blocks have different conditions, and neither subsumes the
                // other.
                let overwritten = eb.updates[i + 1..]
                    .iter()
                    .any(|later| later.target == u.target && later.cond.is_none());
                if overwritten {
                    continue;
                }
                let guard = match &u.cond {
                    Some(c) => {
                        self.record_index_checks(c, Some(fired));
                        self.builder.build_and(fired, self.as_i1(c), "g").unwrap()
                    }
                    None => fired,
                };
                let val = self.emit_target_value(u.target, &u.expr, Some(guard), self.site(u.span));
                staged.push((u.target, guard, val));
            }
        }
        let committed = !staged.is_empty();
        for (target, guard, val) in staged {
            let prev = self.load("cur", target);
            let next = self
                .builder
                .build_select(guard, val, prev, "next")
                .unwrap()
                .into_int_value();
            self.store("cur", target, next);
            self.mark_event(target, prev, next);
        }
        // 5. re-settle combinational after commits.
        if committed {
            self.emit_comb_pass(comb_helpers);
        }
        // 6. advance old <- snap, so changes made *in* this delta appear as
        // edges in the next one — and only then, so each edge fires once.
        for i in 0..self.n {
            let id = SignalId(i);
            self.store("old", id, self.load("snap", id));
        }
        // Loop while this delta had events (there may be more to propagate) and
        // the delta cap — comfortably past any real cascade depth — is not hit.
        let cap = i64.const_int(self.n as u64 + 64, false);
        let dc = self
            .builder
            .build_load(i64, dcount, "dc")
            .unwrap()
            .into_int_value();
        let inc = self
            .builder
            .build_int_add(dc, i64.const_int(1, false), "inc")
            .unwrap();
        self.builder.build_store(dcount, inc).unwrap();
        let under = self
            .builder
            .build_int_compare(IntPredicate::ULT, inc, cap, "under")
            .unwrap();
        let cont = self.builder.build_and(any, under, "cont").unwrap();
        self.builder
            .build_conditional_branch(cont, body, done)
            .unwrap();

        // done: clear event flags and return.
        self.builder.position_at_end(done);
        for i in 0..self.n {
            self.store("event", SignalId(i), self.c(0));
        }
        self.builder.build_return(None).unwrap();
    }

    /// Call one combinational settle pass. The process bodies live in helpers
    /// rather than being duplicated at both call sites in `sx_settle`.
    fn emit_comb_pass(&self, helpers: &[FunctionValue<'ctx>]) {
        for &helper in helpers {
            self.builder.build_call(helper, &[], "").unwrap();
        }
    }

    /// Emit the topologically ordered combinational schedule once, split into
    /// bounded functions. `noinline` is intentional: recreating one giant
    /// `sx_settle` at O2 would restore both the duplicated body and LLVM's
    /// SelectionDAG memory spike.
    fn comb_helpers(&self) -> Vec<FunctionValue<'ctx>> {
        let schedule = self.comb_schedule();
        let noinline_kind = Attribute::get_named_enum_kind_id("noinline");
        debug_assert_ne!(noinline_kind, 0, "LLVM provides the noinline attribute");
        let noinline = self.ctx.create_enum_attribute(noinline_kind, 0);
        let void = self.ctx.void_type();

        schedule
            .chunks(COMB_PROCESSES_PER_HELPER)
            .enumerate()
            .map(|(chunk_index, processes)| {
                let helper = self.module.add_function(
                    &format!("sx_comb_{chunk_index}"),
                    void.fn_type(&[], false),
                    Some(Linkage::Internal),
                );
                helper.add_attribute(AttributeLoc::Function, noinline);
                let entry = self.ctx.append_basic_block(helper, "entry");
                self.builder.position_at_end(entry);
                let previous = self.comb_values.replace(Some(CombValueCache::default()));
                debug_assert!(previous.is_none());
                for process in processes {
                    self.emit_comb(process);
                }
                let emitted = self.comb_values.replace(None);
                debug_assert!(emitted.is_some());
                self.builder.build_return(None).unwrap();
                helper
            })
            .collect()
    }

    /// Emit one assignment value before destination truncation and latch a
    /// dynamic range failure while the offending mathematical value is still
    /// observable. `active` is the driver/update guard.
    fn emit_target_value(
        &self,
        target: SignalId,
        expr: &Expr,
        active: Option<IntValue<'ctx>>,
        site: u32,
    ) -> IntValue<'ctx> {
        self.record_index_checks(expr, active);
        let signal = &self.design.signals[target.0 as usize];
        let width = self.signal_width(target);
        let Some(_) = signal.range else {
            return if signal.integer {
                self.emit_signed_operand_at(expr, width)
            } else {
                self.emit_at(expr, width)
            };
        };
        let check_width = self.expr_width(expr).max(width).max(64);
        let value = self.emit_signed_operand_at(expr, check_width);
        self.record_range_value(target, value, active, site);
        self.fit(value, self.value_ty(width))
    }

    /// Latch checked-index failures along the expression's actual control-flow
    /// path. LLVM `select` evaluates both value operands eagerly in IR, so the
    /// active predicate is narrowed for each arm instead of treating every
    /// syntactically present access as executed.
    fn record_index_checks(&self, expr: &Expr, active: Option<IntValue<'ctx>>) {
        // Std-defined conversions and logic operators contain deeply nested
        // `select`s but normally no dynamic index. Walking those expressions
        // used to emit a complete, dead branch-activity tree for a diagnostic
        // that could never fire. The cheap IR scan avoids creating any LLVM
        // values for such subtrees; checked-index expressions are uncommon and
        // small enough that recursive calls can repeat the predicate safely.
        if !has_checked_index(expr) {
            return;
        }
        let combine = |cx: &Self, outer: Option<IntValue<'ctx>>, inner: IntValue<'ctx>, name| {
            outer
                .map(|outer| cx.builder.build_and(outer, inner, name).unwrap())
                .unwrap_or(inner)
        };
        match expr {
            Expr::CheckedIndex {
                index,
                valid,
                left,
                right,
                span,
            } => {
                // Nested indexing in the index expression is evaluated first.
                self.record_index_checks(index, active);
                let valid = self.as_i1(valid);
                let invalid = self.builder.build_not(valid, "ibad").unwrap();
                let invalid = combine(self, active, invalid, "iactive");
                let site = IndexSite {
                    span: *span,
                    left: *left,
                    right: *right,
                };
                let id = self
                    .index_sites
                    .get(&site)
                    .copied()
                    .expect("checked index site was registered from this design");
                let i32 = self.ctx.i32_type();
                let previous = self
                    .builder
                    .build_load(i32, self.index_error_ptr(), "iprev")
                    .unwrap()
                    .into_int_value();
                let empty = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, previous, i32.const_zero(), "iempty")
                    .unwrap();
                let record = self.builder.build_and(empty, invalid, "irecord").unwrap();
                let next = self
                    .builder
                    .build_select(
                        record,
                        i32.const_int(u64::from(id), false),
                        previous,
                        "inext",
                    )
                    .unwrap()
                    .into_int_value();
                self.builder
                    .build_store(self.index_error_ptr(), next)
                    .unwrap();
                let raw = self.emit_signed_operand_at(index, 64);
                let kept = self
                    .builder
                    .build_load(self.ctx.i64_type(), self.index_value_ptr(), "ivprev")
                    .unwrap()
                    .into_int_value();
                let stored = self
                    .builder
                    .build_select(record, raw, kept, "ivnext")
                    .unwrap()
                    .into_int_value();
                self.builder
                    .build_store(self.index_value_ptr(), stored)
                    .unwrap();
            }
            Expr::Select { cond, then, els } => {
                self.record_index_checks(cond, active);
                let selected = self.as_i1(cond);
                let then_active = combine(self, active, selected, "itaken");
                let not_selected = self.builder.build_not(selected, "inottaken").unwrap();
                let else_active = combine(self, active, not_selected, "ielse");
                self.record_index_checks(then, Some(then_active));
                self.record_index_checks(els, Some(else_active));
            }
            Expr::MetaCmp { inner, .. } => self.record_index_checks(inner, active),
            Expr::CCall { args, .. } => {
                for argument in args {
                    self.record_index_checks(argument, active);
                }
            }
            Expr::Unary { rhs, .. } | Expr::Slice { base: rhs, .. } => {
                self.record_index_checks(rhs, active)
            }
            Expr::TableLookup { index, .. } => self.record_index_checks(index, active),
            // Lowering combines an enclosing source `if` with a dynamic-write
            // target predicate using these nodes. Preserve that source branch
            // boundary for checks even though value emission uses bitwise
            // operations: the indexed write is not executed when its guard is
            // false. `or` is the dual used by nested conditions.
            Expr::Binary {
                op: BinOp::And,
                lhs,
                rhs,
            } => {
                self.record_index_checks(lhs, active);
                let lhs_true = self.as_i1(lhs);
                let rhs_active = combine(self, active, lhs_true, "iand");
                self.record_index_checks(rhs, Some(rhs_active));
            }
            Expr::Binary {
                op: BinOp::Or,
                lhs,
                rhs,
            } => {
                self.record_index_checks(lhs, active);
                let lhs_false = self.builder.build_not(self.as_i1(lhs), "iornot").unwrap();
                let rhs_active = combine(self, active, lhs_false, "ior");
                self.record_index_checks(rhs, Some(rhs_active));
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.record_index_checks(lhs, active);
                self.record_index_checks(rhs, active);
            }
            Expr::Const(_)
            | Expr::WideConst(_)
            | Expr::Real(_)
            | Expr::Logic(_)
            | Expr::Current(_)
            | Expr::Old(_)
            | Expr::Event(_)
            | Expr::Unknown => {}
        }
    }

    /// Latch a ranged-numeric failure: record the offending value and its site,
    /// but only on the control-flow path that actually evaluated the write.
    fn record_range_value(
        &self,
        target: SignalId,
        value: IntValue<'ctx>,
        active: Option<IntValue<'ctx>>,
        site: u32,
    ) {
        let Some((lo, hi)) = self.design.signals[target.0 as usize].range else {
            return;
        };
        let ty = value.get_type();
        let lo = ty.const_int(lo as u64, true);
        let hi = ty.const_int(hi as u64, true);
        let below = self
            .builder
            .build_int_compare(IntPredicate::SLT, value, lo, "rlo")
            .unwrap();
        let above = self
            .builder
            .build_int_compare(IntPredicate::SGT, value, hi, "rhi")
            .unwrap();
        let mut violation = self.builder.build_or(below, above, "rbad").unwrap();
        if let Some(active) = active {
            violation = self
                .builder
                .build_and(active, violation, "ractive")
                .unwrap();
        }
        let i32 = self.ctx.i32_type();
        let previous = self
            .builder
            .build_load(i32, self.range_error_ptr(), "rprev")
            .unwrap()
            .into_int_value();
        let empty = self
            .builder
            .build_int_compare(IntPredicate::EQ, previous, i32.const_zero(), "rempty")
            .unwrap();
        let record = self.builder.build_and(empty, violation, "rrecord").unwrap();
        let id = i32.const_int(u64::from(target.0) + 1, false);
        let next = self
            .builder
            .build_select(record, id, previous, "rnext")
            .unwrap()
            .into_int_value();
        self.builder
            .build_store(self.range_error_ptr(), next)
            .unwrap();
        // The site rides the same `record` predicate as the id above, so a
        // later violation cannot repoint an already-reported failure at its
        // own line.
        let site = i32.const_int(u64::from(site), false);
        let previous_site = self
            .builder
            .build_load(i32, self.range_site_ptr(), "rsprev")
            .unwrap()
            .into_int_value();
        let next_site = self
            .builder
            .build_select(record, site, previous_site, "rsnext")
            .unwrap()
            .into_int_value();
        self.builder
            .build_store(self.range_site_ptr(), next_site)
            .unwrap();
        // Keep the value that actually broke the domain, before `fit` narrows
        // it to the destination width.
        let i64_ty = self.ctx.i64_type();
        let offending = self
            .builder
            .build_int_s_extend_or_bit_cast(value, i64_ty, "roff")
            .unwrap();
        let kept = self
            .builder
            .build_load(i64_ty, self.range_value_ptr(), "rvprev")
            .unwrap()
            .into_int_value();
        let stored = self
            .builder
            .build_select(record, offending, kept, "rvnext")
            .unwrap()
            .into_int_value();
        self.builder
            .build_store(self.range_value_ptr(), stored)
            .unwrap();
    }

    /// `event[target] |= (next != prev)` — a change flags the signal.
    fn mark_event(&self, target: SignalId, prev: IntValue<'ctx>, next: IntValue<'ctx>) {
        let ch = self
            .builder
            .build_int_compare(IntPredicate::NE, next, prev, "ch")
            .unwrap();
        let event = self.load("event", target);
        let changed = self.fit(ch, event.get_type());
        let ev = self.builder.build_or(event, changed, "ev2").unwrap();
        self.store("event", target, ev);
    }

    /// Build the combinational schedule once. Each entry is a target plus its
    /// source-ordered driver indices; the result itself is in dependency order.
    fn comb_schedule(&self) -> Vec<(SignalId, Vec<usize>)> {
        let comb: Vec<_> = self
            .design
            .processes()
            .into_iter()
            .filter_map(|process| match process.kind {
                ProcessKind::Comb { target, drivers } => Some((target, drivers, process.reads)),
                ProcessKind::Event { .. } => None,
            })
            .collect();
        // map: signal -> the comb process (local index) that writes it.
        let mut writer: HashMap<SignalId, usize> = HashMap::new();
        for (index, (target, _, _)) in comb.iter().enumerate() {
            writer.insert(*target, index);
        }
        let m = comb.len();
        let mut deps: Vec<Vec<usize>> = vec![Vec::new(); m];
        let mut indeg = vec![0usize; m];
        for (index, (_, _, reads)) in comb.iter().enumerate() {
            for r in reads {
                if let Some(&w) = writer.get(r) {
                    if w != index {
                        deps[w].push(index);
                        indeg[index] += 1;
                    }
                }
            }
        }
        let mut queue: Vec<usize> = (0..m).filter(|&i| indeg[i] == 0).collect();
        let mut order = Vec::new();
        let mut seen = vec![false; m];
        while let Some(x) = queue.pop() {
            if seen[x] {
                continue;
            }
            seen[x] = true;
            order.push(x);
            for &y in &deps[x] {
                indeg[y] -= 1;
                if indeg[y] == 0 {
                    queue.push(y);
                }
            }
        }
        // Any cyclic remainder in index order.
        for (i, was_seen) in seen.iter().enumerate().take(m) {
            if !was_seen {
                order.push(i);
            }
        }
        let mut processes: Vec<_> = comb
            .into_iter()
            .map(|(target, drivers, _)| Some((target, drivers)))
            .collect();
        order
            .into_iter()
            .map(|index| processes[index].take().expect("schedule index is unique"))
            .collect()
    }

    /// Resolve a combinational target: fold its drivers in source order
    /// (`value = cond ? expr : value`), mask, store to `cur`.
    fn emit_comb(&self, p: &(SignalId, Vec<usize>)) {
        let (target, drivers) = p;
        let prev = self.load("cur", *target);
        let mut val = prev;
        // Everything before the last unconditional driver is selected away by
        // it (spec 3.14: later drivers override within a context), so the
        // value is the same whether or not they are emitted -- but emitting
        // them also ran their range check, and a value that never reached the
        // signal was reported as having left its domain. `t = a + 5; t = 2;`
        // failed with "`t` left its range 0..10 (it was 13)" while `t` held 2,
        // pointing at the line W-P014 had just called dead.
        let live = drivers
            .iter()
            .rposition(|&di| self.design.drivers[di].cond.is_none())
            .unwrap_or(0);
        for &di in &drivers[live..] {
            let d = &self.design.drivers[di];
            let cond = d.cond.as_ref().map(|condition| {
                self.record_index_checks(condition, None);
                self.as_i1(condition)
            });
            let e = self.emit_target_value(*target, &d.expr, cond, self.site(d.span));
            val = match cond {
                Some(cond) => self
                    .builder
                    .build_select(cond, e, val, "drv")
                    .unwrap()
                    .into_int_value(),
                None => e,
            };
        }
        let w = self.signal_width(*target);
        let masked = self.fit(val, self.value_ty(w));
        self.store("cur", *target, masked);
        self.mark_event(*target, prev, masked);
    }

    // --- expressions ------------------------------------------------------

    /// A constant at the ABI word width.
    fn c(&self, v: u64) -> IntValue<'ctx> {
        self.c_at(v, 64)
    }

    /// A constant at an explicit width.
    fn c_at(&self, v: u64, width: u32) -> IntValue<'ctx> {
        self.value_ty(width).const_int(v, false)
    }

    /// Truncate or extend to a logical value width.
    fn mask(&self, v: IntValue<'ctx>, width: u32) -> IntValue<'ctx> {
        self.fit(v, self.value_ty(width))
    }

    /// Natural width of an IR expression. Constants take their minimum useful
    /// width and acquire a wider contextual width from their enclosing
    /// operation or assignment.
    fn expr_width(&self, e: &Expr) -> u32 {
        match e {
            // Resolved away before code generation; `validate` rejects any that
            // survive. Falling through to the inner comparison keeps this total
            // rather than panicking on a shape that should not be here.
            Expr::MetaCmp { inner, .. } => self.expr_width(inner),
            Expr::Const(v) => (64 - v.leading_zeros()).max(1),
            Expr::WideConst(words) => {
                let high = words.last().copied().unwrap_or(0);
                ((words.len().saturating_sub(1) as u32) * 64 + (64 - high.leading_zeros())).max(1)
            }
            Expr::Real(_) | Expr::CCall { .. } => 64,
            Expr::Logic(_) => 1,
            Expr::Current(id) | Expr::Old(id) => self.signal_width(*id).max(1),
            Expr::Event(_) => 1,
            Expr::Unary { rhs, .. } => self.expr_width(rhs),
            Expr::Binary { op, lhs, rhs } => {
                if matches!(
                    op,
                    BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::SLt
                        | BinOp::SLe
                        | BinOp::SGt
                        | BinOp::SGe
                        | BinOp::FEq
                        | BinOp::FNe
                        | BinOp::FLt
                        | BinOp::FLe
                        | BinOp::FGt
                        | BinOp::FGe
                ) {
                    1
                } else if matches!(op, BinOp::Shl) {
                    let lhs_width = self.expr_width(lhs);
                    match rhs.as_ref() {
                        Expr::Const(shift) => {
                            lhs_width.saturating_add((*shift).try_into().unwrap_or(u32::MAX))
                        }
                        _ => lhs_width,
                    }
                } else {
                    self.expr_width(lhs).max(self.expr_width(rhs))
                }
            }
            Expr::Slice { hi, lo, .. } => hi - lo + 1,
            Expr::TableLookup { table, .. } => self.design.lookup_tables[table.0].element_width,
            Expr::CheckedIndex { index, .. } => self.expr_width(index),
            Expr::Select { then, els, .. } => self.expr_width(then).max(self.expr_width(els)),
            Expr::Unknown => 1,
        }
    }

    /// Evaluate a condition to an `i1` (nonzero).
    fn as_i1(&self, e: &Expr) -> IntValue<'ctx> {
        let v = self.emit(e);
        if v.get_type().get_bit_width() == 1 {
            return v;
        }
        self.int_compare(IntPredicate::NE, v, v.get_type().const_zero(), "nz")
    }

    /// zext an `i1` back to the i64 word domain.
    fn zext(&self, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder
            .build_int_z_extend(b, self.i64t(), "z")
            .unwrap()
    }

    /// Emit an expression at its own natural width.
    fn emit(&self, e: &Expr) -> IntValue<'ctx> {
        self.emit_at(e, self.expr_width(e))
    }

    /// Emit an expression at `width`, extending or truncating as needed.
    fn emit_at(&self, e: &Expr, width: u32) -> IntValue<'ctx> {
        match e {
            Expr::MetaCmp { inner, .. } => self.emit_at(inner, width),
            Expr::Const(v) => self.c_at(*v, width),
            Expr::WideConst(words) => self.value_ty(width).const_int_arbitrary_precision(words),
            Expr::Real(x) => self.c_at(x.to_bits(), 64),
            // IR lowering resolves every logic literal to a `Const` (its
            // position in std's logic type), so none reach the backend.
            Expr::Logic(ch) => unreachable!("unresolved logic literal '{ch}' reached the backend"),
            Expr::Current(id) => self.mask(self.load("cur", *id), width),
            Expr::Old(id) => self.mask(self.load("old", *id), width),
            Expr::Event(id) => self.mask(self.load("event", *id), width),
            Expr::Unary { op, rhs } => {
                let a = self.emit_at(rhs, width);
                match op {
                    UnOp::Not => {
                        let z =
                            self.int_compare(IntPredicate::EQ, a, a.get_type().const_zero(), "not");
                        self.fit(z, self.value_ty(width))
                    }
                    UnOp::Neg => self.builder.build_int_neg(a, "neg").unwrap(),
                    // The operand carries f64 bits; take the number it denotes,
                    // truncated toward zero, and put it back in a word.
                    UnOp::RealToInt => {
                        let f = self
                            .builder
                            .build_bit_cast(
                                self.fit(a, self.ctx.i64_type()),
                                self.ctx.f64_type(),
                                "rbits",
                            )
                            .unwrap()
                            .into_float_value();
                        let i = self
                            .builder
                            .build_float_to_signed_int(f, self.ctx.i64_type(), "rtoi")
                            .unwrap();
                        // Signed widening: this is a *number*, not a bit
                        // pattern. Zero-extending it lost the sign whenever
                        // the consumer asked for more than 64 bits — which a
                        // signed comparison always does, since it widens its
                        // operands by one before comparing. `integer(r) < 0`
                        // was therefore false for every negative `r`, while
                        // the same value assigned to a signal was correct.
                        self.fit_signed(i, self.value_ty(width))
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let operation_width = width.max(self.expr_width(e));
                let value = self.emit_binary(*op, lhs, rhs, operation_width);
                self.mask(value, width)
            }
            Expr::Slice { base, hi, lo } => {
                let direct = match base.as_ref() {
                    Expr::Current(id) => Some(StateSliceKey {
                        array: Self::state_array_key("cur"),
                        signal: *id,
                        hi: *hi,
                        lo: *lo,
                    }),
                    Expr::Old(id) => Some(StateSliceKey {
                        array: Self::state_array_key("old"),
                        signal: *id,
                        hi: *hi,
                        lo: *lo,
                    }),
                    Expr::Event(id) => Some(StateSliceKey {
                        array: Self::state_array_key("event"),
                        signal: *id,
                        hi: *hi,
                        lo: *lo,
                    }),
                    _ => None,
                };
                if let Some(value) = direct.and_then(|key| self.cached_slice(key)) {
                    return self.mask(value, width);
                }
                let base_width = self.expr_width(base).max(*hi + 1);
                let b = self.emit_at(base, base_width);
                let sh = if *lo == 0 {
                    b
                } else {
                    self.builder
                        .build_right_shift(b, self.c_at(*lo as u64, base_width), false, "sh")
                        .unwrap()
                };
                let slice_width = hi - lo + 1;
                let sliced = self.mask(sh, slice_width);
                if let Some(key) = direct {
                    self.remember_slice(key, sliced);
                }
                self.mask(sliced, width)
            }
            Expr::TableLookup { table, index } => {
                let metadata = &self.design.lookup_tables[table.0];
                let count = metadata.values.len() as u64;
                let count_width = (64 - count.leading_zeros()).max(1);
                let index_width = self.expr_width(index).max(count_width);
                let index = self.emit_at(index, index_width);
                let in_range = self.int_compare(
                    IntPredicate::ULT,
                    index,
                    self.c_at(count, index_width),
                    "lut.in_range",
                );
                // Select a known-valid address before the GEP. Truncating an
                // arbitrary-width index first could wrap an out-of-range value
                // back into the table; indexing with it directly would make an
                // inbounds GEP poison even when the result is later discarded.
                let safe_index =
                    self.int_select(in_range, index, index.get_type().const_zero(), "lut.safe");
                let safe_index = self.fit(safe_index, self.ctx.i64_type());
                let storage_width = Self::lookup_storage_width(metadata.element_width);
                let storage = self.value_ty(storage_width);
                let array = storage.array_type(metadata.values.len() as u32);
                let global = self
                    .module
                    .get_global(&format!("sx.lookup.{}", table.0))
                    .expect("lookup global was emitted");
                let pointer = unsafe {
                    self.builder
                        .build_in_bounds_gep(
                            array,
                            global.as_pointer_value(),
                            &[self.ctx.i64_type().const_zero(), safe_index],
                            "lut.ptr",
                        )
                        .unwrap()
                };
                let loaded = self
                    .builder
                    .build_load(storage, pointer, "lut.value")
                    .unwrap()
                    .into_int_value();
                let value = self.int_select(in_range, loaded, storage.const_zero(), "lut.result");
                self.mask(value, width)
            }
            // Bounds are recorded by `record_index_checks` with the caller's
            // active control-flow predicate. Value emission stays pure so an
            // eagerly emitted, unselected LLVM branch cannot raise an error.
            Expr::CheckedIndex { index, .. } => self.emit_at(index, width),
            Expr::Select { cond, then, els } => {
                let c = self.as_i1(cond);
                let t = self.emit_at(then, width);
                let e = self.emit_at(els, width);
                self.int_select(c, t, e, "sel")
            }
            Expr::CCall {
                name,
                args,
                f64_args,
                integer_args,
                f64_ret,
                integer_ret,
            } => {
                // Foreign C call: `real` params are doubles (bit-cast from the
                // word), everything else i64. Native linking resolves symbols.
                use inkwell::types::BasicMetadataTypeEnum as MT;
                use inkwell::values::BasicMetadataValueEnum as MV;
                let f64t = self.ctx.f64_type();
                let mut ptypes: Vec<MT> = Vec::new();
                let mut vals: Vec<MV> = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    let v = if integer_args.get(i).copied().unwrap_or(false) {
                        self.emit_signed_operand_at(a, 64)
                    } else {
                        self.emit_at(a, 64)
                    };
                    if f64_args.get(i).copied().unwrap_or(false) {
                        ptypes.push(f64t.into());
                        vals.push(
                            self.builder
                                .build_bit_cast(v, f64t, "farg")
                                .unwrap()
                                .into_float_value()
                                .into(),
                        );
                    } else {
                        ptypes.push(self.i64t().into());
                        vals.push(v.into());
                    }
                }
                let f = self.module.get_function(name).unwrap_or_else(|| {
                    let fnty = if *f64_ret {
                        f64t.fn_type(&ptypes, false)
                    } else {
                        self.i64t().fn_type(&ptypes, false)
                    };
                    self.module
                        .add_function(name, fnty, Some(inkwell::module::Linkage::External))
                });
                let r = match self
                    .builder
                    .build_call(f, &vals, "ccall")
                    .unwrap()
                    .try_as_basic_value()
                {
                    inkwell::values::ValueKind::Basic(v) => v,
                    _ => panic!("extern fn returns a value"),
                };
                // Foreign code may call the public state accessors. Preserve
                // expression evaluation order, but force every later state
                // read in this helper to observe any such mutation.
                self.clear_comb_cache();
                let raw = if *f64_ret {
                    self.builder
                        .build_bit_cast(r.into_float_value(), self.i64t(), "fbits")
                        .unwrap()
                        .into_int_value()
                } else {
                    r.into_int_value()
                };
                if *integer_ret {
                    self.fit_signed(raw, self.value_ty(width))
                } else {
                    self.fit(raw, self.value_ty(width))
                }
            }
            Expr::Unknown => self.c_at(0, width),
        }
    }

    /// Emit a signed operand at a common operation width. Stored constrained
    /// integers must be sign-extended; literals and compound expressions are
    /// emitted directly in the contextual width so positive constants do not
    /// accidentally acquire a sign from their minimum unsigned bit width.
    fn emit_signed_operand_at(&self, e: &Expr, width: u32) -> IntValue<'ctx> {
        match e {
            Expr::Current(id) | Expr::Old(id) => {
                let signal = &self.design.signals[id.0 as usize];
                let natural = self.signal_width(*id).max(1);
                let value = self.emit_at(e, natural);
                let negative_capable =
                    signal.integer && signal.range.map(|(lo, _)| lo < 0).unwrap_or(true);
                if negative_capable && natural < width {
                    self.builder
                        .build_int_s_extend(value, self.value_ty(width), "sext")
                        .unwrap()
                } else {
                    self.fit(value, self.value_ty(width))
                }
            }
            Expr::Unary { op: UnOp::Neg, rhs } => {
                let rhs = self.emit_signed_operand_at(rhs, width);
                self.builder.build_int_neg(rhs, "sneg").unwrap()
            }
            Expr::Select { cond, then, els } => {
                let cond = self.as_i1(cond);
                let then = self.emit_signed_operand_at(then, width);
                let els = self.emit_signed_operand_at(els, width);
                self.int_select(cond, then, els, "ssel")
            }
            _ => self.emit_at(e, width),
        }
    }

    /// Emit a binary operation, selecting the unsigned, signed or float
    /// instruction from the IR operator.
    fn emit_binary(&self, op: BinOp, lhs: &Expr, rhs: &Expr, result_width: u32) -> IntValue<'ctx> {
        // Float ops reinterpret the i64 words as f64.
        if matches!(op, BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv) {
            let f = self.ctx.f64_type();
            let a = self
                .builder
                .build_bit_cast(self.emit_at(lhs, 64), f, "fa")
                .unwrap()
                .into_float_value();
            let b = self
                .builder
                .build_bit_cast(self.emit_at(rhs, 64), f, "fb")
                .unwrap()
                .into_float_value();
            let r = match op {
                BinOp::FAdd => self.builder.build_float_add(a, b, "fadd").unwrap(),
                BinOp::FSub => self.builder.build_float_sub(a, b, "fsub").unwrap(),
                BinOp::FMul => self.builder.build_float_mul(a, b, "fmul").unwrap(),
                _ => self.builder.build_float_div(a, b, "fdiv").unwrap(),
            };
            return self
                .builder
                .build_bit_cast(r, self.value_ty(64), "fbits")
                .unwrap()
                .into_int_value();
        }
        // Float comparison: reinterpret the words as f64 and compare with
        // ordered predicates (NaN -> false, except `!=`), yielding a 0/1 word.
        if matches!(
            op,
            BinOp::FEq | BinOp::FNe | BinOp::FLt | BinOp::FLe | BinOp::FGt | BinOp::FGe
        ) {
            let f = self.ctx.f64_type();
            let a = self
                .builder
                .build_bit_cast(self.emit_at(lhs, 64), f, "fa")
                .unwrap()
                .into_float_value();
            let b = self
                .builder
                .build_bit_cast(self.emit_at(rhs, 64), f, "fb")
                .unwrap()
                .into_float_value();
            let p = match op {
                BinOp::FEq => FloatPredicate::OEQ,
                BinOp::FNe => FloatPredicate::UNE,
                BinOp::FLt => FloatPredicate::OLT,
                BinOp::FLe => FloatPredicate::OLE,
                BinOp::FGt => FloatPredicate::OGT,
                _ => FloatPredicate::OGE,
            };
            let c = self.builder.build_float_compare(p, a, b, "fcmp").unwrap();
            return self.fit(c, self.value_ty(result_width));
        }

        let signed_comparison = matches!(op, BinOp::SLt | BinOp::SLe | BinOp::SGt | BinOp::SGe);
        let signed = signed_comparison
            || matches!(
                op,
                BinOp::SAdd | BinOp::SSub | BinOp::SMul | BinOp::SDiv | BinOp::AShr
            );
        let comparison = matches!(
            op,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::SLt
                | BinOp::SLe
                | BinOp::SGt
                | BinOp::SGe
        );
        let operand_width = if comparison {
            self.expr_width(lhs).max(self.expr_width(rhs))
        } else {
            result_width
        }
        .max(1);
        // A nonnegative constrained integer may use every storage bit for
        // magnitude (`integer<0..3>` is i2). Kernel operations still use a
        // signed mathematical domain, so add a guard bit and either zero- or
        // sign-extend each operand according to its declared range.
        let operand_width = if signed {
            operand_width.saturating_add(1)
        } else {
            operand_width
        };
        if matches!(op, BinOp::Shl | BinOp::Shr | BinOp::AShr) {
            // LLVM shifts are poison when the count is at least the operation
            // width. Hardware and the native harness define those cases as
            // zero, so compare at a width that preserves the entire count,
            // substitute a safe zero count, then select the defined result.
            let shift_width = operand_width.max(self.expr_width(rhs)).max(1);
            let a = if matches!(op, BinOp::AShr) {
                self.emit_signed_operand_at(lhs, shift_width)
            } else {
                self.emit_at(lhs, shift_width)
            };
            let b = self.emit_at(rhs, shift_width);
            let limit = self.c_at(operand_width as u64, shift_width);
            let out_of_range = self
                .builder
                .build_int_compare(IntPredicate::UGE, b, limit, "shoob")
                .unwrap();
            let zero = self.c_at(0, shift_width);
            let safe = self
                .builder
                .build_select(out_of_range, zero, b, "shamt")
                .unwrap()
                .into_int_value();
            let shifted = if matches!(op, BinOp::Shl) {
                self.builder.build_left_shift(a, safe, "shl").unwrap()
            } else {
                self.builder
                    .build_right_shift(a, safe, matches!(op, BinOp::AShr), "shr")
                    .unwrap()
            };
            let out_of_range_value = if matches!(op, BinOp::AShr) {
                let negative = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, a, a.get_type().const_zero(), "shneg")
                    .unwrap();
                self.builder
                    .build_select(negative, a.get_type().const_all_ones(), zero, "shfill")
                    .unwrap()
                    .into_int_value()
            } else {
                zero
            };
            return self
                .builder
                .build_select(out_of_range, out_of_range_value, shifted, "shzero")
                .unwrap()
                .into_int_value();
        }
        let a = if signed {
            self.emit_signed_operand_at(lhs, operand_width)
        } else {
            self.emit_at(lhs, operand_width)
        };
        let b = if signed {
            self.emit_signed_operand_at(rhs, operand_width)
        } else {
            self.emit_at(rhs, operand_width)
        };
        let cmp = |p: IntPredicate, s: &str| {
            let c = self.int_compare(p, a, b, s);
            self.fit(c, self.value_ty(result_width))
        };
        match op {
            BinOp::Add => self.int_binary(CachedIntOp::Add, a, b, "add"),
            BinOp::Sub => self.int_binary(CachedIntOp::Sub, a, b, "sub"),
            BinOp::Mul => self.int_binary(CachedIntOp::Mul, a, b, "mul"),
            BinOp::SAdd => self.int_binary(CachedIntOp::Add, a, b, "sadd"),
            BinOp::SSub => self.int_binary(CachedIntOp::Sub, a, b, "ssub"),
            BinOp::SMul => self.int_binary(CachedIntOp::Mul, a, b, "smul"),
            BinOp::Div => {
                // Match the interpreter: divide-by-zero yields 0 (B0 formalizes).
                let zero = self.c_at(0, operand_width);
                let one = self.c_at(1, operand_width);
                let is0 = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, b, zero, "d0")
                    .unwrap();
                let safe = self
                    .builder
                    .build_select(is0, one, b, "den")
                    .unwrap()
                    .into_int_value();
                let q = self.builder.build_int_unsigned_div(a, safe, "div").unwrap();
                self.builder
                    .build_select(is0, zero, q, "divz")
                    .unwrap()
                    .into_int_value()
            }
            BinOp::SDiv => {
                // LLVM's `sdiv` is poison for both division by zero and
                // MIN/-1. Kernel integer arithmetic is total in simulation:
                // zero yields zero and overflow wraps to MIN.
                let zero = self.c_at(0, operand_width);
                let one = self.c_at(1, operand_width);
                let neg_one = self.value_ty(operand_width).const_all_ones();
                let min = self
                    .builder
                    .build_left_shift(
                        one,
                        self.c_at((operand_width - 1) as u64, operand_width),
                        "sdminv",
                    )
                    .unwrap();
                let is0 = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, b, zero, "sd0")
                    .unwrap();
                let is_min = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, a, min, "sdmin")
                    .unwrap();
                let is_neg_one = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, b, neg_one, "sdneg1")
                    .unwrap();
                let overflow = self.builder.build_and(is_min, is_neg_one, "sdov").unwrap();
                let unsafe_divisor = self.builder.build_or(is0, overflow, "sdbad").unwrap();
                let safe = self
                    .builder
                    .build_select(unsafe_divisor, one, b, "sden")
                    .unwrap()
                    .into_int_value();
                let q = self.builder.build_int_signed_div(a, safe, "sdiv").unwrap();
                let q_or_min = self
                    .builder
                    .build_select(overflow, min, q, "sdivov")
                    .unwrap()
                    .into_int_value();
                self.builder
                    .build_select(is0, zero, q_or_min, "sdivz")
                    .unwrap()
                    .into_int_value()
            }
            BinOp::Shl | BinOp::Shr | BinOp::AShr => unreachable!("shifts return above"),
            // Core logical operators; for boolean 0/1 operands these match
            // their scalar reading, and vectors apply them per bit.
            // operands this matches the logical reading.
            BinOp::And => self.int_binary(CachedIntOp::And, a, b, "and"),
            BinOp::Or => self.int_binary(CachedIntOp::Or, a, b, "or"),
            BinOp::Xor => self.int_binary(CachedIntOp::Xor, a, b, "xor"),
            BinOp::Eq => cmp(IntPredicate::EQ, "eq"),
            BinOp::Ne => cmp(IntPredicate::NE, "ne"),
            BinOp::Lt => cmp(IntPredicate::ULT, "lt"),
            BinOp::Le => cmp(IntPredicate::ULE, "le"),
            BinOp::Gt => cmp(IntPredicate::UGT, "gt"),
            BinOp::Ge => cmp(IntPredicate::UGE, "ge"),
            BinOp::SLt => cmp(IntPredicate::SLT, "slt"),
            BinOp::SLe => cmp(IntPredicate::SLE, "sle"),
            BinOp::SGt => cmp(IntPredicate::SGT, "sgt"),
            BinOp::SGe => cmp(IntPredicate::SGE, "sge"),
            BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv => unreachable!(),
            BinOp::FEq | BinOp::FNe | BinOp::FLt | BinOp::FLe | BinOp::FGt | BinOp::FGe => {
                unreachable!()
            }
        }
    }
}

#[cfg(all(test, not(feature = "bitpack")))]
mod tests {
    use super::*;
    use siox::ir::{Design, Driver, EventBlock, LookupTable, LookupTableId, NextUpdate, Signal};

    /// A minimal test signal: a plain bit vector of `width` at `path`.
    fn sig(path: &str, width: u32) -> Signal {
        Signal {
            path: path.into(),
            declaration_span: siox::diag::Span::new(siox::diag::FileId(0), 0..0),
            width,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        }
    }

    #[test]
    /// A combinational adder emits the expected instructions.
    fn emits_combinational_adder() {
        // y (id 2) = a (0) + b (1), width 8.
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.b", 8), sig("E.y", 8)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(2),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(1))),
                },
                meta: None,
            }],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        // State layout, accessors, settle, and the add+mask are present. The
        // state is a width-packed struct: three 8-bit signals -> three `i8`s.
        assert!(ll.contains("%sx.state = type <{ i8, i8, i8 }>"), "{ll}");
        assert!(
            ll.contains("@cur = internal global %sx.state zeroinitializer"),
            "{ll}"
        );
        assert!(
            !ll.contains("getelementptr inbounds <{"),
            "state GEPs repeated the anonymous layout:\n{ll}"
        );
        assert!(ll.contains("define void @sx_settle()"), "{ll}");
        assert!(ll.contains("define internal void @sx_comb_0()"), "{ll}");
        assert!(ll.contains("call void @sx_comb_0()"), "{ll}");
        assert!(ll.contains("noinline"), "{ll}");
        assert!(ll.contains("define void @sx_set(i32"), "{ll}");
        assert!(ll.contains("define i64 @sx_read(i32"), "{ll}");
        assert!(ll.contains("add i64"), "{ll}");
        assert!(
            ll.contains("and i64") && ll.contains("255"),
            "mask to width 8:\n{ll}"
        );
    }

    #[test]
    /// A packed logic table emits as a compact constant lookup rather than an
    /// unrolled shift chain.
    fn emits_compact_constant_lookup_table() {
        let design = Design {
            signals: vec![sig("E.index", 8), sig("E.value", 4)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: None,
                expr: Expr::TableLookup {
                    table: LookupTableId(0),
                    index: Box::new(Expr::Current(SignalId(0))),
                },
                meta: None,
            }],
            lookup_tables: vec![LookupTable {
                element_width: 4,
                values: vec![3, 5, 7],
            }],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        assert!(
            ll.contains("@sx.lookup.0 = internal constant [3 x i8]"),
            "{ll}"
        );
        assert!(ll.contains("getelementptr inbounds [3 x i8]"), "{ll}");
        assert!(ll.contains("load i8"), "{ll}");
        assert!(ll.contains("icmp ult i8"), "bounds check missing:\n{ll}");
        assert!(
            !ll.contains("lshr i322"),
            "lookup became the old packed wide shift:\n{ll}"
        );
    }

    #[test]
    /// A helper reuses a state load and invalidates it after a write.
    fn combinational_helpers_reuse_and_invalidate_state_loads() {
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.y", 8)],
            drivers: vec![
                Driver {
                    span: None,
                    ctx: 0,
                    target: SignalId(0),
                    cond: None,
                    expr: Expr::Const(7),
                    meta: None,
                },
                Driver {
                    span: None,
                    ctx: 0,
                    target: SignalId(1),
                    cond: None,
                    expr: Expr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(Expr::Current(SignalId(0))),
                        rhs: Box::new(Expr::Current(SignalId(0))),
                    },
                    meta: None,
                },
            ],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        let helper = ll
            .split("define internal void @sx_comb_0")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("one combinational helper");
        assert_eq!(
            helper.matches("load i8, ptr @cur").count(),
            2,
            "the first load is `a`'s old value; its store must invalidate that load, and the two later reads must share one fresh value:\n{helper}"
        );
    }

    #[test]
    /// Identity conditions and full-width slices emit nothing, rather than a
    /// no-op instruction.
    fn codegen_skips_identity_condition_and_slice_operations() {
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.y", 8)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: Some(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Const(1)),
                }),
                expr: Expr::Slice {
                    base: Box::new(Expr::Current(SignalId(0))),
                    hi: 7,
                    lo: 0,
                },
                meta: None,
            }],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        let helper = ll
            .split("define internal void @sx_comb_0")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("one combinational helper");
        assert!(
            !helper.contains("icmp ne i1"),
            "an i1 condition was redundantly compared with zero:\n{helper}"
        );
        assert!(
            !helper.contains("lshr i8"),
            "a full-width slice emitted a right shift by zero:\n{helper}"
        );
    }

    #[test]
    /// A helper reuses an identical direct state slice.
    fn combinational_helpers_reuse_direct_state_slices() {
        let nibble = || Expr::Slice {
            base: Box::new(Expr::Current(SignalId(0))),
            hi: 7,
            lo: 4,
        };
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.y", 4)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(nibble()),
                    rhs: Box::new(nibble()),
                },
                meta: None,
            }],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        let helper = ll
            .split("define internal void @sx_comb_0")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("one combinational helper");
        assert_eq!(
            helper.matches("lshr i8").count(),
            1,
            "the same direct signal slice was extracted more than once:\n{helper}"
        );
    }

    #[test]
    /// A helper reuses an identical integer comparison.
    fn combinational_helpers_reuse_integer_comparisons() {
        let equals_zero = || Expr::Binary {
            op: BinOp::Eq,
            lhs: Box::new(Expr::Slice {
                base: Box::new(Expr::Current(SignalId(0))),
                hi: 7,
                lo: 4,
            }),
            rhs: Box::new(Expr::Const(0)),
        };
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.y", 1)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::And,
                    lhs: Box::new(equals_zero()),
                    rhs: Box::new(equals_zero()),
                },
                meta: None,
            }],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        let helper = ll
            .split("define internal void @sx_comb_0")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("one combinational helper");
        assert_eq!(
            helper.matches("icmp eq i4").count(),
            1,
            "the same comparison was emitted more than once:\n{helper}"
        );
    }

    #[test]
    /// A helper reuses an identical pure integer operation, in either operand
    /// order for a commutative one.
    fn combinational_helpers_reuse_pure_integer_operations() {
        let increment = || Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(Expr::Current(SignalId(0))),
            rhs: Box::new(Expr::Const(1)),
        };
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.y", 8)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(increment()),
                    rhs: Box::new(increment()),
                },
                meta: None,
            }],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        let helper = ll
            .split("define internal void @sx_comb_0")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("one combinational helper");
        assert_eq!(
            helper.matches("add i8").count(),
            1,
            "the same pure integer operation was emitted more than once:\n{helper}"
        );
    }

    #[test]
    /// A helper reuses an identical select.
    fn combinational_helpers_reuse_selects() {
        let choose = || Expr::Select {
            cond: Box::new(Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(Expr::Current(SignalId(0))),
                rhs: Box::new(Expr::Const(0)),
            }),
            then: Box::new(Expr::Const(3)),
            els: Box::new(Expr::Const(5)),
        };
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.y", 8)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(choose()),
                    rhs: Box::new(choose()),
                },
                meta: None,
            }],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        let helper = ll
            .split("define internal void @sx_comb_0")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("one combinational helper");
        assert_eq!(
            helper.matches("select i1").count(),
            1,
            "the same select was emitted more than once:\n{helper}"
        );
        assert!(
            !helper.contains("itaken")
                && !helper.contains("inottaken")
                && !helper.contains("ielse"),
            "index-diagnostic guards were emitted for an expression without a checked index:\n{helper}"
        );
    }

    #[test]
    /// A helper reuses an identical integer cast.
    fn combinational_helpers_reuse_integer_casts() {
        let design = Design {
            signals: vec![sig("E.a", 4), sig("E.y", 8)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(0))),
                },
                meta: None,
            }],
            ..Design::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        let helper = ll
            .split("define internal void @sx_comb_0")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("one combinational helper");
        assert_eq!(
            helper.matches("zext i4").count(),
            1,
            "the same integer cast was emitted more than once:\n{helper}"
        );
    }

    #[test]
    /// Combinational codegen is split into bounded `noinline` helpers and each
    /// is emitted once, so SelectionDAG never sees one very large function.
    fn bounds_and_reuses_combinational_helpers() {
        let process_count = COMB_PROCESSES_PER_HELPER + 1;
        let mut signals = vec![sig("E.input", 8)];
        let mut drivers = Vec::new();
        for index in 0..process_count {
            let target = SignalId((index + 1) as u32);
            signals.push(sig(&format!("E.y{index}"), 8));
            drivers.push(Driver {
                span: None,
                ctx: 0,
                target,
                cond: None,
                expr: Expr::Current(SignalId(0)),
                meta: None,
            });
        }
        // Any event update makes settle emit its post-commit combinational
        // pass, so both sites must call the same helpers rather than owning
        // duplicate process bodies.
        let event_blocks = vec![EventBlock {
            condition: Expr::Const(1),
            updates: vec![NextUpdate {
                target: SignalId(0),
                cond: None,
                expr: Expr::Current(SignalId(0)),
                meta: None,
                span: None,
            }],
            ctx: 1,
        }];
        let design = Design {
            signals,
            drivers,
            event_blocks,
            ..Default::default()
        };

        let ll = emit_module_ir(&design).unwrap();
        assert_eq!(
            ll.matches("define internal void @sx_comb_").count(),
            2,
            "{process_count} processes should form helpers of \
             {COMB_PROCESSES_PER_HELPER} and 1:\n{ll}"
        );
        assert!(
            ll.contains("noinline"),
            "helpers may not be re-inlined:\n{ll}"
        );
        let settle = ll.split("@sx_settle()").nth(1).expect("settle body");
        for helper in ["sx_comb_0", "sx_comb_1"] {
            assert_eq!(
                settle.matches(&format!("call void @{helper}()")).count(),
                2,
                "both settle sites should reuse {helper}:\n{settle}"
            );
        }
    }

    #[test]
    /// The ABI accepts arbitrarily many words; there is no width ceiling.
    fn accepts_arbitrarily_many_abi_words() {
        let design = Design {
            signals: vec![sig("E.a", 512)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(0),
                cond: None,
                expr: Expr::Const(1),
                meta: None,
            }],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(ll.contains("i512"), "{ll}");
        assert_eq!(crate::llvm::words_for(512), 8);
    }

    #[test]
    /// Storage keeps a wide signal's exact width rather than rounding it.
    fn storage_keeps_exact_wide_signal_width() {
        let design = Design {
            signals: vec![sig("E.value", 65)],
            drivers: vec![],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(
            ll.contains("%sx.state = type <{ i65 }>"),
            "65-bit storage was rounded to an unrelated width:\n{ll}"
        );
    }

    #[test]
    /// A width LLVM cannot represent is an error, not a panic.
    fn unsupported_llvm_width_is_an_error_not_a_panic() {
        let design = Design {
            signals: vec![sig("E.enormous", LLVM_MAX_INT_BITS + 1)],
            drivers: vec![],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let error = emit_module_ir(&design).unwrap_err();
        assert!(error.contains("E.enormous"), "{error}");
        assert!(error.contains(&LLVM_MAX_INT_BITS.to_string()), "{error}");
    }

    #[test]
    /// Constants wider than one word emit correctly.
    fn emits_constants_wider_than_one_word() {
        let design = Design {
            signals: vec![sig("E.y", 192)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(0),
                cond: None,
                expr: Expr::WideConst(vec![1, 2, 3]),
                meta: None,
            }],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(
            ll.contains("1020847100762815390427017310442723737601"),
            "{ll}"
        );
    }

    #[test]
    /// Each expression is emitted at its own type width rather than inheriting
    /// the enclosing one.
    fn expressions_keep_their_own_type_width() {
        let design = Design {
            signals: vec![
                sig("E.a8", 8),
                sig("E.b8", 8),
                sig("E.y8", 8),
                sig("E.a128", 128),
                sig("E.b128", 128),
                sig("E.y128", 128),
            ],
            drivers: vec![
                Driver {
                    span: None,
                    ctx: 0,
                    target: SignalId(2),
                    cond: None,
                    expr: Expr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(Expr::Current(SignalId(0))),
                        rhs: Box::new(Expr::Current(SignalId(1))),
                    },
                    meta: None,
                },
                Driver {
                    span: None,
                    ctx: 0,
                    target: SignalId(5),
                    cond: None,
                    expr: Expr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(Expr::Current(SignalId(3))),
                        rhs: Box::new(Expr::Current(SignalId(4))),
                    },
                    meta: None,
                },
            ],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(
            ll.contains("add i8"),
            "narrow operation was globally widened:\n{ll}"
        );
        assert!(
            ll.contains("add i128"),
            "wide operation lost its type width:\n{ll}"
        );
    }

    #[test]
    /// A dynamic shift is guarded, since a shift at or past the width is LLVM
    /// poison rather than zero.
    fn guards_dynamic_shifts_against_llvm_poison() {
        let design = Design {
            signals: vec![sig("E.value", 16), sig("E.amount", 16), sig("E.y", 16)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(2),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Shl,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(1))),
                },
                meta: None,
            }],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(
            ll.contains("icmp uge i16"),
            "missing shift bound check:\n{ll}"
        );
        assert!(
            ll.contains("shoob"),
            "missing guarded shift condition:\n{ll}"
        );
        assert!(ll.contains("shzero"), "missing zero fallback:\n{ll}");
    }

    #[test]
    /// A real-to-integer conversion sign-extends in a wider signed context.
    fn real_to_integer_sign_extends_in_wider_signed_contexts() {
        let design = Design {
            signals: vec![sig("E.r", 64), sig("E.lt", 1)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(1),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::SLt,
                    lhs: Box::new(Expr::Unary {
                        op: UnOp::RealToInt,
                        rhs: Box::new(Expr::Current(SignalId(0))),
                    }),
                    rhs: Box::new(Expr::Const(0)),
                },
                meta: None,
            }],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(ll.contains("fptosi double"), "missing conversion:\n{ll}");
        assert!(
            ll.contains("sext i64") && ll.contains("to i65"),
            "negative converted integers were not sign-extended:\n{ll}"
        );
        assert!(ll.contains("icmp slt i65"), "unsigned comparison:\n{ll}");
        assert!(
            !ll.contains("zext i64 %rtoi"),
            "converted integer was zero-extended:\n{ll}"
        );
    }

    #[test]
    /// Drivers are emitted in dependency order, so a chain declared backwards
    /// still settles in one pass.
    fn topo_orders_a_chain() {
        // Drivers declared out of dependency order: y=c, c=b, b=a. The emitted
        // settle must compute b, then c, then y (each after its input).
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.b", 8), sig("E.c", 8), sig("E.y", 8)],
            drivers: vec![
                Driver {
                    span: None,
                    target: SignalId(3),
                    cond: None,
                    expr: Expr::Current(SignalId(2)),
                    meta: None,
                    ctx: 0,
                }, // y=c
                Driver {
                    span: None,
                    target: SignalId(2),
                    cond: None,
                    expr: Expr::Current(SignalId(1)),
                    meta: None,
                    ctx: 0,
                }, // c=b
                Driver {
                    span: None,
                    target: SignalId(1),
                    cond: None,
                    expr: Expr::Current(SignalId(0)),
                    meta: None,
                    ctx: 0,
                }, // b=a
            ],
            event_blocks: vec![],
            process_ir: Default::default(),
            process_labels: Default::default(),
            resolved_process_labels: Default::default(),
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            logic_encodings: Default::default(),
            lookup_tables: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            metavalue_temps: Default::default(),
            array_element_enums: Default::default(),
            array_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        // In the settle body, the store to b's slot precedes the store to y's.
        let body = ll.split("@sx_comb_0()").nth(1).unwrap();
        // Struct-GEP field indices: `i32 0, i32 <id>`.
        let store_b = body.find("i32 0, i32 1").expect("b store"); // field 1 = b
        let store_y = body.find("i32 0, i32 3").expect("y store"); // field 3 = y
        assert!(store_b < store_y, "b must settle before y:\n{body}");
    }
}

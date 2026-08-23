//! The inkwell emitter.

use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::TargetMachine;
use inkwell::values::{IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use siox::ir::{BinOp, Design, Expr, ProcessKind, SignalId, UnOp};

/// LLVM's `IntegerType::MAX_INT_BITS` (from `llvm/IR/DerivedTypes.h`).
/// This is a backend capability, not a siox language/container limit.
pub(crate) const LLVM_MAX_INT_BITS: u32 = 1 << 23;

/// Run LLVM's default `-O2` pipeline over the module before codegen. The
/// word-based IR is emitted naively — every value is an `i64`, so each `real`
/// op bitcasts to `f64` and back, comparisons of constants stay unfolded, and
/// each settle reloads signal globals. `-O2` folds the constants, eliminates
/// the `i64`↔`f64` bitcast churn, GVNs redundant loads, and DCEs dead work,
/// leaving the FPU/vector codegen to instruction selection.
pub fn optimize_module(module: &Module, tm: &TargetMachine) -> Result<(), String> {
    // Give the optimizer the target's data layout and triple so it sizes
    // pointers, aligns, and vectorizes for the real machine.
    module.set_triple(&tm.get_triple());
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    module
        .run_passes("default<O2>", tm, PassBuilderOptions::create())
        .map_err(|e| format!("LLVM optimization failed: {e}"))
}

#[cfg(all(test, feature = "bitpack"))]
mod bitpack_tests {
    use super::*;
    use siox::ir::Signal;

    #[test]
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
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
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
    let cg = Codegen::new(ctx, design);
    cg.build();
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
        0..=64 => ctx.i64_type(),
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
            ctx.struct_type(&fields, true)
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
        self.state_globals();
        self.accessors();
        self.settle();
    }

    fn range_error_ptr(&self) -> PointerValue<'ctx> {
        self.module
            .get_global("range_error")
            .expect("range error global")
            .as_pointer_value()
    }

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

    fn range_site_ptr(&self) -> PointerValue<'ctx> {
        self.module
            .get_global("range_site")
            .expect("range site global")
            .as_pointer_value()
    }

    // --- state ------------------------------------------------------------

    #[cfg(not(feature = "bitpack"))]
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

    fn array_ptr(&self, name: &str) -> PointerValue<'ctx> {
        self.module.get_global(name).unwrap().as_pointer_value()
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
        let ty = self.slot_ty(id);
        let v = self
            .builder
            .build_load(ty, self.slot_ptr(arr, id), "v")
            .unwrap()
            .into_int_value();
        self.fit(v, self.value_ty(self.signal_width(id)))
    }

    /// Zero-extend or truncate `v` to `ty`. Storage, compute and ABI widths all
    /// differ once a design holds a multi-word signal, so every crossing goes
    /// through here rather than comparing against a hardcoded 64.
    fn fit(&self, v: IntValue<'ctx>, ty: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        let (from, to) = (v.get_type().get_bit_width(), ty.get_bit_width());
        match from.cmp(&to) {
            std::cmp::Ordering::Less => self.builder.build_int_z_extend(v, ty, "zx").unwrap(),
            std::cmp::Ordering::Greater => self.builder.build_int_truncate(v, ty, "tr").unwrap(),
            std::cmp::Ordering::Equal => v,
        }
    }

    /// Signed counterpart of [`Self::fit`]: widening preserves the source sign
    /// bit, while equal-width and narrowing crossings are representation
    /// identical.
    fn fit_signed(&self, v: IntValue<'ctx>, ty: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        let (from, to) = (v.get_type().get_bit_width(), ty.get_bit_width());
        match from.cmp(&to) {
            std::cmp::Ordering::Less => self.builder.build_int_s_extend(v, ty, "sx").unwrap(),
            std::cmp::Ordering::Greater => self.builder.build_int_truncate(v, ty, "tr").unwrap(),
            std::cmp::Ordering::Equal => v,
        }
    }

    /// Store an `i64` compute value into signal `id`'s width-sized slot in
    /// `@<arr>` (truncating; writers already mask to the signal width).
    #[cfg(not(feature = "bitpack"))]
    fn store(&self, arr: &str, id: SignalId, v: IntValue<'ctx>) {
        let ty = self.slot_ty(id);
        let v = self.fit(v, ty);
        self.builder.build_store(self.slot_ptr(arr, id), v).unwrap();
    }

    // --- bit-packed state layout (feature `bitpack`) ----------------------

    #[cfg(feature = "bitpack")]
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
        self.fit(field, self.value_ty(w))
    }

    /// Store signal `id`: read-modify-write its word — clear the field bits,
    /// OR in the masked, shifted value.
    #[cfg(feature = "bitpack")]
    fn store(&self, arr: &str, id: SignalId, v: IntValue<'ctx>) {
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
    fn settle(&self) {
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
        self.emit_comb_pass();
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
                    Some(c) => self.builder.build_and(fired, self.as_i1(c), "g").unwrap(),
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
            self.emit_comb_pass();
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

    /// One combinational settle pass over the processes in dependency order.
    fn emit_comb_pass(&self) {
        let comb = self.comb();
        for pi in self.topo_order() {
            self.emit_comb(&comb[pi]);
        }
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

    /// Combinational processes (target + source-ordered driver indices).
    fn comb(&self) -> Vec<(SignalId, Vec<usize>)> {
        self.design
            .processes()
            .into_iter()
            .filter_map(|p| match p.kind {
                ProcessKind::Comb { target, drivers } => Some((target, drivers)),
                ProcessKind::Event { .. } => None,
            })
            .collect()
    }

    /// Topologically order combinational processes so each runs after the
    /// processes producing the signals it reads (single-pass settle for
    /// acyclic logic). A cyclic remainder is appended in index order.
    fn topo_order(&self) -> Vec<usize> {
        let procs = self.design.processes();
        let comb: Vec<_> = procs
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.kind, ProcessKind::Comb { .. }))
            .collect();
        // map: signal -> the comb process (local index) that writes it.
        let mut writer: HashMap<SignalId, usize> = HashMap::new();
        let mut local: Vec<usize> = Vec::new(); // local index -> comb() index
        for (li, (_, p)) in comb.iter().enumerate() {
            if let ProcessKind::Comb { target, .. } = &p.kind {
                writer.insert(*target, li);
            }
            local.push(li);
        }
        let m = comb.len();
        let mut deps: Vec<Vec<usize>> = vec![Vec::new(); m];
        let mut indeg = vec![0usize; m];
        for (li, (_, p)) in comb.iter().enumerate() {
            for r in &p.reads {
                if let Some(&w) = writer.get(r) {
                    if w != li {
                        deps[w].push(li);
                        indeg[li] += 1;
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
        order
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
            let cond = d.cond.as_ref().map(|condition| self.as_i1(condition));
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

    fn c(&self, v: u64) -> IntValue<'ctx> {
        self.c_at(v, 64)
    }

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
            Expr::Select { then, els, .. } => self.expr_width(then).max(self.expr_width(els)),
            Expr::Unknown => 1,
        }
    }

    /// Evaluate a condition to an `i1` (nonzero).
    fn as_i1(&self, e: &Expr) -> IntValue<'ctx> {
        let v = self.emit(e);
        self.builder
            .build_int_compare(IntPredicate::NE, v, v.get_type().const_zero(), "nz")
            .unwrap()
    }

    /// zext an `i1` back to the i64 word domain.
    fn zext(&self, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder
            .build_int_z_extend(b, self.i64t(), "z")
            .unwrap()
    }

    fn emit(&self, e: &Expr) -> IntValue<'ctx> {
        self.emit_at(e, self.expr_width(e))
    }

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
                        let z = self
                            .builder
                            .build_int_compare(
                                IntPredicate::EQ,
                                a,
                                a.get_type().const_zero(),
                                "not",
                            )
                            .unwrap();
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
                let base_width = self.expr_width(base).max(*hi + 1);
                let b = self.emit_at(base, base_width);
                let sh = self
                    .builder
                    .build_right_shift(b, self.c_at(*lo as u64, base_width), false, "sh")
                    .unwrap();
                let sliced = self.mask(sh, hi - lo + 1);
                self.mask(sliced, width)
            }
            Expr::Select { cond, then, els } => {
                let c = self.as_i1(cond);
                let t = self.emit_at(then, width);
                let e = self.emit_at(els, width);
                self.builder
                    .build_select(c, t, e, "sel")
                    .unwrap()
                    .into_int_value()
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
                self.builder
                    .build_select(cond, then, els, "ssel")
                    .unwrap()
                    .into_int_value()
            }
            _ => self.emit_at(e, width),
        }
    }

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
            let c = self.builder.build_int_compare(p, a, b, s).unwrap();
            self.fit(c, self.value_ty(result_width))
        };
        match op {
            BinOp::Add => self.builder.build_int_add(a, b, "add").unwrap(),
            BinOp::Sub => self.builder.build_int_sub(a, b, "sub").unwrap(),
            BinOp::Mul => self.builder.build_int_mul(a, b, "mul").unwrap(),
            BinOp::SAdd => self.builder.build_int_add(a, b, "sadd").unwrap(),
            BinOp::SSub => self.builder.build_int_sub(a, b, "ssub").unwrap(),
            BinOp::SMul => self.builder.build_int_mul(a, b, "smul").unwrap(),
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
            BinOp::And => self.builder.build_and(a, b, "and").unwrap(),
            BinOp::Or => self.builder.build_or(a, b, "or").unwrap(),
            BinOp::Xor => self.builder.build_xor(a, b, "xor").unwrap(),
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
    use siox::ir::{Design, Driver, Signal};

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
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        // State layout, accessors, settle, and the add+mask are present. The
        // state is a width-packed struct: three 8-bit signals -> three `i8`s.
        assert!(
            ll.contains("@cur = internal global <{ i8, i8, i8 }>"),
            "{ll}"
        );
        assert!(ll.contains("define void @sx_settle()"), "{ll}");
        assert!(ll.contains("define void @sx_set(i32"), "{ll}");
        assert!(ll.contains("define i64 @sx_read(i32"), "{ll}");
        assert!(ll.contains("add i64"), "{ll}");
        assert!(
            ll.contains("and i64") && ll.contains("255"),
            "mask to width 8:\n{ll}"
        );
    }

    #[test]
    fn accepts_arbitrarily_many_abi_words() {
        let design = Design {
            signals: vec![sig("E.a", 512)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(0),
                cond: None,
                expr: Expr::Const(1),
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(ll.contains("i512"), "{ll}");
        assert_eq!(crate::llvm::words_for(512), 8);
    }

    #[test]
    fn storage_keeps_exact_wide_signal_width() {
        let design = Design {
            signals: vec![sig("E.value", 65)],
            drivers: vec![],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(
            ll.contains("@cur = internal global <{ i65 }>"),
            "65-bit storage was rounded to an unrelated width:\n{ll}"
        );
    }

    #[test]
    fn unsupported_llvm_width_is_an_error_not_a_panic() {
        let design = Design {
            signals: vec![sig("E.enormous", LLVM_MAX_INT_BITS + 1)],
            drivers: vec![],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let error = emit_module_ir(&design).unwrap_err();
        assert!(error.contains("E.enormous"), "{error}");
        assert!(error.contains(&LLVM_MAX_INT_BITS.to_string()), "{error}");
    }

    #[test]
    fn emits_constants_wider_than_one_word() {
        let design = Design {
            signals: vec![sig("E.y", 192)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(0),
                cond: None,
                expr: Expr::WideConst(vec![1, 2, 3]),
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        assert!(
            ll.contains("1020847100762815390427017310442723737601"),
            "{ll}"
        );
    }

    #[test]
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
                },
            ],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
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
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
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
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
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
                    ctx: 0,
                }, // y=c
                Driver {
                    span: None,
                    target: SignalId(2),
                    cond: None,
                    expr: Expr::Current(SignalId(1)),
                    ctx: 0,
                }, // c=b
                Driver {
                    span: None,
                    target: SignalId(1),
                    cond: None,
                    expr: Expr::Current(SignalId(0)),
                    ctx: 0,
                }, // b=a
            ],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let ll = emit_module_ir(&design).unwrap();
        // In the settle body, the store to b's slot precedes the store to y's.
        let body = ll.split("@sx_settle()").nth(1).unwrap();
        // Struct-GEP field indices: `i32 0, i32 <id>`.
        let store_b = body.find("i32 0, i32 1").expect("b store"); // field 1 = b
        let store_y = body.find("i32 0, i32 3").expect("y store"); // field 3 = y
        assert!(store_b < store_y, "b must settle before y:\n{body}");
    }
}

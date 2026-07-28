//! LLVM code generation for siox (compiled-backend plan, stage B2).
//!
//! Consumes the process-extracted [`siox::ir::Design`] and builds an LLVM
//! module: three word-width state arrays (`cur`/`old`/`event`), the
//! `sx_set`/`sx_read`/`sx_reset` accessors, and a `sx_settle` that evaluates
//! the combinational processes in dependency order. Sequential (event-block)
//! codegen and the full delta-cycle fixpoint are the next increment.
//!
//! LLVM is the permanent backend — building siox needs an LLVM toolchain (see
//! `Cargo.toml` for the pinned version). Values use their semantic LLVM width
//! internally and cross the native harness ABI as low-word-first `u64` chunks.

mod aot;
mod emit;

pub use aot::emit_object;
pub use emit::emit_module_ir;

/// Width of one word in the generated native harness ABI.
pub const ABI_WORD_BITS: u32 = 64;

/// Number of ABI words required to exchange a value of `bits` bits.
pub const fn words_for(bits: u32) -> u32 {
    if bits == 0 {
        1
    } else {
        bits.div_ceil(ABI_WORD_BITS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_word_counts_round_up_without_a_global_limit() {
        assert_eq!(words_for(0), 1);
        assert_eq!(words_for(1), 1);
        assert_eq!(words_for(64), 1);
        assert_eq!(words_for(65), 2);
        assert_eq!(words_for(512), 8);
    }
}

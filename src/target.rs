//! The machine model the compiler lowers against.
//!
//! A signal is stored in one or more machine **words**. How wide a word is, and
//! how many of them a backend can address, is a property of that backend — not
//! of the language — so every stage that reasons about storage asks here rather
//! than assuming 64.
//!
//! This sits at foundation level (beside [`crate::diag`]) because both the type
//! checker (stage 4, which reports an unsupportable width at its declaration)
//! and lowering (stage 6, which lays signals out) need it, and stage 4 may not
//! depend on stage 6.
//!
//! Selecting a word size:
//!
//! - default: 64-bit words, matching the LLVM/native backends' `i64` state array
//! - `word32`: 32-bit words, for a backend whose natural register is narrower
//!
//! The two are mutually exclusive; enabling `word32` narrows the word
//! everywhere at once, so a design's layout stays consistent across the stages.

/// Bits in one machine word.
#[cfg(not(feature = "word32"))]
pub const WORD_BITS: u32 = 64;

/// Bits in one machine word (`word32` build).
#[cfg(feature = "word32")]
pub const WORD_BITS: u32 = 32;

/// How many machine words hold a `width`-bit value. A zero width (a parametric
/// `unsigned[W]` before elaboration resolves it) still occupies one word, so
/// callers never index an empty range.
pub const fn words_for(width: u32) -> u32 {
    if width <= WORD_BITS {
        1
    } else {
        width.div_ceil(WORD_BITS)
    }
}

/// Whether a value of `width` bits spans more than one word.
pub const fn is_multiword(width: u32) -> bool {
    width > WORD_BITS
}

/// The widest signal the engines can currently hold.
///
/// Multi-word storage is not implemented yet, so this is one word. It is
/// written in terms of [`WORD_BITS`] rather than as a literal: when multi-word
/// lands this becomes `WORD_BITS * MAX_WORDS` and every stage that reports or
/// enforces the limit follows automatically.
pub const MAX_SIGNAL_WIDTH: u32 = WORD_BITS * MAX_WORDS;

/// Machine words a single signal may occupy. One until multi-word lowering
/// (word-indexed `sx_set_word`/`sx_read_word` accessors) is in place.
pub const MAX_WORDS: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_counts_round_up() {
        assert_eq!(words_for(1), 1);
        assert_eq!(words_for(WORD_BITS), 1);
        assert_eq!(words_for(WORD_BITS + 1), 2);
        assert_eq!(words_for(WORD_BITS * 2), 2);
        assert_eq!(words_for(WORD_BITS * 2 + 1), 3);
        // A not-yet-known parametric width still occupies a word.
        assert_eq!(words_for(0), 1);
    }

    #[test]
    fn multiword_starts_past_one_word() {
        assert!(!is_multiword(WORD_BITS));
        assert!(is_multiword(WORD_BITS + 1));
    }
}

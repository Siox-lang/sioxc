//! The machine model the compiler lowers against.
//!
//! A signal is stored in one or more machine **words**. How wide a word is, and
//! how many are needed for a type is derived from its width — never a global
//! language or backend limit.
//!
//! This sits at foundation level (beside [`crate::diag`]) because both the type
//! lowering and ABI generation need the same layout calculation.
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

/// The largest integer word exposed by the backend ABI.
///
/// Logical values may be wider than this. They cross the ABI as consecutive
/// low-word-first chunks without changing their semantic type.
pub const ABI_WORD_BITS: u32 = WORD_BITS;

/// Total storage bits for `elements` values whose individual representation is
/// `element_bits` wide. This is the layout rule behind vectors and arrays:
/// count and element size are separate properties of the type.
pub const fn bits_for(elements: u32, element_bits: u32) -> Option<u32> {
    elements.checked_mul(element_bits)
}

/// How many ABI words hold a `bits`-wide value. A zero width (a parametric type
/// before elaboration resolves it) still occupies one word, so callers never
/// index an empty range.
pub const fn words_for(bits: u32) -> u32 {
    if bits <= ABI_WORD_BITS {
        1
    } else {
        bits.div_ceil(ABI_WORD_BITS)
    }
}

/// Calculate both the total bit size and ABI-word count of a repeated type.
pub const fn layout_for(elements: u32, element_bits: u32) -> Option<(u32, u32)> {
    match bits_for(elements, element_bits) {
        Some(bits) => Some((bits, words_for(bits))),
        None => None,
    }
}

/// Whether a value of `bits` bits spans more than one ABI word.
pub const fn is_multiword(bits: u32) -> bool {
    bits > ABI_WORD_BITS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_counts_round_up() {
        assert_eq!(words_for(1), 1);
        assert_eq!(words_for(ABI_WORD_BITS), 1);
        assert_eq!(words_for(ABI_WORD_BITS + 1), 2);
        assert_eq!(words_for(ABI_WORD_BITS * 2), 2);
        assert_eq!(words_for(ABI_WORD_BITS * 2 + 1), 3);
        // A not-yet-known parametric width still occupies a word.
        assert_eq!(words_for(0), 1);
    }

    #[test]
    fn repeated_type_layout_uses_element_size() {
        assert_eq!(layout_for(128, 1), Some((128, 2)));
        assert_eq!(layout_for(4, 32), Some((128, 2)));
        assert_eq!(layout_for(128, 4), Some((512, 8)));
        assert_eq!(layout_for(u32::MAX, 2), None);
    }

    #[test]
    fn multiword_starts_past_one_word() {
        assert!(!is_multiword(WORD_BITS));
        assert!(is_multiword(WORD_BITS + 1));
    }
}

//! Shared definitions used by the compiled native test harness.
//!
//! Siox testbench statements are compiled by `sioxc` into the native test
//! executable. The core crate retains only backend-independent data and format
//! parsing shared with diagnostics and waveform output.

/// A snapshot of every signal at one simulation time.
pub struct Sample {
    pub time_fs: u64,
    pub values: Vec<SignalValue>,
}

/// An arbitrary-width signal value in low-word-first ABI order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignalValue {
    pub words: Vec<u64>,
}

impl SignalValue {
    pub fn new(mut words: Vec<u64>) -> Self {
        while words.last() == Some(&0) {
            words.pop();
        }
        Self { words }
    }

    pub fn word(&self, index: usize) -> u64 {
        self.words.get(index).copied().unwrap_or(0)
    }

    pub fn bit(&self, index: u32) -> bool {
        self.word(index as usize / 64) & (1u64 << (index % 64)) != 0
    }

    pub fn low_u64(&self) -> u64 {
        self.word(0)
    }
}

impl From<u64> for SignalValue {
    fn from(value: u64) -> Self {
        Self::new(vec![value])
    }
}

impl From<u128> for SignalValue {
    fn from(value: u128) -> Self {
        Self::new(vec![value as u64, (value >> 64) as u64])
    }
}

/// One piece of a `print!` format string.
pub enum FormatPart {
    /// Literal text, with escaped braces already reduced.
    Text(String),
    /// A `{}` placeholder consuming one argument.
    Placeholder,
}

/// Split a format string into literal text and `{}` placeholders.
pub fn format_parts(fmt: &str) -> Vec<FormatPart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '{' if chars.peek() == Some(&'}') => {
                chars.next();
                if !text.is_empty() {
                    parts.push(FormatPart::Text(std::mem::take(&mut text)));
                }
                parts.push(FormatPart::Placeholder);
            }
            _ => text.push(c),
        }
    }
    if !text.is_empty() {
        parts.push(FormatPart::Text(text));
    }
    parts
}

/// Number of arguments consumed by a format string.
pub fn format_arity(fmt: &str) -> usize {
    format_parts(fmt)
        .iter()
        .filter(|part| matches!(part, FormatPart::Placeholder))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_braces_do_not_consume_arguments() {
        assert_eq!(format_arity("{{}} {}"), 1);
        let parts = format_parts("{{}} {}");
        assert!(matches!(&parts[0], FormatPart::Text(text) if text == "{} "));
        assert!(matches!(parts[1], FormatPart::Placeholder));
    }
}

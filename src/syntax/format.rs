//! Format-string structure shared by semantic checks and harness generation.

/// One piece of a `print!`-style format string.
pub enum FormatPart {
    /// Literal text, with escaped braces already reduced.
    Text(String),
    /// A `{}` placeholder consuming one argument.
    Placeholder,
}

/// Split a format string into literal text and `{}` placeholders.
pub fn parts(fmt: &str) -> Vec<FormatPart> {
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
pub fn arity(fmt: &str) -> usize {
    parts(fmt)
        .iter()
        .filter(|part| matches!(part, FormatPart::Placeholder))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_braces_do_not_consume_arguments() {
        assert_eq!(arity("{{}} {}"), 1);
        let parts = parts("{{}} {}");
        assert!(matches!(&parts[0], FormatPart::Text(text) if text == "{} "));
        assert!(matches!(parts[1], FormatPart::Placeholder));
    }
}

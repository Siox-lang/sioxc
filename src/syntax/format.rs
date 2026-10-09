//! Format-string structure shared by semantic checks and lowering.
//!
//! A placeholder is `{}` or `{:spec}`, with Rust's spec grammar:
//! `[[fill]align][+][#][0][width][.precision][type]`, where `align` is `<`,
//! `^` or `>` and `type` is `e`, `E`, `x`, `X`, `b` or `o`.

/// One piece of a `print!`-style format string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatPart {
    /// Literal text, with escaped braces already reduced.
    Text(String),
    /// A placeholder consuming one argument, formatted by its spec.
    Placeholder(FormatSpec),
}

/// How one placeholder formats its argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatSpec {
    /// Padding character, `' '` unless written before an alignment.
    pub fill: char,
    /// Where the value sits in its `width`; `None` is the type's default
    /// (numbers right, everything else left).
    pub align: Option<FormatAlign>,
    /// `+`: always write the sign of a number.
    pub plus: bool,
    /// `#`: prefix radix forms with `0x`, `0b` or `0o`.
    pub alternate: bool,
    /// `0`: pad a number with zeros after its sign.
    pub zero: bool,
    /// Minimum width in characters.
    pub width: Option<u32>,
    /// Digits after the point (`.3`), for reals and their scientific form.
    pub precision: Option<u32>,
    /// Which notation numbers use.
    pub kind: FormatKind,
}

impl Default for FormatSpec {
    fn default() -> Self {
        Self {
            fill: ' ',
            align: None,
            plus: false,
            alternate: false,
            zero: false,
            width: None,
            precision: None,
            kind: FormatKind::Display,
        }
    }
}

impl FormatSpec {
    /// Whether the spec asks for anything beyond plain `{}`.
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }

    /// The numeric part of the spec, which a nested plain `{}` inherits: a
    /// `Display` impl writing `{}` formats with its caller's precision and
    /// notation, as Rust's float `Display` honours `{:.3}`.
    pub fn numeric(&self) -> Self {
        Self {
            plus: self.plus,
            alternate: self.alternate,
            precision: self.precision,
            kind: self.kind,
            ..Self::default()
        }
    }
}

/// Alignment within a placeholder's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatAlign {
    /// `<`
    Left,
    /// `^`
    Center,
    /// `>`
    Right,
}

/// The notation a placeholder asks numbers to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormatKind {
    /// No type letter: decimal, or the value's own form.
    #[default]
    Display,
    /// `e`: scientific, `1.23e4`.
    LowerExp,
    /// `E`: scientific, `1.23E4`.
    UpperExp,
    /// `x`: hexadecimal, lower case.
    LowerHex,
    /// `X`: hexadecimal, upper case.
    UpperHex,
    /// `b`: binary.
    Binary,
    /// `o`: octal.
    Octal,
    /// `?`: the built-in structural form even when the type implements
    /// `Display`, with strings and characters quoted (Rust's `Debug`).
    Debug,
}

impl FormatKind {
    /// Whether this is one of the radix forms, which only integers take.
    pub fn is_radix(self) -> bool {
        matches!(
            self,
            Self::LowerHex | Self::UpperHex | Self::Binary | Self::Octal
        )
    }
}

/// Split a format string into literal text and placeholders. A malformed
/// placeholder still yields one (with a plain spec) so arity stays right;
/// [`errors`] reports it.
pub fn parts(fmt: &str) -> Vec<FormatPart> {
    parse(fmt).0
}

/// Every malformed placeholder in a format string, as a message.
pub fn errors(fmt: &str) -> Vec<String> {
    parse(fmt).1
}

/// Number of arguments consumed by a format string.
pub fn arity(fmt: &str) -> usize {
    parts(fmt)
        .iter()
        .filter(|part| matches!(part, FormatPart::Placeholder(_)))
        .count()
}

fn parse(fmt: &str) -> (Vec<FormatPart>, Vec<String>) {
    let mut parts = Vec::new();
    let mut errors = Vec::new();
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
            '{' => {
                let mut inside = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    inside.push(c);
                }
                if !text.is_empty() {
                    parts.push(FormatPart::Text(std::mem::take(&mut text)));
                }
                let spec = if !closed {
                    errors.push("a `{` placeholder is never closed; write `{{` for a brace".into());
                    FormatSpec::default()
                } else if inside.is_empty() {
                    FormatSpec::default()
                } else if let Some(spec) = inside.strip_prefix(':') {
                    parse_spec(spec).unwrap_or_else(|message| {
                        errors.push(message);
                        FormatSpec::default()
                    })
                } else {
                    errors.push(format!(
                        "`{{{inside}}}` is not a placeholder: arguments are taken in order, so write `{{}}` or `{{:spec}}`"
                    ));
                    FormatSpec::default()
                };
                parts.push(FormatPart::Placeholder(spec));
            }
            _ => text.push(c),
        }
    }
    if !text.is_empty() {
        parts.push(FormatPart::Text(text));
    }
    (parts, errors)
}

fn parse_spec(spec: &str) -> Result<FormatSpec, String> {
    let malformed = || {
        format!(
            "`{{:{spec}}}` is not a format spec; it is `[[fill]align][+][#][0][width][.precision][type]`, \
             with align `<` `^` `>` and type `e` `E` `x` `X` `b` `o` `?`"
        )
    };
    let chars: Vec<char> = spec.chars().collect();
    let align_of = |c: char| match c {
        '<' => Some(FormatAlign::Left),
        '^' => Some(FormatAlign::Center),
        '>' => Some(FormatAlign::Right),
        _ => None,
    };
    let mut out = FormatSpec::default();
    let mut at = 0;
    if chars.len() >= 2 && align_of(chars[1]).is_some() {
        out.fill = chars[0];
        out.align = align_of(chars[1]);
        at = 2;
    } else if let Some(align) = chars.first().copied().and_then(align_of) {
        out.align = Some(align);
        at = 1;
    }
    if chars.get(at) == Some(&'+') {
        out.plus = true;
        at += 1;
    }
    if chars.get(at) == Some(&'#') {
        out.alternate = true;
        at += 1;
    }
    if chars.get(at) == Some(&'0') {
        out.zero = true;
        at += 1;
    }
    let number = |at: &mut usize| -> Option<u32> {
        let start = *at;
        while chars.get(*at).is_some_and(char::is_ascii_digit) {
            *at += 1;
        }
        (start < *at).then(|| chars[start..*at].iter().collect::<String>().parse().ok())?
    };
    out.width = number(&mut at);
    if chars.get(at) == Some(&'.') {
        at += 1;
        out.precision = Some(number(&mut at).ok_or_else(malformed)?);
    }
    out.kind = match chars.get(at) {
        None => FormatKind::Display,
        Some('e') => FormatKind::LowerExp,
        Some('E') => FormatKind::UpperExp,
        Some('x') => FormatKind::LowerHex,
        Some('X') => FormatKind::UpperHex,
        Some('b') => FormatKind::Binary,
        Some('o') => FormatKind::Octal,
        Some('?') => FormatKind::Debug,
        Some(_) => return Err(malformed()),
    };
    if out.kind != FormatKind::Display {
        at += 1;
    }
    if at != chars.len() {
        return Err(malformed());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `{{` is an escaped brace, not a placeholder, so it must not consume an
    /// argument or shift the ones after it.
    fn escaped_braces_do_not_consume_arguments() {
        assert_eq!(arity("{{}} {}"), 1);
        let parts = parts("{{}} {}");
        assert!(matches!(&parts[0], FormatPart::Text(text) if text == "{} "));
        assert!(matches!(&parts[1], FormatPart::Placeholder(spec) if spec.is_plain()));
    }

    #[test]
    /// Each field of Rust's spec grammar lands where it belongs.
    fn specs_follow_rusts_grammar() {
        let spec = |s: &str| match &parts(s)[0] {
            FormatPart::Placeholder(spec) => spec.clone(),
            FormatPart::Text(_) => panic!("no placeholder in {s}"),
        };
        assert_eq!(spec("{:.3}").precision, Some(3));
        let scientific = spec("{:.2e}");
        assert_eq!(
            (scientific.precision, scientific.kind),
            (Some(2), FormatKind::LowerExp)
        );
        assert_eq!(spec("{:?}").kind, FormatKind::Debug);
        assert_eq!(spec("{:>12?}").width, Some(12));
        let padded = spec("{:*^10}");
        assert_eq!(
            (padded.fill, padded.align, padded.width),
            ('*', Some(FormatAlign::Center), Some(10))
        );
        let hex = spec("{:+#010X}");
        assert!(hex.plus && hex.alternate && hex.zero);
        assert_eq!((hex.width, hex.kind), (Some(10), FormatKind::UpperHex));
        assert_eq!(spec("{:>8}").align, Some(FormatAlign::Right));
        assert!(errors("{:q}").len() == 1 && errors("{:.}").len() == 1);
        assert!(errors("{0}").len() == 1 && errors("{").len() == 1);
        assert_eq!(arity("{:x} {} {:.1e}"), 3);
    }
}

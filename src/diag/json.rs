//! The diagnostic wire format. Keep JSON escaping here, independent of CLI/LLVM.

use super::{Applicability, Diagnostic, SourceMap, Span};
use std::fmt::Write;

impl Diagnostic {
    /// One UTF-8 JSON object, without a terminating newline.
    ///
    /// Byte offsets are half-open; lines/columns are one-based and columns
    /// count Unicode scalar values. Missing/invalid locations have null
    /// coordinates, rather than pretending to point to the start of a file.
    pub fn render_json(&self, sources: &SourceMap) -> String {
        let mut out = String::from("{\"$message_type\":\"diagnostic\",\"code\":");
        optional_string(&mut out, self.code);
        out.push_str(",\"severity\":");
        string(&mut out, self.severity.as_str());
        out.push_str(",\"message\":");
        string(&mut out, &self.message);
        out.push_str(",\"primary\":");
        match self.primary {
            Some(primary) => span(&mut out, primary, sources),
            None => out.push_str("null"),
        }
        out.push_str(",\"labels\":[");
        for (index, label) in self.labels.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"span\":");
            span(&mut out, label.span, sources);
            out.push_str(",\"message\":");
            string(&mut out, &label.message);
            out.push('}');
        }
        out.push_str("],\"notes\":[");
        for (index, note) in self.notes.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            string(&mut out, note);
        }
        out.push_str("],\"help\":");
        optional_string(&mut out, self.help.as_deref());
        out.push_str(",\"suggestions\":[");
        for (index, suggestion) in self.suggestions.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"applicability\":");
            string(
                &mut out,
                match suggestion.applicability {
                    Applicability::MachineApplicable => "MachineApplicable",
                    Applicability::MaybeIncorrect => "MaybeIncorrect",
                    Applicability::HasPlaceholders => "HasPlaceholders",
                    Applicability::Unspecified => "Unspecified",
                },
            );
            out.push_str(",\"edits\":[");
            for (index, edit) in suggestion.edits.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str("{\"span\":");
                span(&mut out, edit.span, sources);
                out.push_str(",\"replacement\":");
                string(&mut out, &edit.replacement);
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }
}

fn span(out: &mut String, span: Span, sources: &SourceMap) {
    let source = sources.get(span.file);
    let _ = write!(out, "{{\"file_id\":{},\"file_name\":", span.file.0);
    optional_string(out, source.map(|source| source.name.as_str()));
    let _ = write!(
        out,
        ",\"byte_start\":{},\"byte_end\":{}",
        span.start, span.end
    );
    let coordinates = source.and_then(|source| {
        source.text.get(span.start as usize..span.end as usize)?;
        Some((
            line_col(&source.text, span.start),
            line_col(&source.text, span.end),
        ))
    });
    for (name, value) in [
        ("line_start", coordinates.map(|(start, _)| start.0)),
        ("column_start", coordinates.map(|(start, _)| start.1)),
        ("line_end", coordinates.map(|(_, end)| end.0)),
        ("column_end", coordinates.map(|(_, end)| end.1)),
    ] {
        let _ = write!(out, ",\"{name}\":");
        match value {
            Some(value) => {
                let _ = write!(out, "{value}");
            }
            None => out.push_str("null"),
        }
    }
    out.push('}');
}

fn line_col(text: &str, offset: u32) -> (usize, usize) {
    let prefix = &text[..offset as usize];
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (
        prefix.bytes().filter(|byte| *byte == b'\n').count() + 1,
        prefix[line_start..].chars().count() + 1,
    )
}

fn optional_string(out: &mut String, value: Option<&str>) {
    match value {
        Some(value) => string(out, value),
        None => out.push_str("null"),
    }
}

fn string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            character if character <= '\u{1f}' => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{codes, FileId, Severity};

    #[test]
    fn every_json_control_character_is_escaped() {
        let mut output = String::new();
        string(&mut output, "\"\\\n\r\t\u{08}\u{0c}\0\u{1f}λ🙂");
        assert_eq!(output, "\"\\\"\\\\\\n\\r\\t\\b\\f\\u0000\\u001fλ🙂\"");
        for character in '\0'..='\u{1f}' {
            output.clear();
            string(&mut output, &character.to_string());
            assert!(!output.chars().any(|c| c <= '\u{1f}'));
        }
    }

    #[test]
    fn unicode_multiline_and_empty_spans_keep_byte_ranges_and_scalar_columns() {
        let mut sources = SourceMap::new();
        let file = sources.add("λ\".siox", "aλ🙂\nβ\n");
        let mut output = String::new();
        span(&mut output, Span::new(file, 3..10), &sources);
        assert!(
            output.contains("\"byte_start\":3,\"byte_end\":10"),
            "{output}"
        );
        assert!(
            output.contains("\"line_start\":1,\"column_start\":3,\"line_end\":2,\"column_end\":2"),
            "{output}"
        );
        output.clear();
        span(&mut output, Span::new(file, 11..11), &sources);
        assert!(
            output.contains("\"line_start\":3,\"column_start\":1"),
            "{output}"
        );
        for (start, end) in [(2, 3), (10, 3), (0, 12)] {
            output.clear();
            span(&mut output, Span::new(file, start..end), &sources);
            assert!(output.contains("\"line_start\":null"), "{output}");
        }
        output.clear();
        span(&mut output, Span::new(FileId(99), 0..0), &sources);
        assert!(output.contains("\"file_name\":null"), "{output}");
    }

    #[test]
    fn all_fields_and_atomic_edits_are_retained() {
        let mut sources = SourceMap::new();
        let file = sources.add("test.siox", "abc");
        let at = Span::new(file, 0..1);
        let diagnostic = Diagnostic::warning("message")
            .with_code(codes::UNUSED_SIGNAL)
            .at(at)
            .label(at, "label")
            .note("note")
            .help("help")
            .suggest(at, "new", Applicability::MachineApplicable)
            .suggest_edits(Vec::new(), Applicability::Unspecified);
        assert_eq!(diagnostic.suggestions.len(), 1);
        let json = diagnostic.render_json(&sources);
        for expected in [
            "\"severity\":\"warning\"",
            "\"code\":\"W-P003\"",
            "\"message\":\"label\"",
            "\"notes\":[\"note\"]",
            "\"help\":\"help\"",
            "\"replacement\":\"new\"",
            "\"applicability\":\"MachineApplicable\"",
        ] {
            assert!(json.contains(expected), "{json}");
        }
        for severity in [
            Severity::Error,
            Severity::Warning,
            Severity::Note,
            Severity::Help,
        ] {
            let mut diagnostic = Diagnostic::error("x");
            diagnostic.severity = severity;
            assert!(diagnostic
                .render_json(&sources)
                .contains(&format!("\"severity\":\"{}\"", severity.as_str())));
        }
        for applicability in [
            Applicability::MachineApplicable,
            Applicability::MaybeIncorrect,
            Applicability::HasPlaceholders,
            Applicability::Unspecified,
        ] {
            let diagnostic = Diagnostic::error("x").suggest(at, "new", applicability);
            assert!(diagnostic
                .render_json(&sources)
                .contains(&format!("\"applicability\":\"{applicability:?}\"")));
        }
    }
}

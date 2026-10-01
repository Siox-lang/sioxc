//! Lint levels: `#[allow(...)]`, `#[warn(...)]`, `#[deny(...)]` and
//! `#[forbid(...)]`, modelled on rustc's.
//!
//! Every warning the compiler emits is a *lint* with a stable snake_case name
//! (`possible_latch` for `W-P002`). A directive sets the level of the lints it
//! names for the item it precedes, an inner `#![...]` directive sets it for
//! the whole module, and the command line (`-A`/`-W`/`-D`/`-F`) sets it for
//! everything. The levels then apply as the sink receives each warning:
//!
//! - **Order.** The command line applies first, in the order written, then
//!   every directive whose item contains the warning, outermost first, so the
//!   innermost level wins.
//! - **`forbid`** is `deny` that cannot be lowered: a later `allow`, `warn` or
//!   `deny` of a forbidden lint is `E-P033` and is ignored.
//! - **`warnings`** names every lint at once, as in rustc: `-D warnings` turns
//!   them all into errors.
//! - An unknown lint name is itself the warning `unknown_lints` (`W-P017`),
//!   which can be allowed like any other.
//!
//! Acceptance: an allowed warning is not emitted; a denied one is an error, so
//! it stops later stages and fails the build; a warning or error a level
//! produced says where that level was set.

use std::collections::HashSet;

use super::{codes, Diagnostic, Label, Severity, Span};

/// What happens to a lint's warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// Not reported.
    Allow,
    /// Reported as a warning (the default).
    Warn,
    /// Reported as an error.
    Deny,
    /// Reported as an error, and no inner level may lower it.
    Forbid,
}

impl Level {
    /// The directive word: `allow`, `warn`, `deny` or `forbid`.
    pub fn word(self) -> &'static str {
        match self {
            Level::Allow => "allow",
            Level::Warn => "warn",
            Level::Deny => "deny",
            Level::Forbid => "forbid",
        }
    }

    /// The level a directive word names.
    pub fn from_word(word: &str) -> Option<Level> {
        Some(match word {
            "allow" => Level::Allow,
            "warn" => Level::Warn,
            "deny" => Level::Deny,
            "forbid" => Level::Forbid,
            _ => return None,
        })
    }

    /// The command-line flag that sets this level.
    fn flag(self) -> &'static str {
        match self {
            Level::Allow => "-A",
            Level::Warn => "-W",
            Level::Deny => "-D",
            Level::Forbid => "-F",
        }
    }
}

/// The group naming every lint, as rustc's `warnings`.
pub const WARNINGS: &str = "warnings";

/// Every lint: its name and the warning code it controls.
pub const LINTS: &[(&str, &str)] = &[
    ("possible_latch", codes::POSSIBLE_LATCH),
    ("unused_signal", codes::UNUSED_SIGNAL),
    ("unused_param", codes::UNUSED_PARAM),
    ("unused_import", codes::UNUSED_IMPORT),
    ("unreachable_match_arm", codes::UNREACHABLE_MATCH_ARM),
    ("non_exhaustive_match", codes::NON_EXHAUSTIVE_MATCH),
    ("suspicious_logic_compare", codes::SUSPICIOUS_LOGIC_COMPARE),
    ("suspicious_reset", codes::SUSPICIOUS_RESET),
    ("combinational_loop", codes::COMBINATIONAL_LOOP),
    ("undriven_output", codes::UNDRIVEN_OUTPUT),
    ("unconnected_input", codes::UNCONNECTED_INPUT),
    ("dead_assignment", codes::DEAD_ASSIGNMENT),
    ("unimplemented_attr", codes::UNIMPLEMENTED_ATTR),
    (
        "incomplete_struct_literal",
        codes::INCOMPLETE_STRUCT_LITERAL,
    ),
    ("unknown_lints", codes::UNKNOWN_LINT),
];

/// The lint a warning code belongs to.
pub fn lint_of(code: &str) -> Option<&'static str> {
    LINTS
        .iter()
        .find(|(_, c)| *c == code)
        .map(|(name, _)| *name)
}

/// Whether `name` is a lint or the `warnings` group.
pub fn is_known(name: &str) -> bool {
    name == WARNINGS || LINTS.iter().any(|(n, _)| *n == name)
}

/// One `#[allow(a, b)]`: its level, the lints it names, where it is written,
/// and the extent of the item (or module, for `#![...]`) it governs.
#[derive(Clone, Debug)]
pub struct LintDirective {
    /// The level it sets.
    pub level: Level,
    /// The directive word (`allow`), resolved like any attribute name to
    /// its `std::attrs` declaration.
    pub word: Span,
    /// The lint names inside the parentheses, with their spans.
    pub names: Vec<(String, Span)>,
    /// The whole `#[...]`.
    pub span: Span,
    /// The item, statement or module the level applies within.
    pub scope: Span,
}

/// Where a lint's current level came from, for the note that explains it.
#[derive(Clone)]
enum Source {
    Default,
    CommandLine(Level, String),
    Directive(Level, String, Span),
}

/// The configured levels: the command line and every directive.
#[derive(Default)]
pub struct LintLevels {
    command_line: Vec<(Level, String)>,
    directives: Vec<LintDirective>,
    /// Lints whose "on by default" note has been shown once already.
    noted: HashSet<&'static str>,
}

impl LintLevels {
    /// Levels from the command line (in order) and the directives in source.
    /// Returns the levels and the diagnostics registering them produced:
    /// `unknown_lints` warnings and `forbid` conflicts. The caller emits
    /// those through the sink, so the levels apply to them too.
    pub fn new(
        command_line: Vec<(Level, String)>,
        mut directives: Vec<LintDirective>,
    ) -> (LintLevels, Vec<Diagnostic>) {
        let mut problems = Vec::new();
        for (level, name) in &command_line {
            if !is_known(name) {
                problems.push(
                    Diagnostic::warning(format!("unknown lint: `{name}`"))
                        .with_code(codes::UNKNOWN_LINT)
                        .note(format!(
                            "requested on the command line with `{} {name}`",
                            level.flag()
                        )),
                );
            }
        }
        for directive in &directives {
            for (name, span) in &directive.names {
                if !is_known(name) {
                    problems.push(
                        Diagnostic::warning(format!("unknown lint: `{name}`"))
                            .with_code(codes::UNKNOWN_LINT)
                            .at(*span),
                    );
                }
            }
        }
        // Outermost first, then source order, so applying them in sequence
        // leaves the innermost level in force.
        directives.sort_by_key(|d| {
            (
                d.scope.file.0,
                std::cmp::Reverse(d.scope.end - d.scope.start),
                d.span.start,
            )
        });
        let levels = LintLevels {
            command_line,
            directives,
            noted: HashSet::new(),
        };
        // A directive that would lower a forbidden lint: rustc's E0453.
        for (index, directive) in levels.directives.iter().enumerate() {
            if directive.level == Level::Forbid {
                continue;
            }
            for (name, span) in &directive.names {
                let lints: Vec<&str> = if name == WARNINGS {
                    LINTS.iter().map(|(n, _)| *n).collect()
                } else {
                    vec![name.as_str()]
                };
                let forbidden = lints.iter().find_map(|lint| {
                    match levels.level_at(lint, Some(directive.span), index).1 {
                        Source::CommandLine(Level::Forbid, flag) => Some((None, flag)),
                        Source::Directive(Level::Forbid, how, at) => Some((Some(at), how)),
                        _ => None,
                    }
                });
                if let Some((at, how)) = forbidden {
                    let mut diagnostic = Diagnostic::error(format!(
                        "`{}({name})` incompatible with previous `forbid`",
                        directive.level.word()
                    ))
                    .with_code(codes::FORBIDDEN_LINT_LEVEL)
                    .at(*span)
                    .help("`forbid` cannot be lowered; remove this directive or the `forbid`");
                    diagnostic = match at {
                        Some(at) => diagnostic.label(at, "`forbid` level set here"),
                        None => diagnostic.note(format!(
                            "`forbid` requested on the command line with `{how}`"
                        )),
                    };
                    problems.push(diagnostic);
                    break;
                }
            }
        }
        (levels, problems)
    }

    /// The level of `lint` at `at` (`None`: no location, so only the
    /// command line applies), and where it was set, considering only the
    /// first `before` directives in application order.
    fn level_at(&self, lint: &str, at: Option<Span>, before: usize) -> (Level, Source) {
        let mut level = Level::Warn;
        let mut source = Source::Default;
        for (set, name) in &self.command_line {
            if level == Level::Forbid {
                break;
            }
            if name == lint || name == WARNINGS {
                level = *set;
                source = Source::CommandLine(*set, format!("{} {name}", set.flag()));
            }
        }
        let Some(at) = at else {
            return (level, source);
        };
        for directive in self.directives.iter().take(before) {
            let scope = directive.scope;
            if scope.file != at.file || scope.start > at.start || scope.end < at.end {
                continue;
            }
            for (name, _) in &directive.names {
                if level == Level::Forbid {
                    return (level, source);
                }
                if name == lint || name == WARNINGS {
                    level = directive.level;
                    let how = if name == WARNINGS {
                        format!("`#[{}({WARNINGS})]`", directive.level.word())
                    } else {
                        format!("`#[{}({lint})]`", directive.level.word())
                    };
                    source = Source::Directive(directive.level, how, directive.span);
                }
            }
        }
        (level, source)
    }

    /// Apply the levels to a warning about to be emitted: `None` drops it
    /// (allowed), otherwise it comes back as a warning or an error with a
    /// note saying where its level was set.
    pub fn apply(&mut self, mut diagnostic: Diagnostic) -> Option<Diagnostic> {
        if diagnostic.severity != Severity::Warning {
            return Some(diagnostic);
        }
        let Some(lint) = diagnostic.code.and_then(lint_of) else {
            return Some(diagnostic);
        };
        let (level, source) = self.level_at(lint, diagnostic.primary, self.directives.len());
        match level {
            Level::Allow => return None,
            Level::Warn => {}
            Level::Deny | Level::Forbid => diagnostic.severity = Severity::Error,
        }
        match source {
            Source::Default => {
                if self.noted.insert(lint) {
                    diagnostic = diagnostic.note(format!("`#[warn({lint})]` on by default"));
                }
            }
            Source::CommandLine(_, flag) => {
                let implied = if flag.ends_with(WARNINGS) {
                    format!(" (implied by `{flag}`)")
                } else {
                    String::new()
                };
                diagnostic = diagnostic.note(format!(
                    "`{} {lint}` requested on the command line{implied}",
                    level.flag()
                ));
            }
            Source::Directive(_, how, at) => {
                if how.contains(WARNINGS) {
                    diagnostic =
                        diagnostic.note(format!("`#[{}({lint})]` implied by {how}", level.word()));
                }
                diagnostic.labels.push(Label {
                    span: at,
                    message: "the lint level is defined here".to_string(),
                });
            }
        }
        Some(diagnostic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId(0),
            start,
            end,
        }
    }

    /// A directive at `at` (its own `#[...]`) governing `scope`.
    fn directive(level: Level, names: &[&str], at: u32, scope: (u32, u32)) -> LintDirective {
        LintDirective {
            level,
            word: span(at + 2, at + 7),
            names: names
                .iter()
                .map(|n| (n.to_string(), span(at + 8, at + 9)))
                .collect(),
            span: span(at, at + 10),
            scope: span(scope.0, scope.1),
        }
    }

    /// An `unused_signal` warning at `at`.
    fn unused(at: u32) -> Diagnostic {
        Diagnostic::warning("signal `x` is never read")
            .with_code(codes::UNUSED_SIGNAL)
            .at(span(at, at + 1))
    }

    /// Emit `warnings` through a sink configured with the given levels and
    /// return what it kept.
    fn run(
        command_line: &[(Level, &str)],
        directives: Vec<LintDirective>,
        warnings: Vec<Diagnostic>,
    ) -> Vec<Diagnostic> {
        let mut sink = DiagnosticSink::new();
        sink.set_lint_levels(
            command_line
                .iter()
                .map(|(l, n)| (*l, n.to_string()))
                .collect(),
            directives,
        );
        for warning in warnings {
            sink.emit(warning);
        }
        sink.diagnostics().to_vec()
    }

    #[test]
    /// Every warning code is a lint, so every warning can be controlled.
    fn every_warning_code_is_a_lint() {
        let source = include_str!("../diag.rs");
        for line in source.lines() {
            let Some(code) = line.split('"').nth(1).filter(|c| c.starts_with("W-P")) else {
                continue;
            };
            if line.contains("pub const") {
                assert!(lint_of(code).is_some(), "{code} has no lint name");
            }
        }
    }

    #[test]
    /// `std::attrs::Lint` lists exactly the compiler's lints and `warnings`,
    /// so the vocabulary users read in std is the one the compiler accepts.
    fn std_lint_enum_matches_the_compiler() {
        let attrs = include_str!("../../std/attrs.siox");
        let body = attrs
            .split("pub enum Lint {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("std::attrs declares `enum Lint`");
        let mut declared: Vec<&str> = body
            .lines()
            .map(|line| {
                line.split("//")
                    .next()
                    .unwrap()
                    .trim()
                    .trim_end_matches(',')
            })
            .filter(|name| !name.is_empty())
            .collect();
        let mut known: Vec<&str> = LINTS.iter().map(|(name, _)| *name).collect();
        known.push(WARNINGS);
        declared.sort();
        known.sort();
        assert_eq!(declared, known);
    }

    #[test]
    /// By default a lint warns, and the first warning of each lint names it.
    fn default_level_warns_and_names_the_lint_once() {
        let kept = run(&[], Vec::new(), vec![unused(10), unused(20)]);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|d| d.severity == Severity::Warning));
        assert_eq!(kept[0].notes, ["`#[warn(unused_signal)]` on by default"]);
        assert!(kept[1].notes.is_empty(), "noted once, as rustc does");
    }

    #[test]
    /// `allow` drops the warning inside its item only; `deny` makes it an
    /// error that points at the directive; the innermost level wins.
    fn innermost_directive_wins() {
        let directives = vec![
            directive(Level::Deny, &["unused_signal"], 0, (0, 100)),
            directive(Level::Allow, &["unused_signal"], 30, (30, 50)),
        ];
        let kept = run(&[], directives, vec![unused(40), unused(60)]);
        assert_eq!(kept.len(), 1, "the allowed one is dropped");
        assert_eq!(kept[0].severity, Severity::Error);
        assert_eq!(kept[0].primary, Some(span(60, 61)));
        assert!(kept[0]
            .labels
            .iter()
            .any(|l| l.message == "the lint level is defined here" && l.span == span(0, 10)));
    }

    #[test]
    /// `warnings` names every lint, and a later, more specific level in the
    /// same place overrides it.
    fn the_warnings_group_and_order() {
        let kept = run(
            &[(Level::Deny, "warnings"), (Level::Allow, "unused_signal")],
            Vec::new(),
            vec![
                unused(5),
                Diagnostic::warning("dead")
                    .with_code(codes::DEAD_ASSIGNMENT)
                    .at(span(7, 8)),
            ],
        );
        assert_eq!(kept.len(), 1, "unused_signal is allowed after the group");
        assert_eq!(kept[0].severity, Severity::Error);
        assert_eq!(
            kept[0].notes,
            ["`-D dead_assignment` requested on the command line (implied by `-D warnings`)"]
        );
    }

    #[test]
    /// `forbid` cannot be lowered: an inner `allow` is E-P033 and ignored,
    /// from source or from the command line. `allow` then `forbid` on one
    /// item is not a conflict.
    fn forbid_cannot_be_lowered() {
        let kept = run(
            &[],
            vec![
                directive(Level::Forbid, &["unused_signal"], 0, (0, 100)),
                directive(Level::Allow, &["unused_signal"], 30, (30, 50)),
            ],
            vec![unused(40)],
        );
        let codes: Vec<_> = kept.iter().map(|d| (d.code, d.severity)).collect();
        assert_eq!(
            codes,
            [
                (Some(codes::FORBIDDEN_LINT_LEVEL), Severity::Error),
                (Some(codes::UNUSED_SIGNAL), Severity::Error),
            ]
        );

        let kept = run(
            &[(Level::Forbid, "warnings")],
            vec![directive(Level::Allow, &["unused_signal"], 30, (30, 50))],
            vec![unused(40)],
        );
        assert_eq!(kept[0].code, Some(codes::FORBIDDEN_LINT_LEVEL));
        assert_eq!(kept[1].severity, Severity::Error);

        let kept = run(
            &[],
            vec![
                directive(Level::Allow, &["unused_signal"], 0, (0, 100)),
                directive(Level::Forbid, &["unused_signal"], 11, (0, 100)),
            ],
            vec![unused(40)],
        );
        assert_eq!(kept.len(), 1, "no conflict, and the later forbid holds");
        assert_eq!(kept[0].severity, Severity::Error);
    }

    #[test]
    /// An unknown lint is the `unknown_lints` warning, which is itself a
    /// lint: allowing it silences the report.
    fn unknown_lints_are_a_lint() {
        let kept = run(
            &[(Level::Warn, "nope")],
            vec![directive(Level::Allow, &["also_nope"], 0, (0, 100))],
            Vec::new(),
        );
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|d| d.code == Some(codes::UNKNOWN_LINT)));
        assert_eq!(
            kept[0].notes[0],
            "requested on the command line with `-W nope`"
        );

        let kept = run(
            &[(Level::Allow, "unknown_lints")],
            vec![directive(Level::Allow, &["also_nope"], 0, (0, 100))],
            Vec::new(),
        );
        assert!(kept.is_empty());
    }
}

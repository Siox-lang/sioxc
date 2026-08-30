//! Recursive-descent parser: [`Token`] stream -> [`Module`] AST.
//!
//! Spec Stage 2 work items: module-item parser, type parser, expression
//! parser, statement parser, attribute parser, entity/impl/trait/struct/enum
//! parsers, instance-construction parser, pattern parser. Acceptance:
//! valid examples parse, invalid syntax yields useful spans, recovery works,
//! and the pretty-printer round-trips simple examples.
//!
//! Notes on a few intentional simplifications:
//! - `Comment` trivia is stripped on construction; the grammar never sees it.
//! - `a::b` is greedily a path (namespaces/types/variants/views); an attribute
//!   rides the VHDL-style tick `a'attr` and becomes [`Expr::SysAttr`]. The two
//!   sigils never overlap, so no name list is consulted pre-resolve.
//! - Float / hex-string literal tokens map to [`Expr::Int`] (which stores raw
//!   text); a dedicated literal node can be added when a later stage needs it.

use crate::diag::{Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;
use crate::syntax::token::{Token, TokenKind};
use std::collections::HashMap;

/// Lightweight declaration pass used before the full Pratt parse. It finds
/// attributed `impl custom<"symbol", ...>` blocks without parsing their
/// bodies, producing the operator table needed to group expressions.
pub fn discover_custom_operators(src: &str, tokens: &[Token]) -> HashMap<String, u8> {
    let text = |t: &Token| &src[t.span.start as usize..t.span.end as usize];
    let mut out = HashMap::new();
    let mut i = 0;
    let mut pending_precedence = None;
    while i < tokens.len() {
        if tokens[i].kind == TokenKind::Comment {
            i += 1;
            continue;
        }
        if tokens[i].kind == TokenKind::Pound {
            let mut j = i + 1;
            while j < tokens.len() && tokens[j].kind != TokenKind::RBracket {
                if tokens[j].kind == TokenKind::Ident && text(&tokens[j]) == "precedence" {
                    let mut k = j + 1;
                    while k < tokens.len() && tokens[k].kind != TokenKind::RBracket {
                        if tokens[k].kind == TokenKind::Int {
                            pending_precedence =
                                text(&tokens[k]).replace('_', "").parse::<u8>().ok();
                            break;
                        }
                        k += 1;
                    }
                }
                j += 1;
            }
            i = j.saturating_add(1);
            continue;
        }
        if tokens[i].kind == TokenKind::Pub && pending_precedence.is_some() {
            i += 1;
            continue;
        }
        let Some(precedence) = pending_precedence.take() else {
            i += 1;
            continue;
        };
        if tokens[i].kind == TokenKind::Impl {
            let limit = (i + 20).min(tokens.len());
            let mut j = i + 1;
            while j < limit {
                if tokens[j].kind == TokenKind::Ident && text(&tokens[j]) == "Operator" {
                    while j < limit && tokens[j].kind != TokenKind::StrLit {
                        j += 1;
                    }
                    if j < limit {
                        let symbol = text(&tokens[j]).trim_matches('"').to_string();
                        // A reserved grammar symbol (`=`, `::`, …) must not enter
                        // the custom-operator table, or it would shadow the
                        // language's own use of the token; the type checker
                        // reports the impl as an error instead.
                        if !crate::syntax::ast::is_reserved_operator(&symbol) {
                            out.insert(symbol, precedence);
                        }
                    }
                    break;
                }
                j += 1;
            }
        }
        i += 1;
    }
    out
}

/// How deeply expressions and blocks may nest. A recursive-descent parser has
/// no natural bound, so deeply nested input — or *unbalanced* input that looks
/// deeply nested, like a run of unclosed `(` — recursed until the stack
/// overflowed and the process aborted, with no diagnostic at all. Real
/// programs nest single digits deep. The bound has to hold on the *smallest*
/// stack the parser runs on, not the main thread's: a 2MB thread (a Rust test
/// thread, and plausibly a language-server worker) gives out well before the
/// 8MB main thread does, so this is set from the former.
const MAX_NESTING: u32 = 128;

pub struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    sink: &'a mut DiagnosticSink,
    custom_operators: HashMap<String, u8>,
    /// Current expression/block nesting, against `MAX_NESTING`.
    depth: u32,
    /// Whether the depth limit has already been reported, so one over-deep
    /// expression yields one diagnostic rather than one per level.
    depth_reported: bool,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str, tokens: Vec<Token>, sink: &'a mut DiagnosticSink) -> Self {
        // Strip comment trivia so the grammar can ignore it. The trailing `Eof`
        // is always kept.
        //
        // `Unknown` goes with it. The lexer has already reported that run of
        // unrecognized input, so every rule that met one added diagnostics
        // about a name or separator it could not find *after* the real cause
        // had been named — one stray token cost three to eight errors
        // depending on which list it landed in. Dropping it here fixes every
        // list at once, rather than teaching each of them the same lesson.
        let tokens: Vec<Token> = tokens
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::Comment | TokenKind::Unknown))
            .collect();
        Parser {
            src,
            tokens,
            pos: 0,
            sink,
            custom_operators: HashMap::new(),
            depth: 0,
            depth_reported: false,
        }
    }

    /// Supply custom textual operators discovered before full expression
    /// parsing. Values use the parser's binding-power scale.
    pub fn with_custom_operators(mut self, operators: &HashMap<String, u8>) -> Self {
        self.custom_operators.clone_from(operators);
        self
    }

    // --- top level ----------------------------------------------------------

    pub fn parse_module(&mut self) -> Module {
        let start = self.span();
        self.expect(TokenKind::Module, "to begin a module");
        let path = self.parse_path();
        self.expect(TokenKind::Semi, "after the module path");

        let mut items = Vec::new();
        while !self.at(TokenKind::Eof) {
            let before = self.pos;
            match self.parse_item() {
                Some(item) => items.push(item),
                None => self.recover_to_item_boundary(),
            }
            // Guarantee forward progress even if a sub-parser consumed nothing.
            if self.pos == before {
                self.bump();
            }
        }
        Module {
            path,
            items,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_item(&mut self) -> Option<Item> {
        let attrs = self.parse_attrs();
        let is_pub = self.eat(TokenKind::Pub);
        let is_extern = self.eat(TokenKind::Extern);

        // `extern "C" { fn ...; }` — a foreign-function block.
        if is_extern && self.at(TokenKind::StrLit) {
            let start = self.span();
            if is_pub {
                self.error_at(
                    self.prev_span(),
                    "an extern block has no visibility; mark its functions `pub` individually",
                );
            }
            let t = self.bump();
            let abi = self.text_of(t.span).trim_matches('"').to_string();
            if abi != "C" {
                self.error_at(t.span, "only the \"C\" ABI is supported");
            }
            self.expect(TokenKind::LBrace, "to open an extern block");
            let mut fns = Vec::new();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let before = self.pos;
                let fn_is_pub = self.eat(TokenKind::Pub);
                if self.eat(TokenKind::Fn) {
                    let fstart = self.span();
                    let name = self.parse_ident();
                    let f = self.parse_fn_after_name(fstart, name, fn_is_pub);
                    if f.body.is_some() {
                        self.error_at(f.name.span, "extern functions have no body");
                    }
                    fns.push(f);
                } else {
                    self.error_here("expected `fn` declarations in an extern block");
                }
                if self.pos == before {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBrace, "to close an extern block");
            return Some(Item::ExternBlock {
                abi,
                fns,
                span: start.to(self.prev_span()),
            });
        }

        if !attrs.is_empty() && !matches!(self.kind(), TokenKind::Entity | TokenKind::Impl) {
            self.error_here("attributes are only allowed on entities and implementations");
        }

        let item = match self.kind() {
            TokenKind::Using => Item::Using(self.parse_using(is_pub)),
            TokenKind::Const => Item::Const(self.parse_const(is_pub)),
            TokenKind::Fn => {
                let start = self.span();
                self.bump();
                let name = self.parse_ident();
                Item::Fn(self.parse_fn_after_name(start, name, is_pub))
            }
            TokenKind::Struct => Item::Struct(self.parse_struct(is_pub)),
            TokenKind::View => Item::View(self.parse_view(is_pub)),
            TokenKind::Enum => Item::Enum(self.parse_enum(is_pub)),
            TokenKind::Entity => Item::Entity(self.parse_entity(attrs, is_pub, is_extern)),
            TokenKind::Impl => {
                if is_pub {
                    self.error_at(
                        self.prev_span(),
                        "an `impl` block has no visibility; mark inherent methods `pub` individually",
                    );
                }
                Item::Impl(self.parse_impl(attrs))
            }
            TokenKind::Trait => Item::Trait(self.parse_trait(is_pub)),
            TokenKind::Attr => Item::AttrDecl(self.parse_attr_decl(is_pub)),
            _ => {
                self.error_here(
                    "expected an item (using, const, fn, struct, view, enum, entity, impl, trait, attr)",
                );
                return None;
            }
        };
        Some(item)
    }

    fn recover_to_item_boundary(&mut self) {
        while !self.at(TokenKind::Eof) {
            if matches!(
                self.kind(),
                TokenKind::Pound
                    | TokenKind::Pub
                    | TokenKind::Extern
                    | TokenKind::Using
                    | TokenKind::Fn
                    | TokenKind::Const
                    | TokenKind::Struct
                    | TokenKind::View
                    | TokenKind::Enum
                    | TokenKind::Entity
                    | TokenKind::Impl
                    | TokenKind::Trait
                    | TokenKind::Attr
            ) {
                return;
            }
            let was_semi = self.at(TokenKind::Semi);
            self.bump();
            if was_semi {
                return;
            }
        }
    }

    // --- attributes ---------------------------------------------------------

    fn parse_attrs(&mut self) -> Vec<Attr> {
        let mut attrs = Vec::new();
        while self.at(TokenKind::Pound) {
            let start = self.span();
            self.bump(); // `#`
            self.expect(TokenKind::LBracket, "to open an attribute");
            let name = self.parse_path();
            let value = if self.eat(TokenKind::Eq) {
                Some(self.parse_expr(false))
            } else {
                None
            };
            self.expect(TokenKind::RBracket, "to close an attribute");
            attrs.push(Attr {
                name,
                value,
                span: start.to(self.prev_span()),
            });
        }
        attrs
    }

    // --- using / const ------------------------------------------------------

    fn parse_using(&mut self, is_pub: bool) -> Using {
        let start = self.span();
        self.bump(); // `using`
        let path = self.parse_path();

        let kind =
            if self.at(TokenKind::ColonColon) && self.kind_at(self.pos + 1) == &TokenKind::LBrace {
                // `using a::b::{ c, d };`
                self.bump(); // `::`
                self.bump(); // `{`
                let mut names = Vec::new();
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    // Operator traits import by their quoted name: `{"+", Boolean}`.
                    names.push(self.parse_trait_name());
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RBrace, "to close an import list");
                UsingKind::Import { base: path, names }
            } else if self.at(TokenKind::Eq) {
                // `using Word = unsigned[32];`
                self.bump(); // `=`
                let name = path.segments.last().cloned().unwrap_or_else(|| Ident {
                    text: String::new(),
                    span: path.span,
                });
                if path.segments.len() != 1 {
                    self.error_at(path.span, "an alias name must be a single identifier");
                }
                let ty = self.parse_type();
                UsingKind::Alias { name, ty }
            } else {
                // `using a::b::C;` — last segment is the imported name.
                let mut segments = path.segments.clone();
                let name = segments.pop().unwrap_or_else(|| Ident {
                    text: String::new(),
                    span: path.span,
                });
                let base = Path {
                    segments,
                    span: path.span,
                };
                UsingKind::Import {
                    base,
                    names: vec![name],
                }
            };
        self.expect(TokenKind::Semi, "after a `using`");
        Using {
            is_pub,
            kind,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_const(&mut self, is_pub: bool) -> ConstDecl {
        let start = self.span();
        self.bump(); // `const`
        let name = self.parse_ident();
        self.expect(TokenKind::Colon, "before a const type");
        let ty = self.parse_type();
        self.expect(TokenKind::Eq, "before a const value");
        let value = self.parse_expr(false);
        self.expect(TokenKind::Semi, "after a const");
        ConstDecl {
            is_pub,
            name,
            ty,
            value,
            span: start.to(self.prev_span()),
        }
    }

    // --- struct / enum ------------------------------------------------------

    fn parse_struct(&mut self, is_pub: bool) -> StructDecl {
        let start = self.span();
        self.bump(); // `struct`
        let name = self.parse_ident();
        let params = self.parse_params_opt();
        // Newtype: `struct B(A);` — a distinct type over `A`'s representation
        // (spec 3.28). The form takes no body, so extension is not even
        // expressible, and it mirrors the constructor it declares: `B(x)`.
        let base = if self.eat(TokenKind::LParen) {
            let base = self.parse_type();
            self.expect(TokenKind::RParen, "to close a newtype base");
            self.expect(TokenKind::Semi, "after a newtype declaration");
            return StructDecl {
                is_pub,
                name,
                params,
                base: Some(base),
                fields: Vec::new(),
                span: start.to(self.prev_span()),
            };
        } else {
            // `struct B : A` was the newtype form until parens replaced it.
            // Recognize it so the migration reads as one clear instruction
            // rather than "expected `{`" three tokens later.
            self.deprecated_colon_base("struct", &name.text)
        };
        self.expect(TokenKind::LBrace, "to open a struct body");
        let mut fields = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let fstart = self.span();
            let field_is_pub = self.eat(TokenKind::Pub);
            let fname = self.parse_ident();
            self.expect(TokenKind::Colon, "before a field type");
            let ty = self.parse_type();
            fields.push(Field {
                is_pub: field_is_pub,
                name: fname,
                ty,
                span: fstart.to(self.prev_span()),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close a struct body");
        StructDecl {
            is_pub,
            name,
            params,
            base,
            fields,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_view(&mut self, is_pub: bool) -> ViewDecl {
        let start = self.span();
        self.bump(); // `view`
        let name = self.parse_ident();
        let params = self.parse_params_opt();
        self.expect(TokenKind::For, "between a view name and its backing struct");
        let target = self.parse_type();
        self.expect(TokenKind::LBrace, "to open a view body");
        let mut fields = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let fstart = self.span();
            // `name direction` — the name leads, as everywhere else. A leading
            // direction is the older `out valid;` form; name the move once
            // instead of failing on `out` as a field name and cascading.
            if let Some(dir) = self.eat_direction() {
                let dir_span = self.prev_span();
                let name = self.parse_ident();
                self.sink.emit(
                    Diagnostic::error("a view field's direction goes after its name")
                        .at(dir_span.to(self.prev_span()))
                        .help(format!(
                            "write `{} {}` — the name leads, as it does in a port",
                            name.text,
                            crate::syntax::pretty::dir_str(dir),
                        )),
                );
                if !self.at(TokenKind::RBrace) && !self.eat(TokenKind::Semi) {
                    self.expect(TokenKind::Comma, "after a view field");
                }
                fields.push(ViewField {
                    dir,
                    name,
                    span: fstart.to(self.prev_span()),
                });
                continue;
            }
            let name = self.parse_ident();
            if self.eat(TokenKind::Colon) {
                self.error_here("view fields inherit their types from the backing struct");
                let _ = self.parse_type();
            }
            let Some(dir) = self.eat_direction() else {
                self.error_here("expected `in`, `out`, or `inout` after a view field name");
                self.bump();
                continue;
            };
            if !self.at(TokenKind::RBrace) {
                self.expect_member_separator("view field");
            }
            fields.push(ViewField {
                dir,
                name,
                span: fstart.to(self.prev_span()),
            });
        }
        self.expect(TokenKind::RBrace, "to close a view body");
        ViewDecl {
            is_pub,
            name,
            params,
            target,
            fields,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_enum(&mut self, is_pub: bool) -> EnumDecl {
        let start = self.span();
        self.bump(); // `enum`
        let name = self.parse_ident();
        // Newtype: `enum Logic(ULogic);` — the same variants under a new
        // nominal type (spec 3.28). Parenthesised like the struct form, and
        // like it takes no body, so an enum can never extend its base. The
        // parens sit on the enum's *name*, not on a variant, so there is no
        // clash with a payload-carrying variant should those ever land.
        let repr = if self.eat(TokenKind::LParen) {
            let base = self.parse_type();
            self.expect(TokenKind::RParen, "to close a newtype base");
            self.expect(TokenKind::Semi, "after a newtype declaration");
            return EnumDecl {
                is_pub,
                name,
                repr: Some(base),
                variants: Vec::new(),
                span: start.to(self.prev_span()),
            };
        } else {
            self.deprecated_colon_base("enum", &name.text)
        };
        self.expect(TokenKind::LBrace, "to open an enum body");
        let mut variants = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let vstart = self.span();
            // Logic-literal variant names (`enum Bit { '0', '1' }`, spec Stage
            // 11) keep their quotes in the name text, matching use-site
            // literals.
            let vname = if self.at(TokenKind::CharacterLit) {
                let t = self.bump();
                Ident {
                    text: self.text_of(t.span).to_string(),
                    span: t.span,
                }
            } else {
                self.parse_ident()
            };
            let value = if self.eat(TokenKind::Eq) {
                Some(self.parse_expr(false))
            } else {
                None
            };
            variants.push(EnumVariant {
                name: vname,
                value,
                span: vstart.to(self.prev_span()),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close an enum body");
        EnumDecl {
            is_pub,
            name,
            repr,
            variants,
            span: start.to(self.prev_span()),
        }
    }

    // --- entity -------------------------------------------------------------

    fn parse_entity(&mut self, attrs: Vec<Attr>, is_pub: bool, is_extern: bool) -> EntityDecl {
        let start = self.span();
        self.bump(); // `entity`
        let name = self.parse_ident();
        let params = self.parse_params_opt();
        self.expect(TokenKind::LBrace, "to open an entity body");
        let mut ports = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            let (port, terminated) = self.parse_port();
            ports.push(port);
            // A port that never reached its `;` leaves the rest of the line in
            // the stream, and each leftover token is then retried as a fresh
            // port — repeating the same three diagnostics per token. Skip to
            // the boundary so one malformed port reports once.
            if !terminated {
                self.recover_to_port_boundary();
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close an entity body");
        EntityDecl {
            attrs,
            is_pub,
            is_extern,
            name,
            params,
            ports,
            span: start.to(self.prev_span()),
        }
    }

    /// Parses one port. The flag reports whether it reached its terminating
    /// `,` — a port that did is a clean stopping point even if it errored
    /// earlier (`a Bit,`), so the caller must not then skip the next port.
    fn parse_port(&mut self) -> (Port, bool) {
        let start = self.span();
        // `name: Type direction` — a port reads like a struct field, with the
        // slot after the type holding either a direction (`clk: Bit in`) or a
        // view (`bus: Stream Source`, consumed by `parse_type`).
        //
        // A leading direction is the older `in clk: Bit;` form. Recognize it
        // and report the move once, rather than letting `in` fail as a port
        // name and cascade three more diagnostics across the same line.
        if let Some(dir) = self.eat_direction() {
            return self.legacy_leading_direction_port(start, dir);
        }
        if self.eat(TokenKind::Pub) {
            self.error_at(
                self.prev_span(),
                "entity ports are already part of the interface; remove `pub`",
            );
        }
        let name = self.parse_ident();
        self.expect(TokenKind::Colon, "before a port type");
        let ty = self.parse_type();
        let dir = self.eat_direction();
        // A trailing comma on the last port is optional, as in a struct.
        let terminated = self.at(TokenKind::RBrace) || self.expect_member_separator("port");
        let port = Port {
            dir,
            name,
            ty,
            span: start.to(self.prev_span()),
        };
        (port, terminated)
    }

    /// The `,` between two members of a brace-delimited body. A `;` there is
    /// the pre-migration separator, so it is consumed with a diagnostic that
    /// names the replacement — the alternative, "expected `,`", is accurate
    /// but leaves the reader to guess that the whole form changed.
    fn expect_member_separator(&mut self, what: &str) -> bool {
        if self.at(TokenKind::Semi) {
            let span = self.span();
            self.bump();
            self.sink.emit(
                Diagnostic::error(format!("a {what} is followed by `,`, not `;`"))
                    .at(span)
                    .help(
                        "every brace-delimited declaration in the language separates \
                         its members with commas, and the last one carries none",
                    ),
            );
            return true;
        }
        self.expect(TokenKind::Comma, &format!("after a {what}"))
    }

    /// The pre-migration port form, `in clk: Bit;`. Parsed in full so the
    /// entity still elaborates and later stages report on it, with one
    /// diagnostic naming the move instead of a cascade. The old `;` is
    /// accepted here as a terminator for the same reason — a file written
    /// against the old syntax should produce the one error that explains it.
    fn legacy_leading_direction_port(&mut self, start: Span, dir: Direction) -> (Port, bool) {
        let dir_span = self.prev_span();
        let name = self.parse_ident();
        self.expect(TokenKind::Colon, "before a port type");
        let ty = self.parse_type();
        self.sink.emit(
            Diagnostic::error("a port's direction goes after its type")
                .at(dir_span.to(self.prev_span()))
                .help(format!(
                    "write `{}: {} {}` — a port reads like a struct field, with \
                     the slot after the type holding the direction",
                    name.text,
                    crate::syntax::pretty::type_str(&ty),
                    crate::syntax::pretty::dir_str(dir),
                )),
        );
        let terminated = self.at(TokenKind::RBrace)
            || self.eat(TokenKind::Semi)
            || self.expect(TokenKind::Comma, "after a port");
        let port = Port {
            dir: Some(dir),
            name,
            ty,
            span: start.to(self.prev_span()),
        };
        (port, terminated)
    }

    /// Panic-mode recovery inside an entity body: consume through the next
    /// `,` so the following port is parsed from a clean start.
    fn recover_to_port_boundary(&mut self) {
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.eat(TokenKind::Comma) {
                return;
            }
            self.bump();
        }
    }

    // --- impl ---------------------------------------------------------------

    fn parse_impl(&mut self, attrs: Vec<Attr>) -> ImplDecl {
        let start = self.span();
        self.bump(); // `impl`

        // Rust-style trait-impl parameters precede the trait:
        // `impl<T: Resolve> Resolve for T[]`. Keep the older target-impl form
        // `impl<W: integer> Counter<W> { ... }` below, where the parameters follow
        // the target name.
        let mut params = if self.at(TokenKind::Lt) {
            self.parse_params()
        } else {
            Params::default()
        };

        // `impl "+" for T` names an operator trait by its quoted string.
        let head_path = if self.at(TokenKind::StrLit) {
            let name = self.parse_trait_name();
            let span = name.span;
            Path {
                segments: vec![name],
                span,
            }
        } else {
            self.parse_path()
        };
        // A `<name: bound>` list declares impl parameters; a `<expr>` list is
        // generic arguments that stay inside the target type.
        let head_args = if self.at(TokenKind::Lt) {
            if params.params.is_empty() && self.angle_is_param_list(self.pos) {
                params = self.parse_params();
                None
            } else {
                Some(self.parse_generic_args())
            }
        } else {
            None
        };

        if self.at(TokenKind::For) {
            // `impl Trait<...> for [dir] Target` — `<...>` is the trait's type
            // arguments (`impl Add<integer> for Complex`).
            self.bump();
            let trait_ = Some(head_path);
            let target = self.parse_type();
            let items = self.parse_impl_body();
            return ImplDecl {
                attrs,
                params,
                trait_,
                trait_args: head_args.unwrap_or_default(),
                target,
                items,
                span: start.to(self.prev_span()),
            };
        }

        // No trait: the head is the target type.
        let head_span = head_path.span;
        let mut target = Type::Path(head_path);
        if let Some(args) = head_args {
            target = Type::Generic {
                base: Box::new(target),
                args,
                span: head_span,
            };
        }
        // A view applied to its backing struct: `impl Stream<T> Source`. The
        // backing type leads and the view follows, as in a port's type slot.
        if !self.at(TokenKind::LBrace) {
            let backing = target;
            let view = match self.parse_type_core() {
                Type::Path(p) => p,
                _ => {
                    self.error_here("a view name after its backing type cannot have arguments");
                    Path {
                        segments: Vec::new(),
                        span: head_span,
                    }
                }
            };
            target = Type::View {
                view,
                target: Box::new(backing),
                span: head_span.to(self.prev_span()),
            };
        }
        let items = self.parse_impl_body();
        ImplDecl {
            attrs,
            params,
            trait_: None,
            trait_args: Vec::new(),
            target,
            items,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_impl_body(&mut self) -> Vec<ImplItem> {
        self.expect(TokenKind::LBrace, "to open an impl body");
        let mut items = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            if let Some(it) = self.parse_impl_item() {
                items.push(it);
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close an impl body");
        items
    }

    fn parse_impl_item(&mut self) -> Option<ImplItem> {
        // `#[external_clock] let p: Pll = { .. };` — per-instance attributes.
        let attrs = self.parse_attrs();
        if !attrs.is_empty() && !self.at(TokenKind::Let) {
            self.error_here("attributes on impl items are only allowed on `let` declarations");
        }
        let is_pub = self.eat(TokenKind::Pub);
        if is_pub && !self.at(TokenKind::Fn) {
            self.error_at(
                self.prev_span(),
                "only functions may be `pub` inside an implementation",
            );
        }
        match self.kind() {
            TokenKind::Const => Some(ImplItem::Const(self.parse_const(false))),
            // `let value: T = e;` is state/signal; `fn send(self, ...) { ... }`
            // is a method.
            TokenKind::Let => {
                let start = self.span();
                self.bump();
                let name = self.parse_ident();
                Some(ImplItem::Let(self.parse_let_rest(attrs, start, name)))
            }
            TokenKind::Fn => {
                let start = self.span();
                self.bump();
                let name = self.parse_ident();
                Some(ImplItem::Fn(self.parse_fn_after_name(start, name, is_pub)))
            }
            TokenKind::Process => {
                let start = self.span();
                self.bump();
                let name = self.at(TokenKind::Ident).then(|| self.parse_ident());
                let body = self.parse_block();
                Some(ImplItem::Process(ProcessDecl {
                    name,
                    span: start.to(body.span),
                    body,
                }))
            }
            TokenKind::In | TokenKind::Out | TokenKind::Inout => {
                // Bus-mode leaf direction: `in clk;`.
                let start = self.span();
                let dir = self.eat_direction().unwrap();
                let name = self.parse_ident();
                self.expect(TokenKind::Semi, "after a view field");
                Some(ImplItem::ModeField {
                    dir,
                    name,
                    span: start.to(self.prev_span()),
                })
            }
            _ => Some(ImplItem::Stmt(self.parse_stmt())),
        }
    }

    fn parse_let_after_name(&mut self, start: Span, name: Ident) -> LetDecl {
        self.parse_let_rest(Vec::new(), start, name)
    }

    fn parse_let_rest(&mut self, attrs: Vec<Attr>, start: Span, name: Ident) -> LetDecl {
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let value = if self.eat(TokenKind::Eq) {
            Some(self.parse_expr(false))
        } else {
            None
        };
        self.expect(TokenKind::Semi, "after a `let`");
        LetDecl {
            attrs,
            name,
            ty,
            value,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_fn_after_name(&mut self, start: Span, name: Ident, is_pub: bool) -> FnDecl {
        let mut generics = self.parse_params_opt();
        let params = self.parse_fn_params();
        let ret = if self.eat(TokenKind::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };
        self.parse_where_into(&mut generics);
        let body = if self.at(TokenKind::LBrace) {
            Some(self.parse_block())
        } else {
            self.expect(TokenKind::Semi, "after a method signature");
            None
        };
        FnDecl {
            is_pub,
            name,
            generics,
            params,
            ret,
            body,
            span: start.to(self.prev_span()),
        }
    }

    /// Parse an optional `where` clause and desugar its predicates onto the
    /// declaration's generic parameters: `where T: Ord` sets the bound of the
    /// param `T`, so `fn f<T>(..) where T: Ord` == `fn f<T: Ord>(..)`.
    fn parse_where_into(&mut self, generics: &mut Params) {
        if !(self.at(TokenKind::Ident) && self.cur_text() == "where") {
            return;
        }
        self.bump(); // `where`
        while !self.at(TokenKind::LBrace) && !self.at(TokenKind::Semi) && !self.at(TokenKind::Eof) {
            let tspan = self.span();
            let target = self.parse_type();
            self.expect(TokenKind::Colon, "in a `where` predicate");
            let bound = self.parse_type();
            // Attach the bound to the matching generic parameter.
            let head = match &target {
                Type::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
                _ => None,
            };
            match head.and_then(|h| generics.params.iter_mut().find(|p| p.name.text == h)) {
                Some(p) => p.bound = Some(bound),
                None => self.error_at(tspan, "`where` names an unknown type parameter"),
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
    }

    fn parse_fn_params(&mut self) -> Vec<FnParam> {
        self.expect(TokenKind::LParen, "to open a parameter list");
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let pstart = self.span();
            if self.at(TokenKind::SelfKw) {
                self.bump();
                params.push(FnParam {
                    is_self: true,
                    name: None,
                    ty: None,
                    span: pstart.to(self.prev_span()),
                });
            } else {
                let name = self.parse_ident();
                let ty = if self.eat(TokenKind::Colon) {
                    Some(self.parse_type())
                } else {
                    None
                };
                params.push(FnParam {
                    is_self: false,
                    name: Some(name),
                    ty,
                    span: pstart.to(self.prev_span()),
                });
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "to close a parameter list");
        params
    }

    // --- trait / attr decl --------------------------------------------------

    /// A trait name: an identifier, or a quoted operator string for operator
    /// traits (`trait "+"`, spec 3.25).
    fn parse_trait_name(&mut self) -> Ident {
        if self.at(TokenKind::StrLit) {
            // Pre-Rust-style operator traits were quoted (`impl "+" for T`).
            let t = self.bump();
            let text = self.text_of(t.span).trim_matches('"').to_string();
            self.error_at(
                t.span,
                format!(
                    "quoted operator traits were removed; use `Operator<\"{text}\", Input, Output>`"
                ),
            );
            Ident {
                text: "Operator".to_string(),
                span: t.span,
            }
        } else {
            self.parse_ident()
        }
    }

    fn parse_trait(&mut self, is_pub: bool) -> TraitDecl {
        let start = self.span();
        self.bump(); // `trait`
        let name = self.parse_trait_name();
        let params = self.parse_params_opt();
        self.expect(TokenKind::LBrace, "to open a trait body");
        let mut items = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            let istart = self.span();
            if self.eat(TokenKind::Pub) {
                self.error_at(
                    self.prev_span(),
                    "trait methods inherit the trait's visibility; remove `pub`",
                );
            }
            if !self.eat(TokenKind::Fn) {
                self.error_here("expected a `fn` method signature in trait body");
                break;
            }
            let mname = self.parse_ident();
            items.push(self.parse_fn_after_name(istart, mname, false));
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close a trait body");
        TraitDecl {
            is_pub,
            name,
            params,
            items,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_attr_decl(&mut self, is_pub: bool) -> AttrDecl {
        let start = self.span();
        self.bump(); // `attr`
        let name = self.parse_ident();
        self.expect(TokenKind::Colon, "before an attribute type");
        let ty = self.parse_type();
        self.expect(TokenKind::For, "before attribute targets");
        // Targets are a fixed vocabulary that includes keywords (`entity`,
        // `let`, `port`, `instance`, ...), so accept any name-like token.
        let mut targets = Vec::new();
        loop {
            targets.push(self.parse_word());
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Semi, "after an attribute declaration");
        AttrDecl {
            is_pub,
            name,
            ty,
            targets,
            span: start.to(self.prev_span()),
        }
    }

    // --- statements ---------------------------------------------------------

    fn parse_block(&mut self) -> Block {
        let start = self.span();
        if self.enter_nesting() {
            self.expect(TokenKind::LBrace, "to open a block");
            return Block {
                stmts: Vec::new(),
                span: start,
            };
        }
        let parsed = self.parse_block_inner(start);
        self.depth -= 1;
        parsed
    }

    fn parse_block_inner(&mut self, start: crate::diag::Span) -> Block {
        self.expect(TokenKind::LBrace, "to open a block");
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            stmts.push(self.parse_stmt());
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close a block");
        Block {
            stmts,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_stmt(&mut self) -> Stmt {
        match self.kind() {
            TokenKind::Let => {
                let start = self.span();
                self.bump();
                let name = self.parse_ident();
                Stmt::Let(self.parse_let_after_name(start, name))
            }
            TokenKind::If => Stmt::If(self.parse_if()),
            TokenKind::Match => Stmt::Match(self.parse_match()),
            TokenKind::For => self.parse_for(),
            TokenKind::Return => {
                let start = self.span();
                self.bump();
                let value = if self.at(TokenKind::Semi) {
                    None
                } else {
                    Some(self.parse_expr(false))
                };
                self.expect(TokenKind::Semi, "after a `return`");
                Stmt::Return {
                    value,
                    span: start.to(self.prev_span()),
                }
            }
            // `wait <expr>;` / `await <expr>;` timing primitives (no parens):
            // modeled as a call. `await 10ns` advances time; `await clk.rising()`
            // waits for an edge; `await cond` waits until a condition holds.
            TokenKind::Ident if self.cur_text() == "wait" || self.cur_text() == "await" => {
                let start = self.span();
                // `await` is the one timing primitive; `wait` errors but is
                // parsed as `await` so later stages still run (best-effort).
                if self.cur_text() == "wait" {
                    self.error_here("`wait` was removed; use `await <duration>`");
                }
                let mut ident = self.parse_ident();
                ident.text = "await".to_string();
                let callee = Expr::Path(Path {
                    segments: vec![ident],
                    span: start,
                });
                let arg = self.parse_expr(false);
                self.expect(TokenKind::Semi, "after a timing primitive");
                let span = start.to(self.prev_span());
                Stmt::Expr(Expr::Call {
                    callee: Box::new(callee),
                    type_args: Vec::new(),
                    args: vec![arg],
                    bang: false,
                    span,
                })
            }
            _ => self.parse_expr_or_assign_stmt(),
        }
    }

    fn parse_expr_or_assign_stmt(&mut self) -> Stmt {
        let start = self.span();
        let lhs = self.parse_expr(false);
        if self.eat(TokenKind::Eq) {
            let value = self.parse_expr(false);
            // Optional VHDL-style delay: `clk = !clk after 5ns;`.
            let after = if self.at(TokenKind::Ident) && self.cur_text() == "after" {
                self.bump();
                Some(self.parse_expr(false))
            } else {
                None
            };
            self.expect(TokenKind::Semi, "after an assignment");
            Stmt::Assign {
                target: lhs,
                value,
                after,
                span: start.to(self.prev_span()),
            }
        } else if let Some(op) = Self::compound_binop_impl(self.kind()) {
            // `x += e` desugars to `x = x + e` (spec 3.12).
            self.bump();
            let rhs = self.parse_expr(false);
            self.expect(TokenKind::Semi, "after a compound assignment");
            let span = start.to(self.prev_span());
            let value = Expr::Binary {
                op,
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(rhs),
                span,
            };
            Stmt::Assign {
                target: lhs,
                value,
                after: None,
                span,
            }
        } else {
            // No implicit tail-expression returns: every expression statement is
            // terminated by `;`. A function returns a value via `return`.
            self.expect(TokenKind::Semi, "after an expression statement");
            Stmt::Expr(lhs)
        }
    }

    fn compound_binop_impl(k: &TokenKind) -> Option<BinOp> {
        Some(match k {
            TokenKind::PlusEq => BinOp::Add,
            TokenKind::MinusEq => BinOp::Sub,
            TokenKind::StarEq => BinOp::Mul,
            TokenKind::SlashEq => BinOp::Div,
            TokenKind::AmpEq => BinOp::And,
            TokenKind::PipeEq => BinOp::Or,
            _ => return None,
        })
    }

    fn parse_if(&mut self) -> IfStmt {
        let start = self.span();
        self.bump(); // `if`
        let cond = self.parse_expr(true);
        let then = self.parse_block();
        let else_ = if self.eat(TokenKind::Else) {
            if self.at(TokenKind::If) {
                Some(Box::new(ElseBranch::If(self.parse_if())))
            } else {
                Some(Box::new(ElseBranch::Block(self.parse_block())))
            }
        } else {
            None
        };
        IfStmt {
            cond,
            then,
            else_,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_match(&mut self) -> MatchStmt {
        let start = self.span();
        self.bump(); // `match`
        let scrutinee = self.parse_expr(true);
        self.expect(TokenKind::LBrace, "to open a match body");
        let mut arms = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            let astart = self.span();
            let pattern = self.parse_pattern();
            self.expect(TokenKind::FatArrow, "after a match pattern");
            let body = if self.at(TokenKind::LBrace) {
                self.parse_block()
            } else {
                // Single statement arm: `"00--" => op = Op::Alu,`.
                let sstart = self.span();
                let stmt = self.parse_arm_single_stmt();
                Block {
                    stmts: vec![stmt],
                    span: sstart.to(self.prev_span()),
                }
            };
            self.eat(TokenKind::Comma);
            arms.push(MatchArm {
                pattern,
                body,
                span: astart.to(self.prev_span()),
            });
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close a match body");
        MatchStmt {
            scrutinee,
            arms,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_arm_single_stmt(&mut self) -> Stmt {
        let start = self.span();
        // A `,`-terminated arm body: `=> return e`, `=> a = b`, or `=> e`.
        if self.at(TokenKind::Return) {
            self.bump();
            let value = if self.at(TokenKind::Comma) || self.at(TokenKind::RBrace) {
                None
            } else {
                Some(self.parse_expr(false))
            };
            return Stmt::Return {
                value,
                span: start.to(self.prev_span()),
            };
        }
        let lhs = self.parse_expr(false);
        if self.eat(TokenKind::Eq) {
            let value = self.parse_expr(false);
            Stmt::Assign {
                target: lhs,
                value,
                after: None,
                span: start.to(self.prev_span()),
            }
        } else {
            Stmt::Expr(lhs)
        }
    }

    fn parse_for(&mut self) -> Stmt {
        let start = self.span();
        self.bump(); // `for`
        let var = self.parse_ident();
        self.expect(TokenKind::In, "after the loop variable");
        let range = self.parse_expr(true);
        let body = self.parse_block();
        Stmt::For {
            var,
            range,
            body,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_pattern(&mut self) -> Pattern {
        let start = self.span();
        let first = self.parse_pattern_atom();
        if !self.at(TokenKind::Pipe) {
            return first;
        }
        // `A | B | C`: an or-pattern.
        let mut alts = vec![first];
        while self.eat(TokenKind::Pipe) {
            alts.push(self.parse_pattern_atom());
        }
        Pattern::Or {
            alts,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_pattern_atom(&mut self) -> Pattern {
        match self.kind() {
            TokenKind::Ident if self.cur_text() == "_" => {
                self.bump();
                Pattern::Wildcard
            }
            // A radix bit pattern `x"A?"` / `o"7?"` (spec 3.22): a one-letter
            // prefix glued to a string; `?` masks one radix group (nibble/triad)
            // as a don't-care.
            //
            // Any letter parses, exactly as in expression position: which
            // prefixes exist is std's to say, and which the compiler can
            // evaluate is `RADIX_PREFIXES`'. Type checking rejects the rest —
            // an undecodable pattern is already an error there. Matching
            // `"x" | "o"` here made an unsupported prefix a raw parse error in
            // a `match` and a clean diagnostic in an expression.
            TokenKind::Ident
                if is_prefix_letter(self.cur_text())
                    && self.kind_at(self.pos + 1) == &TokenKind::StrLit
                    && self.span_at(self.pos + 1).start == self.span().end =>
            {
                let p = self.bump();
                let base = self.text_of(p.span).to_string();
                let t = self.bump();
                let digits = self.text_of(t.span).trim_matches('"');
                Pattern::BitPattern {
                    text: format!("{base}\"{digits}\""),
                    span: p.span.to(t.span),
                }
            }
            // A character literal (`'0'`, `'Z'`) selects a variant of a
            // char-valued enum, `Logic` above all. The same literal already
            // works in expression position (`l == '0'`), and a `match` over an
            // enum is spec 3.22; without this arm `Logic` — the one enum every
            // design uses — could not be matched on at all.
            TokenKind::CharacterLit => {
                let t = self.bump();
                let text = self.text_of(t.span);
                let ch = text.chars().nth(1).unwrap_or('?');
                Pattern::CharLit { ch, span: t.span }
            }
            // A bare-string bit pattern `"1-1-0000"` (spec 3.22): each char is
            // one bit, `-` (the `std_ulogic` don't-care) matches either value.
            // This replaces the old `b"…"` prefix form.
            TokenKind::StrLit => {
                let t = self.bump();
                let digits = self.text_of(t.span).trim_matches('"');
                Pattern::BitPattern {
                    text: format!("\"{digits}\""),
                    span: t.span,
                }
            }
            // An integer literal (`5`) or inclusive range (`0..9`, `-1..1`).
            TokenKind::Int | TokenKind::Minus => {
                let start = self.span();
                let lo = self.parse_pattern_int();
                let hi = if self.eat(TokenKind::DotDot) {
                    self.parse_pattern_int()
                } else {
                    lo
                };
                Pattern::Range {
                    lo,
                    hi,
                    span: start.to(self.prev_span()),
                }
            }
            _ => Pattern::Path(self.parse_path()),
        }
    }

    /// A (possibly negative, hex/binary/decimal) integer literal in a pattern.
    fn parse_pattern_int(&mut self) -> i64 {
        let neg = self.eat(TokenKind::Minus);
        let t = self.bump();
        let txt = self.text_of(t.span).replace('_', "");
        let magnitude = if let Some(h) = txt.strip_prefix("0x").or_else(|| txt.strip_prefix("0X")) {
            i128::from_str_radix(h, 16)
        } else if let Some(b) = txt.strip_prefix("0b").or_else(|| txt.strip_prefix("0B")) {
            i128::from_str_radix(b, 2)
        } else {
            txt.parse()
        };
        let value = magnitude.map(|value| if neg { -value } else { value });
        match value.ok().and_then(|value| i64::try_from(value).ok()) {
            Some(value) => value,
            None => {
                self.error_at(t.span, "integer pattern is outside the supported i64 range");
                0
            }
        }
    }

    // --- expressions (Pratt) ------------------------------------------------

    fn parse_expr(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
        if self.enter_nesting() {
            // Too deep: stop descending and hand back a placeholder. The
            // caller's error recovery consumes the rest.
            return Expr::Int {
                text: String::new(),
                span: start,
            };
        }
        let parsed = self.parse_expr_inner(no_struct, start);
        self.depth -= 1;
        parsed
    }

    fn parse_expr_inner(&mut self, no_struct: bool, start: crate::diag::Span) -> Expr {
        if self.eat(TokenKind::DotDot) {
            if self.eat(TokenKind::Eq) {
                self.error_here("Siox ranges are already inclusive; use `..` instead of `..=`");
            }
            let hi = self
                .range_bound_follows()
                .then(|| Box::new(self.parse_bin(0, no_struct)));
            return Expr::PartialRange {
                lo: None,
                hi,
                span: start.to(self.prev_span()),
            };
        }
        let lhs = self.parse_bin(0, no_struct);
        if self.at(TokenKind::DotDot) {
            self.bump();
            if self.eat(TokenKind::Eq) {
                self.error_here("Siox ranges are already inclusive; use `..` instead of `..=`");
            }
            if self.range_bound_follows() {
                let hi = self.parse_bin(0, no_struct);
                Expr::Range {
                    lo: Box::new(lhs),
                    hi: Box::new(hi),
                    span: start.to(self.prev_span()),
                }
            } else {
                Expr::PartialRange {
                    lo: Some(Box::new(lhs)),
                    hi: None,
                    span: start.to(self.prev_span()),
                }
            }
        } else {
            lhs
        }
    }

    fn range_bound_follows(&self) -> bool {
        !matches!(
            self.kind(),
            TokenKind::RBracket
                | TokenKind::RParen
                | TokenKind::RBrace
                | TokenKind::LBrace
                | TokenKind::Comma
                | TokenKind::Semi
                | TokenKind::Gt
                | TokenKind::FatArrow
                | TokenKind::Eof
        )
    }

    fn parse_bin(&mut self, min_bp: u8, no_struct: bool) -> Expr {
        let start = self.span();
        let mut lhs = self.parse_unary(no_struct);
        loop {
            let (op, lbp, rbp, consumed) = match self.kind() {
                TokenKind::Star => (BinOp::Mul, 90, 91, 1),
                TokenKind::Slash => (BinOp::Div, 90, 91, 1),
                TokenKind::Plus => (BinOp::Add, 80, 81, 1),
                TokenKind::Minus => (BinOp::Sub, 80, 81, 1),
                TokenKind::Shl => (BinOp::Shl, 70, 71, 1),
                TokenKind::Shr => (BinOp::Shr, 70, 71, 1),
                TokenKind::Lt => (BinOp::Lt, 60, 61, 1),
                TokenKind::Gt => (BinOp::Gt, 60, 61, 1),
                TokenKind::LtEq => (BinOp::Le, 60, 61, 1),
                TokenKind::GtEq => (BinOp::Ge, 60, 61, 1),
                TokenKind::EqEq => (BinOp::Eq, 50, 51, 1),
                TokenKind::BangEq => (BinOp::Ne, 50, 51, 1),
                // Core and declared custom textual operators lex as identifiers.
                TokenKind::Ident => match self.cur_text() {
                    "and" => (BinOp::And, 40, 41, 1),
                    "or" => (BinOp::Or, 30, 31, 1),
                    _ => match self.custom_operator_at() {
                        Some(found) => found,
                        None => break,
                    },
                },
                _ => match self.custom_operator_at() {
                    Some(found) => found,
                    // Punctuation that is not core syntax can only have been
                    // meant as an operator, so breaking here would end the
                    // expression and report the leftovers instead of the
                    // cause. Name it, and bind it tightest so it is always
                    // consumed at the innermost level — one diagnostic, and
                    // the rest of the expression still parses.
                    None if self.at(TokenKind::CustomOp) => {
                        let symbol = self.cur_text().to_string();
                        self.undeclared_operator(&symbol);
                        (
                            BinOp::Custom {
                                symbol,
                                precedence: u8::MAX,
                            },
                            u8::MAX,
                            u8::MAX,
                            1,
                        )
                    }
                    None => break,
                },
            };
            if lbp < min_bp {
                break;
            }
            for _ in 0..consumed {
                self.bump();
            }
            let rhs = self.parse_bin(rbp, no_struct);
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: start.to(self.prev_span()),
            };
        }
        lhs
    }

    /// Punctuation used as an operator that no `impl Operator<..>` declares.
    /// The parser learns its operators from those impls, so an undeclared one
    /// is indistinguishable from the end of an expression — which is why the
    /// symbol itself has to be named here rather than left to the caller.
    fn undeclared_operator(&mut self, symbol: &str) {
        let span = self.span();
        self.sink.emit(
            Diagnostic::error(format!("no operator `{symbol}` is declared"))
                .at(span)
                .help(format!(
                    "operators come from the standard library, not the grammar: \
                     declare one with `#[precedence = N] impl Operator<\"{symbol}\", \
                     Rhs, Output> for Lhs`"
                )),
        );
    }

    /// Longest declared custom operator beginning at the current token. This
    /// supports both word operators and punctuation split across lexer tokens.
    fn custom_operator_at(&self) -> Option<(BinOp, u8, u8, usize)> {
        let start = self.span().start as usize;
        let tail = self.src.get(start..)?;
        let (symbol, &precedence) = self
            .custom_operators
            .iter()
            .filter(|(symbol, _)| tail.starts_with(symbol.as_str()))
            .max_by_key(|(symbol, _)| symbol.len())?;
        let wanted_end = start + symbol.len();
        let mut consumed = 0;
        let mut end = start;
        while self.pos + consumed < self.tokens.len() && end < wanted_end {
            let span = self.tokens[self.pos + consumed].span;
            if span.start as usize != end {
                return None;
            }
            end = span.end as usize;
            consumed += 1;
        }
        (end == wanted_end).then(|| {
            (
                BinOp::Custom {
                    symbol: symbol.clone(),
                    precedence,
                },
                precedence,
                precedence.saturating_add(1),
                consumed,
            )
        })
    }

    fn parse_unary(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
        // Rust-style if-expression: `if c { a } else { b }` (else required).
        if self.at(TokenKind::If) {
            return self.parse_if_expr();
        }
        // Match-expression: `match s { A => e1, _ => e2 }` in value position.
        if self.at(TokenKind::Match) {
            let m = self.parse_match();
            return Expr::Match {
                scrutinee: Box::new(m.scrutinee),
                arms: m.arms,
                span: m.span,
            };
        }
        // `!x` is what every C/Verilog/Rust habit reaches for, but `!` here
        // only ever marks a macro call (`assert!`). Take it as negation and say
        // so, instead of cascading "expected an expression" through the rest of
        // the statement.
        if self.at(TokenKind::Bang) && self.kind_at(self.pos + 1) != &TokenKind::LParen {
            let bang = self.bump();
            self.error_at(bang.span, "unary `!` is not an operator; use `not`");
            let rhs = self.parse_unary(no_struct);
            return Expr::Unary {
                op: UnOp::Not,
                rhs: Box::new(rhs),
                span: start.to(self.prev_span()),
            };
        }
        let op = match self.kind() {
            TokenKind::Minus => Some(UnOp::Neg),
            // `not` is the textual logical-negation prefix operator.
            TokenKind::Ident if self.cur_text() == "not" => Some(UnOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let rhs = self.parse_unary(no_struct);
            return Expr::Unary {
                op,
                rhs: Box::new(rhs),
                span: start.to(self.prev_span()),
            };
        }
        self.parse_postfix(no_struct)
    }

    /// `if c { a } else { b }` / `if c { a } else if d { b } else { c }` in
    /// expression position. A value-producing `if` must be total, so `else`
    /// is required; each branch is a single expression.
    fn parse_if_expr(&mut self) -> Expr {
        let start = self.span();
        self.bump(); // `if`
        let cond = self.parse_expr(true);
        self.expect(TokenKind::LBrace, "to open an if-expression branch");
        let then = self.parse_expr(false);
        self.expect(TokenKind::RBrace, "to close an if-expression branch");
        self.expect(
            TokenKind::Else,
            "— an `if` used as a value needs an `else` branch",
        );
        let els = if self.at(TokenKind::If) {
            self.parse_if_expr()
        } else {
            self.expect(TokenKind::LBrace, "to open the else branch");
            let e = self.parse_expr(false);
            self.expect(TokenKind::RBrace, "to close the else branch");
            e
        };
        Expr::IfExpr {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
            span: start.to(self.prev_span()),
        }
    }

    fn parse_postfix(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
        let mut e = self.parse_primary(no_struct);
        loop {
            match self.kind() {
                TokenKind::Dot => {
                    self.bump();
                    let field = self.parse_ident();
                    e = Expr::Field {
                        base: Box::new(e),
                        field,
                        span: start.to(self.prev_span()),
                    };
                }
                // `sig'event` — a VHDL-style attribute accessor (spec 3.9). The
                // tick is exclusively for attributes; `::` is namespace/type
                // selection, absorbed by `parse_primary`.
                TokenKind::Tick => {
                    self.bump();
                    let attr = self.parse_ident();
                    e = Expr::SysAttr {
                        base: Box::new(e),
                        attr,
                        span: start.to(self.prev_span()),
                    };
                }
                TokenKind::LBracket => {
                    self.bump();
                    let index = self.parse_expr(false);
                    self.expect(TokenKind::RBracket, "to close an index");
                    e = Expr::Index {
                        base: Box::new(e),
                        index: Box::new(index),
                        span: start.to(self.prev_span()),
                    };
                }
                TokenKind::LParen => {
                    let args = self.parse_call_args();
                    e = Expr::Call {
                        callee: Box::new(e),
                        type_args: Vec::new(),
                        args,
                        bang: false,
                        span: start.to(self.prev_span()),
                    };
                }
                TokenKind::Lt if matches!(e, Expr::Path(_)) && self.angle_then_lparen(self.pos) => {
                    let type_args = self.parse_call_type_args();
                    let args = self.parse_call_args();
                    e = Expr::Call {
                        callee: Box::new(e),
                        type_args,
                        args,
                        bang: false,
                        span: start.to(self.prev_span()),
                    };
                }
                // `assert!(...)` — bang call.
                TokenKind::Bang if self.kind_at(self.pos + 1) == &TokenKind::LParen => {
                    self.bump(); // `!`
                    let args = self.parse_call_args();
                    e = Expr::Call {
                        callee: Box::new(e),
                        type_args: Vec::new(),
                        args,
                        bang: true,
                        span: start.to(self.prev_span()),
                    };
                }
                _ => break,
            }
        }
        e
    }

    fn parse_call_args(&mut self) -> Vec<Expr> {
        self.expect(TokenKind::LParen, "to open a call");
        let mut args = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            args.push(self.parse_expr(false));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "to close a call");
        args
    }

    /// Explicit type arguments on a constructor-like call: `read<T>(path)`.
    /// These are deliberately type-only; value-generic function arguments are
    /// inferred from ordinary parameters in Phase 1.
    fn parse_call_type_args(&mut self) -> Vec<Type> {
        self.expect(TokenKind::Lt, "to open call type arguments");
        let mut args = Vec::new();
        while !self.at_generic_end() && !self.at(TokenKind::Eof) {
            args.push(self.parse_type());
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.close_generic("to close call type arguments");
        args
    }

    fn parse_primary(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
        match self.kind() {
            TokenKind::Int | TokenKind::Float => {
                let t = self.bump();
                let text = self.text_of(t.span).to_string();
                // An identifier glued to the number is a unit/type suffix:
                // `1ns`, `10MHz`, `5i` (the lexer splits them into two tokens).
                if self.at(TokenKind::Ident) && self.span().start == t.span.end {
                    let suffix = self.parse_ident();
                    let span = t.span.to(self.prev_span());
                    return Expr::SuffixLit { text, suffix, span };
                }
                Expr::Int { text, span: t.span }
            }
            TokenKind::CharacterLit => {
                let t = self.bump();
                let text = self.text_of(t.span);
                let ch = text.chars().nth(1).unwrap_or('?');
                Expr::CharLit { ch, span: t.span }
            }
            TokenKind::StrLit => {
                let t = self.bump();
                let raw = self.text_of(t.span);
                let text = unescape(raw.trim_matches('"'));
                Expr::StrLit { text, span: t.span }
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr(false);
                self.expect(TokenKind::RParen, "to close a parenthesized expression");
                inner
            }
            TokenKind::Ident if self.cur_text() == "true" || self.cur_text() == "false" => {
                // `true`/`false` are not primitives — they are the two variants
                // of std's `enum Bool`. Desugar to the `Bool::<variant>` path so
                // they resolve, type, and evaluate through the ordinary
                // enum-variant machinery; std owns their values, not the parser.
                let variant = self.cur_text().to_string();
                let t = self.bump();
                let seg = |text: &str| Ident {
                    text: text.to_string(),
                    span: t.span,
                };
                Expr::Path(Path {
                    segments: vec![seg("Bool"), seg(&variant)],
                    span: t.span,
                })
            }
            // A one-letter prefix glued to a string is a bit-string literal:
            // `x"123ABC"` (hex) / `o"17"` (octal). The parser only recognizes
            // the *shape* (a single-letter prefix); which prefixes are valid is
            // owned by std's `impl Prefix for T` and checked in typeck. A bare
            // `"1010"` needs no prefix (it reads as a Logic array by context).
            TokenKind::Ident
                if is_prefix_letter(self.cur_text())
                    && self.kind_at(self.pos + 1) == &TokenKind::StrLit
                    && self.span_at(self.pos + 1).start == self.span().end =>
            {
                let p = self.bump();
                let base = self.text_of(p.span).chars().next().unwrap_or('x');
                let t = self.bump();
                let digits = self.text_of(t.span).trim_matches('"').to_string();
                Expr::BitStrLit {
                    base,
                    digits,
                    span: p.span.to(t.span),
                }
            }
            TokenKind::Ident | TokenKind::SelfKw => self.parse_path_expr_or_construct(no_struct),
            // A leading `{`: `{ .field = ... }` is a name-less struct literal
            // (typed from context); `{ a, b }` is a bit concatenation.
            TokenKind::LBrace
                if matches!(
                    self.kind_at(self.pos + 1),
                    TokenKind::Dot | TokenKind::DotDot
                ) =>
            {
                self.parse_construct(start, None)
            }
            TokenKind::LBrace => self.parse_concat(start),
            // `[a, b, c]` is an array literal (spec 3.23), distinct from `{..}`
            // concatenation and from `t[i]` indexing.
            TokenKind::LBracket => {
                self.bump();
                let mut elems = Vec::new();
                while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                    elems.push(self.parse_expr(false));
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RBracket, "to close an array literal");
                Expr::Array {
                    elems,
                    span: start.to(self.prev_span()),
                }
            }
            _ => {
                self.error_here("expected an expression");
                // Synthesize a placeholder so callers can keep going.
                Expr::Int {
                    text: String::new(),
                    span: start,
                }
            }
        }
    }

    fn parse_concat(&mut self, start: Span) -> Expr {
        self.expect(TokenKind::LBrace, "to open a concatenation");
        let mut parts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            // `{ x, .b = 4 }` — spec 3.12: a connection block is all
            // positional or all `.port = value`, never both. Without this the
            // `.` restarted expression parsing and one mistake cascaded into
            // nine errors, none of which named the actual rule.
            if self.at(TokenKind::Dot) {
                self.error_here(
                    "a connection block is either all positional or all `.port = value`",
                );
                // Consume the rest of the block so the file still parses.
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    self.bump();
                }
                break;
            }
            parts.push(self.parse_expr(false));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close a concatenation");
        Expr::Concat {
            parts,
            span: start.to(self.prev_span()),
        }
    }

    /// A path expression, possibly an instance/struct construction. `::` is
    /// pure namespace/type selection; attributes ride the tick (`x'old`) and
    /// are handled by the postfix loop as [`Expr::SysAttr`].
    fn parse_path_expr_or_construct(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
        let mut segments = vec![self.parse_ident()];
        while self.at(TokenKind::ColonColon) && self.kind_at(self.pos + 1) == &TokenKind::Ident {
            self.bump(); // `::`
            segments.push(self.parse_ident());
        }
        let path = Path {
            segments,
            span: start.to(self.prev_span()),
        };

        // Construction: `Counter<W = 8> { ... }` or `Packet { ... }`.
        if self.at(TokenKind::Lt) && self.angle_then_brace(self.pos) {
            let args = self.parse_generic_args();
            let ty = Type::Generic {
                base: Box::new(Type::Path(path)),
                args,
                span: start.to(self.prev_span()),
            };
            return self.parse_construct(start, Some(ty));
        }
        if self.at(TokenKind::LBrace) && !no_struct {
            return self.parse_construct(start, Some(Type::Path(path)));
        }
        Expr::Path(path)
    }

    fn parse_construct(&mut self, start: Span, ty: Option<Type>) -> Expr {
        self.expect(TokenKind::LBrace, "to open a construction");
        let mut args = Vec::new();
        // A leading `..base` is a struct spread-update: take every field from
        // `base`, then override with the explicit `.field = v` args that follow.
        let spread = if self.eat(TokenKind::DotDot) {
            let base = self.parse_expr(false);
            self.eat(TokenKind::Comma);
            Some(Box::new(base))
        } else {
            None
        };
        // A block is either all-named explicit (`.a = x`) or all positional
        // (`x, y` — bound by declaration order). Mixing the two is rejected once
        // we know which shape the first argument set. There is no bare `.a`
        // name-shorthand: a `.field` always takes a value. A spread forces the
        // named form.
        let mut positional: Option<bool> = spread.is_some().then_some(false);
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let cstart = self.span();
            let is_pos = !self.at(TokenKind::Dot);
            match positional {
                None => positional = Some(is_pos),
                Some(prev) if prev != is_pos => {
                    self.error_here("cannot mix positional and `.field` connections");
                }
                _ => {}
            }
            let (field, value) = if is_pos {
                // Positional: a bare expression, bound by ordinal position.
                (None, Some(self.parse_expr(false)))
            } else {
                self.expect(TokenKind::Dot, "before a connection field");
                let field = Some(self.parse_ident());
                let value = if self.eat(TokenKind::Eq) {
                    Some(self.parse_expr(false))
                } else {
                    self.error_here(
                        "a `.field` connection needs a value: `.field = signal` \
                         (or drop the dot for positional `{ signal }`)",
                    );
                    None
                };
                (field, value)
            };
            args.push(ConnectArg {
                field,
                value,
                span: cstart.to(self.prev_span()),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close a construction");
        Expr::Construct {
            ty,
            args,
            spread,
            span: start.to(self.prev_span()),
        }
    }

    // --- types --------------------------------------------------------------

    fn parse_type(&mut self) -> Type {
        let start = self.span();
        let first = self.parse_type_core();
        // Two adjacent type names form an applied view: `Stream<T> Source`.
        // The backing type comes first and the view follows it, matching a
        // port's `name: Type direction` shape — the slot after the type holds
        // either a direction or a view.
        if matches!(self.kind(), TokenKind::Ident) && self.cur_text() != "where" {
            let view_ty = self.parse_type_core();
            let view = match view_ty {
                Type::Path(p) => p,
                _ => {
                    self.error_here("a view name after its backing type cannot have arguments");
                    return first;
                }
            };
            return Type::View {
                view,
                target: Box::new(first),
                span: start.to(self.prev_span()),
            };
        }
        first
    }

    fn parse_type_core(&mut self) -> Type {
        let start = self.span();
        let path = self.parse_path();
        let mut ty = Type::Path(path);
        if self.at(TokenKind::Lt) {
            let args = self.parse_generic_args();
            ty = Type::Generic {
                base: Box::new(ty),
                args,
                span: start.to(self.prev_span()),
            };
        }
        while self.at(TokenKind::LBracket) {
            self.bump();
            // `Char[]` is an unconstrained array: the range is set at use.
            let index = if self.at(TokenKind::RBracket) {
                None
            } else {
                Some(Box::new(self.parse_expr(false)))
            };
            self.expect(TokenKind::RBracket, "to close a type index");
            ty = Type::Indexed {
                base: Box::new(ty),
                index,
                span: start.to(self.prev_span()),
            };
        }
        ty
    }

    fn parse_generic_args(&mut self) -> Vec<GenericArg> {
        self.expect(TokenKind::Lt, "to open a generic argument list");
        let mut args = Vec::new();
        while !self.at_generic_end() && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Ident) && self.kind_at(self.pos + 1) == &TokenKind::Eq {
                let name = self.parse_ident();
                self.bump(); // `=`
                if self.generic_arg_starts_nested_type() {
                    let ty = self.parse_type();
                    args.push(GenericArg::NamedType { name, ty });
                } else {
                    let value = self.parse_generic_value();
                    args.push(GenericArg::Named { name, value });
                }
            } else if self.generic_arg_starts_nested_type() {
                args.push(GenericArg::PositionalType(self.parse_type()));
            } else {
                args.push(GenericArg::Positional(self.parse_generic_value()));
            }
            self.check_generic_operand();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.close_generic("to close a generic argument list");
        args
    }

    /// A bare name can be either a value or a type parameter, but a path
    /// immediately followed by its own `<...>` is unambiguously a nested type
    /// application in a surrounding generic argument list.
    fn generic_arg_starts_nested_type(&self) -> bool {
        if !self.at(TokenKind::Ident) {
            return false;
        }
        let mut position = self.pos + 1;
        while self.kind_at(position) == &TokenKind::ColonColon
            && self.kind_at(position + 1) == &TokenKind::Ident
        {
            position += 2;
        }
        self.kind_at(position) == &TokenKind::Lt
    }

    /// One generic argument value: a postfix expression, optionally extended
    /// into a range (`integer<0..255>`, value-range constraints on numerics).
    /// Bounds may be negative (`integer<-32768..32767>`).
    fn parse_generic_value(&mut self) -> Expr {
        let start = self.span();
        let lo = self.parse_generic_atom();
        if self.at(TokenKind::DotDot) {
            self.bump();
            let hi = self.parse_generic_atom();
            let span = start.to(self.prev_span());
            return Expr::Range {
                lo: Box::new(lo),
                hi: Box::new(hi),
                span,
            };
        }
        lo
    }

    /// A generic argument is a postfix expression, so `Bank<K * 2>` stops
    /// after `K` and the list then fails to close — reported as "expected `>`",
    /// which says nothing about the cause or the cure. The restriction is
    /// deliberate (`Bank<K > 2>` would otherwise be ambiguous, the problem
    /// Rust solves by requiring braces), and parentheses already work, so
    /// point at them.
    fn check_generic_operand(&mut self) {
        let operator = match self.kind() {
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Shl => "<<",
            // `>>` is deliberately absent: `close_generic` splits it into two
            // closing brackets so a bound like `struct Wrap<T: Meter<8>>`
            // parses, and reading it as a shift here would reject every one.
            _ => return,
        };
        let span = self.span();
        self.error_at(
            span,
            format!("`{operator}` needs parentheses here: write `(a {operator} b)`"),
        );
        // Consume the rest of the expression so the list can still close and
        // the parse keeps going (best-effort, per the stage conventions).
        self.bump();
        let _ = self.parse_generic_atom();
    }

    fn parse_generic_atom(&mut self) -> Expr {
        if self.at(TokenKind::Minus) {
            let start = self.span();
            self.bump();
            let rhs = self.parse_postfix(false);
            let span = start.to(self.prev_span());
            return Expr::Unary {
                op: UnOp::Neg,
                rhs: Box::new(rhs),
                span,
            };
        }
        self.parse_postfix(false)
    }

    // --- params -------------------------------------------------------------

    fn parse_params_opt(&mut self) -> Params {
        if self.at(TokenKind::Lt) {
            self.parse_params()
        } else {
            Params::default()
        }
    }

    fn parse_params(&mut self) -> Params {
        self.expect(TokenKind::Lt, "to open a parameter list");
        let mut params = Vec::new();
        while !self.at_generic_end() && !self.at(TokenKind::Eof) {
            let pstart = self.span();
            let name = self.parse_ident();
            let bound = if self.eat(TokenKind::Colon) {
                Some(self.parse_type())
            } else {
                None
            };
            params.push(Param {
                name,
                bound,
                span: pstart.to(self.prev_span()),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.close_generic("to close a parameter list");
        Params { params }
    }

    fn parse_path(&mut self) -> Path {
        let start = self.span();
        let mut segments = vec![self.parse_ident()];
        while self.at(TokenKind::ColonColon) && self.kind_at(self.pos + 1) == &TokenKind::Ident {
            self.bump(); // `::`
            segments.push(self.parse_ident());
        }
        Path {
            segments,
            span: start.to(self.prev_span()),
        }
    }

    fn parse_ident(&mut self) -> Ident {
        if self.at(TokenKind::Ident) || self.at(TokenKind::SelfKw) {
            let t = self.bump();
            Ident {
                text: self.text_of(t.span).to_string(),
                span: t.span,
            }
        } else {
            self.error_here("expected an identifier");
            Ident {
                text: String::new(),
                span: self.span(),
            }
        }
    }

    /// Like [`Self::parse_ident`] but also accepts keyword tokens used as plain
    /// names (e.g. attribute targets `entity`, `let`, `port`).
    fn parse_word(&mut self) -> Ident {
        if self.is_name_token() {
            let t = self.bump();
            Ident {
                text: self.text_of(t.span).to_string(),
                span: t.span,
            }
        } else {
            self.error_here("expected a name");
            Ident {
                text: String::new(),
                span: self.span(),
            }
        }
    }

    fn is_name_token(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Ident
                | TokenKind::SelfKw
                | TokenKind::Module
                | TokenKind::Using
                | TokenKind::Pub
                | TokenKind::Entity
                | TokenKind::Impl
                | TokenKind::Struct
                | TokenKind::View
                | TokenKind::Enum
                | TokenKind::Trait
                | TokenKind::Attr
                | TokenKind::Const
                | TokenKind::Let
                | TokenKind::Fn
                | TokenKind::In
                | TokenKind::Out
                | TokenKind::Inout
                | TokenKind::If
                | TokenKind::Else
                | TokenKind::Match
                | TokenKind::For
                | TokenKind::Return
                | TokenKind::Extern
        )
    }

    fn eat_direction(&mut self) -> Option<Direction> {
        let dir = match self.kind() {
            TokenKind::In => Direction::In,
            TokenKind::Out => Direction::Out,
            TokenKind::Inout => Direction::Inout,
            _ => return None,
        };
        self.bump();
        Some(dir)
    }

    // --- angle-bracket lookahead --------------------------------------------

    /// True if the `<...>` starting at `i` contains a top-level `:` (a parameter
    /// list `<W: integer>` rather than a generic-argument list `<8>`).
    fn angle_is_param_list(&self, mut i: usize) -> bool {
        let mut depth = 0u32;
        loop {
            match self.kind_at(i) {
                TokenKind::Lt => depth += 1,
                TokenKind::Shl => depth += 2,
                TokenKind::Gt => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return false;
                    }
                }
                TokenKind::Shr => {
                    depth = depth.saturating_sub(2);
                    if depth == 0 {
                        return false;
                    }
                }
                TokenKind::Colon if depth == 1 => return true,
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
    }

    /// True if the `<...>` starting at `i` is immediately followed by `{`,
    /// marking an instance construction `Counter<...> { ... }`.
    fn angle_then_brace(&self, i: usize) -> bool {
        match self.matched_angle_end(i) {
            Some(end) => self.kind_at(end) == &TokenKind::LBrace,
            None => false,
        }
    }

    /// True if the `<...>` starting at `i` is immediately followed by `(`,
    /// marking an explicitly typed call such as `read<string>(path)`.
    fn angle_then_lparen(&self, i: usize) -> bool {
        match self.matched_angle_end(i) {
            Some(end) => self.kind_at(end) == &TokenKind::LParen,
            None => false,
        }
    }

    fn matched_angle_end(&self, mut i: usize) -> Option<usize> {
        let mut depth = 0u32;
        loop {
            match self.kind_at(i) {
                TokenKind::Lt => depth += 1,
                TokenKind::Shl => depth += 2,
                TokenKind::Gt => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                TokenKind::Shr => {
                    depth = depth.saturating_sub(2);
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
            i += 1;
        }
    }

    // --- cursor primitives --------------------------------------------------

    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn kind_at(&self, i: usize) -> &TokenKind {
        &self.tokens[i.min(self.tokens.len() - 1)].kind
    }

    fn span_at(&self, i: usize) -> Span {
        self.tokens[i.min(self.tokens.len() - 1)].span
    }

    fn at(&self, k: TokenKind) -> bool {
        self.peek().kind == k
    }

    fn span(&self) -> Span {
        self.peek().span
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            self.tokens[0].span
        } else {
            self.tokens[(self.pos - 1).min(self.tokens.len() - 1)].span
        }
    }

    fn cur_text(&self) -> &str {
        self.text_of(self.peek().span)
    }

    fn text_of(&self, span: Span) -> &str {
        &self.src[span.start as usize..span.end as usize]
    }

    fn bump(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    /// Whether the current token can close a generic `<...>` — a `>`, or a `>>`
    /// (`Shr`) that closes one level of a nested generic (`Box<Box<T>>`).
    fn at_generic_end(&self) -> bool {
        self.at(TokenKind::Gt) || self.at(TokenKind::Shr)
    }

    /// Close one generic `<...>`. A `>` is consumed normally; a `>>` is split in
    /// place — one `>` closes this level, the other stays for the enclosing
    /// generic — so `Box<Box<T>>` parses without a space between the angles.
    fn close_generic(&mut self, ctx: &str) -> bool {
        if self.at(TokenKind::Gt) {
            self.bump();
            true
        } else if self.at(TokenKind::Shr) {
            // Rewrite `>>` to a single `>` covering its second character and
            // leave it at the current position for the outer close.
            let sp = self.peek().span;
            self.tokens[self.pos] = Token {
                kind: TokenKind::Gt,
                span: Span::new(sp.file, sp.start + 1..sp.end),
            };
            true
        } else {
            self.expect(TokenKind::Gt, ctx)
        }
    }

    fn eat(&mut self, k: TokenKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// `struct B : A` / `enum B : A` — the pre-paren newtype spelling, and the
    /// shape an inheriting language would reach for. Report it precisely and
    /// keep parsing the base so the rest of the declaration still yields a
    /// usable AST (a body after it is extension, which typeck then names).
    fn deprecated_colon_base(&mut self, kw: &str, name: &str) -> Option<Type> {
        if !self.at(TokenKind::Colon) {
            return None;
        }
        let span = self.span();
        self.bump(); // `:`
        let base = self.parse_type();
        let base_txt = crate::syntax::pretty::type_str(&base);
        self.sink.emit(
            Diagnostic::error(format!("`{kw} {name} : {base_txt}` is not a declaration"))
                .at(span)
                .help(format!(
                    "a newtype is written with parentheses: `{kw} {name}({base_txt});`. \
                     To build a bigger type, hold this one as a field instead — \
                     derivation never adds members"
                )),
        );
        Some(base)
    }

    fn expect(&mut self, k: TokenKind, ctx: &str) -> bool {
        if self.at(k.clone()) {
            self.bump();
            true
        } else {
            let span = self.span();
            self.error_at(span, format!("expected {} {}", k.describe(), ctx));
            false
        }
    }

    /// Take one level of nesting. Returns true when the limit is reached, in
    /// which case the caller must not descend further.
    fn enter_nesting(&mut self) -> bool {
        if self.depth >= MAX_NESTING {
            if !self.depth_reported {
                self.depth_reported = true;
                self.error_here(format!(
                    "expression or block nests more than {MAX_NESTING} levels deep"
                ));
            }
            return true;
        }
        self.depth += 1;
        false
    }

    fn error_here(&mut self, msg: impl Into<String>) {
        let span = self.span();
        self.error_at(span, msg);
    }

    fn error_at(&mut self, span: Span, msg: impl Into<String>) {
        self.sink.emit(Diagnostic::error(msg).at(span));
    }
}

/// A bit-string prefix is a single letter glued to a string (`x"AB"`). The
/// parser recognizes the shape; std's `impl Prefix for T` owns which letters
/// are valid (checked in typeck), so a stray `q"…"` parses then errors clearly.
fn is_prefix_letter(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic()) && chars.next().is_none()
}

/// Process the standard escapes in a string literal body: `\\n`, `\\t`,
/// `\\r`, `\\0`, `\\"`, `\\\\`. An unknown escape keeps the backslash
/// verbatim (best-effort; the lexer already validated termination).
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {

    /// A recursive-descent parser has no natural depth bound: 2000 nested
    /// parentheses — or the same run left *unclosed* — overflowed the stack
    /// and aborted the process with no diagnostic. The LSP shares this parser,
    /// so that took the editor's language server down with it.
    #[test]
    fn deep_nesting_is_reported_rather_than_overflowing_the_stack() {
        let nest = |n: usize, closed: bool| {
            format!(
                "module m;\nentity E {{ a: Bit in, y: Bit out }}\nimpl E {{ y = {}a{}; }}\n",
                "(".repeat(n),
                if closed { ")".repeat(n) } else { String::new() }
            )
        };
        // Real programs nest single digits deep; this much still parses.
        let mut sink = DiagnosticSink::new();
        crate::syntax::parse_module(FileId(0), &nest(100, true), &mut sink);
        assert_eq!(sink.error_count(), 0, "100 levels must still parse");

        // Past the limit it is a diagnostic, and only one of them.
        for closed in [true, false] {
            let mut sink = DiagnosticSink::new();
            crate::syntax::parse_module(FileId(0), &nest(2000, closed), &mut sink);
            let deep = sink
                .diagnostics()
                .iter()
                .filter(|d| d.message.contains("nests more than"))
                .count();
            assert_eq!(deep, 1, "one depth diagnostic, closed={closed}");
        }
    }
    use super::*;
    use crate::diag::FileId;

    fn parse(src: &str) -> (Module, usize) {
        let mut sink = DiagnosticSink::new();
        let m = crate::syntax::parse_module(FileId(0), src, &mut sink);
        (m, sink.error_count())
    }

    fn parse_ok(src: &str) -> Module {
        let (m, errors) = parse(src);
        assert_eq!(errors, 0, "unexpected parse errors in:\n{src}");
        m
    }

    fn diagnostics(src: &str) -> Vec<Diagnostic> {
        let mut sink = DiagnosticSink::new();
        crate::syntax::parse_module(FileId(0), src, &mut sink);
        sink.diagnostics().to_vec()
    }

    /// The pre-migration port form. `in` is not a port name, so without a
    /// dedicated diagnostic it failed four times across one line and never
    /// mentioned that the direction had moved. One error, naming the fix.
    #[test]
    fn leading_direction_on_a_port_reports_the_move_once() {
        let diags = diagnostics("module m;\nentity E {\n  in clk: Bit;\n  out q: Bit;\n}\n");
        assert_eq!(diags.len(), 2, "one per port, got {diags:#?}");
        assert!(diags[0].message.contains("direction goes after its type"));
        assert!(diags[0]
            .help
            .as_ref()
            .is_some_and(|h| h.contains("clk: Bit in")));
    }

    /// The old form still yields a usable entity, per the best-effort rule:
    /// later stages report on it rather than seeing an empty port list.
    #[test]
    fn leading_direction_still_produces_the_ports() {
        let (m, _) = parse("module m;\nentity E {\n  in clk: Bit;\n  out q: Bit;\n}\n");
        let Some(Item::Entity(e)) = m.items.first() else {
            panic!("expected an entity")
        };
        let got: Vec<_> = e
            .ports
            .iter()
            .map(|p| (p.name.text.as_str(), p.dir))
            .collect();
        assert_eq!(
            got,
            [("clk", Some(Direction::In)), ("q", Some(Direction::Out))]
        );
    }

    #[test]
    fn leading_direction_on_a_view_field_reports_the_move_once() {
        let diags = diagnostics("module m;\nview V for S {\n  out a;\n  in b;\n}\n");
        assert_eq!(diags.len(), 2, "one per field, got {diags:#?}");
        assert!(diags[0].message.contains("direction goes after its name"));
        assert!(diags[0].help.as_ref().is_some_and(|h| h.contains("a out")));
    }

    /// A `;` between members is the old separator, and "expected `,`" alone
    /// left the reader to infer that the whole form had changed.
    #[test]
    fn semicolon_between_members_names_the_comma() {
        let diags = diagnostics("module m;\nentity E {\n  clk: Bit in;\n  q: Bit out\n}\n");
        assert_eq!(diags.len(), 1, "got {diags:#?}");
        assert!(diags[0].message.contains("`,`, not `;`"));
    }

    /// A run of unrecognized bytes is one mistake wherever it lands. It used
    /// to cost three to eight diagnostics depending on the list — each rule
    /// reporting the name or separator it could not find *after* the lexer had
    /// already named the cause. `Unknown` is trivia now, so this holds for
    /// every list in the grammar rather than the ones that were reported.
    #[test]
    fn a_stray_token_run_reports_once_in_any_list() {
        let cases = [
            ("entity ports", "entity E { a: Bit in, @@@ y: Bit out }"),
            ("struct body", "struct S { a: Bit, @@@ b: Bit }"),
            ("enum body", "enum E { A, @@@ B }"),
            (
                "view body",
                "struct S { a: Bit, b: Bit }\nview V for S { a out, @@@ b in }",
            ),
            (
                "param list",
                "entity E<W: integer, @@@ X: integer> { y: Bit out }",
            ),
            ("import list", "using std::bits::{unsigned, @@@ signed};"),
            (
                "impl statements",
                "entity E { a: Bit in, y: Bit out }\nimpl E { y = a; @@@ }",
            ),
            (
                "call arguments",
                "entity E { y: Bit out }\nimpl E { y = f(1, @@@ 2); }",
            ),
            (
                "array literal",
                "entity E { y: Bit out }\nimpl E { let a: Bit[2] = ['0', @@@ '1']; y = a[0]; }",
            ),
            (
                "match arms",
                "entity E { s: unsigned[2] in, y: Bit out }\n\
                 impl E { y = match s { 0 => '0', @@@ _ => '1' }; }",
            ),
        ];
        for (what, body) in cases {
            let diags = diagnostics(&format!("module m;\n{body}\n"));
            assert_eq!(diags.len(), 1, "in {what}: {diags:#?}");
            assert!(
                diags[0].message.contains("unexpected characters `@@@`"),
                "in {what}: {:?}",
                diags[0].message
            );
        }
    }

    /// Multi-byte input coalesces by character, not by byte, and the message
    /// quotes what was written. The old message formatted one byte as a
    /// `char`, which mangled anything non-ASCII.
    #[test]
    fn a_stray_token_run_is_quoted_by_character() {
        let diags = diagnostics("module m;\nimpl E { y = a; ¡¿ }\n");
        assert_eq!(diags.len(), 1, "got {diags:#?}");
        assert!(diags[0].message.contains("`¡¿`"), "{:?}", diags[0].message);
    }

    /// Dropping the token must not drop its neighbours: the ports on either
    /// side of the stray run still parse.
    #[test]
    fn a_stray_token_keeps_the_ports_around_it() {
        let (m, _) = parse("module m;\nentity E { a: Bit in, @@@ y: Bit out }\n");
        let Some(Item::Entity(e)) = m.items.first() else {
            panic!("expected an entity")
        };
        let names: Vec<&str> = e.ports.iter().map(|p| p.name.text.as_str()).collect();
        assert_eq!(names, ["a", "y"]);
    }

    /// The parser learns its operators from `impl Operator<..>`, so an
    /// undeclared one looked exactly like the end of an expression: `a % b`
    /// reported "expected `;` after a `let`" and left the reader hunting a
    /// punctuation error. Eleven diagnostics for one unknown symbol.
    #[test]
    fn an_undeclared_operator_is_named() {
        let diags = diagnostics("module m;\nimpl E { let a: integer = 1 % 2; }\n");
        assert_eq!(diags.len(), 1, "got {diags:#?}");
        assert!(diags[0].message.contains("no operator `%` is declared"));
        assert!(diags[0]
            .help
            .as_ref()
            .is_some_and(|h| h.contains("impl Operator<\"%\"")));
    }

    /// A stray statement in an entity body used to be retried as a fresh port
    /// once per leftover token, repeating the same three diagnostics until the
    /// body ran out (14 errors for one mistake). Recovery skips to the `;`.
    #[test]
    fn malformed_port_reports_once() {
        let (_, errors) = parse("module m;\nentity E { a: Bit in, y = 1; }\n");
        assert!(errors <= 3, "one bad port should not cascade, got {errors}");
    }

    /// Recovery must not overshoot: a port that errored but still reached its
    /// `;` is a clean boundary, so the ports after it still parse.
    #[test]
    fn recovery_keeps_the_ports_after_a_bad_one() {
        let (m, _) = parse("module m;\nentity E { a Bit in, b: Bit in, c: Bit out, }\n");
        let Some(Item::Entity(e)) = m.items.first() else {
            panic!("expected an entity")
        };
        let names: Vec<&str> = e.ports.iter().map(|p| p.name.text.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn module_header_and_imports() {
        let m = parse_ok(
            "module std::logic;\nusing std::logic::{Bit, Logic};\nusing Word = unsigned[32];\n",
        );
        assert_eq!(m.path.segments.len(), 2);
        assert_eq!(m.path.segments[1].text, "logic");
        assert_eq!(m.items.len(), 2);
        assert!(matches!(&m.items[0], Item::Using(_)));
    }

    #[test]
    fn entity_with_params_and_ports() {
        let m = parse_ok(
            "module m;\nentity Counter<W: integer> {\n  clk: Bit in,\n  rst: Logic in,\n  en: Bit in,\n  count: unsigned[W] out,\n}\n",
        );
        let Item::Entity(e) = &m.items[0] else {
            panic!("expected entity")
        };
        assert_eq!(e.name.text, "Counter");
        assert_eq!(e.params.params.len(), 1);
        assert_eq!(e.ports.len(), 4);
        assert_eq!(e.ports[0].dir, Some(Direction::In));
        assert_eq!(e.ports[3].dir, Some(Direction::Out));
    }

    #[test]
    fn struct_enum_const() {
        let m = parse_ok(
            "module m;\nconst DEFAULT_WIDTH: usize = 8;\nstruct Packet<T> { valid: Bit, data: T }\nenum State {  Idle = 0, Start = 1, Shift = 2, Done = 3 }\n",
        );
        assert_eq!(m.items.len(), 3);
        let Item::Enum(e) = &m.items[2] else {
            panic!("expected enum")
        };
        assert_eq!(e.variants.len(), 4);
        // A variant-carrying enum has no base: `repr` is only ever the newtype
        // base now, and that form takes no body.
        assert!(e.repr.is_none());
    }

    /// `struct B(A);` is the newtype form; a body is not part of it, so
    /// extension cannot be written at all.
    #[test]
    fn newtype_is_parenthesised() {
        let m =
            parse_ok("module m;\nstruct A { x: Bit }\nstruct B(A);\nenum E { P }\nenum F(E);\n");
        let Item::Struct(s) = &m.items[1] else {
            panic!("expected struct")
        };
        assert!(
            s.base.is_some() && s.fields.is_empty(),
            "a newtype has a base and no fields"
        );
        let Item::Enum(e) = &m.items[3] else {
            panic!("expected enum")
        };
        assert!(
            e.repr.is_some() && e.variants.is_empty(),
            "same for the enum form"
        );
    }

    /// `struct B : A` was the newtype spelling before parens, and is the shape
    /// an inheriting language would reach for. Report it with the migration in
    /// the message rather than a bare "expected `{`" further along.
    #[test]
    fn colon_base_is_reported_with_the_new_spelling() {
        for src in [
            "module m;\nstruct A { x: Bit }\nstruct B : A;\n",
            "module m;\nstruct A { x: Bit }\nstruct B : A { y: Bit }\n",
            "module m;\nenum A { X }\nenum B : A;\n",
            "module m;\nenum A { X }\nenum B : A { Z }\n",
        ] {
            let mut sink = DiagnosticSink::new();
            crate::syntax::parse_module(FileId(0), src, &mut sink);
            let msgs: Vec<_> = sink
                .diagnostics()
                .iter()
                .map(|d| d.message.clone())
                .collect();
            assert!(
                msgs.iter().any(|m| m.contains("is not a declaration")),
                "want the migration message for:\n{src}\ngot {msgs:?}"
            );
            let helps: Vec<_> = sink
                .diagnostics()
                .iter()
                .filter_map(|d| d.help.clone())
                .collect();
            assert!(
                helps.iter().any(|h| h.contains("parentheses")),
                "the help should name the new spelling, got {helps:?}"
            );
        }
    }

    #[test]
    fn impl_with_state_and_explicit_process() {
        let m = parse_ok(
            "module m;\nimpl<W: integer> Counter<W> {\n  const MAX: unsigned[W] = (1 << W) - 1;\n  let value: unsigned[W] = 0;\n  process update {\n    if clk.rising() {\n      if rst == '1' {\n        value = 0;\n      } else {\n        value = value + 1;\n      }\n    }\n  }\n  count = value;\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else {
            panic!("expected impl")
        };
        assert_eq!(i.params.params.len(), 1);
        // const, let, process, concurrent assignment.
        assert_eq!(i.items.len(), 4);
        assert!(matches!(i.items[0], ImplItem::Const(_)));
        assert!(matches!(i.items[1], ImplItem::Let(_)));
        let ImplItem::Process(process) = &i.items[2] else {
            panic!("expected process")
        };
        assert_eq!(
            process.name.as_ref().map(|name| name.text.as_str()),
            Some("update")
        );
        assert!(matches!(process.body.stmts[0], Stmt::If(_)));
        assert!(matches!(i.items[3], ImplItem::Stmt(Stmt::Assign { .. })));
    }

    #[test]
    fn visibility_is_retained_on_functions_fields_and_methods() {
        let m = parse_ok(
            "module m;\npub fn exported() {}\nfn hidden() {}\npub struct S { pub open: integer, closed: integer }\nimpl S { pub fn get(self) -> integer { return self.open; } fn secret(self) -> integer { return self.closed; } }\n",
        );
        assert!(matches!(&m.items[0], Item::Fn(function) if function.is_pub));
        assert!(matches!(&m.items[1], Item::Fn(function) if !function.is_pub));
        let Item::Struct(struct_) = &m.items[2] else {
            panic!("expected struct")
        };
        assert!(struct_.fields[0].is_pub);
        assert!(!struct_.fields[1].is_pub);
        let Item::Impl(impl_) = &m.items[3] else {
            panic!("expected impl")
        };
        assert!(matches!(&impl_.items[0], ImplItem::Fn(function) if function.is_pub));
        assert!(matches!(&impl_.items[1], ImplItem::Fn(function) if !function.is_pub));
    }

    #[test]
    fn interface_members_reject_redundant_visibility() {
        let mut sink = DiagnosticSink::new();
        crate::syntax::parse_module(
            FileId(0),
            "module m;\nentity E { pub value: integer out }\ntrait T { pub fn value(self) -> integer; }\n",
            &mut sink,
        );
        let messages: Vec<&str> = sink
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert!(messages
            .iter()
            .any(|message| message.contains("ports are already")));
        assert!(messages
            .iter()
            .any(|message| message.contains("inherit the trait")));
    }

    #[test]
    fn generic_trait_impl_parameters_precede_the_trait() {
        let m = parse_ok(
            "module m;\n\
             impl<T: Resolve> Resolve for T[] {\n\
               fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n",
        );
        let Item::Impl(im) = &m.items[0] else {
            panic!("expected impl")
        };
        assert_eq!(im.params.params.len(), 1);
        assert_eq!(im.params.params[0].name.text, "T");
        assert_eq!(
            im.trait_
                .as_ref()
                .and_then(|path| path.segments.last())
                .map(|name| name.text.as_str()),
            Some("Resolve")
        );
        assert!(matches!(im.target, Type::Indexed { index: None, .. }));
    }

    /// `!x` is the reflex from C/Verilog/Rust, but `!` here only marks a macro
    /// call — it used to cascade four "expected an expression" errors through
    /// Diagnostics name syntax the way a user types it. These used to render
    /// the Rust variant ("expected Semi after a port"), which is meaningless
    /// to a reader of siox source.
    #[test]
    fn expected_token_is_named_in_source_spelling() {
        let mut sink = DiagnosticSink::new();
        crate::syntax::parse_module(
            FileId(0),
            "module m;\nentity E { a: Bit in\nb: Bit in,\n}\n",
            &mut sink,
        );
        let msgs: Vec<_> = sink
            .diagnostics()
            .iter()
            .map(|d| d.message.clone())
            .collect();
        assert!(
            msgs.iter().any(|m| m.contains("`,`")),
            "want a `,` spelling, got {msgs:?}"
        );
        assert!(
            !msgs.iter().any(|m| m.contains("Semi")),
            "leaked a variant name: {msgs:?}"
        );
    }

    /// the rest of the statement.
    #[test]
    fn unary_bang_suggests_not() {
        let mut sink = DiagnosticSink::new();
        let src = "module m;\nimpl M {\n  clk = !clk after 5ns;\n}\n";
        crate::syntax::parse_module(FileId(0), src, &mut sink);
        let msgs: Vec<_> = sink
            .diagnostics()
            .iter()
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(msgs.len(), 1, "one clear error, not a cascade: {msgs:?}");
        assert!(msgs[0].contains("use `not`"), "{msgs:?}");
        // The macro form is untouched.
        let (_, errors) =
            parse("module m;\n#[test] entity T {}\nimpl T { assert!(1 == 1, \"ok\"); }\n");
        assert_eq!(errors, 0, "`assert!` still parses");
    }

    #[test]
    fn sysattr_vs_path_in_expressions() {
        let m = parse_ok(
            "module m;\nimpl M {\n  if state'old == State::Idle {\n    started = '1';\n  }\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::If(iff)) = &i.items[0] else {
            panic!("expected if")
        };
        // LHS of `==` is `state'old` (SysAttr); RHS is `State::Idle` (Path).
        let Expr::Binary { lhs, rhs, op, .. } = &iff.cond else {
            panic!("expected binary")
        };
        assert_eq!(op, &BinOp::Eq);
        assert!(matches!(**lhs, Expr::SysAttr { .. }));
        let Expr::Path(p) = &**rhs else {
            panic!("expected path")
        };
        assert_eq!(p.segments.len(), 2);
    }

    #[test]
    fn trait_and_clocklike_impl() {
        let m = parse_ok(
            "module m;\ntrait ClockLike {\n  fn rising(self);\n  fn edge(self);\n}\nimpl ClockLike for Logic {\n  fn rising(self) {\n    return self'event and self'old == '0' and self == '1';\n  }\n  fn edge(self) {\n    return self'event;\n  }\n}\n",
        );
        let Item::Trait(t) = &m.items[0] else {
            panic!("expected trait")
        };
        assert_eq!(t.items.len(), 2);
        assert!(t.items[0].body.is_none());
        let Item::Impl(i) = &m.items[1] else {
            panic!("expected impl")
        };
        assert_eq!(i.trait_.as_ref().unwrap().segments[0].text, "ClockLike");
        assert!(matches!(i.items[0], ImplItem::Fn(_)));
    }

    #[test]
    fn bus_modes_and_construction() {
        let m = parse_ok(
            "module m;\nstruct Stream<T> { clk: Bit, valid: Bit, ready: Bit, data: T }\nview Source<T> for Stream<T> {\n  clk in,\n  valid out,\n  ready in,\n  data out,\n}\nimpl Stream<T> Source { fn ready(self) -> Bit { return self.ready; } }\nentity Producer {\n  bus: Stream<unsigned[32]> Source,\n}\n",
        );
        let Item::View(v) = &m.items[1] else {
            panic!("expected view")
        };
        assert_eq!(v.name.text, "Source");
        assert_eq!(v.fields.len(), 4);
        let Item::Entity(e) = &m.items[3] else {
            panic!("expected entity")
        };
        assert_eq!(e.ports[0].dir, None); // direction lives in the view fields
        assert!(matches!(&e.ports[0].ty, Type::View { .. }));
    }

    #[test]
    fn direction_keywords_cannot_be_view_names() {
        for name in ["in", "out", "inout"] {
            let src = format!(
                "module m;\nstruct Bus {{ bit: Bit }}\nview {name} for Bus {{ bit in, }}\n"
            );
            let (_, errors) = parse(&src);
            assert!(
                errors > 0,
                "`{name}` must stay reserved for default port directions"
            );
        }
    }

    #[test]
    fn partial_ranges_parse_and_inclusive_equals_is_rejected() {
        let m = parse_ok("module m;\nimpl T { a = v[..4]; b = v[1..]; c = v[..]; }\n");
        let Item::Impl(im) = &m.items[0] else {
            panic!()
        };
        for item in &im.items {
            let ImplItem::Stmt(Stmt::Assign {
                value: Expr::Index { index, .. },
                ..
            }) = item
            else {
                panic!("expected indexed assignment value")
            };
            assert!(matches!(index.as_ref(), Expr::PartialRange { .. }));
        }
        let (_, errors) = parse("module m;\nimpl T { a = v[..=2]; }\n");
        assert!(
            errors > 0,
            "`..=` is redundant because Siox ranges are inclusive"
        );
    }

    #[test]
    fn nested_generic_close_splits_shr() {
        // A `>>` closing two angle levels (a nested generic bound) parses: the
        // `>>` token is split so one `>` closes `Bar<Bit>` and the other the
        // param list. A plain shift expression still parses as a shift.
        parse_ok("module m;\nfn f<T: Bar<Bit>>(x: T) -> Bit { return x.b(); }\n");
        parse_ok("module m;\ntrait Foo<U> { fn g(self) -> Bar<U>; }\n");
        let m = parse_ok("module m;\nimpl M {\n  y = a >> b;\n}\n");
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::Assign { value, .. }) = &i.items[0] else {
            panic!()
        };
        assert!(
            matches!(value, Expr::Binary { op: BinOp::Shr, .. }),
            "`a >> b` stays a shift"
        );
    }

    #[test]
    fn typed_construct_calls_keep_their_type_argument() {
        let module = parse_ok(
            "module m;\nimpl E { let rom: unsigned[16][2] = read<unsigned[16]>(\"rom.bin\"); }\n",
        );
        let Item::Impl(implementation) = &module.items[0] else {
            panic!("expected impl")
        };
        let ImplItem::Let(declaration) = &implementation.items[0] else {
            panic!("expected let")
        };
        let Some(Expr::Call {
            type_args, args, ..
        }) = &declaration.value
        else {
            panic!("expected typed call")
        };
        assert_eq!(type_args.len(), 1);
        assert_eq!(args.len(), 1);
        assert_eq!(
            crate::syntax::pretty::type_str(&type_args[0]),
            "unsigned[16]"
        );
    }

    #[test]
    fn instance_construction_explicit_and_positional() {
        // Explicit form: every `.field` carries a value.
        let m = parse_ok(
            "module m;\nimpl Test {\n  let c: Counter<W = 8> = {\n    .clk = clk,\n    .rst = rst,\n    .count = count8,\n  };\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Let(l) = &i.items[0] else {
            panic!("expected let")
        };
        let Some(Expr::Construct { args, .. }) = &l.value else {
            panic!("expected construct")
        };
        assert_eq!(args.len(), 3);
        assert!(args.iter().all(|a| a.field.is_some() && a.value.is_some()));

        // Positional form: bare expressions, no dots — lexes as a brace concat
        // whose parts elaboration binds to ports by order.
        let m = parse_ok("module m;\nimpl Test {\n  let c: Counter = { clk, rst, count8 };\n}\n");
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Let(l) = &i.items[0] else {
            panic!("expected let")
        };
        assert!(matches!(&l.value, Some(Expr::Concat { parts, .. }) if parts.len() == 3));
    }

    #[test]
    fn bare_field_shorthand_is_rejected() {
        // The old name-shorthand `.clk` (dot, no value) is no longer a form.
        let (_, errors) =
            parse("module m;\nimpl Test {\n  let c: Counter = { .clk, .rst = rst };\n}\n");
        assert!(errors > 0, "`.clk` without a value should be a parse error");
    }

    #[test]
    fn textual_logical_operators_and_precedence() {
        // `a and b or c` must parse as `(a and b) or c` (and binds tighter).
        let m = parse_ok("module m;\nimpl M {\n  y = a and b or c;\n}\n");
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::Assign { value, .. }) = &i.items[0] else {
            panic!()
        };
        let Expr::Binary { op, lhs, .. } = value else {
            panic!("expected binary")
        };
        assert_eq!(op, &BinOp::Or); // top-level is `or`
        assert!(matches!(**lhs, Expr::Binary { op: BinOp::And, .. }));
    }

    #[test]
    fn match_enum_and_wildcard() {
        let m = parse_ok(
            "module m;\nimpl M {\n  match state {\n    State::Idle => { next = State::Start; }\n    _ => next = State::Idle,\n  }\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::Match(mt)) = &i.items[0] else {
            panic!("expected match")
        };
        assert_eq!(mt.arms.len(), 2);
        assert!(matches!(mt.arms[0].pattern, Pattern::Path(_)));
        assert!(matches!(mt.arms[1].pattern, Pattern::Wildcard));
    }

    #[test]
    fn overflowing_integer_pattern_is_not_silently_zero() {
        let (_, errors) = parse(
            "module m;\nimpl M {\n\
             match value { 18446744073709551616 => y = 1, _ => y = 0 }\n\
             }\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn minimum_i64_pattern_is_accepted() {
        let module = parse_ok(
            "module m;\nimpl M {\n\
             match value { -9223372036854775808 => y = 1, _ => y = 0 }\n\
             }\n",
        );
        let Item::Impl(im) = &module.items[0] else {
            panic!("expected impl")
        };
        let ImplItem::Stmt(Stmt::Match(statement)) = &im.items[0] else {
            panic!("expected match")
        };
        assert!(matches!(
            statement.arms[0].pattern,
            Pattern::Range {
                lo: i64::MIN,
                hi: i64::MIN,
                ..
            }
        ));
    }

    #[test]
    fn attr_decl_application_and_extern_entity() {
        let m = parse_ok(
            "module m;\npub attr top: Bool for entity;\nattr keep: Bool for let, port;\n#[top]\nentity Top {\n  y: Bit out,\n}\nextern entity BlackBox<W: integer> {\n  a: unsigned[W] in,\n  b: unsigned[W] out,\n}\n",
        );
        let Item::AttrDecl(a) = &m.items[0] else {
            panic!("expected attr decl")
        };
        assert!(a.is_pub);
        assert_eq!(a.targets.len(), 1);
        let Item::AttrDecl(a2) = &m.items[1] else {
            panic!()
        };
        assert_eq!(a2.targets.len(), 2);
        let Item::Entity(top) = &m.items[2] else {
            panic!("expected entity")
        };
        assert_eq!(top.attrs.len(), 1);
        assert_eq!(top.attrs[0].name.segments[0].text, "top");
        let Item::Entity(bb) = &m.items[3] else {
            panic!()
        };
        assert!(bb.is_extern);
    }

    #[test]
    fn test_entity_with_stimulus() {
        let m = parse_ok(
            "module m;\n#[test]\nentity CounterTest {\n}\nimpl CounterTest {\n  let clk: Bit = '0';\n  let dut = Counter<W = 8> {\n    .clk = clk,\n    .count = count,\n  };\n  await 10ns;\n  rst = '0';\n  for i in 0..10 {\n    await clk.rising();\n  }\n  assert!(count == 10, \"counter should increment 10 times\");\n}\n",
        );
        let Item::Impl(i) = &m.items[1] else {
            panic!("expected impl")
        };
        // clk, dut, await, rst=, for, assert.
        assert_eq!(i.items.len(), 6);
        assert!(matches!(
            i.items[2],
            ImplItem::Stmt(Stmt::Expr(Expr::Call { .. }))
        ));
        assert!(matches!(i.items[4], ImplItem::Stmt(Stmt::For { .. })));
        let ImplItem::Stmt(Stmt::Expr(Expr::Call { bang, .. })) = &i.items[5] else {
            panic!("expected assert call")
        };
        assert!(*bang);
    }

    #[test]
    fn recovers_after_a_bad_item() {
        let (m, errors) = parse("module m;\n@@@ junk\nentity Good { y: Bit out, }\n");
        assert!(errors > 0);
        // The good entity after the junk still parses.
        assert!(m
            .items
            .iter()
            .any(|it| matches!(it, Item::Entity(e) if e.name.text == "Good")));
    }
}

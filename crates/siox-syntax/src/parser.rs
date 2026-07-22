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
//! - `a::b` is greedily a path, except a trailing `::<system-attribute>`
//!   (`event/old/rising/...`) which becomes [`Expr::SysAttr`]. The fixed
//!   system-attribute set is the only syntactic signal available pre-resolve.
//! - Float / hex-string literal tokens map to [`Expr::Int`] (which stores raw
//!   text); a dedicated literal node can be added when a later stage needs it.

use crate::ast::*;
use crate::token::{Token, TokenKind};
use siox_diag::{Diagnostic, DiagnosticSink, Span};
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
                            pending_precedence = text(&tokens[k]).parse::<u8>().ok();
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
                if tokens[j].kind == TokenKind::Ident && text(&tokens[j]) == "custom" {
                    while j < limit && tokens[j].kind != TokenKind::StrLit {
                        j += 1;
                    }
                    if j < limit {
                        out.insert(text(&tokens[j]).trim_matches('"').to_string(), precedence);
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

pub struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    sink: &'a mut DiagnosticSink,
    custom_operators: HashMap<String, u8>,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str, tokens: Vec<Token>, sink: &'a mut DiagnosticSink) -> Self {
        // Strip comment trivia so the grammar can ignore it. The trailing `Eof`
        // is always kept.
        let tokens: Vec<Token> =
            tokens.into_iter().filter(|t| t.kind != TokenKind::Comment).collect();
        Parser { src, tokens, pos: 0, sink, custom_operators: HashMap::new() }
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
        Module { path, items, span: start.to(self.prev_span()) }
    }

    fn parse_item(&mut self) -> Option<Item> {
        let attrs = self.parse_attrs();
        let is_pub = self.eat(TokenKind::Pub);
        let is_extern = self.eat(TokenKind::Extern);

        // `extern "C" { fn ...; }` — a foreign-function block.
        if is_extern && self.at(TokenKind::StrLit) {
            let start = self.span();
            let t = self.bump();
            let abi = self.text_of(t.span).trim_matches('"').to_string();
            if abi != "C" {
                self.error_at(t.span, "only the \"C\" ABI is supported");
            }
            self.expect(TokenKind::LBrace, "to open an extern block");
            let mut fns = Vec::new();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let before = self.pos;
                self.eat(TokenKind::Pub);
                if self.eat(TokenKind::Fn) {
                    let fstart = self.span();
                    let name = self.parse_ident();
                    let f = self.parse_fn_after_name(fstart, name);
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
            return Some(Item::ExternBlock { abi, fns, span: start.to(self.prev_span()) });
        }

        if !attrs.is_empty() && !matches!(self.kind(), TokenKind::Entity | TokenKind::Impl) {
            self.error_here("attributes are only allowed on entities and implementations");
        }

        let item = match self.kind() {
            TokenKind::Using => Item::Using(self.parse_using()),
            TokenKind::Const => Item::Const(self.parse_const(is_pub)),
            TokenKind::Fn => {
                let start = self.span();
                self.bump();
                let name = self.parse_ident();
                Item::Fn(self.parse_fn_after_name(start, name))
            }
            TokenKind::Struct => Item::Struct(self.parse_struct(is_pub)),
            TokenKind::Enum => Item::Enum(self.parse_enum(is_pub)),
            TokenKind::Entity => Item::Entity(self.parse_entity(attrs, is_pub, is_extern)),
            TokenKind::Impl => Item::Impl(self.parse_impl(attrs)),
            TokenKind::Trait => Item::Trait(self.parse_trait(is_pub)),
            TokenKind::Attr => Item::AttrDecl(self.parse_attr_decl(is_pub)),
            _ => {
                self.error_here(
                    "expected an item (using, const, fn, struct, enum, entity, impl, trait, attr)",
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
            let value = if self.eat(TokenKind::Eq) { Some(self.parse_expr(false)) } else { None };
            self.expect(TokenKind::RBracket, "to close an attribute");
            attrs.push(Attr { name, value, span: start.to(self.prev_span()) });
        }
        attrs
    }

    // --- using / const ------------------------------------------------------

    fn parse_using(&mut self) -> Using {
        let start = self.span();
        self.bump(); // `using`
        let path = self.parse_path();

        let kind = if self.at(TokenKind::ColonColon) && self.kind_at(self.pos + 1) == &TokenKind::LBrace
        {
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
            // `using Word = uint[32];`
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
            let base = Path { segments, span: path.span };
            UsingKind::Import { base, names: vec![name] }
        };
        self.expect(TokenKind::Semi, "after a `using`");
        Using { kind, span: start.to(self.prev_span()) }
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
        ConstDecl { is_pub, name, ty, value, span: start.to(self.prev_span()) }
    }

    // --- struct / enum ------------------------------------------------------

    fn parse_struct(&mut self, is_pub: bool) -> StructDecl {
        let start = self.span();
        self.bump(); // `struct`
        let name = self.parse_ident();
        let params = self.parse_params_opt();
        // Nominal derivation: `struct B : A` / `struct B : A { ... }`.
        let base = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        // A derived struct may be bodyless (`struct B : A;` newtype form).
        if base.is_some() && self.eat(TokenKind::Semi) {
            return StructDecl {
                is_pub,
                name,
                params,
                base,
                fields: Vec::new(),
                span: start.to(self.prev_span()),
            };
        }
        self.expect(TokenKind::LBrace, "to open a struct body");
        let mut fields = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let fstart = self.span();
            let fname = self.parse_ident();
            self.expect(TokenKind::Colon, "before a field type");
            let ty = self.parse_type();
            fields.push(Field { name: fname, ty, span: fstart.to(self.prev_span()) });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close a struct body");
        StructDecl { is_pub, name, params, base, fields, span: start.to(self.prev_span()) }
    }

    fn parse_enum(&mut self, is_pub: bool) -> EnumDecl {
        let start = self.span();
        self.bump(); // `enum`
        let name = self.parse_ident();
        let repr = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        // A derived enum may be bodyless (`enum Logic : ULogic;` — same
        // variants, new nominal type).
        if repr.is_some() && self.eat(TokenKind::Semi) {
            return EnumDecl { is_pub, name, repr, variants: Vec::new(), span: start.to(self.prev_span()) };
        }
        self.expect(TokenKind::LBrace, "to open an enum body");
        let mut variants = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let vstart = self.span();
            // Logic-literal variant names (`enum Bit { '0', '1' }`, spec Stage
            // 11) keep their quotes in the name text, matching use-site
            // literals.
            let vname = if self.at(TokenKind::CharacterLit) {
                let t = self.bump();
                Ident { text: self.text_of(t.span).to_string(), span: t.span }
            } else {
                self.parse_ident()
            };
            let value = if self.eat(TokenKind::Eq) { Some(self.parse_expr(false)) } else { None };
            variants.push(EnumVariant { name: vname, value, span: vstart.to(self.prev_span()) });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close an enum body");
        EnumDecl { is_pub, name, repr, variants, span: start.to(self.prev_span()) }
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
            ports.push(self.parse_port());
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close an entity body");
        EntityDecl { attrs, is_pub, is_extern, name, params, ports, span: start.to(self.prev_span()) }
    }

    fn parse_port(&mut self) -> Port {
        let start = self.span();
        // A leading direction keyword is the port direction (`in clk: Bit`).
        // Otherwise direction comes from the type's bus mode (`bus: in Packet`).
        let dir = self.eat_direction();
        let name = self.parse_ident();
        self.expect(TokenKind::Colon, "before a port type");
        let ty = self.parse_type();
        self.expect(TokenKind::Semi, "after a port");
        Port { dir, name, ty, span: start.to(self.prev_span()) }
    }

    // --- impl ---------------------------------------------------------------

    fn parse_impl(&mut self, attrs: Vec<Attr>) -> ImplDecl {
        let start = self.span();
        self.bump(); // `impl`

        // Leading direction for a bus-mode impl without a trait: `impl out S::Source`.
        let dir1 = self.eat_direction();
        // `impl "+" for T` names an operator trait by its quoted string.
        let head_path = if self.at(TokenKind::StrLit) {
            let name = self.parse_trait_name();
            let span = name.span;
            Path { segments: vec![name], span }
        } else {
            self.parse_path()
        };
        // A `<name: bound>` list declares impl parameters; a `<expr>` list is
        // generic arguments that stay inside the target type.
        let mut params = Params::default();
        let head_args = if self.at(TokenKind::Lt) {
            if self.angle_is_param_list(self.pos) {
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
            let mode_dir = self.eat_direction();
            let target = self.parse_type_after_optional_dir(mode_dir);
            let items = self.parse_impl_body();
            return ImplDecl {
                attrs,
                params,
                trait_,
                trait_args: head_args.unwrap_or_default(),
                mode_dir: dir1.or(mode_dir),
                target,
                items,
                span: start.to(self.prev_span()),
            };
        }

        // No trait: the head is the target type.
        let head_span = head_path.span;
        let mut target = Type::Path(head_path);
        if let Some(args) = head_args {
            target = Type::Generic { base: Box::new(target), args, span: head_span };
        }
        // Optional bus-mode `::Source` suffix and array suffixes on the target.
        if dir1.is_some() {
            let mode = if self.eat(TokenKind::ColonColon) { Some(self.parse_ident()) } else { None };
            target = Type::Mode {
                dir: dir1.unwrap(),
                inner: Box::new(target),
                mode,
                span: head_span.to(self.prev_span()),
            };
        }
        let items = self.parse_impl_body();
        ImplDecl {
            attrs,
            params,
            trait_: None,
            trait_args: Vec::new(),
            mode_dir: dir1,
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
                Some(ImplItem::Fn(self.parse_fn_after_name(start, name)))
            }
            TokenKind::In | TokenKind::Out | TokenKind::Inout => {
                // Bus-mode leaf direction: `in clk;`.
                let start = self.span();
                let dir = self.eat_direction().unwrap();
                let name = self.parse_ident();
                self.expect(TokenKind::Semi, "after a bus-mode field");
                Some(ImplItem::ModeField { dir, name, span: start.to(self.prev_span()) })
            }
            _ => Some(ImplItem::Stmt(self.parse_stmt())),
        }
    }

    fn parse_let_after_name(&mut self, start: Span, name: Ident) -> LetDecl {
        self.parse_let_rest(Vec::new(), start, name)
    }

    fn parse_let_rest(&mut self, attrs: Vec<Attr>, start: Span, name: Ident) -> LetDecl {
        let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        let value = if self.eat(TokenKind::Eq) { Some(self.parse_expr(false)) } else { None };
        self.expect(TokenKind::Semi, "after a `let`");
        LetDecl { attrs, name, ty, value, span: start.to(self.prev_span()) }
    }

    fn parse_fn_after_name(&mut self, start: Span, name: Ident) -> FnDecl {
        let mut generics = self.parse_params_opt();
        let params = self.parse_fn_params();
        let ret = if self.eat(TokenKind::Arrow) { Some(self.parse_type()) } else { None };
        self.parse_where_into(&mut generics);
        let body = if self.at(TokenKind::LBrace) {
            Some(self.parse_block())
        } else {
            self.expect(TokenKind::Semi, "after a method signature");
            None
        };
        FnDecl { name, generics, params, ret, body, span: start.to(self.prev_span()) }
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
                let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
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
            let name = crate::ast::op_trait_name(&text).unwrap_or("Add").to_string();
            self.error_at(
                t.span,
                format!("quoted operator traits were removed; use the Rust-style name (`{name}`)"),
            );
            Ident { text: name, span: t.span }
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
            if !self.eat(TokenKind::Fn) {
                self.error_here("expected a `fn` method signature in trait body");
                break;
            }
            let mname = self.parse_ident();
            items.push(self.parse_fn_after_name(istart, mname));
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close a trait body");
        TraitDecl { is_pub, name, params, items, span: start.to(self.prev_span()) }
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
        AttrDecl { is_pub, name, ty, targets, span: start.to(self.prev_span()) }
    }

    // --- statements ---------------------------------------------------------

    fn parse_block(&mut self) -> Block {
        let start = self.span();
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
        Block { stmts, span: start.to(self.prev_span()) }
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
                let value = if self.at(TokenKind::Semi) { None } else { Some(self.parse_expr(false)) };
                self.expect(TokenKind::Semi, "after a `return`");
                Stmt::Return { value, span: start.to(self.prev_span()) }
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
                let callee = Expr::Path(Path { segments: vec![ident], span: start });
                let arg = self.parse_expr(false);
                self.expect(TokenKind::Semi, "after a timing primitive");
                let span = start.to(self.prev_span());
                Stmt::Expr(Expr::Call { callee: Box::new(callee), args: vec![arg], bang: false, span })
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
            Stmt::Assign { target: lhs, value, after, span: start.to(self.prev_span()) }
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
            Stmt::Assign { target: lhs, value, after: None, span }
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
        IfStmt { cond, then, else_, span: start.to(self.prev_span()) }
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
                // Single statement arm: `b"00??" => op = Op::Alu,`.
                let sstart = self.span();
                let stmt = self.parse_arm_single_stmt();
                Block { stmts: vec![stmt], span: sstart.to(self.prev_span()) }
            };
            self.eat(TokenKind::Comma);
            arms.push(MatchArm { pattern, body, span: astart.to(self.prev_span()) });
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "to close a match body");
        MatchStmt { scrutinee, arms, span: start.to(self.prev_span()) }
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
            return Stmt::Return { value, span: start.to(self.prev_span()) };
        }
        let lhs = self.parse_expr(false);
        if self.eat(TokenKind::Eq) {
            let value = self.parse_expr(false);
            Stmt::Assign { target: lhs, value, after: None, span: start.to(self.prev_span()) }
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
        Stmt::For { var, range, body, span: start.to(self.prev_span()) }
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
        Pattern::Or { alts, span: start.to(self.prev_span()) }
    }

    fn parse_pattern_atom(&mut self) -> Pattern {
        match self.kind() {
            TokenKind::Ident if self.cur_text() == "_" => {
                self.bump();
                Pattern::Wildcard
            }
            // A bit pattern `b"01??"` / `x"A?"` (spec 3.22): a one-letter
            // prefix glued to a string, like the bit-string literal. `?`
            // digits are don't-cares.
            TokenKind::Ident
                if matches!(self.cur_text(), "x" | "b")
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
            // An integer literal (`5`) or inclusive range (`0..9`, `-1..1`).
            TokenKind::Int | TokenKind::Minus => {
                let start = self.span();
                let lo = self.parse_pattern_int();
                let hi = if self.eat(TokenKind::DotDot) {
                    self.parse_pattern_int()
                } else {
                    lo
                };
                Pattern::Range { lo, hi, span: start.to(self.prev_span()) }
            }
            _ => Pattern::Path(self.parse_path()),
        }
    }

    /// A (possibly negative, hex/binary/decimal) integer literal in a pattern.
    fn parse_pattern_int(&mut self) -> i64 {
        let neg = self.eat(TokenKind::Minus);
        let t = self.bump();
        let txt = self.text_of(t.span).replace('_', "");
        let v = if let Some(h) = txt.strip_prefix("0x").or_else(|| txt.strip_prefix("0X")) {
            i64::from_str_radix(h, 16).unwrap_or(0)
        } else if let Some(b) = txt.strip_prefix("0b").or_else(|| txt.strip_prefix("0B")) {
            i64::from_str_radix(b, 2).unwrap_or(0)
        } else {
            txt.parse().unwrap_or(0)
        };
        if neg {
            -v
        } else {
            v
        }
    }

    // --- expressions (Pratt) ------------------------------------------------

    fn parse_expr(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
        let lhs = self.parse_bin(0, no_struct);
        if self.at(TokenKind::DotDot) {
            self.bump();
            let hi = self.parse_bin(0, no_struct);
            Expr::Range { lo: Box::new(lhs), hi: Box::new(hi), span: start.to(self.prev_span()) }
        } else {
            lhs
        }
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
                BinOp::Custom { symbol: symbol.clone(), precedence },
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
        let op = match self.kind() {
            TokenKind::Minus => Some(UnOp::Neg),
            // `not` is the textual logical-negation prefix operator.
            TokenKind::Ident if self.cur_text() == "not" => Some(UnOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let rhs = self.parse_unary(no_struct);
            return Expr::Unary { op, rhs: Box::new(rhs), span: start.to(self.prev_span()) };
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
        self.expect(TokenKind::Else, "— an `if` used as a value needs an `else` branch");
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
                // A remaining `::` here is a system attribute (`x::old`); plain
                // `::` path segments were already absorbed by `parse_primary`.
                TokenKind::ColonColon => {
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
                Expr::LogicLit { ch, span: t.span }
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
                let value = self.cur_text() == "true";
                let t = self.bump();
                Expr::Bool { value, span: t.span }
            }
            // A one-letter prefix glued to a string is a bit-string literal:
            // `x"123ABC"` (hex) / `b"0101"` (binary).
            TokenKind::Ident
                if matches!(self.cur_text(), "x" | "b")
                    && self.kind_at(self.pos + 1) == &TokenKind::StrLit
                    && self.span_at(self.pos + 1).start == self.span().end =>
            {
                let p = self.bump();
                let base = self.text_of(p.span).chars().next().unwrap_or('x');
                let t = self.bump();
                let digits = self.text_of(t.span).trim_matches('"').to_string();
                Expr::BitStrLit { base, digits, span: p.span.to(t.span) }
            }
            TokenKind::Ident | TokenKind::SelfKw => self.parse_path_expr_or_construct(no_struct),
            // A leading `{`: `{ .field = ... }` is a name-less struct literal
            // (typed from context); `{ a, b }` is a bit concatenation.
            TokenKind::LBrace
                if matches!(self.kind_at(self.pos + 1), TokenKind::Dot | TokenKind::DotDot) =>
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
                Expr::Array { elems, span: start.to(self.prev_span()) }
            }
            _ => {
                self.error_here("expected an expression");
                // Synthesize a placeholder so callers can keep going.
                Expr::Int { text: String::new(), span: start }
            }
        }
    }

    fn parse_concat(&mut self, start: Span) -> Expr {
        self.expect(TokenKind::LBrace, "to open a concatenation");
        let mut parts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            parts.push(self.parse_expr(false));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close a concatenation");
        Expr::Concat { parts, span: start.to(self.prev_span()) }
    }

    /// A path expression, possibly an instance/struct construction. Path
    /// extension stops before a trailing `::<system-attribute>`, which the
    /// postfix loop turns into a [`Expr::SysAttr`].
    fn parse_path_expr_or_construct(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
        let mut segments = vec![self.parse_ident()];
        while self.at(TokenKind::ColonColon)
            && self.kind_at(self.pos + 1) == &TokenKind::Ident
            && !is_sysattr(self.text_at(self.pos + 1))
        {
            self.bump(); // `::`
            segments.push(self.parse_ident());
        }
        let path = Path { segments, span: start.to(self.prev_span()) };

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
            args.push(ConnectArg { field, value, span: cstart.to(self.prev_span()) });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close a construction");
        Expr::Construct { ty, args, spread, span: start.to(self.prev_span()) }
    }

    // --- types --------------------------------------------------------------

    fn parse_type(&mut self) -> Type {
        let start = self.span();
        if let Some(dir) = self.eat_direction() {
            let inner = self.parse_type_core();
            let mode = if self.eat(TokenKind::ColonColon) { Some(self.parse_ident()) } else { None };
            return Type::Mode {
                dir,
                inner: Box::new(inner),
                mode,
                span: start.to(self.prev_span()),
            };
        }
        self.parse_type_core()
    }

    fn parse_type_after_optional_dir(&mut self, dir: Option<Direction>) -> Type {
        let start = self.prev_span();
        match dir {
            Some(dir) => {
                let inner = self.parse_type_core();
                let mode = if self.eat(TokenKind::ColonColon) { Some(self.parse_ident()) } else { None };
                Type::Mode {
                    dir,
                    inner: Box::new(inner),
                    mode,
                    span: start.to(self.prev_span()),
                }
            }
            None => self.parse_type_core(),
        }
    }

    fn parse_type_core(&mut self) -> Type {
        let start = self.span();
        let path = self.parse_path();
        let mut ty = Type::Path(path);
        if self.at(TokenKind::Lt) {
            let args = self.parse_generic_args();
            ty = Type::Generic { base: Box::new(ty), args, span: start.to(self.prev_span()) };
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
            ty = Type::Indexed { base: Box::new(ty), index, span: start.to(self.prev_span()) };
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
                let value = self.parse_generic_value();
                args.push(GenericArg::Named { name, value });
            } else {
                args.push(GenericArg::Positional(self.parse_generic_value()));
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.close_generic("to close a generic argument list");
        args
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
            return Expr::Range { lo: Box::new(lo), hi: Box::new(hi), span };
        }
        lo
    }

    fn parse_generic_atom(&mut self) -> Expr {
        if self.at(TokenKind::Minus) {
            let start = self.span();
            self.bump();
            let rhs = self.parse_postfix(false);
            let span = start.to(self.prev_span());
            return Expr::Unary { op: UnOp::Neg, rhs: Box::new(rhs), span };
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
            let bound = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
            params.push(Param { name, bound, span: pstart.to(self.prev_span()) });
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
        Path { segments, span: start.to(self.prev_span()) }
    }

    fn parse_ident(&mut self) -> Ident {
        if self.at(TokenKind::Ident) || self.at(TokenKind::SelfKw) {
            let t = self.bump();
            Ident { text: self.text_of(t.span).to_string(), span: t.span }
        } else {
            self.error_here("expected an identifier");
            Ident { text: String::new(), span: self.span() }
        }
    }

    /// Like [`Self::parse_ident`] but also accepts keyword tokens used as plain
    /// names (e.g. attribute targets `entity`, `let`, `port`).
    fn parse_word(&mut self) -> Ident {
        if self.is_name_token() {
            let t = self.bump();
            Ident { text: self.text_of(t.span).to_string(), span: t.span }
        } else {
            self.error_here("expected a name");
            Ident { text: String::new(), span: self.span() }
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

    fn text_at(&self, i: usize) -> &str {
        self.text_of(self.tokens[i.min(self.tokens.len() - 1)].span)
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
            self.tokens[self.pos] =
                Token { kind: TokenKind::Gt, span: Span::new(sp.file, sp.start + 1..sp.end) };
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

    fn expect(&mut self, k: TokenKind, ctx: &str) -> bool {
        if self.at(k.clone()) {
            self.bump();
            true
        } else {
            let span = self.span();
            self.error_at(span, format!("expected {:?} {}", k, ctx));
            false
        }
    }

    fn error_here(&mut self, msg: impl Into<String>) {
        let span = self.span();
        self.error_at(span, msg);
    }

    fn error_at(&mut self, span: Span, msg: impl Into<String>) {
        self.sink.emit(Diagnostic::error(msg).at(span));
    }
}

/// The fixed set of system attributes (`x::event`, `clk.rising()`, `d::width`).
/// A `::`-suffix matching one of these reads as a [`Expr::SysAttr`] rather than
/// extending a path. Spec 3.9 / 3.10 / 3.23.
fn is_sysattr(name: &str) -> bool {
    matches!(
        name,
        // Phase 1 digital + range attributes (spec 3.9 / 3.23). `rising`/
        // `falling`/`edge` are still lexed as sysattrs only so the type checker
        // reports them as unknown attributes rather than silently reading them
        // as a path — they are std `ClockLike` methods now, not attributes.
        "event"
            | "old"
            | "rising"
            | "falling"
            | "edge"
            | "width"
            | "len"
            | "range"
            | "high"
            | "low"
            | "left"
            | "right"
            | "direction"
            // `::ddt` is analogue (Phase 2); recognized only so it parses as a
            // system attribute and can be rejected rather than silently accepted
            // (spec Stage 4). The rest of the analogue set is a Phase-2 concern.
            | "ddt"
    )
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
    use super::*;
    use siox_diag::FileId;

    fn parse(src: &str) -> (Module, usize) {
        let mut sink = DiagnosticSink::new();
        let m = crate::parse_module(FileId(0), src, &mut sink);
        (m, sink.error_count())
    }

    fn parse_ok(src: &str) -> Module {
        let (m, errors) = parse(src);
        assert_eq!(errors, 0, "unexpected parse errors in:\n{src}");
        m
    }

    #[test]
    fn module_header_and_imports() {
        let m = parse_ok("module std::logic;\nusing std::logic::{Bit, Logic};\nusing Word = uint[32];\n");
        assert_eq!(m.path.segments.len(), 2);
        assert_eq!(m.path.segments[1].text, "logic");
        assert_eq!(m.items.len(), 2);
        assert!(matches!(&m.items[0], Item::Using(_)));
    }

    #[test]
    fn entity_with_params_and_ports() {
        let m = parse_ok(
            "module m;\nentity Counter<W: integer> {\n  in clk: Bit;\n  in rst: Logic;\n  in en: Bit;\n  out count: uint[W];\n}\n",
        );
        let Item::Entity(e) = &m.items[0] else { panic!("expected entity") };
        assert_eq!(e.name.text, "Counter");
        assert_eq!(e.params.params.len(), 1);
        assert_eq!(e.ports.len(), 4);
        assert_eq!(e.ports[0].dir, Some(Direction::In));
        assert_eq!(e.ports[3].dir, Some(Direction::Out));
    }

    #[test]
    fn struct_enum_const() {
        let m = parse_ok(
            "module m;\nconst DEFAULT_WIDTH: usize = 8;\nstruct Packet<T> { valid: Bit, data: T }\nenum State: uint[2] { Idle = 0, Start = 1, Shift = 2, Done = 3 }\n",
        );
        assert_eq!(m.items.len(), 3);
        let Item::Enum(e) = &m.items[2] else { panic!("expected enum") };
        assert_eq!(e.variants.len(), 4);
        assert!(e.repr.is_some());
    }

    #[test]
    fn impl_with_state_and_sequential_block() {
        let m = parse_ok(
            "module m;\nimpl Counter<W: integer> {\n  const MAX: uint[W] = (1 << W) - 1;\n  let value: uint[W] = 0;\n  if clk.rising() {\n    if rst == '1' {\n      value = 0;\n    } else {\n      value = value + 1;\n    }\n  }\n  count = value;\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!("expected impl") };
        assert_eq!(i.params.params.len(), 1);
        // const, let, if-block, assignment.
        assert_eq!(i.items.len(), 4);
        assert!(matches!(i.items[0], ImplItem::Const(_)));
        assert!(matches!(i.items[1], ImplItem::Let(_)));
        assert!(matches!(i.items[2], ImplItem::Stmt(Stmt::If(_))));
        assert!(matches!(i.items[3], ImplItem::Stmt(Stmt::Assign { .. })));
    }

    #[test]
    fn sysattr_vs_path_in_expressions() {
        let m = parse_ok(
            "module m;\nimpl M {\n  if state::old == State::Idle {\n    started = '1';\n  }\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::If(iff)) = &i.items[0] else { panic!("expected if") };
        // LHS of `==` is `state::old` (SysAttr); RHS is `State::Idle` (Path).
        let Expr::Binary { lhs, rhs, op, .. } = &iff.cond else { panic!("expected binary") };
        assert_eq!(op, &BinOp::Eq);
        assert!(matches!(**lhs, Expr::SysAttr { .. }));
        let Expr::Path(p) = &**rhs else { panic!("expected path") };
        assert_eq!(p.segments.len(), 2);
    }

    #[test]
    fn trait_and_clocklike_impl() {
        let m = parse_ok(
            "module m;\ntrait ClockLike {\n  fn rising(self);\n  fn edge(self);\n}\nimpl ClockLike for Logic {\n  fn rising(self) {\n    return self::event and self::old == '0' and self == '1';\n  }\n  fn edge(self) {\n    return self::event;\n  }\n}\n",
        );
        let Item::Trait(t) = &m.items[0] else { panic!("expected trait") };
        assert_eq!(t.items.len(), 2);
        assert!(t.items[0].body.is_none());
        let Item::Impl(i) = &m.items[1] else { panic!("expected impl") };
        assert_eq!(i.trait_.as_ref().unwrap().segments[0].text, "ClockLike");
        assert!(matches!(i.items[0], ImplItem::Fn(_)));
    }

    #[test]
    fn bus_modes_and_construction() {
        let m = parse_ok(
            "module m;\nstruct Stream<T> { clk: Bit, valid: Bit, ready: Bit, data: T }\nimpl out Stream<T>::Source {\n  in clk;\n  out valid;\n  in ready;\n}\nentity Producer {\n  bus: out Stream<uint[32]>::Source;\n}\n",
        );
        let Item::Impl(i) = &m.items[1] else { panic!("expected impl") };
        assert!(matches!(i.mode_dir, Some(Direction::Out)));
        assert!(matches!(&i.target, Type::Mode { mode: Some(_), .. }));
        assert!(matches!(i.items[0], ImplItem::ModeField { dir: Direction::In, .. }));
        let Item::Entity(e) = &m.items[2] else { panic!("expected entity") };
        assert_eq!(e.ports[0].dir, None); // direction lives in the bus-mode type
        assert!(matches!(&e.ports[0].ty, Type::Mode { .. }));
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
        let ImplItem::Stmt(Stmt::Assign { value, .. }) = &i.items[0] else { panic!() };
        assert!(matches!(value, Expr::Binary { op: BinOp::Shr, .. }), "`a >> b` stays a shift");
    }

    #[test]
    fn instance_construction_explicit_and_positional() {
        // Explicit form: every `.field` carries a value.
        let m = parse_ok(
            "module m;\nimpl Test {\n  let c: Counter<W = 8> = {\n    .clk = clk,\n    .rst = rst,\n    .count = count8,\n  };\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Let(l) = &i.items[0] else { panic!("expected let") };
        let Some(Expr::Construct { args, .. }) = &l.value else { panic!("expected construct") };
        assert_eq!(args.len(), 3);
        assert!(args.iter().all(|a| a.field.is_some() && a.value.is_some()));

        // Positional form: bare expressions, no dots — lexes as a brace concat
        // whose parts elaboration binds to ports by order.
        let m = parse_ok("module m;\nimpl Test {\n  let c: Counter = { clk, rst, count8 };\n}\n");
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Let(l) = &i.items[0] else { panic!("expected let") };
        assert!(matches!(&l.value, Some(Expr::Concat { parts, .. }) if parts.len() == 3));
    }

    #[test]
    fn bare_field_shorthand_is_rejected() {
        // The old name-shorthand `.clk` (dot, no value) is no longer a form.
        let (_, errors) = parse("module m;\nimpl Test {\n  let c: Counter = { .clk, .rst = rst };\n}\n");
        assert!(errors > 0, "`.clk` without a value should be a parse error");
    }

    #[test]
    fn textual_logical_operators_and_precedence() {
        // `a and b or c` must parse as `(a and b) or c` (and binds tighter).
        let m = parse_ok("module m;\nimpl M {\n  y = a and b or c;\n}\n");
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::Assign { value, .. }) = &i.items[0] else { panic!() };
        let Expr::Binary { op, lhs, .. } = value else { panic!("expected binary") };
        assert_eq!(op, &BinOp::Or); // top-level is `or`
        assert!(matches!(**lhs, Expr::Binary { op: BinOp::And, .. }));
    }

    #[test]
    fn match_enum_and_wildcard() {
        let m = parse_ok(
            "module m;\nimpl M {\n  match state {\n    State::Idle => { next = State::Start; }\n    _ => next = State::Idle,\n  }\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::Match(mt)) = &i.items[0] else { panic!("expected match") };
        assert_eq!(mt.arms.len(), 2);
        assert!(matches!(mt.arms[0].pattern, Pattern::Path(_)));
        assert!(matches!(mt.arms[1].pattern, Pattern::Wildcard));
    }

    #[test]
    fn attr_decl_application_and_extern_entity() {
        let m = parse_ok(
            "module m;\npub attr top: Bool for entity;\nattr keep: Bool for let, port;\n#[top]\nentity Top {\n  out y: Bit;\n}\nextern entity BlackBox<W: integer> {\n  in a: uint[W];\n  out b: uint[W];\n}\n",
        );
        let Item::AttrDecl(a) = &m.items[0] else { panic!("expected attr decl") };
        assert!(a.is_pub);
        assert_eq!(a.targets.len(), 1);
        let Item::AttrDecl(a2) = &m.items[1] else { panic!() };
        assert_eq!(a2.targets.len(), 2);
        let Item::Entity(top) = &m.items[2] else { panic!("expected entity") };
        assert_eq!(top.attrs.len(), 1);
        assert_eq!(top.attrs[0].name.segments[0].text, "top");
        let Item::Entity(bb) = &m.items[3] else { panic!() };
        assert!(bb.is_extern);
    }

    #[test]
    fn test_entity_with_stimulus() {
        let m = parse_ok(
            "module m;\n#[test]\nentity CounterTest {\n}\nimpl CounterTest {\n  let clk: Bit = '0';\n  let dut = Counter<W = 8> {\n    .clk = clk,\n    .count = count,\n  };\n  await 10ns;\n  rst = '0';\n  for i in 0..10 {\n    await clk.rising();\n  }\n  assert!(count == 10, \"counter should increment 10 times\");\n}\n",
        );
        let Item::Impl(i) = &m.items[1] else { panic!("expected impl") };
        // clk, dut, await, rst=, for, assert.
        assert_eq!(i.items.len(), 6);
        assert!(matches!(i.items[2], ImplItem::Stmt(Stmt::Expr(Expr::Call { .. }))));
        assert!(matches!(i.items[4], ImplItem::Stmt(Stmt::For { .. })));
        let ImplItem::Stmt(Stmt::Expr(Expr::Call { bang, .. })) = &i.items[5] else {
            panic!("expected assert call")
        };
        assert!(*bang);
    }

    #[test]
    fn recovers_after_a_bad_item() {
        let (m, errors) = parse("module m;\n@@@ junk\nentity Good { out y: Bit; }\n");
        assert!(errors > 0);
        // The good entity after the junk still parses.
        assert!(m.items.iter().any(|it| matches!(it, Item::Entity(e) if e.name.text == "Good")));
    }
}

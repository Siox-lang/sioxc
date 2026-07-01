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
//! - `<=`, `>=`, `!=` are not yet lexable, so only `==`, `<`, `>` compare.

use crate::ast::*;
use crate::token::{Token, TokenKind};
use siox_diag::{Diagnostic, DiagnosticSink, Span};

pub struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    sink: &'a mut DiagnosticSink,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str, tokens: Vec<Token>, sink: &'a mut DiagnosticSink) -> Self {
        // Strip comment trivia so the grammar can ignore it. The trailing `Eof`
        // is always kept.
        let tokens: Vec<Token> =
            tokens.into_iter().filter(|t| t.kind != TokenKind::Comment).collect();
        Parser { src, tokens, pos: 0, sink }
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

        if !attrs.is_empty() && !self.at(TokenKind::Entity) {
            self.error_here("attributes are only allowed on entities");
        }

        let item = match self.kind() {
            TokenKind::Using => Item::Using(self.parse_using()),
            TokenKind::Const => Item::Const(self.parse_const(is_pub)),
            TokenKind::Struct => Item::Struct(self.parse_struct(is_pub)),
            TokenKind::Enum => Item::Enum(self.parse_enum(is_pub)),
            TokenKind::Entity => Item::Entity(self.parse_entity(attrs, is_pub, is_extern)),
            TokenKind::Impl => Item::Impl(self.parse_impl()),
            TokenKind::Trait => Item::Trait(self.parse_trait(is_pub)),
            TokenKind::Attr => Item::AttrDecl(self.parse_attr_decl(is_pub)),
            _ => {
                self.error_here(
                    "expected an item (using, const, struct, enum, entity, impl, trait, attr)",
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
                names.push(self.parse_ident());
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
        StructDecl { is_pub, name, params, fields, span: start.to(self.prev_span()) }
    }

    fn parse_enum(&mut self, is_pub: bool) -> EnumDecl {
        let start = self.span();
        self.bump(); // `enum`
        let name = self.parse_ident();
        let repr = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        self.expect(TokenKind::LBrace, "to open an enum body");
        let mut variants = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let vstart = self.span();
            // Logic-literal variant names (`enum Bit { '0', '1' }`, spec Stage
            // 11) keep their quotes in the name text, matching use-site
            // literals.
            let vname = if self.at(TokenKind::LogicLit) {
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
        // A leading direction keyword is the port direction (`in clk: Clock`).
        // Otherwise direction comes from the type's bus mode (`bus: in Packet`).
        let dir = self.eat_direction();
        let name = self.parse_ident();
        self.expect(TokenKind::Colon, "before a port type");
        let ty = self.parse_type();
        self.expect(TokenKind::Semi, "after a port");
        Port { dir, name, ty, span: start.to(self.prev_span()) }
    }

    // --- impl ---------------------------------------------------------------

    fn parse_impl(&mut self) -> ImplDecl {
        let start = self.span();
        self.bump(); // `impl`

        // Leading direction for a bus-mode impl without a trait: `impl out S::Source`.
        let dir1 = self.eat_direction();
        let head_path = self.parse_path();
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
            // `impl Trait<...> for [dir] Target`.
            self.bump();
            let trait_ = Some(head_path);
            let mode_dir = self.eat_direction();
            let target = self.parse_type_after_optional_dir(mode_dir);
            let items = self.parse_impl_body();
            return ImplDecl {
                params,
                trait_,
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
        ImplDecl { params, trait_: None, mode_dir: dir1, target, items, span: start.to(self.prev_span()) }
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
        match self.kind() {
            TokenKind::Const => Some(ImplItem::Const(self.parse_const(false))),
            // `let value: T = e;` is state/signal; `fn send(self, ...) { ... }`
            // is a method.
            TokenKind::Let => {
                let start = self.span();
                self.bump();
                let name = self.parse_ident();
                Some(ImplItem::Let(self.parse_let_after_name(start, name)))
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
        let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        let value = if self.eat(TokenKind::Eq) { Some(self.parse_expr(false)) } else { None };
        self.expect(TokenKind::Semi, "after a `let`");
        LetDecl { name, ty, value, span: start.to(self.prev_span()) }
    }

    fn parse_fn_after_name(&mut self, start: Span, name: Ident) -> FnDecl {
        let params = self.parse_fn_params();
        let ret = if self.eat(TokenKind::Arrow) { Some(self.parse_type()) } else { None };
        let body = if self.at(TokenKind::LBrace) {
            Some(self.parse_block())
        } else {
            self.expect(TokenKind::Semi, "after a method signature");
            None
        };
        FnDecl { name, params, ret, body, span: start.to(self.prev_span()) }
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

    fn parse_trait(&mut self, is_pub: bool) -> TraitDecl {
        let start = self.span();
        self.bump(); // `trait`
        let name = self.parse_ident();
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
            // `wait <expr>;` simulation primitive (no parens): modeled as a call.
            TokenKind::Ident if self.cur_text() == "wait" => {
                let start = self.span();
                let callee = Expr::Path(Path {
                    segments: vec![self.parse_ident()],
                    span: start,
                });
                let arg = self.parse_expr(false);
                self.expect(TokenKind::Semi, "after a `wait`");
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
            self.expect(TokenKind::Semi, "after an assignment");
            Stmt::Assign { target: lhs, value, span: start.to(self.prev_span()) }
        } else {
            // No implicit tail-expression returns: every expression statement is
            // terminated by `;`. A function returns a value via `return`.
            self.expect(TokenKind::Semi, "after an expression statement");
            Stmt::Expr(lhs)
        }
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
            Stmt::Assign { target: lhs, value, span: start.to(self.prev_span()) }
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
        match self.kind() {
            TokenKind::Ident if self.cur_text() == "_" => {
                self.bump();
                Pattern::Wildcard
            }
            // Bit-pattern patterns (`b"01??"`) will return via the string-overload
            // mechanism; for now a pattern is a wildcard or an enum path.
            _ => Pattern::Path(self.parse_path()),
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
            let (op, lbp, rbp) = match self.kind() {
                TokenKind::Star => (BinOp::Mul, 90, 91),
                TokenKind::Slash => (BinOp::Div, 90, 91),
                TokenKind::Plus => (BinOp::Add, 80, 81),
                TokenKind::Minus => (BinOp::Sub, 80, 81),
                TokenKind::Shl => (BinOp::Shl, 70, 71),
                TokenKind::Shr => (BinOp::Shr, 70, 71),
                TokenKind::Lt => (BinOp::Lt, 60, 61),
                TokenKind::Gt => (BinOp::Gt, 60, 61),
                TokenKind::EqEq => (BinOp::Eq, 50, 51),
                // Textual logical operators (`and`/`or`/...). They lex as plain
                // identifiers and are recognised here in operator position.
                TokenKind::Ident => match self.cur_text() {
                    "and" => (BinOp::And, 40, 41),
                    "nand" => (BinOp::Nand, 40, 41),
                    "xor" => (BinOp::Xor, 35, 36),
                    "xnor" => (BinOp::Xnor, 35, 36),
                    "or" => (BinOp::Or, 30, 31),
                    "nor" => (BinOp::Nor, 30, 31),
                    _ => break,
                },
                _ => break,
            };
            if lbp < min_bp {
                break;
            }
            self.bump();
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

    fn parse_unary(&mut self, no_struct: bool) -> Expr {
        let start = self.span();
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
                Expr::Int { text: self.text_of(t.span).to_string(), span: t.span }
            }
            TokenKind::LogicLit => {
                let t = self.bump();
                let text = self.text_of(t.span);
                let ch = text.chars().nth(1).unwrap_or('?');
                Expr::LogicLit { ch, span: t.span }
            }
            TokenKind::StrLit => {
                let t = self.bump();
                let raw = self.text_of(t.span);
                let text = raw.trim_matches('"').to_string();
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
            TokenKind::Ident | TokenKind::SelfKw => self.parse_path_expr_or_construct(no_struct),
            // A leading `{`: `{ .field = ... }` is a name-less struct literal
            // (typed from context); `{ a, b }` is a bit concatenation.
            TokenKind::LBrace if self.kind_at(self.pos + 1) == &TokenKind::Dot => {
                self.parse_construct(start, None)
            }
            TokenKind::LBrace => self.parse_concat(start),
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
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let cstart = self.span();
            self.expect(TokenKind::Dot, "before a connection field");
            let field = self.parse_ident();
            let value = if self.eat(TokenKind::Eq) { Some(self.parse_expr(false)) } else { None };
            args.push(ConnectArg { field, value, span: cstart.to(self.prev_span()) });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "to close a construction");
        Expr::Construct { ty, args, span: start.to(self.prev_span()) }
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
            let index = self.parse_expr(false);
            self.expect(TokenKind::RBracket, "to close a type index");
            ty = Type::Indexed {
                base: Box::new(ty),
                index: Box::new(index),
                span: start.to(self.prev_span()),
            };
        }
        ty
    }

    fn parse_generic_args(&mut self) -> Vec<GenericArg> {
        self.expect(TokenKind::Lt, "to open a generic argument list");
        let mut args = Vec::new();
        while !self.at(TokenKind::Gt) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Ident) && self.kind_at(self.pos + 1) == &TokenKind::Eq {
                let name = self.parse_ident();
                self.bump(); // `=`
                let value = self.parse_postfix(false);
                args.push(GenericArg::Named { name, value });
            } else {
                args.push(GenericArg::Positional(self.parse_postfix(false)));
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "to close a generic argument list");
        args
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
        while !self.at(TokenKind::Gt) && !self.at(TokenKind::Eof) {
            let pstart = self.span();
            let name = self.parse_ident();
            let bound = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
            params.push(Param { name, bound, span: pstart.to(self.prev_span()) });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "to close a parameter list");
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

/// The fixed set of system attributes (`x::event`, `clk::rising`, `d::width`).
/// A `::`-suffix matching one of these reads as a [`Expr::SysAttr`] rather than
/// extending a path. Spec 3.9 / 3.10 / 3.23.
fn is_sysattr(name: &str) -> bool {
    matches!(
        name,
        // Phase 1 digital + range attributes (spec 3.9 / 3.10 / 3.23).
        "event"
            | "old"
            | "rising"
            | "falling"
            | "edge"
            | "width"
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
        let m = parse_ok("module std::logic;\nusing std::logic::{Bit, Logic, Clock};\nusing Word = uint[32];\n");
        assert_eq!(m.path.segments.len(), 2);
        assert_eq!(m.path.segments[1].text, "logic");
        assert_eq!(m.items.len(), 2);
        assert!(matches!(&m.items[0], Item::Using(_)));
    }

    #[test]
    fn entity_with_params_and_ports() {
        let m = parse_ok(
            "module m;\nentity Counter<W: integer> {\n  in clk: Clock;\n  in rst: Logic;\n  in en: Bit;\n  out count: uint[W];\n}\n",
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
            "module m;\nimpl Counter<W: integer> {\n  const MAX: uint[W] = (1 << W) - 1;\n  let value: uint[W] = 0;\n  if clk::rising {\n    if rst == '1' {\n      value = 0;\n    } else {\n      value = value + 1;\n    }\n  }\n  count = value;\n}\n",
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
        assert_eq!(*op, BinOp::Eq);
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
            "module m;\nstruct Stream<T> { clk: Clock, valid: Bit, ready: Bit, data: T }\nimpl out Stream<T>::Source {\n  in clk;\n  out valid;\n  in ready;\n}\nentity Producer {\n  bus: out Stream<uint[32]>::Source;\n}\n",
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
    fn instance_construction_with_generics_and_shorthand() {
        let m = parse_ok(
            "module m;\nimpl Test {\n  let c = Counter<W = 8> {\n    .clk,\n    .rst = rst,\n    .count = count8,\n  };\n}\n",
        );
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Let(l) = &i.items[0] else { panic!("expected let") };
        let Some(Expr::Construct { args, ty, .. }) = &l.value else { panic!("expected construct") };
        assert!(matches!(ty, Some(Type::Generic { .. })));
        assert_eq!(args.len(), 3);
        assert!(args[0].value.is_none()); // `.clk` shorthand
        assert!(args[1].value.is_some());
    }

    #[test]
    fn textual_logical_operators_and_precedence() {
        // `a and b or c` must parse as `(a and b) or c` (and binds tighter).
        let m = parse_ok("module m;\nimpl M {\n  y = a and b or c;\n}\n");
        let Item::Impl(i) = &m.items[0] else { panic!() };
        let ImplItem::Stmt(Stmt::Assign { value, .. }) = &i.items[0] else { panic!() };
        let Expr::Binary { op, lhs, .. } = value else { panic!("expected binary") };
        assert_eq!(*op, BinOp::Or); // top-level is `or`
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
            "module m;\n#[test]\nentity CounterTest {\n}\nimpl CounterTest {\n  let clk: Logic = '0';\n  let dut = Counter<W = 8> {\n    .clk,\n    .count,\n  };\n  wait 10.ns;\n  rst = '0';\n  for i in 0..10 {\n    tick(clk);\n  }\n  assert!(count == 10, \"counter should increment 10 times\");\n}\n",
        );
        let Item::Impl(i) = &m.items[1] else { panic!("expected impl") };
        // clk, dut, wait, rst=, for, assert.
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

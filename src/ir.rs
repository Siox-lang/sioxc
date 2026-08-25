//! Digital simulation IR for siox Phase 1 (spec Stage 6).
//!
//! Lowers the typed, elaborated design into a simulator-friendly form where
//! event dependencies and combinational dependencies are explicit, and
//! sequential next-state updates are separated from immediate local
//! assignments. `::event` and `::old` become explicit IR operations.
//!
//! Spec IR distinction:
//! ```text
//! Driver(signal, expression, condition)              // combinational
//! OnEvent(event_condition): next(signal) = expression // sequential
//! ```
//! and `Rising(clk)` lowers to
//! `Event(clk) && Old(clk) == '0' && Current(clk) == '1'`.
//!
//! The IR data types are deliberately **language-neutral** — they use their own
//! `BinOp`/`UnOp` and never reference the siox AST — so that other HDL frontends
//! could target the same IR. Only `lower` (the siox frontend lowering) consumes
//! the siox AST.
//!
//! Phase-1 scope: lowers the behaviour of each non-extern entity in the design,
//! with the entity's declared (possibly parametric) widths. Per-instance width
//! specialization and cross-instance flattening/connection are follow-ups.

use std::collections::{HashMap, HashSet};

use crate::diag::DiagnosticSink;
use crate::elab::Hierarchy;
use crate::resolve::{DefId, Resolved};
use crate::syntax::ast::{self, BinOp as AstBinOp, UnOp as AstUnOp};
use crate::syntax::Module;

type OperatorImpls<'a> = HashMap<(String, String), Vec<(&'a ast::FnDecl, Option<String>)>>;
type NumericRangeInfo = (u32, bool, Option<(i64, i64)>);

/// Functions available to lowering and constant evaluation.
///
/// Module-level and foreign functions use the resolver's stable declaration
/// identity, so equal leaf names in different modules cannot overwrite one
/// another. Static associated functions do not yet receive their own `DefId`;
/// they remain in a deliberately separate `Type::name` registry.
pub struct FunctionIndex<'a> {
    resolved: &'a Resolved,
    free: HashMap<DefId, &'a ast::FnDecl>,
    associated: HashMap<String, &'a ast::FnDecl>,
}

impl<'a> FunctionIndex<'a> {
    pub fn new(resolved: &'a Resolved) -> Self {
        Self {
            resolved,
            free: HashMap::new(),
            associated: HashMap::new(),
        }
    }

    /// Register a module-level or foreign function declaration.
    pub fn insert_free(&mut self, function: &'a ast::FnDecl) {
        if let Some(id) = self.resolved.declared(function.name.span) {
            self.free.insert(id, function);
        }
    }

    /// Register a static associated function, replacing an inherited default.
    pub fn insert_associated(&mut self, key: String, function: &'a ast::FnDecl) {
        self.associated.insert(key, function);
    }

    /// Register an inherited static default unless the impl overrides it.
    pub fn insert_associated_default(&mut self, key: String, function: &'a ast::FnDecl) {
        self.associated.entry(key).or_insert(function);
    }

    /// Resolve a call expression to the declaration selected by name
    /// resolution, falling back to the separate associated-function registry.
    pub fn get(&self, callee: &ast::Expr) -> Option<&'a ast::FnDecl> {
        if let ast::Expr::Path(path) = callee {
            if let Some(id) = self.resolved.resolved(path.span) {
                if self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Fn) {
                    return self.free.get(&id).copied();
                }
            }
            if let Some(key) = self.associated_path_key(path) {
                return self.associated.get(&key).copied();
            }
        }
        None
    }

    /// Stable table key for a module constant declaration. Implementation
    /// constants have no module-level declaration identity and deliberately
    /// retain their local leaf spelling.
    pub fn constant_decl_key(&self, constant: &ast::ConstDecl) -> String {
        self.resolved
            .declared(constant.name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Const))
            .and_then(|id| self.resolved.qualified_name(id))
            .unwrap_or_else(|| constant.name.text.clone())
    }

    /// Resolver-selected key for a constant path. A bare implementation
    /// constant or function parameter is local and falls back to its leaf;
    /// multi-segment non-constant paths (notably enum variants) return `None`.
    pub fn constant_path_key(&self, path: &ast::Path) -> Option<String> {
        if let Some(id) = self.resolved.resolved(path.span) {
            if self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Const) {
                return self.resolved.qualified_name(id);
            }
        }
        match path.segments.as_slice() {
            [name] => Some(name.text.clone()),
            _ => None,
        }
    }

    /// Constant key for scalar/struct paths (`N`, `a::N`, `K.field`). Indexing
    /// is handled by the separate constant-array table, whose base uses this
    /// same helper.
    pub fn constant_expr_key(&self, expression: &ast::Expr) -> Option<String> {
        match expression {
            ast::Expr::Path(path) => self.constant_path_key(path),
            ast::Expr::Field { base, field, .. } => {
                Some(format!("{}.{}", self.constant_expr_key(base)?, field.text))
            }
            _ => None,
        }
    }

    /// Stable table key for a module type-alias declaration.
    pub fn type_alias_decl_key(&self, name: &ast::Ident) -> String {
        self.resolved
            .declared(name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::TypeAlias))
            .and_then(|id| self.resolved.qualified_name(id))
            .unwrap_or_else(|| name.text.clone())
    }

    /// Stable table key for an enum declaration.
    pub fn enum_decl_key(&self, name: &ast::Ident) -> String {
        self.resolved
            .declared(name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Enum))
            .and_then(|id| self.enum_id_key(id))
            .unwrap_or_else(|| name.text.clone())
    }

    /// Resolver-selected identity of a path that names an enum.
    pub fn enum_path_key(&self, path: &ast::Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Enum))
            .then(|| self.enum_id_key(id))
            .flatten()
    }

    /// Enum named directly by a type/call path, or by the owner portion of
    /// `Enum::new`.
    pub fn enum_owner_key(&self, path: &ast::Path) -> Option<String> {
        self.enum_path_key(path).or_else(|| {
            let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
            let id = self.resolved.resolved(owner.span)?;
            (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Enum))
                .then(|| self.enum_id_key(id))
                .flatten()
        })
    }

    /// Stable table key for a struct declaration.
    pub fn struct_decl_key(&self, name: &ast::Ident) -> String {
        self.resolved
            .declared(name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Struct))
            .and_then(|id| self.struct_id_key(id))
            .unwrap_or_else(|| name.text.clone())
    }

    /// Resolver-selected identity of a path that names a struct.
    pub fn struct_path_key(&self, path: &ast::Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Struct))
            .then(|| self.struct_id_key(id))
            .flatten()
    }

    /// Nominal type named directly by a constructor path, or by the owner of
    /// `T::new`. Every declared owner uses its identity-preserving key.
    pub fn type_owner_key(&self, path: &ast::Path) -> Option<String> {
        let type_key = |id: DefId| match self.resolved.kind_of(id)? {
            crate::resolve::DefKind::Enum => self.enum_id_key(id),
            crate::resolve::DefKind::TypeAlias => self.resolved.qualified_name(id),
            crate::resolve::DefKind::Struct => self.struct_id_key(id),
            _ => None,
        };
        self.resolved
            .resolved(path.span)
            .and_then(type_key)
            .or_else(|| {
                (path.segments.last()?.text == "new").then_some(())?;
                let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
                self.resolved.resolved(owner.span).and_then(type_key)
            })
    }

    /// Stable lookup key for `Type::function`, based on the resolved owner
    /// rather than the spelling at the call site.  Thus an imported
    /// `Pair::tag` and its fully qualified `a::record::Pair::tag` spelling
    /// select the same implementation without conflating another module's
    /// `Pair`.
    fn associated_path_key(&self, path: &ast::Path) -> Option<String> {
        let function = path.segments.last()?;
        let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
        let id = self.resolved.resolved(owner.span)?;
        let owner = match self.resolved.kind_of(id)? {
            crate::resolve::DefKind::Enum => self.enum_id_key(id),
            crate::resolve::DefKind::TypeAlias => self.resolved.qualified_name(id),
            crate::resolve::DefKind::Struct => self.struct_id_key(id),
            _ => None,
        }?;
        Some(format!("{owner}::{}", function.text))
    }

    /// Enum identity and variant leaf selected by a variant expression path.
    pub fn enum_variant_key(&self, path: &ast::Path) -> Option<(String, String)> {
        let variant = self.resolved.resolved(path.span)?;
        let definition = self.resolved.def(variant)?;
        if definition.kind != crate::resolve::DefKind::EnumVariant {
            return None;
        }
        let written_owner = path
            .segments
            .get(path.segments.len().checked_sub(2)?)
            .and_then(|name| self.resolved.resolved(name.span))
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Enum));
        let owner = self.enum_id_key(written_owner.or(definition.parent)?)?;
        Some((owner, definition.name.clone()))
    }

    /// Preserve the ordinary leaf in the common case and qualify colliding
    /// enum leaves. Output metadata is user-facing, while the qualified form
    /// remains injective when separate modules both declare (for example)
    /// `State`. A canonical standard-library enum retains its historical leaf
    /// key so compiler-known `Bool`, `Logic`, and `Ordering` tables do not move
    /// merely because user code declares a namesake; that user declaration is
    /// the one that receives a qualified key.
    fn enum_id_key(&self, id: DefId) -> Option<String> {
        let definition = self.resolved.def(id)?;
        let colliders: Vec<_> = self
            .resolved
            .defs()
            .iter()
            .filter(|other| {
                other.kind == crate::resolve::DefKind::Enum
                    && other.name == definition.name
                    && other.module != definition.module
            })
            .collect();
        let is_std = definition
            .module
            .as_deref()
            .is_some_and(|module| module == "std" || module.starts_with("std::"));
        let other_std = colliders.iter().any(|other| {
            other
                .module
                .as_deref()
                .is_some_and(|module| module == "std" || module.starts_with("std::"))
        });
        if !colliders.is_empty() && (!is_std || other_std) {
            self.resolved.qualified_name(id)
        } else {
            Some(definition.name.clone())
        }
    }

    /// Struct counterpart of [`Self::enum_id_key`]. Standard vector/kernel
    /// newtypes retain their historical short keys; a user namesake receives
    /// a qualified key, while unrelated user collisions qualify both sides.
    fn struct_id_key(&self, id: DefId) -> Option<String> {
        let definition = self.resolved.def(id)?;
        let colliders: Vec<_> = self
            .resolved
            .defs()
            .iter()
            .filter(|other| {
                other.kind == crate::resolve::DefKind::Struct
                    && other.name == definition.name
                    && other.module != definition.module
            })
            .collect();
        let is_std = definition
            .module
            .as_deref()
            .is_some_and(|module| module == "std" || module.starts_with("std::"));
        let other_std = colliders.iter().any(|other| {
            other
                .module
                .as_deref()
                .is_some_and(|module| module == "std" || module.starts_with("std::"))
        });
        if !colliders.is_empty() && (!is_std || other_std) {
            self.resolved.qualified_name(id)
        } else {
            Some(definition.name.clone())
        }
    }

    /// Resolver-selected key for a type-alias use. Non-alias single-segment
    /// paths retain their surface spelling so kernel and local types can use
    /// the same callers without entering the module-alias table.
    pub fn type_alias_path_key(&self, path: &ast::Path) -> Option<String> {
        if let Some(id) = self.resolved.resolved(path.span) {
            if self.resolved.kind_of(id) == Some(crate::resolve::DefKind::TypeAlias) {
                return self.resolved.qualified_name(id);
            }
        }
        match path.segments.as_slice() {
            [name] => Some(name.text.clone()),
            _ => None,
        }
    }

    /// Identity-preserving head name for a declared type. Alias heads are
    /// qualified; other types retain their ordinary leaf spelling.
    pub fn type_head_key(&self, ty: &ast::Type) -> Option<String> {
        match ty {
            ast::Type::Path(path) => self
                .enum_path_key(path)
                .or_else(|| self.struct_path_key(path))
                .or_else(|| self.type_alias_path_key(path))
                .or_else(|| path.segments.last().map(|name| name.text.clone())),
            ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
                self.type_head_key(base)
            }
            ast::Type::View { view, .. } => view.segments.last().map(|name| name.text.clone()),
        }
    }
}

/// A design ready to simulate: signals, combinational drivers, and event blocks.
/// `(operator, left type, right type, span)` for an unmatched operator.
type BadOperator = (String, String, Option<String>, crate::diag::Span);

#[derive(Default)]
pub struct Design {
    pub signals: Vec<Signal>,
    pub drivers: Vec<Driver>,
    pub event_blocks: Vec<EventBlock>,
    /// Enum name -> (discriminant -> variant symbol), over every module
    /// (including `std`). Consumers render a `Signal::enum_type` value as its
    /// symbol (`'X'`, `Idle`) instead of a bare number.
    pub enum_syms: HashMap<String, HashMap<u64, String>>,
    /// Enum name -> the enum it is a newtype over (`Logic` -> `ULogic`). A
    /// conversion `T(x)` between enums is only representation-identity when a
    /// chain connects them; without this the testbench emitter had the target
    /// enum but no way to ask whether the source was related to it, so it
    /// passed *every* `EnumName(x)` straight through.
    pub enum_bases: HashMap<String, String>,
    /// Vector family -> its element enum (`unsigned` -> `Logic`). A *bit* of a
    /// packed vector is not a signal of its own, so an operator on one has no
    /// type to dispatch by unless the family says what its elements are.
    pub vector_element_of_family: HashMap<String, String>,
    /// Type name -> its `impl New for T` uninitialized default value (`Logic` ->
    /// `'U'`), so testbench-local seeding matches the hardware signal default.
    pub new_defaults: HashMap<String, u64>,
    /// Directory that relative `read<T>`/`exists` paths resolve
    /// against — the design's source directory. Empty means the current working
    /// directory (the default; a bare `Design` reads CWD-relative).
    pub base_dir: std::path::PathBuf,
    /// A `Logic`-vector signal id -> its metavalue-companion signal id. The
    /// companion carries which elements are metavalues (`'X'`/`'Z'`/…), the
    /// storage half of X/Z vector propagation. Absent for metavalue-free
    /// vectors, so a design that never touches metavalues is unchanged. See
    /// "X/Z propagation through vectors" in `docs/simulation.md`.
    pub meta_of: HashMap<u32, u32>,
    /// Packed-vector signal -> enum used by each element. This is declaration
    /// metadata (`struct F(E[]); impl Vector for F {}`), not a std type-name
    /// convention. Consumers use it to render metavalue companions.
    pub vector_element_enums: HashMap<u32, String>,
    /// Concrete source value -> its complete recursive type layout. Keys use
    /// the same hierarchical spelling as `Signal::path`; aggregates have an
    /// entry even though storage is flattened into leaf signals. Backends and
    /// tooling consume this instead of reconstructing struct inheritance,
    /// generic substitutions, array ranges, or packed-vector shape from the
    /// frontend AST.
    pub source_layouts: HashMap<String, SourceLayout>,
}

/// An inclusive source range in written order. `left > right` is descending;
/// layout never sorts the endpoints because direction is observable through
/// the language's range attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutRange {
    pub left: i64,
    pub right: i64,
}

impl LayoutRange {
    /// Number of positions in the inclusive range, checked without converting
    /// either endpoint to an unsigned host integer first.
    pub fn len(self) -> Option<u64> {
        u64::try_from((i128::from(self.left) - i128::from(self.right)).unsigned_abs())
            .ok()?
            .checked_add(1)
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn ascending(self) -> bool {
        self.left <= self.right
    }
}

/// The representation semantics of one scalar storage leaf. Nominal identity
/// remains on `LayoutKind::Scalar`; this enum describes how engines interpret
/// its bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScalarDomain {
    Bits,
    Integer,
    Real,
    Character,
    Enum(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutDirection {
    In,
    Out,
    InOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutField {
    pub name: String,
    /// Direction supplied by an applied view. Ordinary struct fields have no
    /// direction; permissions belong to the connection using the layout.
    pub direction: Option<LayoutDirection>,
    pub layout: SourceLayout,
}

/// A frontend-independent, recursively complete layout for one concrete source
/// value. `span` anchors diagnostics; `kind` retains the distinction between a
/// packed vector (one signal) and an ordinary repeated array (one layout per
/// element), which a bit count alone cannot recover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLayout {
    pub span: crate::diag::Span,
    pub kind: LayoutKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutKind {
    Scalar {
        width: u32,
        domain: ScalarDomain,
        nominal: Option<String>,
        /// Dynamic value constraint for ranged numerics, not an index range.
        value_range: Option<(i64, i64)>,
    },
    /// A source array represented by one packed signal.
    Packed {
        width: u32,
        family: String,
        range: Option<LayoutRange>,
        element_enum: Option<String>,
    },
    /// A source array represented recursively (and currently flattened into
    /// one signal per scalar leaf).
    Array {
        range: Option<LayoutRange>,
        element: Box<SourceLayout>,
    },
    Struct {
        name: String,
        /// Applied directional view, when this value was declared through one.
        view: Option<String>,
        fields: Vec<LayoutField>,
    },
    /// A best-effort placeholder for an unresolved/parametric source type.
    /// Keeping it in the tree is more useful to diagnostics and tools than
    /// silently dropping that branch.
    Opaque { name: String, width: Option<u32> },
}

impl SourceLayout {
    /// Total logical bits in this value, with recursive checked arithmetic.
    /// Unknown widths and an overflow return `None` rather than inventing a
    /// truncated aggregate size.
    pub fn bit_width(&self) -> Option<u64> {
        match &self.kind {
            LayoutKind::Scalar { width, .. } | LayoutKind::Packed { width, .. } => {
                (*width != 0).then_some(u64::from(*width))
            }
            LayoutKind::Array { range, element } => {
                (*range)?.len()?.checked_mul(element.bit_width()?)
            }
            LayoutKind::Struct { fields, .. } => fields.iter().try_fold(0u64, |total, field| {
                total.checked_add(field.layout.bit_width()?)
            }),
            LayoutKind::Opaque { width, .. } => width.map(u64::from),
        }
    }

    /// Number of scalar storage leaves after recursive aggregate flattening.
    pub fn leaf_count(&self) -> Option<u64> {
        match &self.kind {
            LayoutKind::Scalar { .. } | LayoutKind::Packed { .. } => Some(1),
            LayoutKind::Array { range, element } => {
                (*range)?.len()?.checked_mul(element.leaf_count()?)
            }
            LayoutKind::Struct { fields, .. } => fields.iter().try_fold(0u64, |total, field| {
                total.checked_add(field.layout.leaf_count()?)
            }),
            LayoutKind::Opaque { .. } => None,
        }
    }

    pub fn index_range(&self) -> Option<LayoutRange> {
        match &self.kind {
            LayoutKind::Packed { range, .. } | LayoutKind::Array { range, .. } => *range,
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalId(pub u32);

#[derive(Clone, Debug)]
pub struct Signal {
    /// Hierarchical path, e.g. `Counter.count`.
    pub path: String,
    /// Source declaration that created this scalar storage leaf. Flattened
    /// fields/elements and metavalue companions retain their owning port or
    /// `let` declaration so diagnostics emitted after lowering still have an
    /// authoritative source anchor.
    pub declaration_span: crate::diag::Span,
    /// Bit width; `0` means "not yet known" (a parametric width).
    pub width: u32,
    /// A `real`-typed value: the 64-bit slot holds f64 bits, and arithmetic
    /// uses the float operators.
    pub real: bool,
    /// A kernel `integer` value uses signed ABI-word comparisons/division and
    /// formatting in native testbench expressions.
    pub integer: bool,
    /// A `Char`-typed value: the slot holds a symbol (stored as its Unicode
    /// code point — an implementation detail); character literals compared or
    /// assigned to it read through the Unicode table.
    pub char: bool,
    /// A ranged numeric's value domain (`integer<left..right>`, spec 3.26): the
    /// simulation checks every settled value against it — a dynamic range
    /// assert. Plain `unsigned[N]`/`signed[N]` wrap instead (documented semantics).
    pub range: Option<(i64, i64)>,
    /// The declared initial value's bit pattern (`let v: T = 1;`), stored as
    /// low-word-first 64-bit chunks. Engines reset signals to it (VHDL-style
    /// initial values), not to zero. The vector grows with the signal width;
    /// there is no initializer-width ceiling.
    pub init: Vec<u64>,
    /// The enum type name, when this signal holds an enum value (`Logic`,
    /// `Bit`, a user FSM `State`). Lets consumers render the stored
    /// discriminant as its variant symbol (`'X'`, `Idle`) instead of a number.
    pub enum_type: Option<String>,
}

/// A combinational driver: `signal = expr` under `cond` (spec 3.14 source-order
/// override is resolved during lowering into a priority chain).
#[derive(Clone, Debug)]
pub struct Driver {
    pub target: SignalId,
    pub cond: Option<Expr>,
    pub expr: Expr,
    /// Explicit discriminant-plane expression retained while lowering a write
    /// whose value expression alone cannot describe its metavalues (notably a
    /// bit-string literal and a dynamic packed-element write). The metavalue
    /// propagation pass consumes this and emits the ordinary companion driver;
    /// finalized IR always has `None` here.
    pub meta: Option<Expr>,
    /// Driver context (spec 3.14): one per impl block / per port connection.
    /// Within a context later drivers override; a signal driven from several
    /// contexts folds via its type's `Resolve` impl (or errors without one).
    pub ctx: u32,
    /// The assignment this driver came from, when one statement produced it.
    /// `None` for drivers the lowering synthesized rather than read (a port
    /// connection, a metavalue companion), which have no line to point at.
    /// A dynamic range failure anchors its report here in preference to the
    /// signal's declaration.
    pub span: Option<crate::diag::Span>,
}

/// An event-controlled block: on `condition`, queue `next(target) = expr`
/// (spec 3.13 next-state semantics).
#[derive(Clone, Debug)]
pub struct EventBlock {
    pub condition: Expr,
    pub updates: Vec<NextUpdate>,
    /// The driver context that lowered this block — one per impl block, the
    /// same identity `Driver::ctx` carries (spec 3.14: override within a
    /// context, resolution across). Several blocks share it when one impl
    /// writes from more than one event, or when a generate loop unrolls a
    /// single clocked statement, and a partial write in a later block merges
    /// over what the earlier ones in that context left behind.
    pub ctx: u32,
}

#[derive(Clone, Debug)]
pub struct NextUpdate {
    pub target: SignalId,
    pub cond: Option<Expr>,
    pub expr: Expr,
    /// Clocked counterpart of [`Driver::meta`], consumed by metavalue
    /// propagation before the IR reaches a simulator backend.
    pub meta: Option<Expr>,
    /// The assignment this update came from — see [`Driver::span`].
    pub span: Option<crate::diag::Span>,
}

/// The std type a bare, otherwise-untyped logic literal defaults to. Its
/// variants (`'0','1','Z','X','U','W','L','H','-'`) and their positions come
/// from `std/logic.siox` — the compiler names the type but holds no value
/// table of its own. A typed context (an enum signal/local or a comparison
/// counterpart) overrides this via `enum_variants`.
pub const DEFAULT_LOGIC_TYPE: &str = "ULogic";

/// IR expression. `::event`/`::old` are first-class so the scheduler can read
/// them directly; `clk.rising()` lowers into `Event`/`Old`/`Current`.
#[derive(Clone, Debug)]
pub enum Expr {
    Const(u64),
    /// An integer constant wider than one ABI word, low-word first.
    WideConst(Vec<u64>),
    /// A vector comparison awaiting its metavalue rule.
    ///
    /// `numeric_std` answers false when either operand holds an unknown, and
    /// true for `/=`. That cannot be decided while lowering: a computed
    /// operand's companion does not exist until `propagate_metavalues` has run,
    /// so a guard emitted here would silently skip exactly the operands worth
    /// guarding. Nor can it be recovered afterwards by matching the finished
    /// shape -- `<=`, `>=`, `==` and `/=` reach their answer through `<=>` and
    /// an `Ordering`, leaving nothing that looks like a vector comparison.
    ///
    /// So the comparison is marked where it is built and resolved once the
    /// companions are known. `validate` rejects any that survive, since the
    /// backends have no meaning for one.
    MetaCmp {
        /// `/=` inverts the rule: unknown operands are definitely not equal.
        ne: bool,
        /// The compared values, whose companions decide the answer.
        operands: Vec<Expr>,
        inner: Box<Expr>,
    },
    /// A `real` constant; evaluates to its f64 bit pattern.
    Real(f64),
    Logic(char),
    Current(SignalId),
    Old(SignalId),
    Event(SignalId),
    Unary {
        op: UnOp,
        rhs: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Bit slice `base[hi..lo]` (inclusive), value `(base >> lo) & mask(hi-lo+1)`.
    Slice {
        base: Box<Expr>,
        hi: u32,
        lo: u32,
    },
    /// `cond ? then : els` — produced by inlining operator-trait impl bodies
    /// (`if`/`else` chains of `return`s become nested selects).
    Select {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// A foreign C call (`extern "C"` declarations, spec 3.27): `real`
    /// parameters/results are f64 (bit-pattern operands), everything else a
    /// 64-bit word. Native linking resolves the named symbol.
    CCall {
        name: String,
        args: Vec<Expr>,
        f64_args: Vec<bool>,
        integer_args: Vec<bool>,
        f64_ret: bool,
        integer_ret: bool,
    },
    /// A reference that could not be lowered (unknown signal, unsupported form).
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    /// `integer(x)` on a `real`: the f64 *value* truncated toward zero, not
    /// its bit pattern. Every other conversion is a raw resize, and a real
    /// reaching that path reinterpreted its bits — `integer(3.5)` gave the low
    /// word of `0x400C000000000000`, i.e. 0.
    RealToInt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    /// Signed kernel-`integer` arithmetic. Add/subtract/multiply use the same
    /// bit operation as their unsigned counterparts, but retain signedness so
    /// a wider enclosing expression sign-extends their operands/results.
    SAdd,
    SSub,
    SMul,
    SDiv,
    And,
    Or,
    /// Bitwise exclusive-or. Native like `And`/`Or` so the metavalue companion
    /// can apply the `std_logic_1164` table per element: std spells `xor` as
    /// `(a or b) - (a and b)`, which is right for two-valued arithmetic but
    /// makes the companion lowering see a subtraction and poison the whole
    /// vector. `nand`/`nor`/`xnor`/`not` all reduce to this.
    Xor,
    Shl,
    Shr,
    /// Arithmetic right shift for the signed kernel `integer`.
    AShr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Signed kernel-`integer` ordering comparisons.
    SLt,
    SLe,
    SGt,
    SGe,
    /// Float arithmetic on f64-bit values (`real` operands).
    FAdd,
    FSub,
    FMul,
    FDiv,
    /// Float comparison on f64-bit values (`real` operands); the result is a
    /// `Bool` (0/1), computed with ordered IEEE-754 semantics — integer compare
    /// on the raw bits would misorder negatives and `±0.0`.
    FEq,
    FNe,
    FLt,
    FLe,
    FGt,
    FGe,
}

/// Lower the elaborated design into simulation IR. Relative file-read paths
/// resolve against the current working directory (see [`lower_in`] to set a
/// source-relative base directory).
pub fn lower(
    modules: &[Module],
    resolved: &Resolved,
    hier: &Hierarchy,
    sink: &mut DiagnosticSink,
) -> Design {
    lower_in(modules, resolved, hier, sink, std::path::Path::new(""))
}

/// Lower with `base_dir` as the root that relative `read<T>`
/// paths resolve against (the design's source directory), so a program that
/// bakes in a data file works regardless of the working directory.
pub fn lower_in(
    modules: &[Module],
    resolved: &Resolved,
    hier: &Hierarchy,
    sink: &mut DiagnosticSink,
    base_dir: &std::path::Path,
) -> Design {
    let mut l = Lowering::new(sink, resolved);
    l.expr_types = hier.expr_types.clone();
    l.base_dir = base_dir.to_path_buf();
    l.out.base_dir = base_dir.to_path_buf();
    // Enum discriminants first: `collect` folds constants, and a constant may
    // *be* a variant (`const M: Mode = Mode::Fast;`). Populating the map
    // afterwards left the fold with nothing to look the variant up in, so the
    // constant never entered the tables and every read of it reported the
    // name as unknown.
    l.enum_variants = enum_discriminants(modules, &l.free_fns);
    l.enum_first_disc = enum_first_discriminants(modules, &l.free_fns);
    l.collect(modules);
    l.new_defaults = l.enum_first_disc.clone();
    l.new_defaults.extend(l.compute_new_defaults());
    l.out.new_defaults = l.new_defaults.clone();
    // Reverse each enum's variant map (name -> disc) into disc -> symbol, so
    // consumers can render stored discriminants symbolically.
    l.out.enum_syms = l
        .enum_variants
        .iter()
        .map(|(ty, vars)| {
            (
                ty.clone(),
                vars.iter().map(|(sym, &d)| (d, sym.clone())).collect(),
            )
        })
        .collect();
    l.enum_reprs = enum_reprs(modules, &l.free_fns);
    l.vector_families = vector_families(modules, &l.free_fns);
    // Record what each family's elements are, so a consumer that sees only
    // the finished `Design` (the testbench emitter) can type a bit of a
    // packed vector the same way lowering does.
    for family in &l.vector_families {
        if let Some(element) = l.vector_element_enum(family) {
            l.out
                .vector_element_of_family
                .insert(family.clone(), element);
        }
    }
    {
        let enums = enum_index(modules, &l.free_fns);
        for (name, e) in &enums {
            if let Some(b) = enum_base_name(e, &enums, &l.free_fns) {
                l.enum_bases.insert(name.clone(), b);
            }
        }
        // Hand the chain to the finished `Design`, so the testbench emitter
        // can ask whether two enums are related rather than assuming it.
        l.out.enum_bases = l.enum_bases.clone();
    }

    // The entity types that appear in the elaborated hierarchy, in first-seen
    // order, deduplicated. Each entity's parameters are taken from its first
    // instance, so `unsigned[W]` lowers with the instance's concrete `W`.
    let mut seen = Vec::new();
    for inst in &hier.instances {
        if !seen.contains(&inst.entity_id) {
            seen.push(inst.entity_id);
            l.entity_params.entry(inst.entity_id).or_insert_with(|| {
                inst.params
                    .iter()
                    .filter_map(|(n, v)| match v {
                        crate::elab::ParamValue::Int(i) => Some((n.clone(), *i)),
                        crate::elab::ParamValue::Unknown => None,
                    })
                    .collect()
            });
        }
    }
    // Hierarchy owns the authoritative result of generate elaboration. Keep
    // its per-parent instance-array facts keyed by the same dotted paths IR
    // uses while recursively lowering bodies.
    for &root in &hier.roots {
        let path = hier.root_path(root);
        l.collect_instance_array_facts(hier, root, &path);
    }
    // Lower only the top-level designs — the `#[top]`/`#[test]` roots. Their
    // sub-instances (and a testbench's DUTs) are lowered recursively from there,
    // each per-instance, so no entity is lowered standalone by type.
    let mut roots = Vec::new();
    for &r in &hier.roots {
        let ent = hier.instance(r).entity_id;
        if !roots.iter().any(|(id, _)| *id == ent) {
            roots.push((ent, hier.root_path(r)));
        }
    }
    for (entity, path) in &roots {
        l.lower_entity(*entity, path);
    }
    l.report_depth_exceeded();
    l.report_bad_operators();
    l.report_bad_conversions();
    l.report_unresolved_names();
    l.report_unelaborated_instance_uses();
    l.report_unsupported_exprs();
    l.lint_possible_latches();
    l.resolve_driver_contexts();
    l.propagate_metavalues();
    l.reconstruct_reads();
    l.lint_combinational_loops();
    l.lint_undriven_outputs();
    l.lint_unused_signals();
    // Resolve any logic literal that no typed context claimed to its position
    // in std's default logic type, so the IR the backends consume carries only
    // `Const`s — no raw chars, no compiler-side value table.
    l.normalize_logic_literals();
    l.out
}

struct Lowering<'a> {
    sink: &'a mut DiagnosticSink,
    resolved: &'a Resolved,
    expr_types: HashMap<crate::diag::Span, crate::types::Ty>,
    /// Root for relative compile-time file reads (the source directory).
    base_dir: std::path::PathBuf,
    /// Signals given a default by a match wildcard arm — excluded from the
    /// possible-latch lint even though their lowered drivers are conditional.
    lint_defaulted: std::collections::HashSet<u32>,
    entities: HashMap<DefId, &'a ast::EntityDecl>,
    impls: HashMap<DefId, Vec<&'a ast::ImplDecl>>,
    /// Trait name -> its declaration, for the defaulted methods an
    /// implementing type inherits (spec 3.20: a trait body is a contract, and
    /// a method *with* a body is a default the impl may omit — which the type
    /// checker already allows, so dispatch has to find it).
    trait_decls: HashMap<String, &'a ast::TraitDecl>,
    /// Type head -> the traits it implements, for that fallback.
    implemented_traits: HashMap<String, Vec<String>>,
    /// Entity name -> its instance's concrete parameter values.
    entity_params: HashMap<DefId, HashMap<String, i64>>,
    /// Enum name -> variant name -> discriminant value.
    enum_variants: HashMap<String, HashMap<String, u64>>,
    /// Enum name -> discriminant of its *first* (declaration-order) variant,
    /// the derived `new()` default (VHDL `T'LEFT`): an uninitialized enum signal
    /// powers on holding this value, so it is always a valid member of the type.
    enum_first_disc: HashMap<String, u64>,
    /// Type name -> its `impl New for T` default value (a constant `new()`
    /// body), the uninitialized value a signal of that type powers on to. Beats
    /// the structural first-variant default. (`New for Logic` -> `'U'`.)
    new_defaults: HashMap<String, u64>,
    /// Struct name -> its declaration (for flattening struct signals).
    structs: HashMap<String, &'a ast::StructDecl>,
    /// Named, storage-free directional views.
    views: HashMap<String, &'a ast::ViewDecl>,
    /// View name -> per-leaf directions.
    view_dirs: HashMap<String, HashMap<String, ast::Direction>>,
    /// Enum name -> its bit width (repr, or bits for the variant count).
    enum_reprs: HashMap<String, u32>,
    /// Enum name -> base enum name (derivation chain, enums only).
    enum_bases: HashMap<String, String>,
    /// (trait name, target type) -> the impl's fns with the impl's declared
    /// rhs type (the `integer` in `impl Add<integer> for T`; `None` reads as
    /// `Self`). Overloads select by that rhs, or the fn's rhs parameter type.
    op_impls: OperatorImpls<'a>,
    /// Generic implementations whose target is an unconstrained scalar array.
    /// Nominal Vector families forward these when their element satisfies the
    /// implementation's constraint.
    blanket_array_impls: HashMap<String, String>,
    /// Literal suffix -> (target type, fn), for suffix inlining.
    suffix_impls: HashMap<String, (String, &'a ast::FnDecl)>,
    /// Module-level and static associated functions, inlined at call sites /
    /// const-evaluated without collapsing namespaced free functions by leaf.
    free_fns: FunctionIndex<'a>,
    /// Inline depth guard (recursive fns must const-fold; runaway inlining
    /// stops here).
    inline_depth: std::cell::Cell<u32>,
    /// Structs whose fields are currently being expanded, so a cyclic
    /// derivation terminates instead of overflowing the stack.
    expanding_structs: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Functions whose inlining hit the depth guard. Lowering runs behind
    /// `&self`, so the diagnostic is recorded here and flushed by `lower`
    /// instead of silently leaving an `Unknown` in the driver.
    depth_exceeded: std::cell::RefCell<Vec<(String, crate::diag::Span)>>,
    /// Value names that resolved to nothing while lowering. Name
    /// resolution deliberately leaves plain value identifiers to later
    /// stages, and this is the stage that knows every signal, constant
    /// and parameter — so an unmatched name here is a genuine typo. It
    /// used to become a silent `Unknown`, which `check` reported as ok
    /// and a build reported as "driver 0 contains an Unknown".
    unresolved_names: std::cell::RefCell<Vec<(String, crate::diag::Span)>>,
    /// Lexical, storage-free values declared by `let` inside a hardware block.
    /// Each assignment replaces the binding with an expression (a conditional
    /// assignment becomes a `Select`), so event blocks retain next-state
    /// semantics for signals while their local values update immediately.
    block_scopes: std::cell::RefCell<Vec<HashMap<String, BlockLocal>>>,
    /// Field/index expressions with no hardware form. Recorded here with the
    /// source spelling, which the IR no longer has by the time validation sees
    /// an `Unknown`.
    unsupported_exprs: std::cell::RefCell<Vec<(String, crate::diag::Span)>>,
    /// Concrete instance-array shapes from hierarchy elaboration, keyed by
    /// the owning instance's dotted IR path.
    instance_array_facts: HashMap<String, Vec<crate::elab::InstanceArrayFact>>,
    /// Dotted path of the body currently being lowered.
    cur_instance_path: String,
    /// Reads of declared slots omitted by the active generate conditions.
    /// Expression lowering is intentionally `&self`, so these are collected
    /// through interior mutability and emitted after the design is lowered.
    unelaborated_instance_uses: std::cell::RefCell<Vec<UnelaboratedInstanceUse>>,
    /// The receiver signal bound to `self` while inlining a method body, so a
    /// `self'event`/`self'old` sysattr in the body resolves to the receiver's
    /// signal (the `ClockLike` edge methods are defined this way in std).
    self_signal: std::cell::Cell<Option<SignalId>>,
    /// Type-family of each generic-fn parameter during inlining (param name ->
    /// the concrete argument's family), so operator dispatch in the body uses
    /// the caller's type (e.g. signed's signed `Ord`, not the kernel compare).
    param_types: std::cell::RefCell<HashMap<String, String>>,
    /// Parameters of the function currently being inlined whose *declared*
    /// type is the kernel `integer`.
    ///
    /// Signedness of an operation is decided from the recorded type of its
    /// operand expressions, and a free function's body is type-checked with
    /// no parameters in scope, so every use of a parameter records as
    /// `Ty::Error`. `if v < 0` inside `fn f(v: integer)` therefore compiled to
    /// an *unsigned* comparison: `abs(n)` returned `n` for every negative `n`,
    /// and `min`/`max` picked the wrong side — in hardware only, since the
    /// testbench evaluates the body in C where the values are signed.
    param_integers: std::cell::RefCell<HashSet<String>>,
    /// A bound parameter's width, alongside its family in `param_types`. The
    /// family alone let the body dispatch `signed`'s operators while
    /// `self'length` inside them fell back to 1, so the sign-bit test shifted
    /// by 0 and `abs(-5)` returned 251.
    param_widths: std::cell::RefCell<HashMap<String, u32>>,
    /// Module-level integer constants (`const N: integer = 4`).
    consts: HashMap<String, i64>,
    /// Exact literal values for module constants, including values wider than
    /// the signed width/parameter evaluator can represent.
    const_values: HashMap<String, Expr>,
    /// Constant lookup tables (`const TAB: unsigned[8][4] = [..]`), whose
    /// elements are folded individually. `const_values` holds one scalar
    /// per name and has no room for a sequence, so an indexed read of a
    /// const array used to find nothing and lower to `Unknown`.
    const_arrays: HashMap<String, Vec<Expr>>,
    /// Module-level `real` constants (`const PI: real = 3.14159...`).
    consts_real: HashMap<String, f64>,
    /// Module-level range constants (`const BYTE: range = 7..0`), as written
    /// (left, right) so direction is preserved.
    const_ranges: HashMap<String, (i64, i64)>,
    /// Type aliases (`using Word = unsigned[32]`).
    aliases: HashMap<String, ast::Type>,
    /// The active entity's width environment (consts + instance params),
    /// for const-evaluating slice bounds during expression lowering.
    cur_env: HashMap<String, i64>,
    /// The active entity's type-parameter bindings (`T -> unsigned[8]` for a
    /// generic entity `Buf<unsigned[8]>`), substituted into port/signal types.
    cur_type_env: HashMap<String, ast::Type>,
    /// The stack of entity names currently being lowered (`lower_body`), so a
    /// sub-instance that would re-enter an entity already on the stack is
    /// skipped instead of recursing forever. The elaborator has already
    /// emitted the `cyclic instantiation` diagnostic; this just keeps lowering
    /// from overflowing on the same cycle (best-effort, spec cross-cutting).
    lower_stack: Vec<DefId>,
    /// Plain (non-bus-mode, non-`inout`) `out` port signals, for the
    /// undriven-output warning after all drivers are collected.
    plain_out_ports: Vec<SignalId>,
    /// Internal `let` signals with no initializer, in a non-`#[test]` entity —
    /// they must be driven, so an undriven one is a forgotten assignment.
    undriven_lets: Vec<SignalId>,
    /// Internal component locals eligible for W-P003. Test/top locals are
    /// externally observed by the runner and deliberately excluded.
    unused_lets: Vec<SignalId>,
    out: Design,
    /// Signal name -> id, valid while lowering a single entity.
    locals: HashMap<String, SignalId>,
    /// Local name -> its enum type name (operator-impl operands).
    local_enum: HashMap<String, String>,
    /// Local name -> its struct type name (multi-signal operands/targets).
    /// Applied views store the view name here so method dispatch uses the
    /// view's nominal interface rather than the backing struct.
    local_struct: HashMap<String, String>,
    /// Local name -> its backing struct representation. This differs from
    /// `local_struct` for an applied view (`Controller Bus`): methods dispatch
    /// on `Controller`, while reads of the aggregate bind all fields of `Bus`.
    local_struct_repr: HashMap<String, String>,
    /// Locals of the symbol base type `Char`.
    local_char: std::collections::HashSet<String>,
    /// Array-typed locals -> their ordered element indices (whole-array
    /// assignment and string literals expand per element).
    local_array: HashMap<String, Vec<i64>>,
    /// Binary operators where the left operand's type has `Operator` impls but
    /// none accepts the right operand. For an aggregate struct there is no
    /// builtin arithmetic to fall back to, so the expression produced nothing
    /// and the assignment it fed was silently dropped.
    bad_operators: std::cell::RefCell<Vec<BadOperator>>,
    /// Conversions `T(x)` where `T` names a real struct/enum but no `From`
    /// impl and no derivation connects it to the argument's type. Lowering
    /// left an `Unknown`, which surfaced at the very end as "contains an
    /// Unknown (unlowered) expression" with no code and no span.
    bad_conversions: std::cell::RefCell<Vec<(String, Option<String>, crate::diag::Span)>>,
    /// Locals whose element type is an entity (`let stage: Inc[3]`), so an
    /// out-of-range element can be named an instance the way the `types` check
    /// names it rather than being called an array element.
    instance_arrays: HashSet<String>,
    /// Out-of-range constant indices already reported, keyed by
    /// (array, index, span start). One source index is visited once per
    /// generate iteration, so without this a loop reports the same element
    /// as many times as the loop is long.
    reported_oob: HashSet<(String, i64, u32)>,
    /// Generated dead assignments already reported, keyed by the two source
    /// sites and the concrete target after loop/parameter substitution. The
    /// same entity body may be lowered for several instances; diagnostics are
    /// source facts and must not repeat per instance.
    reported_generated_dead_assignments: HashSet<(crate::diag::Span, crate::diag::Span, String)>,
    /// The assignment statement being lowered, so the drivers and next-state
    /// updates it produces can carry it without every construction site
    /// having to be handed one. `None` outside statement lowering — the
    /// drivers synthesized there answer to no source line.
    cur_span: Option<crate::diag::Span>,
    /// The active driver context (bumped per impl block / connection).
    cur_ctx: u32,
    /// Driver context -> the source site that created it (a port connection's
    /// value expression). Lets the conflicting-driver error point at each
    /// contributing connection instead of naming only the signal.
    ctx_span: HashMap<u32, crate::diag::Span>,
    /// Signal -> declared type name (enum / unsigned / signed), for Resolve lookup.
    sig_type: HashMap<u32, String>,
    /// Array-derived Logic vector families (`struct F : Logic[]`) -> signed?.
    /// unsigned/signed are just the first two; the family set is read from the
    /// declarations, not hardcoded.
    vector_families: std::collections::HashSet<String>,
    /// Numeric-vector locals -> the family name, for operator-impl dispatch
    /// (kernel `integer`/`real` keep builtin operators; unsigned/signed live in std).
    local_numeric: HashMap<String, String>,
}

/// A lowered value: a scalar expression, or one expression per struct field
/// (a struct-typed value has no single-signal representation).
#[derive(Clone, Debug)]
enum Val {
    Scalar(Expr),
    Fields(Vec<(String, Expr)>),
}

/// One suffix in a flattened aggregate access. Keeping the source expression
/// for an index lets a runtime access expand into a mux or gated writes over
/// the concrete leaf signals.
#[derive(Clone, Copy)]
enum AccessStep<'e> {
    Field(&'e str),
    Index(&'e ast::Expr),
}

enum DynamicWriteTarget {
    Whole {
        signal: SignalId,
        hit: Expr,
    },
    PackedBit {
        signal: SignalId,
        position: u32,
        hit: Expr,
    },
}

/// One block-local binding. The declared type is retained because substituting
/// the value expression alone would lose its operator family and width.
#[derive(Clone, Debug)]
struct BlockLocal {
    value: Val,
    ty: ast::Type,
}

#[derive(Clone, Debug)]
struct UnelaboratedInstanceUse {
    slot: String,
    parent_path: String,
    use_span: crate::diag::Span,
    declaration_span: crate::diag::Span,
}

impl UnelaboratedInstanceUse {
    fn slot_root(&self) -> &str {
        self.slot
            .split_once('[')
            .map_or(self.slot.as_str(), |(root, _)| root)
    }
}

/// `cond ? then : els` over values; struct values select per field.
fn select_val(cond: Expr, then: Val, els: Val) -> Val {
    match (then, els) {
        (Val::Scalar(t), Val::Scalar(e)) => Val::Scalar(Expr::Select {
            cond: Box::new(cond),
            then: Box::new(t),
            els: Box::new(e),
        }),
        (Val::Fields(ts), Val::Fields(es)) => Val::Fields(
            ts.into_iter()
                .map(|(name, t)| {
                    let e = es
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, e)| e.clone())
                        .unwrap_or(Expr::Unknown);
                    (
                        name,
                        Expr::Select {
                            cond: Box::new(cond.clone()),
                            then: Box::new(t),
                            els: Box::new(e),
                        },
                    )
                })
                .collect(),
        ),
        _ => Val::Scalar(Expr::Unknown),
    }
}

impl<'a> Lowering<'a> {
    fn nominal_id(&self, name: &str) -> Option<DefId> {
        let definitions = self.resolved.defs();
        let qualified = definitions.iter().enumerate().find(|(index, _)| {
            self.resolved
                .qualified_name(DefId(*index as u32))
                .as_deref()
                == Some(name)
        });
        // Compiler-known standard types deliberately keep a short IR key
        // when a user declares a namesake. Prefer that canonical declaration
        // for a short lookup; qualified user keys have already matched above.
        let standard = (!name.contains("::")).then(|| {
            definitions.iter().enumerate().find(|(_, definition)| {
                definition.name == name
                    && definition
                        .module
                        .as_deref()
                        .is_some_and(|module| module == "std" || module.starts_with("std::"))
            })
        });
        qualified
            .or_else(|| standard.flatten())
            .or_else(|| {
                definitions
                    .iter()
                    .enumerate()
                    .find(|(_, definition)| definition.name == name)
            })
            .map(|(index, _)| DefId(index as u32))
    }

    fn collect_instance_array_facts(
        &mut self,
        hierarchy: &Hierarchy,
        id: crate::elab::InstanceId,
        path: &str,
    ) {
        let instance = hierarchy.instance(id);
        if !instance.instance_arrays.is_empty() {
            self.instance_array_facts
                .insert(path.to_string(), instance.instance_arrays.clone());
        }
        for &child in &instance.children {
            let child_instance = hierarchy.instance(child);
            self.collect_instance_array_facts(
                hierarchy,
                child,
                &format!("{path}.{}", child_instance.name),
            );
        }
    }

    fn new(sink: &'a mut DiagnosticSink, resolved: &'a Resolved) -> Self {
        Lowering {
            sink,
            resolved,
            expr_types: HashMap::new(),
            base_dir: std::path::PathBuf::new(),
            lint_defaulted: std::collections::HashSet::new(),
            entities: HashMap::new(),
            impls: HashMap::new(),
            trait_decls: HashMap::new(),
            implemented_traits: HashMap::new(),
            entity_params: HashMap::new(),
            enum_variants: HashMap::new(),
            enum_first_disc: HashMap::new(),
            new_defaults: HashMap::new(),
            structs: HashMap::new(),
            views: HashMap::new(),
            view_dirs: HashMap::new(),
            enum_reprs: HashMap::new(),
            enum_bases: HashMap::new(),
            op_impls: HashMap::new(),
            blanket_array_impls: HashMap::new(),
            suffix_impls: HashMap::new(),
            free_fns: FunctionIndex::new(resolved),
            inline_depth: std::cell::Cell::new(0),
            expanding_structs: std::cell::RefCell::new(std::collections::HashSet::new()),
            depth_exceeded: std::cell::RefCell::new(Vec::new()),
            unresolved_names: std::cell::RefCell::new(Vec::new()),
            block_scopes: std::cell::RefCell::new(Vec::new()),
            unsupported_exprs: std::cell::RefCell::new(Vec::new()),
            instance_array_facts: HashMap::new(),
            cur_instance_path: String::new(),
            unelaborated_instance_uses: std::cell::RefCell::new(Vec::new()),
            self_signal: std::cell::Cell::new(None),
            param_types: std::cell::RefCell::new(HashMap::new()),
            param_integers: std::cell::RefCell::new(HashSet::new()),
            param_widths: std::cell::RefCell::new(HashMap::new()),
            consts: HashMap::new(),
            const_values: HashMap::new(),
            const_arrays: HashMap::new(),
            consts_real: HashMap::new(),
            const_ranges: HashMap::new(),
            aliases: HashMap::new(),
            cur_env: HashMap::new(),
            cur_type_env: HashMap::new(),
            lower_stack: Vec::new(),
            plain_out_ports: Vec::new(),
            undriven_lets: Vec::new(),
            unused_lets: Vec::new(),
            out: Design::default(),
            locals: HashMap::new(),
            local_enum: HashMap::new(),
            local_struct: HashMap::new(),
            local_struct_repr: HashMap::new(),
            local_char: std::collections::HashSet::new(),
            local_array: HashMap::new(),
            bad_operators: std::cell::RefCell::new(Vec::new()),
            bad_conversions: std::cell::RefCell::new(Vec::new()),
            instance_arrays: HashSet::new(),
            cur_span: None,
            reported_oob: HashSet::new(),
            reported_generated_dead_assignments: HashSet::new(),
            local_numeric: HashMap::new(),
            vector_families: std::collections::HashSet::new(),
            cur_ctx: 0,
            ctx_span: HashMap::new(),
            sig_type: HashMap::new(),
        }
    }

    fn collect(&mut self, modules: &'a [Module]) {
        let mut constant_decls = Vec::new();
        for m in modules {
            for item in &m.items {
                match item {
                    ast::Item::Entity(e) => {
                        if let Some(id) = self.resolved.declared(e.name.span) {
                            self.entities.insert(id, e);
                        }
                    }
                    ast::Item::Fn(f) => {
                        self.free_fns.insert_free(f);
                    }
                    ast::Item::ExternBlock { fns, .. } => {
                        for f in fns {
                            self.free_fns.insert_free(f);
                        }
                    }
                    ast::Item::Struct(s) => {
                        self.structs
                            .insert(self.free_fns.struct_decl_key(&s.name), s);
                    }
                    ast::Item::View(v) => {
                        let key = declared_view_key(v, &self.free_fns);
                        self.views.insert(key.clone(), v);
                        self.view_dirs.insert(
                            key,
                            v.fields
                                .iter()
                                .map(|f| (f.name.text.clone(), f.dir))
                                .collect(),
                        );
                    }
                    // Module constants join the width environment; range
                    // constants (`const BYTE: range = 7..0`) keep their
                    // written direction. Aliases substitute during lowering.
                    ast::Item::Const(c) => {
                        constant_decls.push((self.free_fns.constant_decl_key(c), c));
                    }
                    ast::Item::Using(u) => {
                        if let ast::UsingKind::Alias { name, ty } = &u.kind {
                            self.aliases
                                .insert(self.free_fns.type_alias_decl_key(name), ty.clone());
                        }
                    }
                    ast::Item::Trait(t) => {
                        self.trait_decls.insert(t.name.text.clone(), t);
                    }
                    ast::Item::Impl(im) if im.trait_.is_none() => {
                        self.register_static_fns(im);
                        if let Some(id) = type_def_id(&im.target, self.resolved) {
                            self.impls.entry(id).or_default().push(im);
                        }
                    }
                    // A trait impl's first fn is the operator body for
                    // `impl "+" for T` (spec 3.25); an `impl Suffix<"ns", _>
                    // for T` defines the literal suffix named by its symbol
                    // argument, its `suffix` method inlined at the use site
                    // (spec 3.24).
                    ast::Item::Impl(im) => {
                        let tr = im.trait_.as_ref().and_then(|t| t.segments.last());
                        let target = self.free_fns.type_head_key(&im.target);
                        if let (Some(tr), Some(ty)) = (tr, target.as_ref()) {
                            self.implemented_traits
                                .entry(ty.clone())
                                .or_default()
                                .push(tr.text.clone());
                        }
                        self.register_static_fns(im);
                        if let (Some(tr), Some(ty)) = (tr, target.as_ref()) {
                            if tr.text == "Suffix" {
                                let symbol = im.trait_args.first().and_then(|a| match a {
                                    ast::GenericArg::Positional(ast::Expr::StrLit {
                                        text, ..
                                    }) => Some(text.clone()),
                                    _ => None,
                                });
                                if let Some(symbol) = symbol {
                                    for it in &im.items {
                                        if let ast::ImplItem::Fn(f) = it {
                                            self.suffix_impls
                                                .insert(symbol.clone(), (ty.clone(), f));
                                        }
                                    }
                                }
                            } else {
                                // `impl Operator<"+", integer, _> for T`: the
                                // symbol keys the impl and the next trait
                                // argument names the rhs operand type. A
                                // non-operator trait (Resolve/New/From) keys by
                                // its own name and reads its first type arg.
                                let op_symbol = (tr.text == "Operator")
                                    .then(|| im.trait_args.first())
                                    .flatten()
                                    .and_then(|a| match a {
                                        ast::GenericArg::Positional(ast::Expr::StrLit {
                                            text,
                                            ..
                                        }) => Some(text.clone()),
                                        _ => None,
                                    });
                                let input_index = usize::from(op_symbol.is_some());
                                let rhs_arg =
                                    im.trait_args.get(input_index).and_then(|a| match a {
                                        ast::GenericArg::Positional(ast::Expr::Path(p)) => {
                                            p.segments.last().map(|s| s.text.clone())
                                        }
                                        _ => None,
                                    });
                                let operator = op_symbol.unwrap_or_else(|| tr.text.clone());
                                if is_blanket_array_impl(im) {
                                    let requirement =
                                        blanket_requirement(im).unwrap_or_else(|| operator.clone());
                                    self.blanket_array_impls.insert(operator, requirement);
                                    continue;
                                }
                                for it in &im.items {
                                    if let ast::ImplItem::Fn(f) = it {
                                        self.op_impls
                                            .entry((operator.clone(), ty.clone()))
                                            .or_default()
                                            .push((f, rhs_arg.clone()));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // A trait's `self`-less defaults are associated functions the
        // implementing type inherits, callable as `Thing::tag()`. Registering
        // them needs every trait declaration, so it waits until collection is
        // done — a trait may be written after the impl that implements it.
        let inherited: Vec<(String, &'a ast::FnDecl)> = self
            .implemented_traits
            .iter()
            .flat_map(|(ty, traits)| traits.iter().map(move |tr| (ty.clone(), tr)))
            .filter_map(|(ty, tr)| self.trait_decls.get(tr.as_str()).map(|t| (ty, *t)))
            .flat_map(|(ty, t)| {
                t.items
                    .iter()
                    .filter(|f| f.body.is_some() && !f.params.iter().any(|p| p.is_self))
                    .map(move |f| (ty.clone(), f))
            })
            .collect();
        for (ty, f) in inherited {
            // The impl's own statics went in during collection, so this only
            // supplies what it omitted.
            self.free_fns
                .insert_associated_default(format!("{ty}::{}", f.name.text), f);
        }
        // Constants are order-independent. Keep the narrow signed value table
        // for widths/generate conditions, and a separate exact literal table
        // for signal values so a 128-bit constant never passes through i64.
        for _ in 0..=constant_decls.len() {
            let mut progressed = false;
            for (key, constant) in &constant_decls {
                let scope = self.consts.clone();
                progressed |= self.fold_const(key, constant, &scope);
            }
            if !progressed {
                break;
            }
        }
    }

    /// Register an impl's *static* associated fns (those without a `self`
    /// parameter) under a `Type::name` key, so `Unicode::code(c)` is callable
    /// in expressions through the same const-folding/inlining path as a
    /// module-level `fn`. Methods take `self` and dispatch via the receiver.
    fn register_static_fns(&mut self, im: &'a ast::ImplDecl) {
        let Some(ty) = self.free_fns.type_head_key(&im.target) else {
            return;
        };
        for it in &im.items {
            if let ast::ImplItem::Fn(f) = it {
                if !f.params.iter().any(|p| p.is_self) {
                    self.free_fns
                        .insert_associated(format!("{ty}::{}", f.name.text), f);
                }
            }
        }
    }

    fn lower_entity(&mut self, entity_id: DefId, root_path: &str) {
        let Some(edecl) = self.entities.get(&entity_id).copied() else {
            return;
        };
        // Extern entities are black boxes.
        if edecl.is_extern {
            return;
        }
        let mut env = self.consts.clone();
        env.extend(
            self.entity_params
                .get(&entity_id)
                .cloned()
                .unwrap_or_default(),
        );
        if has_attr(edecl, "test") {
            // A testbench: lower only its DUT instances, each per-instance under
            // the testbench path (`CounterTest.dut.*`), so two instances of one
            // entity are distinct. Stimulus statements are interpreted by the
            // runner, and testbench<->DUT connections go through the runner's
            // signal map — so no top-level connection drivers here. Its local
            // values still need concrete layouts: the native runner consumes
            // the finished IR rather than independently specializing AST
            // declarations.
            self.persist_testbench_layouts(entity_id, root_path, &env);
            self.lower_testbench_duts(entity_id, root_path, &env);
            return;
        }
        // A top-level DUT: signals are entity-qualified (`Counter.count`), and
        // widths come from its first instance's parameters.
        self.lower_body(entity_id, root_path, &env, &HashMap::new(), &HashMap::new());
    }

    /// Persist concrete layouts for testbench-owned values without creating
    /// hardware signals for them. Native execution owns their storage, while
    /// `Design` remains the authoritative source for aggregate shape, ranges,
    /// scalar families, and widths.
    fn persist_testbench_layouts(
        &mut self,
        entity_id: DefId,
        entity: &str,
        env: &HashMap<String, i64>,
    ) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        for im in impls {
            for item in &im.items {
                let ast::ImplItem::Let(declaration) = item else {
                    continue;
                };
                if instance_let_parts(declaration, &self.entities, self.resolved).is_some() {
                    continue;
                }
                let Some(ty) = declaration.ty.as_ref() else {
                    continue;
                };
                let layout = self.source_layout(ty, env);
                self.persist_layout_tree(entity, &declaration.name.text, &layout);
            }
        }
    }

    fn persist_layout_tree(&mut self, entity: &str, name: &str, layout: &SourceLayout) {
        self.out
            .source_layouts
            .insert(format!("{entity}.{name}"), layout.clone());
        match &layout.kind {
            LayoutKind::Struct { fields, .. } => {
                for field in fields {
                    self.persist_layout_tree(
                        entity,
                        &format!("{name}.{}", field.name),
                        &field.layout,
                    );
                }
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                for index in loop_range(range.left, range.right) {
                    self.persist_layout_tree(entity, &format!("{name}[{index}]"), element);
                }
            }
            LayoutKind::Array { range: None, .. }
            | LayoutKind::Packed { .. }
            | LayoutKind::Scalar { .. }
            | LayoutKind::Opaque { .. } => {}
        }
    }

    /// Lower each `let inst: Sub = { .. }` DUT of a testbench into its own
    /// namespace `<testbench>.<inst>.*` (with the DUT's internal logic and
    /// sub-instances). No testbench signals, statements, or top connections.
    fn lower_testbench_duts(&mut self, entity_id: DefId, name: &str, env: &HashMap<String, i64>) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        // Every port a testbench name is connected to, across all DUTs — when
        // one name binds an `out` and `in` ports (a DUT feeding another, or
        // its own input), the out drives the ins as real hardware, so the
        // value propagates on every settle without runner involvement.
        let mut bindings: HashMap<
            String,
            Vec<(SignalId, Option<ast::Direction>, crate::diag::Span)>,
        > = HashMap::new();
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    if let Some((cty, args)) = instance_let_parts(l, &self.entities, self.resolved)
                    {
                        if let Some(sub) = type_def_id(&cty, self.resolved) {
                            let sub_path = format!("{name}.{}", l.name.text);
                            let mut sub_env = self.consts.clone();
                            sub_env.extend(self.construct_params(&cty, sub, env));
                            let sub_tenv = self.construct_type_params(&cty, sub);
                            let sub_ports = self.lower_body(
                                sub,
                                &sub_path,
                                &sub_env,
                                &sub_tenv,
                                &HashMap::new(),
                            );
                            for (port, value) in self.norm_conns(&args, sub) {
                                // The testbench name the port binds to; a
                                // literal/expression connection has no name.
                                let Some(tbname) = expr_path(&value) else {
                                    continue;
                                };
                                if let Some(&(sig, dir)) = sub_ports.get(&port) {
                                    bindings.entry(tbname).or_default().push((
                                        sig,
                                        dir,
                                        ast::expr_span(&value),
                                    ));
                                    continue;
                                }
                                // A struct/bus port is not one signal: it is
                                // flattened into leaves (`bus.valid`, ...), so
                                // the scalar lookup above finds nothing and the
                                // binding used to be dropped — two testbench
                                // DUTs sharing a struct local were left
                                // unconnected, silently. Bind each leaf to the
                                // matching leaf of the testbench name.
                                let prefix = format!("{port}.");
                                for (pname, &(sig, dir)) in sub_ports.iter() {
                                    let Some(leaf) = pname.strip_prefix(&prefix) else {
                                        continue;
                                    };
                                    bindings
                                        .entry(format!("{tbname}.{leaf}"))
                                        .or_default()
                                        .push((sig, dir, ast::expr_span(&value)));
                                }
                            }
                        }
                    }
                }
            }
        }
        for (tbname, ports) in &bindings {
            // A tristate net needs one shared node that folds each driver's
            // *expression*; the entity path builds one, this path has no
            // testbench signal to build it on. Connecting an `inout` here used
            // to bind nothing at all and read back high-Z, as though no one
            // were driving — report it instead of simulating a lie.
            if ports
                .iter()
                .any(|(_, d, _)| *d == Some(ast::Direction::Inout))
            {
                let span = ports
                    .iter()
                    .find(|(_, d, _)| *d == Some(ast::Direction::Inout))
                    .map(|(_, _, span)| *span)
                    .expect("an inout binding has a source span");
                self.sink.emit(
                    crate::diag::Diagnostic::error(format!(
                        "`{tbname}` connects an `inout` port between testbench instances"
                    ))
                    .with_code(crate::diag::codes::INVALID_METHOD_CALL)
                    .at(span)
                    .help(
                        "a shared tristate net is built inside an entity — wire the \
                         instances there and drive that entity from the testbench",
                    ),
                );
                continue;
            }
            let outs: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d, _)| *d == Some(ast::Direction::Out))
                .map(|&(s, _, _)| s)
                .collect();
            let ins: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d, _)| *d == Some(ast::Direction::In))
                .map(|&(s, _, _)| s)
                .collect();
            if outs.is_empty() || ins.is_empty() {
                continue;
            }
            // Each out contributes in its own context; several outs onto one
            // name then fold through the type's Resolve (or error), exactly
            // like parallel drivers anywhere else.
            for &o in &outs {
                let ctx = self.next_ctx();
                for &i in &ins {
                    self.out.drivers.push(Driver {
                        span: self.cur_span,
                        target: i,
                        cond: None,
                        expr: Expr::Current(o),
                        meta: None,
                        ctx,
                    });
                }
            }
        }
    }

    /// Lower entity `ename`'s body, naming signals under `path` (the instance
    /// path — `Counter.count` at the top, `Add2.s1.a` for a sub-instance) in the
    /// width environment `env`. Sub-instances (`let s: Sub = { .p = x, .. }`) are
    /// lowered recursively under `path.s` and their port connections become
    /// drivers. Returns each port's (signal, direction) so a parent can wire to
    /// it. Runs in a fresh name scope, restoring the caller's on return.
    /// Fold one constant declaration into the constant tables, returning
    /// whether it produced a value. `scope` is what integer expressions
    /// evaluate against: module constants see the constant table, an
    /// implementation's own constants also see the entity's parameters.
    ///
    /// Shared so the two callers cannot diverge by *kind* — the first version
    /// of implementation constants folded integers only, so a `const` holding
    /// a lookup table or a real reported its own name as unknown while the
    /// module-level spelling of it worked.
    fn fold_const(
        &mut self,
        name: &str,
        constant: &ast::ConstDecl,
        scope: &HashMap<String, i64>,
    ) -> bool {
        if self.const_ranges.contains_key(name)
            || self.consts.contains_key(name)
            || self.consts_real.contains_key(name)
            || self.const_values.contains_key(name)
            || self.const_arrays.contains_key(name)
        {
            return false;
        }
        if let ast::Expr::Range { lo, hi, .. } = &constant.value {
            if let (Some(left), Some(right)) =
                (self.eval_const(lo, scope), self.eval_const(hi, scope))
            {
                self.const_ranges.insert(name.to_string(), (left, right));
                return true;
            }
        } else if let ast::Expr::Array { elems, .. } = &constant.value {
            // A constant lookup table. Every element has to fold, or the table
            // is left for a later round of the fixed point (an element may
            // name a constant not yet resolved).
            let values: Option<Vec<Expr>> = elems
                .iter()
                .map(|e| lower_const_value(e, &self.const_values, scope, &self.free_fns))
                .collect();
            if let Some(values) = values {
                self.const_arrays.insert(name.to_string(), values);
                return true;
            }
        } else if let Some(args) = self
            .free_fns
            .type_head_key(&constant.ty)
            .and_then(|head| self.positional_struct_args(&head, &constant.value))
        {
            // `const P: Pair = { 6, 7 };` — the same constant written without
            // field names. The declared type says these braces are a struct
            // literal, so they bind by declaration order.
            let Some(fields) = self.const_struct_fields(&constant.ty, &args) else {
                return false;
            };
            for (field, value) in fields {
                let key = format!("{name}.{field}");
                if let Some(narrow) = self.eval_const(&value, scope) {
                    self.consts.insert(key.clone(), narrow);
                }
                let Some(lowered) =
                    lower_const_value(&value, &self.const_values, scope, &self.free_fns)
                else {
                    return false;
                };
                self.const_values.insert(key, lowered);
            }
            return true;
        } else if let ast::Expr::Construct { args, .. } = &constant.value {
            // `const K: Pair = { .a = 4, .b = 5 };` — a struct constant is one
            // folded value per field, keyed by the dotted path a read spells.
            // Nothing folded it before, so the constant never entered any
            // table: `K.a` reported "has no hardware form" (a message about
            // runtime indices, on a source with no index) and `p = K` reported
            // `K` as an unknown name — both after stage 4 had accepted the
            // declaration with no diagnostic at all.
            let Some(fields) = self.const_struct_fields(&constant.ty, args) else {
                return false;
            };
            for (field, value) in fields {
                let key = format!("{name}.{field}");
                if let Some(narrow) = self.eval_const(&value, scope) {
                    self.consts.insert(key.clone(), narrow);
                }
                let Some(lowered) =
                    lower_const_value(&value, &self.const_values, scope, &self.free_fns)
                else {
                    return false;
                };
                self.const_values.insert(key, lowered);
            }
            return true;
        } else if let ast::Expr::Int { text, .. } = &constant.value {
            if text.contains('.') {
                if let Ok(value) = text.replace('_', "").parse::<f64>() {
                    self.consts_real.insert(name.to_string(), value);
                    return true;
                }
            } else if let Some(value) = integer_const(text) {
                if let Expr::Const(word) = value {
                    if let Ok(narrow) = i64::try_from(word) {
                        self.consts.insert(name.to_string(), narrow);
                    }
                    self.const_values
                        .insert(name.to_string(), Expr::Const(word));
                } else {
                    self.const_values.insert(name.to_string(), value);
                }
                return true;
            }
        } else if let ast::Expr::Path(path) = &constant.value {
            // `const M: Mode = Mode::Fast;` — an enum variant is a value like
            // any other. Only a single-segment path was folded, so the
            // constant never entered the tables and every read of it reported
            // the name as unknown; binding it to a signal first happened to
            // work, which is what made it look supported.
            if path.segments.len() >= 2 {
                if let Some(disc) = self.enum_variant_path(path) {
                    self.consts.insert(name.to_string(), disc as i64);
                    self.const_values
                        .insert(name.to_string(), Expr::Const(disc));
                    return true;
                }
            }
            if let Some(source) = self.free_fns.constant_path_key(path) {
                if let Some(value) = self.const_values.get(&source).cloned() {
                    self.const_values.insert(name.to_string(), value);
                    return true;
                } else if let Some(&value) = self.consts.get(&source) {
                    self.consts.insert(name.to_string(), value);
                    return true;
                }
            }
        } else if let Some(value) =
            lower_const_value(&constant.value, &self.const_values, scope, &self.free_fns)
        {
            self.const_values.insert(name.to_string(), value);
            if let Some(narrow) = self.eval_const(&constant.value, scope) {
                self.consts.insert(name.to_string(), narrow);
            }
            return true;
        } else if let Some(value) = self.eval_const(&constant.value, scope) {
            self.consts.insert(name.to_string(), value);
            return true;
        }
        false
    }

    fn eval_const(&self, expression: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
        eval_const_fns(expression, env, &self.free_fns, 0)
    }

    fn lower_body(
        &mut self,
        entity_id: DefId,
        path: &str,
        env: &HashMap<String, i64>,
        type_env: &HashMap<String, ast::Type>,
        aliases: &HashMap<String, SignalId>,
    ) -> HashMap<String, (SignalId, Option<ast::Direction>)> {
        let Some(edecl) = self.entities.get(&entity_id).copied() else {
            return HashMap::new();
        };

        // Save the caller's scope; give this body a fresh one.
        let saved_instance_path = std::mem::replace(&mut self.cur_instance_path, path.to_string());
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_enum = std::mem::take(&mut self.local_enum);
        let saved_struct = std::mem::take(&mut self.local_struct);
        let saved_struct_repr = std::mem::take(&mut self.local_struct_repr);
        let saved_char = std::mem::take(&mut self.local_char);
        let saved_array = std::mem::take(&mut self.local_array);
        let saved_numeric = std::mem::take(&mut self.local_numeric);
        let saved_instance_arrays = std::mem::take(&mut self.instance_arrays);
        // A Rust-style binder may rename: `impl<M: integer> Counter<M>` calls
        // the entity's first parameter `M` inside its own body. Every lookup
        // below — signal widths as much as expressions — goes through `env`,
        // which is keyed by the entity's declared names, so extend it with
        // each impl's names bound by position, as Rust binds them.
        let mut renamed = env.clone();
        let mut renamed_types = type_env.clone();
        {
            let bodies: Vec<&ast::ImplDecl> =
                self.impls.get(&entity_id).cloned().unwrap_or_default();
            for im in bodies {
                let ast::Type::Generic { args, .. } = &im.target else {
                    continue;
                };
                for (i, arg) in args.iter().enumerate() {
                    let ast::GenericArg::Positional(ast::Expr::Path(path)) = arg else {
                        continue;
                    };
                    let ([seg], Some(param)) =
                        (path.segments.as_slice(), edecl.params.params.get(i))
                    else {
                        continue;
                    };
                    if seg.text == param.name.text {
                        continue;
                    }
                    if let Some(&value) = env.get(&param.name.text) {
                        renamed.insert(seg.text.clone(), value);
                    }
                    if let Some(ty) = type_env.get(&param.name.text) {
                        renamed_types.insert(seg.text.clone(), ty.clone());
                    }
                }
            }
        }
        // Constants declared *inside* an implementation (spec 3.3) were never
        // collected — only module-level ones were — so `const MAX: unsigned[W]
        // = (1 << W) - 1;` compiled and then every read reported the name as
        // unknown. They fold here rather than globally because the spec's own
        // example depends on the entity's parameters, so one declaration is a
        // different number per instance. They go into the *env* as well as the
        // constant tables: array sizes and slice bounds resolve through the
        // env, so a constant missing from it left `let regs: unsigned[8][K]`
        // with no elements at all.
        let saved_consts = self.consts.clone();
        let saved_const_values = self.const_values.clone();
        {
            let body_consts: Vec<&ast::ConstDecl> = self
                .impls
                .get(&entity_id)
                .map(|impls| {
                    impls
                        .iter()
                        .flat_map(|im| &im.items)
                        .filter_map(|item| match item {
                            ast::ImplItem::Const(c) => Some(c),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            for _ in 0..=body_consts.len() {
                let mut progressed = false;
                for c in &body_consts {
                    let mut scope = self.consts.clone();
                    scope.extend(renamed.iter().map(|(k, v)| (k.clone(), *v)));
                    if self.fold_const(&c.name.text, c, &scope) {
                        progressed = true;
                        // Array sizes and slice bounds resolve through the
                        // env, so an integer constant has to reach it too.
                        if let Some(&value) = self.consts.get(&c.name.text) {
                            renamed.insert(c.name.text.clone(), value);
                        }
                    }
                }
                if !progressed {
                    break;
                }
            }
        }
        let env = &renamed;
        let type_env = &renamed_types;
        let saved_env = std::mem::replace(&mut self.cur_env, env.clone());
        let saved_type_env = std::mem::replace(&mut self.cur_type_env, type_env.clone());
        self.lower_stack.push(entity_id);
        // Ports (struct/array-typed ones flatten to leaves), then the port map.
        // An `inout` port aliased to a parent net reuses that net's signal
        // instead of allocating its own: the body's `pin = expr` then drives the
        // shared net (resolving across instances) and reads of `pin` read the
        // resolved value — Verilog's bidirectional-port model.
        for p in &edecl.ports {
            self.add_typed_signal(path, &p.name.text, &p.ty, env, p.span);
        }
        // An aliased `inout` port repoints its (leaf) name at the shared parent
        // net (keeping the type metadata just registered), so the body drives and
        // reads that net directly. The port's own allocated signal is left
        // unused. A scalar port aliases one name (`s`); a struct/array `inout`
        // port aliases each flattened leaf (`s.valid`, `s.data`).
        for (name, &net) in aliases {
            if self.locals.contains_key(name) {
                self.locals.insert(name.clone(), net);
            }
        }
        // The port map. A scalar port is one entry (`s`); a struct/array port
        // flattens to one entry per leaf (`s.valid`, `s.data`, `bus[0]`), each
        // tagged with the port's direction, so a parent can wire every leaf.
        // (Only port signals exist in `locals` at this point — `let` state
        // signals are added below — so the prefix scan can't catch a non-port.)
        let mut ports: HashMap<String, (SignalId, Option<ast::Direction>)> = HashMap::new();
        let mut new_out_ports: Vec<SignalId> = Vec::new();
        for p in &edecl.ports {
            let dot = format!("{}.", p.name.text);
            let idx = format!("{}[", p.name.text);
            // An applied-view port (`bus: Source Stream`) gives each leaf its
            // direction from the view (`out valid; in ready;`); a plain port
            // applies its single direction to every leaf.
            let view = self.view_of(&p.ty).and_then(|k| self.view_dirs.get(&k));
            for (k, &id) in &self.locals {
                if *k == p.name.text || k.starts_with(&dot) || k.starts_with(&idx) {
                    let dir = match view {
                        Some(m) => k
                            .strip_prefix(&dot)
                            .and_then(|field| m.get(field).copied())
                            .or(p.dir),
                        None => p.dir,
                    };
                    // A plain (non-bus-mode) `out` port must be driven inside the
                    // entity; record it for the undriven check. Bus-mode leaves
                    // and `inout` are excluded (their drive model differs).
                    if !edecl.is_extern && view.is_none() && dir == Some(ast::Direction::Out) {
                        new_out_ports.push(id);
                    }
                    ports.insert(k.clone(), (id, dir));
                }
            }
        }
        self.plain_out_ports.extend(new_out_ports);

        // `let` items: instance bindings are collected for recursion; the rest
        // become state signals.
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        let mut subinsts: Vec<(String, ast::Type, Vec<ast::ConnectArg>)> = Vec::new();
        // Generate loops (`for i in 0..n { let s: Sub = { .. } }`) unroll here,
        // substituting the loop index into each instance's type args and
        // connections so the flattened element signals (`wires[i]`) resolve.
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Stmt(s) = item {
                    gather_generate(
                        s,
                        env,
                        &[],
                        &self.entities,
                        self.resolved,
                        &self.free_fns,
                        &mut subinsts,
                    );
                }
            }
        }
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    // `let s: Sub = { .. }` / `let s: Sub [= { .. }]`: a
                    // sub-instance, not a signal. A `let s: T` whose `T` is
                    // *this* entity's type parameter (bound to a concrete type,
                    // e.g. `unsigned[8]`) is a signal even when some entity is also
                    // named `T` — let it fall through to the signal path, where
                    // `add_typed_signal` substitutes `T` via `cur_type_env`.
                    if let Some((cty, args)) = instance_let_parts(l, &self.entities, self.resolved)
                    {
                        let is_type_param =
                            type_head_name(&cty).is_some_and(|h| self.cur_type_env.contains_key(h));
                        if !is_type_param {
                            subinsts.push((l.name.text.clone(), cty, args));
                            continue;
                        }
                    }
                    // `let s: string = "hello";`: the literal sets the range.
                    let unconstrained = match &l.ty {
                        None => true,
                        Some(t) => matches!(
                            self.resolve_alias(t),
                            ast::Type::Indexed { index: None, .. }
                        ),
                    };
                    if unconstrained {
                        if let Some(ast::Expr::StrLit { text, .. }) = &l.value {
                            self.add_char_array(path, &l.name.text, text.chars().count(), l.span);
                            continue;
                        }
                        // `let s: string = read<string>("f.txt");` — the
                        // compiler reads UTF-8; its code-point length sets the
                        // otherwise unconstrained range.
                        if let Some((requested, fpath)) =
                            l.value.as_ref().and_then(Self::fs_read_call)
                        {
                            if self.type_resolves_to(requested, "string") {
                                match std::fs::read_to_string(self.base_dir.join(fpath)) {
                                    Ok(text) => {
                                        let chars: Vec<char> = text.chars().collect();
                                        self.add_char_array(
                                            path,
                                            &l.name.text,
                                            chars.len(),
                                            l.span,
                                        );
                                        for (i, c) in chars.iter().enumerate() {
                                            if let Some(&id) =
                                                self.locals.get(&format!("{}[{i}]", l.name.text))
                                            {
                                                self.out.signals[id.0 as usize].init =
                                                    vec![*c as u32 as u64];
                                            }
                                        }
                                    }
                                    Err(e) => self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<string>(\"{fpath}\"): {e}"
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    ),
                                }
                                continue;
                            }
                        }
                    }
                    if let Some(ty) = &l.ty {
                        self.add_typed_signal(path, &l.name.text, ty, env, l.span);
                    } else {
                        self.add_signal(path, &l.name.text, 0, l.span);
                    }
                    // A string initializer on a flattened array
                    // (`let arr: Color[3] = "rgb"`) seeds each element: a
                    // char-enum variant, or a `Char` code point.
                    if let Some(ast::Expr::StrLit { text, .. }) = &l.value {
                        if let Some(indices) = self.local_array.get(&l.name.text).cloned() {
                            for (c, i) in text.chars().zip(&indices) {
                                if let Some(&id) = self.locals.get(&format!("{}[{i}]", l.name.text))
                                {
                                    let en = self.out.signals[id.0 as usize].enum_type.clone();
                                    let v = en
                                        .and_then(|e| self.char_disc(c, &e))
                                        .unwrap_or(c as u32 as u64);
                                    self.out.signals[id.0 as usize].init = vec![v];
                                }
                            }
                        }
                    }
                    // A struct-literal initializer (`let p: P = { .a = 1 }`)
                    // seeds each field signal. The testbench interpreter has
                    // always honoured this; hardware lowering did not, so an
                    // entity-level struct local silently powered on at 0.
                    if let Some(ast::Expr::Construct { args, spread, .. }) = &l.value {
                        let head = l.ty.as_ref().and_then(type_head_name).map(str::to_string);
                        self.seed_struct_literal(
                            &l.name.text,
                            head.as_deref(),
                            args,
                            spread.as_deref(),
                            l.span,
                        );
                    } else if self.local_struct.contains_key(&l.name.text) {
                        // A struct local initialized by anything else. The
                        // scalar fold below never sees these: it is reached
                        // through `locals[name]`, and a struct has signals only
                        // under `name.field`. So `let p: Pair = make(6)` seeded
                        // nothing and every field powered on at zero, silently.
                        if let Some(value) = &l.value {
                            let head = l.ty.as_ref().and_then(type_head_name).map(str::to_string);
                            match self.struct_literal_from_call(value) {
                                Some((fields, spread)) => self.seed_struct_literal(
                                    &l.name.text,
                                    head.as_deref(),
                                    &fields,
                                    spread.as_ref(),
                                    l.span,
                                ),
                                // A default construction (`Pair::new()`,
                                // `Pair()`) names no declared function, and its
                                // structural zeros are the right answer — the
                                // one shape here that must stay silent.
                                None if Self::is_default_construction(value)
                                    && !self.resolves_to_declared_fn(value) => {}
                                // Everything else has no power-on value this
                                // can fold: a body that is not one returned
                                // literal, or a read of another signal. Say so
                                // rather than powering on at zero — the help
                                // names the spelling that works. This matches
                                // the scalar rule exactly, where even a copy
                                // from a constant-initialized local
                                // (`let b: unsigned[8] = a`) is E-P021: an
                                // initializer folds constants, and reading a
                                // signal is not folding.
                                // A whole struct constant (`let p: Pair = K`)
                                // *is* a constant, and the scalar spelling of
                                // it folds, so this seeds rather than reports.
                                None if self.seed_from_struct_const(&l.name.text, value) => {}
                                // A positional literal (`let p: Pair = { 6, 7 }`)
                                // is the named form without the field names.
                                None if head
                                    .as_deref()
                                    .and_then(|h| self.positional_struct_args(h, value))
                                    .is_some() =>
                                {
                                    let args = head
                                        .as_deref()
                                        .and_then(|h| self.positional_struct_args(h, value))
                                        .unwrap_or_default();
                                    self.seed_struct_literal(
                                        &l.name.text,
                                        head.as_deref(),
                                        &args,
                                        None,
                                        l.span,
                                    );
                                }
                                None => {
                                    self.report_non_constant_init(&l.name.text, l.span);
                                }
                            }
                        }
                    }
                    // An array-literal initializer (`let rom: unsigned[8][4] =
                    // [1, 2, 3, 4]`) seeds each element, as the string and
                    // struct-literal forms above do. Without it a lookup table
                    // written this way powered on at 0 in every element and
                    // read back as zeros with no diagnostic.
                    // This used to walk the elements itself, with `enumerate`
                    // for the index and no case for an element that is a
                    // struct. `seed_elements` is the same walk done once: it
                    // takes the indices from the declared range (so a
                    // non-zero-based array seeds the right elements) and seeds
                    // an aggregate element through the struct path.
                    if let Some(ast::Expr::Array { elems, .. }) = &l.value {
                        let name = l.name.text.clone();
                        self.seed_elements(&name, elems.iter().collect(), l.span);
                    }
                    // A value-less internal `let` in a component entity must be
                    // driven; record its leaves for the undriven check. Root
                    // entities are excluded: a `#[test]` testbench's locals are
                    // driven by the runner, and a `#[top]` harness's wires are
                    // stimulus fed externally — neither is a forgotten drive.
                    // An instance array (`let stage: Inc[N]`, Inc an entity) is
                    // built element-wise, not driven — never a signal to check.
                    let is_instance_array = l.ty.as_ref().is_some_and(|ty| {
                        type_def_id(ty, self.resolved)
                            .is_some_and(|id| self.entities.contains_key(&id))
                            && !type_head_name(ty)
                                .is_some_and(|head| self.cur_type_env.contains_key(head))
                    });
                    if is_instance_array {
                        self.instance_arrays.insert(l.name.text.clone());
                    }
                    if l.value.is_none()
                        && !is_instance_array
                        && !has_attr(edecl, "test")
                        && !has_attr(edecl, "top")
                    {
                        let dot = format!("{}.", l.name.text);
                        let idx = format!("{}[", l.name.text);
                        let leaves: Vec<SignalId> = self
                            .locals
                            .iter()
                            .filter(|(k, _)| {
                                **k == l.name.text || k.starts_with(&dot) || k.starts_with(&idx)
                            })
                            .map(|(_, &id)| id)
                            .collect();
                        self.undriven_lets.extend(leaves);
                    }
                    if !is_instance_array && !has_attr(edecl, "test") && !has_attr(edecl, "top") {
                        let dot = format!("{}.", l.name.text);
                        let idx = format!("{}[", l.name.text);
                        self.unused_lets.extend(
                            self.locals
                                .iter()
                                .filter(|(k, _)| {
                                    **k == l.name.text || k.starts_with(&dot) || k.starts_with(&idx)
                                })
                                .map(|(_, &id)| id),
                        );
                    }
                    // A typed file constructor is owned by elaboration here:
                    // text decodes UTF-8 into Char leaves, while binary packs
                    // little-endian integers and then stores them through the
                    // requested destination representation.
                    if let Some((requested, fpath)) = l.value.as_ref().and_then(Self::fs_read_call)
                    {
                        if self.type_resolves_to(requested, "string") {
                            if let Some(indices) = self.local_array.get(&l.name.text).cloned() {
                                match std::fs::read_to_string(self.base_dir.join(fpath)) {
                                    Ok(text) => {
                                        let chars = text.chars().collect::<Vec<_>>();
                                        if chars.len() > indices.len() {
                                            self.sink.emit(
                                                crate::diag::Diagnostic::error(format!(
                                                    "read<string>(\"{fpath}\"): {} characters do not fit `{}` ({} elements)",
                                                    chars.len(),
                                                    l.name.text,
                                                    indices.len()
                                                ))
                                                .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                                .at(l.span),
                                            );
                                        }
                                        for (position, index) in indices.iter().enumerate() {
                                            if let Some(&id) = self
                                                .locals
                                                .get(&format!("{}[{index}]", l.name.text))
                                            {
                                                self.out.signals[id.0 as usize].init = vec![chars
                                                    .get(position)
                                                    .copied()
                                                    .map(|character| character as u32 as u64)
                                                    .unwrap_or(0)];
                                            }
                                        }
                                    }
                                    Err(error) => self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<string>(\"{fpath}\"): {error}"
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    ),
                                }
                            }
                            continue;
                        }

                        let targets = self
                            .local_array
                            .get(&l.name.text)
                            .map(|indices| {
                                indices
                                    .iter()
                                    .filter_map(|index| {
                                        self.locals
                                            .get(&format!("{}[{index}]", l.name.text))
                                            .copied()
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .or_else(|| self.locals.get(&l.name.text).copied().map(|id| vec![id]))
                            .unwrap_or_default();
                        match std::fs::read(self.base_dir.join(fpath)) {
                            Ok(bytes) if !targets.is_empty() => {
                                let element_width =
                                    self.out.signals[targets[0].0 as usize].width.max(1);
                                let element_bytes = element_width.div_ceil(8) as usize;
                                let capacity = element_bytes.saturating_mul(targets.len());
                                if bytes.len() > capacity {
                                    self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<{}>(\"{fpath}\"): {} bytes do not fit `{}` ({} elements x {element_bytes} bytes)",
                                            crate::syntax::pretty::type_str(requested),
                                            bytes.len(),
                                            l.name.text,
                                            targets.len()
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    );
                                }
                                for (position, id) in targets.into_iter().enumerate() {
                                    self.out.signals[id.0 as usize].init = file_integer_words(
                                        &bytes,
                                        position.saturating_mul(element_bytes),
                                        element_bytes,
                                        element_width,
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(error) => self.sink.emit(
                                crate::diag::Diagnostic::error(format!(
                                    "read<{}>(\"{fpath}\"): {error}",
                                    crate::syntax::pretty::type_str(requested)
                                ))
                                .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                .at(l.span),
                            ),
                        }
                        continue;
                    }
                    // A constant initializer is the signal's reset value.
                    if let (Some(v), Some(&id)) = (&l.value, self.locals.get(&l.name.text)) {
                        let en = self.out.signals[id.0 as usize].enum_type.clone();
                        let is_char = self.out.signals[id.0 as usize].char;
                        if let Some(bits) = self.const_init_value(v, en.as_deref(), is_char) {
                            let w = self.out.signals[id.0 as usize].width;
                            let masked = if w > 0 && w < 64 {
                                bits & ((1u64 << w) - 1)
                            } else {
                                bits
                            };
                            self.out.signals[id.0 as usize].init = vec![masked];
                        } else {
                            self.report_non_constant_init(&l.name.text, l.span);
                        }
                        // A metavalue-carrying string init (`"01X0"`) needs a
                        // companion signal to record which elements are `'X'`/… —
                        // the storage half of X/Z vector propagation (stage 1c).
                        if let Some((base, digits)) = Self::bit_string_parts(v) {
                            let (value_words, discs) = self.decode_bit_string_words(base, digits);
                            if value_words.len() > 1 {
                                self.out.signals[id.0 as usize].init = value_words;
                            }
                            if Self::has_metavalue(&discs) {
                                self.ensure_meta_companion(id, discs);
                            }
                        }
                    }
                }
            }
        }

        // Sub-instances: lower each under `path.inst`, then wire its ports. An
        // `in` port is driven from the parent's signal; an `out` port drives the
        // parent's. The recursion saves/restores this body's scope, so the
        // parent's names resolve again here.
        for (inst, cty, conns) in &subinsts {
            let Some(sub_id) = type_def_id(cty, self.resolved) else {
                continue;
            };
            // Cyclic instantiation (already diagnosed by the elaborator): don't
            // recurse back into an entity that is still being lowered.
            if self.lower_stack.contains(&sub_id) {
                continue;
            }
            let sub_path = format!("{path}.{inst}");
            let mut sub_env = self.consts.clone();
            sub_env.extend(self.construct_params(cty, sub_id, env));
            let sub_type_env = self.construct_type_params(cty, sub_id);

            // Resolve `inout` connections to the parent net they share *before*
            // lowering the child, so its port aliases to that net. A scalar
            // inout whose parent side isn't a plain signal is left un-aliased
            // (falls back to the in/out wiring below).
            // Normalized `(port, value)` connections (positional bound to port
            // order), used both for inout aliasing and the wiring below.
            let norm = self.norm_conns(conns, sub_id);
            let mut aliases: HashMap<String, SignalId> = HashMap::new();
            if let Some(decl) = self.entities.get(&sub_id).copied() {
                for p in &decl.ports {
                    if p.dir != Some(ast::Direction::Inout) {
                        continue;
                    }
                    let value = norm
                        .iter()
                        .find(|(port, _)| *port == p.name.text)
                        .map(|(_, v)| v.clone());
                    let Some(value) = value else { continue };
                    // Scalar inout: the whole port shares the parent net.
                    if let Some(net) = self.target_signal(&value) {
                        aliases.insert(p.name.text.clone(), net);
                    }
                    // Struct/array inout: alias each leaf of the connected net
                    // (`link.valid`, `bus[0]`) onto the matching port leaf
                    // (`s.valid`, `pin[0]`), so every leaf resolves across the
                    // instances through the shared net.
                    if let Some(net_path) = expr_path(&value) {
                        let dot = format!("{net_path}.");
                        let idx = format!("{net_path}[");
                        for (k, &id) in &self.locals {
                            if let Some(rest) = k.strip_prefix(&dot) {
                                aliases.insert(format!("{}.{}", p.name.text, rest), id);
                            } else if let Some(rest) = k.strip_prefix(&idx) {
                                aliases.insert(format!("{}[{}", p.name.text, rest), id);
                            }
                        }
                    }
                }
            }

            let sub_ports = self.lower_body(sub_id, &sub_path, &sub_env, &sub_type_env, &aliases);
            // Expose the sub-instance's ports in this scope so `inst.port`
            // (and `stage[i].port`) reads resolve to the child's signal —
            // an output need not be wired to a local to be read.
            for (port, &(sig, _)) in &sub_ports {
                self.locals.entry(format!("{inst}.{port}")).or_insert(sig);
            }
            for (field, value) in &norm {
                let field = field.as_str();
                // The child port's leaves: the port itself (`s`) plus any
                // flattened struct/array members (`s.valid`, `bus[0]`).
                let dot = format!("{field}.");
                let idx = format!("{field}[");
                let mut leaves: Vec<(String, SignalId, Option<ast::Direction>)> = sub_ports
                    .iter()
                    .filter(|(k, _)| **k == *field || k.starts_with(&dot) || k.starts_with(&idx))
                    .map(|(k, &(id, d))| (k.clone(), id, d))
                    .collect();
                if leaves.is_empty() {
                    continue;
                }

                // A scalar port (one leaf named exactly `field`): the connection
                // value may be any expression (`.en = ea`, `.val = 5`).
                if leaves.len() == 1 && leaves[0].0 == *field {
                    let (_, child_id, dir) = leaves[0];
                    // An aliased inout is already wired to the shared net.
                    if dir == Some(ast::Direction::Inout) && aliases.contains_key(field) {
                        continue;
                    }
                    if dir == Some(ast::Direction::Out) {
                        if let Some(target) = self.target_signal(value) {
                            let ctx = self.next_ctx_at(ast::expr_span(value));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target,
                                cond: None,
                                expr: Expr::Current(child_id),
                                meta: None,
                                ctx,
                            });
                        }
                    } else {
                        let expr = self.lower_expr(value);
                        let ctx = self.next_ctx_at(ast::expr_span(value));
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: child_id,
                            cond: None,
                            expr,
                            meta: None,
                            ctx,
                        });
                    }
                    continue;
                }

                // A composite (struct/array) port connected to a *literal*
                // has no parent signal to wire leaf-by-leaf, so drive each
                // leaf from the matching field. Without this the whole
                // connection was dropped in silence and the port kept its
                // default — a scalar port has always accepted a value here.
                if expr_path(value).is_none() {
                    if let ast::Expr::Construct { args, .. } = value {
                        let mut fields: HashMap<String, &ast::Expr> = HashMap::new();
                        literal_leaves(args, "", &mut fields);
                        for (k, child_id, dir) in &leaves {
                            if *dir == Some(ast::Direction::Out) {
                                continue;
                            }
                            let Some(field_value) = fields.get(&k[field.len()..]) else {
                                continue;
                            };
                            let expr = self.lower_expr(field_value);
                            let ctx = self.next_ctx_at(ast::expr_span(field_value));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: *child_id,
                                cond: None,
                                expr,
                                meta: None,
                                ctx,
                            });
                        }
                    }
                    // An *array* literal on a composite port (`.v = [1, 9]`)
                    // drives one leaf per element. Only the struct form was
                    // handled, so this connection was dropped in silence and
                    // the child read its default — a scalar port has always
                    // accepted a literal here.
                    if let ast::Expr::Array { elems, .. } = value {
                        // Declared index order, numerically: `v[10]` must not
                        // sort before `v[2]`.
                        let mut elements: Vec<(i64, SignalId, Option<ast::Direction>)> = leaves
                            .iter()
                            .filter_map(|(k, id, dir)| {
                                let rest = k.strip_prefix(&idx)?;
                                let index = rest.strip_suffix(']')?.parse::<i64>().ok()?;
                                Some((index, *id, *dir))
                            })
                            .collect();
                        elements.sort_by_key(|(index, _, _)| *index);
                        for ((_, child_id, dir), elem) in elements.iter().zip(elems) {
                            if *dir == Some(ast::Direction::Out) {
                                continue;
                            }
                            let expr = self.lower_expr(elem);
                            let ctx = self.next_ctx_at(ast::expr_span(elem));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: *child_id,
                                cond: None,
                                expr,
                                meta: None,
                                ctx,
                            });
                        }
                    }
                    continue;
                }
                // A composite (struct/array) port: wire each leaf to the matching
                // leaf of the parent signal (`.s = link` -> `s.valid`<->`link.valid`).
                // The parent side must be a signal path.
                let Some(base) = expr_path(value) else {
                    continue;
                };
                leaves.sort_by(|a, b| a.0.cmp(&b.0));
                for (k, child_id, dir) in leaves {
                    let suffix = &k[field.len()..]; // ".valid", "[0]"
                    let Some(&parent_id) = self.locals.get(&format!("{base}{suffix}")) else {
                        continue;
                    };
                    // An `inout` leaf is already aliased to this parent net (same
                    // signal), so its drivers fold through `Resolve` directly —
                    // wiring it again would make a self-driver.
                    if parent_id == child_id {
                        continue;
                    }
                    let ctx = self.next_ctx_at(ast::expr_span(value));
                    if dir == Some(ast::Direction::Out) {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: parent_id,
                            cond: None,
                            expr: Expr::Current(child_id),
                            meta: None,
                            ctx,
                        });
                    } else {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: child_id,
                            cond: None,
                            expr: Expr::Current(parent_id),
                            meta: None,
                            ctx,
                        });
                    }
                }
            }
        }

        // Behaviour: each impl block is one driver context (spec 3.14 —
        // override within, resolution across).
        for im in &impls {
            self.lint_generated_dead_assignments(im.items.iter().filter_map(|item| match item {
                ast::ImplItem::Stmt(statement) => Some(statement),
                _ => None,
            }));
            self.cur_ctx += 1;
            for item in &im.items {
                if let ast::ImplItem::Stmt(stmt) = item {
                    self.lower_stmt(stmt, None);
                }
            }
        }

        // Restore the caller's scope.
        self.lower_stack.pop();
        self.locals = saved_locals;
        self.local_enum = saved_enum;
        self.local_struct = saved_struct;
        self.local_struct_repr = saved_struct_repr;
        self.local_char = saved_char;
        self.local_array = saved_array;
        self.local_numeric = saved_numeric;
        self.instance_arrays = saved_instance_arrays;
        self.cur_env = saved_env;
        self.cur_type_env = saved_type_env;
        self.consts = saved_consts;
        self.const_values = saved_const_values;
        self.cur_instance_path = saved_instance_path;
        ports
    }

    /// Concrete parameter bindings written on an instance type
    /// (`Counter<W = 8>`, or positionally as `Counter<8>`).
    ///
    /// Positional args bind the declaration's parameters in order, matching
    /// `construct_type_params` — each takes the ones it owns, value params
    /// here and bare type params there. Dropping the positional form left the
    /// parameter unbound, which surfaced far downstream as "signal has unknown
    /// width (0)" rather than anything pointing at the instance.
    fn construct_params(
        &self,
        ty: &ast::Type,
        entity_id: DefId,
        env: &HashMap<String, i64>,
    ) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        let ast::Type::Generic { args, .. } = ty else {
            return out;
        };
        let decl = self.entities.get(&entity_id);
        for (i, a) in args.iter().enumerate() {
            match a {
                ast::GenericArg::Named { name, value } => {
                    if let Some(v) = self.eval_const(value, env) {
                        out.insert(name.text.clone(), v);
                    }
                }
                ast::GenericArg::Positional(e) => {
                    let Some(p) = decl.and_then(|d| d.params.params.get(i)) else {
                        continue;
                    };
                    // A bare type param is `construct_type_params`' business.
                    if p.bound.is_none() {
                        continue;
                    }
                    if let Some(v) = self.eval_const(e, env) {
                        out.insert(p.name.text.clone(), v);
                    }
                }
                ast::GenericArg::PositionalType(_) | ast::GenericArg::NamedType { .. } => {}
            }
        }
        out
    }

    /// Type-parameter bindings for a generic entity instance (`Buf<unsigned[8]>` ->
    /// `T -> unsigned[8]`): the entity's bare type params (bound `None`), matched to
    /// the construct's generic args positionally or by name.
    fn construct_type_params(
        &self,
        ty: &ast::Type,
        entity_id: DefId,
    ) -> HashMap<String, ast::Type> {
        let mut out = HashMap::new();
        let (Some(decl), ast::Type::Generic { args, .. }) = (self.entities.get(&entity_id), ty)
        else {
            return out;
        };
        let type_params: Vec<&ast::Param> = decl
            .params
            .params
            .iter()
            .filter(|p| p.bound.is_none())
            .collect();
        for (i, a) in args.iter().enumerate() {
            match a {
                ast::GenericArg::Named { name, value } => {
                    if type_params.iter().any(|p| p.name.text == name.text) {
                        if let Some(t) = expr_to_type(value) {
                            out.insert(name.text.clone(), t);
                        }
                    }
                }
                ast::GenericArg::NamedType { name, ty } => {
                    if type_params.iter().any(|p| p.name.text == name.text) {
                        out.insert(name.text.clone(), ty.clone());
                    }
                }
                ast::GenericArg::Positional(e) => {
                    if let (Some(p), Some(t)) = (decl.params.params.get(i), expr_to_type(e)) {
                        if p.bound.is_none() {
                            out.insert(p.name.text.clone(), t);
                        }
                    }
                }
                ast::GenericArg::PositionalType(ty) => {
                    if let Some(p) = decl.params.params.get(i) {
                        if p.bound.is_none() {
                            out.insert(p.name.text.clone(), ty.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// Combinational-loop lint (W-P010): a combinational signal whose value
    /// depends on itself through only combinational drivers is a zero-delay
    /// cycle with no register to break it — it has no well-defined settled
    /// value (the engines stop it at an arbitrary point). Event-block
    /// (sequential) targets break a cycle, so only comb→comb edges count.
    fn lint_combinational_loops(&mut self) {
        use std::collections::{BTreeSet, HashMap, HashSet};
        let procs = self.out.processes();
        // Signals driven combinationally, and for each its comb dependencies
        // (reads that are themselves combinational targets).
        let comb_targets: HashSet<u32> = procs
            .iter()
            .filter_map(|p| match p.kind {
                ProcessKind::Comb { target, .. } => Some(target.0),
                _ => None,
            })
            .collect();
        let mut deps: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in &procs {
            if let ProcessKind::Comb { target, .. } = p.kind {
                let e = deps.entry(target.0).or_default();
                for r in &p.reads {
                    if comb_targets.contains(&r.0) {
                        e.push(r.0);
                    }
                }
            }
        }
        // A signal on a cycle can reach itself. Report each such signal once.
        let reaches_self = |start: u32| -> bool {
            let mut stack = deps.get(&start).cloned().unwrap_or_default();
            let mut seen: HashSet<u32> = HashSet::new();
            while let Some(n) = stack.pop() {
                if n == start {
                    return true;
                }
                if seen.insert(n) {
                    if let Some(next) = deps.get(&n) {
                        stack.extend(next.iter().copied());
                    }
                }
            }
            false
        };
        let mut looped: BTreeSet<u32> = BTreeSet::new();
        for &t in &comb_targets {
            if reaches_self(t) {
                looped.insert(t);
            }
        }
        for t in looped {
            let signal = &self.out.signals[t as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!(
                    "`{path}` is in a combinational loop — its value depends on itself \
                     with no register in the path, so it has no settled value"
                ))
                .with_code(crate::diag::codes::COMBINATIONAL_LOOP)
                .at(signal.declaration_span)
                .help("break the loop with a clocked register, or an unconditional default"),
            );
        }
    }

    /// Undriven-output lint (W-P011): a plain `out` port (non-bus, non-`inout`,
    /// non-extern) with no combinational driver and no event-block update is
    /// never driven inside its entity — its value is stuck at the reset default.
    fn lint_undriven_outputs(&mut self) {
        let mut driven: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for d in &self.out.drivers {
            driven.insert(d.target.0);
        }
        for eb in &self.out.event_blocks {
            for u in &eb.updates {
                driven.insert(u.target.0);
            }
        }
        let ports = std::mem::take(&mut self.plain_out_ports);
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for sig in ports {
            if !seen.insert(sig.0) || driven.contains(&sig.0) {
                continue;
            }
            let signal = &self.out.signals[sig.0 as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!("output port `{path}` is never driven"))
                    .with_code(crate::diag::codes::UNDRIVEN_OUTPUT)
                    .at(signal.declaration_span)
                    .help("drive it inside the entity, or make it an `in`/`inout` port"),
            );
        }
        // Internal value-less `let` signals that are never driven — a forgotten
        // assignment (they read `0` forever).
        let lets = std::mem::take(&mut self.undriven_lets);
        for sig in lets {
            if !seen.insert(sig.0) || driven.contains(&sig.0) {
                continue;
            }
            let signal = &self.out.signals[sig.0 as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!("signal `{path}` is never driven"))
                    .with_code(crate::diag::codes::UNDRIVEN_OUTPUT)
                    .at(signal.declaration_span)
                    .help("assign it, give it an initial value, or remove it"),
            );
        }
    }

    /// Unused-signal lint (W-P003): an internal component local that no
    /// combinational or sequential process reads contributes no observable
    /// behavior. Root test/top locals are excluded because the native runner
    /// reads them outside the hardware IR.
    fn lint_unused_signals(&mut self) {
        let processes = self.out.processes();
        let read: std::collections::HashSet<u32> = processes
            .iter()
            .flat_map(|process| process.reads.iter().map(|id| id.0))
            .collect();
        let driven: std::collections::HashSet<u32> = self
            .out
            .drivers
            .iter()
            .map(|driver| driver.target.0)
            .chain(
                self.out
                    .event_blocks
                    .iter()
                    .flat_map(|block| block.updates.iter().map(|update| update.target.0)),
            )
            .collect();
        let mut seen = std::collections::HashSet::new();
        for signal in std::mem::take(&mut self.unused_lets) {
            if !seen.insert(signal.0) || read.contains(&signal.0) || !driven.contains(&signal.0) {
                continue;
            }
            let signal = &self.out.signals[signal.0 as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!("signal `{path}` is never read"))
                    .with_code(crate::diag::codes::UNUSED_SIGNAL)
                    .at(signal.declaration_span)
                    .help("remove it, or use its value in observable logic"),
            );
        }
    }

    /// Possible-latch lint (W-P002): a *combinational* signal that is only ever
    /// assigned under a condition keeps its previous value when no condition
    /// holds — an inferred latch. We flag the clean case: a single driver
    /// context whose drivers are all conditional. Event-block (sequential)
    /// signals hold by design, and multi-context signals go through `Resolve`,
    /// so both are excluded to avoid false positives.
    fn lint_possible_latches(&mut self) {
        use std::collections::{BTreeMap, BTreeSet};
        // Sequential state: any signal a clocked block updates.
        let mut sequential: BTreeSet<u32> = BTreeSet::new();
        for eb in &self.out.event_blocks {
            for u in &eb.updates {
                sequential.insert(u.target.0);
            }
        }
        // Per signal: its driver contexts, and whether any driver is a default.
        let mut ctxs: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        let mut has_default: BTreeMap<u32, bool> = BTreeMap::new();
        for d in &self.out.drivers {
            ctxs.entry(d.target.0).or_default().insert(d.ctx);
            let e = has_default.entry(d.target.0).or_insert(false);
            *e |= d.cond.is_none();
        }
        for (t, default) in &has_default {
            if *default
                || sequential.contains(t)
                || ctxs[t].len() > 1
                || self.lint_defaulted.contains(t)
            {
                continue;
            }
            let signal = &self.out.signals[*t as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!(
                    "`{path}` is only assigned under a condition, so it holds its \
                     previous value otherwise (inferred latch)"
                ))
                .with_code(crate::diag::codes::POSSIBLE_LATCH)
                .at(signal.declaration_span)
                .help("give it an unconditional default assignment"),
            );
        }
    }

    /// Report value names that matched no signal, constant or parameter.
    /// Resolution leaves plain value identifiers alone by design, so this is
    /// the first stage with the whole picture — and it used to drop them into
    /// an `Unknown` that made `check` pass and a build fail with a driver
    /// index instead of the name.
    fn report_unresolved_names(&mut self) {
        let mut names = std::mem::take(&mut *self.unresolved_names.borrow_mut());
        names.sort_by_key(|(name, span)| (span.start, name.clone()));
        names.dedup();
        for (name, span) in names {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!("no value named `{name}` is in scope"))
                    .with_code(crate::diag::codes::UNKNOWN_NAME)
                    .at(span)
                    .help(
                        "a value has to be a port, a `let` signal or local, a \
                         constant, or a parameter of the enclosing entity",
                    ),
            );
        }
    }

    /// Report a reference to an in-range instance-array slot that concrete
    /// generate elaboration omitted. This is distinct from an out-of-bounds
    /// index: the slot belongs to the declaration, but no child instance (and
    /// therefore no port signals) exists for this parameter set.
    fn report_unelaborated_instance_uses(&mut self) {
        let mut uses = std::mem::take(&mut *self.unelaborated_instance_uses.borrow_mut());
        uses.sort_by_key(|use_| {
            (
                use_.use_span.file.0,
                use_.use_span.start,
                use_.slot.clone(),
                use_.parent_path.clone(),
            )
        });
        uses.dedup_by(|left, right| {
            left.use_span == right.use_span
                && left.slot == right.slot
                && left.parent_path == right.parent_path
        });
        for use_ in uses {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "instance `{}` was not elaborated in `{}`",
                    use_.slot, use_.parent_path
                ))
                .with_code(crate::diag::codes::INSTANCE_NOT_ELABORATED)
                .at(use_.use_span)
                .label(
                    use_.declaration_span,
                    format!("instance array `{}` declared here", use_.slot_root()),
                )
                .help(
                    "this slot was omitted by the active generate conditions; \
                     guard the reference with the same condition, or construct \
                     the slot for this parameter set",
                ),
            );
        }
    }

    /// Record `e` when its flattened path begins with a declared-but-unbuilt
    /// instance slot in the body currently being lowered.
    fn record_unelaborated_instance_use(&self, e: &ast::Expr) -> bool {
        let Some(path) = self.folded_elem_path(e).or_else(|| expr_path(e)) else {
            return false;
        };
        let Some(facts) = self.instance_array_facts.get(&self.cur_instance_path) else {
            return false;
        };
        for fact in facts {
            for &index in &fact.declared {
                if fact.built.contains(&index) {
                    continue;
                }
                let slot = format!("{}[{index}]", fact.name);
                if path == slot
                    || path
                        .strip_prefix(&slot)
                        .is_some_and(|rest| rest.starts_with('.'))
                {
                    self.unelaborated_instance_uses
                        .borrow_mut()
                        .push(UnelaboratedInstanceUse {
                            slot,
                            parent_path: self.cur_instance_path.clone(),
                            use_span: ast::expr_span(e),
                            declaration_span: fact.span,
                        });
                    return true;
                }
            }
        }
        false
    }

    /// A generic entity analysed without a concrete parameter set may not yet
    /// know either its instance-array bounds or which generate branches build
    /// its slots. Do not turn that uncertainty into E-P017; concrete
    /// instantiations carry a hierarchy fact and are handled above.
    fn is_unresolved_instance_array_reference(&self, e: &ast::Expr) -> bool {
        let Some(path) = self.folded_elem_path(e).or_else(|| expr_path(e)) else {
            return false;
        };
        let Some((root, _)) = path.split_once('[') else {
            return false;
        };
        self.instance_arrays.contains(root)
            && !self
                .instance_array_facts
                .get(&self.cur_instance_path)
                .is_some_and(|facts| facts.iter().any(|fact| fact.name == root))
    }

    /// An assignment whose left side lowering cannot place. Two different
    /// mistakes arrive here and used to share one message that carried no
    /// span, no code and no help: an index form with no lowering contract, and
    /// an expression that is not a place at all.
    fn report_bad_assign_target(&mut self, target: &ast::Expr) {
        let text = crate::syntax::pretty::expr_string(target);
        let span = ast::expr_span(target);
        let diag = if matches!(target, ast::Expr::Index { .. }) {
            crate::diag::Diagnostic::error(format!("cannot assign to `{text}`"))
                .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                .help(
                    "runtime indices may traverse declared arrays and packed vectors; \
                     packed-vector slice bounds must be constant, and a custom container needs \
                     an `IndexAssign` implementation",
                )
        } else {
            crate::diag::Diagnostic::error(format!("`{text}` cannot be assigned to"))
                .with_code(crate::diag::codes::INVALID_ASSIGN_TARGET)
                .help(
                    "an assignment target is a signal, a field, an index, a slice, \
                     or a concatenation of those",
                )
        };
        self.sink.emit(diag.at(span));
    }

    /// Report field/index expressions that reached no hardware form. From the
    /// IR these are anonymous `Unknown`s, and validation could only say which
    /// signal's driver held one; here the source spelling is still available.
    fn report_unsupported_exprs(&mut self) {
        let mut exprs = std::mem::take(&mut *self.unsupported_exprs.borrow_mut());
        exprs.sort_by_key(|(text, span)| (span.start, text.clone()));
        exprs.dedup();
        for (text, span) in exprs {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!("`{text}` has no hardware form"))
                    .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                    .at(span)
                    .help(
                        "runtime indices may traverse declared arrays and packed vectors; \
                         packed-vector slice bounds must be constant, and a custom container needs \
                         an `Index` implementation",
                    ),
            );
        }
    }

    /// Report a constant index that falls outside the array it indexes, at the
    /// point where every index has finally become constant.
    ///
    /// `types` reports this already (`E-P003`), but only for an index that is a
    /// *literal in the source*. An index that becomes constant later — a
    /// generate loop's unrolled variable, or an entity parameter substituted at
    /// elaboration — arrived here unchecked, and lowering had no complaint to
    /// make: a write to an element that does not exist found no signal and was
    /// dropped, and a read of one clamped to the last element. Both silent.
    /// `for i in 0..3 { v[i] = .. }` on a 4-element `v` is the same statement
    /// as `v[4] = ..` after unrolling, and only one of the two was an error.
    ///
    /// The shape that motivates it: ranges are directional (`2..1` counts
    /// down), so a parameterized `for i in 0..(N - 1)` with `N = 0` iterates
    /// `0, -1` and quietly drove element -1.
    #[allow(clippy::type_complexity)]
    fn report_bad_indices(
        &mut self,
        found: Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        for (name, value, lo, hi, len, noun, span) in found {
            if !self.reported_oob.insert((name.clone(), value, span.start)) {
                continue;
            }
            // Worded exactly as the `types` check words it, so the two routes
            // to the same mistake read the same.
            let unit = if noun == "bit" { "vector" } else { "array" };
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "{noun} {value} is outside `{lo}..{hi}` of this {len}-{noun} {unit}"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(span)
                .help(
                    "the index is constant after elaboration — check the generate \
                     range or the parameter it came from against the declared size",
                ),
            );
        }
    }

    /// Walk `e` for indexed reads/writes whose index const-folds in the current
    /// environment and lands outside the base array. Reported through the
    /// caller so the walk itself can stay behind `&self`.
    #[allow(clippy::type_complexity)]
    fn collect_bad_indices(
        &self,
        e: &ast::Expr,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        use ast::Expr as E;
        match e {
            E::Index { base, index, span } => {
                self.collect_bad_indices(base, out);
                self.collect_bad_indices(index, out);
                // A runtime index (`mem[addr]`) does not fold and is not this
                // check's business. A *slice* is: both its bounds fold the same
                // way a scalar index does, and an unchecked one either reads
                // the missing bits as 0 (`x[i+8..i]` on an 8-bit `x`) or wraps
                // negative through a `u32` and surfaces as an internal
                // "slice bounds lo 4294967295 > hi 1" with no source location.
                let Some(path) = expr_path(base) else {
                    return;
                };
                let bounds: Vec<i64> = match index.as_ref() {
                    E::Range { lo, hi, .. } => [lo, hi]
                        .iter()
                        .filter_map(|b| self.eval_const(b, &self.cur_env))
                        .collect(),
                    // An omitted bound is supplied by the vector itself, so
                    // only the written one can be wrong.
                    E::PartialRange { lo, hi, .. } => [lo, hi]
                        .iter()
                        .filter_map(|b| b.as_deref())
                        .filter_map(|b| self.eval_const(b, &self.cur_env))
                        .collect(),
                    other => self.eval_const(other, &self.cur_env).into_iter().collect(),
                };
                for v in bounds {
                    self.check_one_index(&path, v, *span, out);
                }
            }

            E::Field { base, .. } | E::SysAttr { base, .. } | E::Unary { rhs: base, .. } => {
                self.collect_bad_indices(base, out)
            }
            E::Binary { lhs, rhs, .. }
            | E::Range {
                lo: lhs, hi: rhs, ..
            } => {
                self.collect_bad_indices(lhs, out);
                self.collect_bad_indices(rhs, out);
            }
            E::PartialRange { lo, hi, .. } => {
                if let Some(lo) = lo {
                    self.collect_bad_indices(lo, out);
                }
                if let Some(hi) = hi {
                    self.collect_bad_indices(hi, out);
                }
            }
            E::IfExpr {
                cond, then, els, ..
            } => {
                self.collect_bad_indices(cond, out);
                self.collect_bad_indices(then, out);
                self.collect_bad_indices(els, out);
            }
            E::Match {
                scrutinee, arms, ..
            } => {
                self.collect_bad_indices(scrutinee, out);
                for a in arms {
                    for s in &a.body.stmts {
                        self.collect_stmt_bad_indices(s, out);
                    }
                }
            }
            E::Call { callee, args, .. } => {
                self.collect_bad_indices(callee, out);
                for a in args {
                    self.collect_bad_indices(a, out);
                }
            }
            E::Construct { args, spread, .. } => {
                for a in args {
                    if let Some(v) = &a.value {
                        self.collect_bad_indices(v, out);
                    }
                }
                if let Some(s) = spread {
                    self.collect_bad_indices(s, out);
                }
            }
            E::Concat { parts: es, .. } | E::Array { elems: es, .. } => {
                for x in es {
                    self.collect_bad_indices(x, out);
                }
            }
            E::Int { .. }
            | E::SuffixLit { .. }
            | E::BitStrLit { .. }
            | E::CharLit { .. }
            | E::StrLit { .. }
            | E::Path(_) => {}
        }
    }

    /// Check one folded index or slice bound against `path`'s declared bounds.
    ///
    /// An element array carries its own index list, so a declared descending
    /// range (`Bit[7..0]` -> 7, 6, .., 0) needs no special case. Anything else
    /// falls back to the declared range, which covers packed vectors indexed
    /// by bit.
    #[allow(clippy::type_complexity)]
    fn check_one_index(
        &self,
        path: &str,
        v: i64,
        span: crate::diag::Span,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        if let Some(indices) = self.local_array.get(path) {
            if !indices.contains(&v) {
                let (lo, hi) = (
                    indices.iter().copied().min().unwrap_or(0),
                    indices.iter().copied().max().unwrap_or(0),
                );
                let noun = if self.instance_arrays.contains(path) {
                    "instance"
                } else {
                    "element"
                };
                out.push((path.to_string(), v, lo, hi, indices.len(), noun, span));
            }
        } else if let Some((left, right)) = self.persisted_range(path) {
            let (lo, hi) = (left.min(right), left.max(right));
            if v < lo || v > hi {
                let len = (hi - lo + 1) as usize;
                out.push((path.to_string(), v, lo, hi, len, "bit", span));
            }
        }
    }

    /// The statement form of [`Self::collect_bad_indices`]. A nested block is
    /// walked too: a `for` body is re-dispatched here once per iteration with
    /// its index substituted, and until then the loop variable does not fold,
    /// so the untaken walk finds nothing and costs nothing.
    #[allow(clippy::type_complexity)]
    fn collect_stmt_bad_indices(
        &self,
        s: &ast::Stmt,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        match s {
            ast::Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                self.collect_bad_indices(target, out);
                self.collect_bad_indices(value, out);
                if let Some(a) = after {
                    self.collect_bad_indices(a, out);
                }
            }
            ast::Stmt::Let(l) => {
                if let Some(v) = &l.value {
                    self.collect_bad_indices(v, out);
                }
            }
            ast::Stmt::Expr(e) => self.collect_bad_indices(e, out),
            ast::Stmt::Return { value: Some(v), .. } => self.collect_bad_indices(v, out),
            ast::Stmt::Return { .. } => {}
            ast::Stmt::If(iff) => self.collect_if_bad_indices(iff, out),
            ast::Stmt::Match(m) => {
                self.collect_bad_indices(&m.scrutinee, out);
                for a in &m.arms {
                    for s in &a.body.stmts {
                        self.collect_stmt_bad_indices(s, out);
                    }
                }
            }
            ast::Stmt::For { range, body, .. } => {
                self.collect_bad_indices(range, out);
                for s in &body.stmts {
                    self.collect_stmt_bad_indices(s, out);
                }
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn collect_if_bad_indices(
        &self,
        iff: &ast::IfStmt,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        self.collect_bad_indices(&iff.cond, out);
        // A condition that const-folds selects one branch at elaboration and
        // the other is never built, so its indices are not the design's. The
        // idiom this protects is the ordinary one for a generated chain —
        // `if i == 0 { s[0] = d; } else { s[i] = s[i - 1]; }` — whose `else`
        // reads `s[-1]` on the very iteration that does not take it.
        let taken = self.eval_const(&iff.cond, &self.cur_env).map(|v| v != 0);
        if taken != Some(false) {
            for s in &iff.then.stmts {
                self.collect_stmt_bad_indices(s, out);
            }
        }
        if taken == Some(true) {
            return;
        }
        match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(b)) => {
                for s in &b.stmts {
                    self.collect_stmt_bad_indices(s, out);
                }
            }
            Some(ast::ElseBranch::If(inner)) => self.collect_if_bad_indices(inner, out),
            None => {}
        }
    }

    /// Report a binary operator with no impl for its right operand's type.
    fn report_bad_operators(&mut self) {
        let mut items = std::mem::take(&mut *self.bad_operators.borrow_mut());
        items.sort_by_key(|(o, l, r, sp)| (sp.start, o.clone(), l.clone(), r.clone()));
        items.dedup();
        for (op, lhs, rhs, span) in items {
            let with = match &rhs {
                Some(r) => format!("a right operand of type `{r}`"),
                None => "this right operand".to_string(),
            };
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "no `{op}` operator for `{lhs}` with {with}"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(span)
                .help(
                    "an operator is `impl Operator<\"<sym>\", Rhs, Out> for T`, and the \
                     right operand has to match `Rhs` — a bare integer literal only \
                     matches an `Rhs` of the same type as the left operand, so convert \
                     it explicitly (`x * unsigned[8](3)`)",
                ),
            );
        }
    }

    /// Report a conversion with no route from its argument's type.
    ///
    /// `T(x)` on a named type dispatches to `impl From<S> for T` or to a total
    /// derivation (spec 3.17/3.28). With neither, lowering left an `Unknown`
    /// and the failure surfaced after every stage had reported success:
    /// "the driver for `T.e.y`: contains an Unknown (unlowered) expression",
    /// naming a signal rather than the expression, with no code and no span.
    /// `Bit(l)` on a `Logic` is the shape that finds this — narrowing away
    /// `'X'`/`'Z'` is exactly what std declines to provide a `From` for, and
    /// it is what anyone writes when shifting a bit out of a vector.
    fn report_bad_conversions(&mut self) {
        let mut items = std::mem::take(&mut *self.bad_conversions.borrow_mut());
        items.sort_by_key(|(t, s, span)| (span.start, t.clone(), s.clone()));
        items.dedup();
        for (target, src, span) in items {
            let from = match &src {
                Some(s) => format!("`{s}`"),
                None => "this argument's type".to_string(),
            };
            self.sink.emit(
                crate::diag::Diagnostic::error(format!("no conversion from {from} to `{target}`"))
                    .with_code(crate::diag::codes::TYPE_MISMATCH)
                    .at(span)
                    .help(
                        "`T(x)` needs an `impl From<S> for T`, or a derivation \
                     chain between the two types; conversions are never implicit",
                    ),
            );
        }
    }

    /// Report any function whose inlining hit the depth guard. Recursion in
    /// hardware has to terminate at elaboration — either the arguments
    /// const-fold, or the recursion is unbounded and there is no finite circuit
    /// for it. Without this the bail-out silently leaves `Unknown` mid-driver.
    fn report_depth_exceeded(&mut self) {
        let mut calls = std::mem::take(&mut *self.depth_exceeded.borrow_mut());
        calls.sort_by_key(|(name, span)| (span.file.0, span.start, name.clone()));
        calls.dedup();
        for (name, span) in calls {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "`{name}` recursed deeper than the inline limit, so it has no \
                     finite hardware form"
                ))
                .with_code(crate::diag::codes::UNBOUNDED_RECURSION)
                .at(span)
                .help(
                    "recursion must terminate at compile time — give it a \
                     constant-foldable argument, or rewrite it as a loop",
                ),
            );
        }
    }

    /// Spec 3.14 + Resolve: a signal driven from several contexts folds each
    /// context's contribution (its override chain over a 'Z' base) through
    /// the type's `Resolve` impl; a type without one is unresolved, and
    /// parallel drivers are an elaboration error.
    fn resolve_driver_contexts(&mut self) {
        use std::collections::BTreeMap;
        // target -> ctx -> ordered driver indices
        let mut by_target: BTreeMap<u32, BTreeMap<u32, Vec<usize>>> = BTreeMap::new();
        for (i, d) in self.out.drivers.iter().enumerate() {
            by_target
                .entry(d.target.0)
                .or_default()
                .entry(d.ctx)
                .or_default()
                .push(i);
        }
        let mut replaced: Vec<(u32, Expr, Option<Expr>)> = Vec::new();
        for (t, ctxs) in &by_target {
            // Metavalue companions are an implementation plane of their parent
            // signal. The parent's element-wise Resolve replaces their drivers
            // together; diagnosing the temporary per-context companion drivers
            // as an independent unresolved net would be a false conflict.
            if self.out.meta_of.values().any(|companion| companion == t) {
                continue;
            }
            if ctxs.len() < 2 {
                continue;
            }
            let ty = self.sig_type.get(t).cloned().unwrap_or_default();
            let direct_resolve = self
                .op_impls
                .contains_key(&("Resolve".to_string(), ty.clone()));
            let element_resolve = self
                .out
                .vector_element_enums
                .get(t)
                .filter(|element| {
                    self.blanket_array_impls
                        .get("Resolve")
                        .is_some_and(|requirement| {
                            self.op_impls
                                .contains_key(&(requirement.clone(), (*element).clone()))
                        })
                })
                .cloned();
            let has_resolve = direct_resolve || element_resolve.is_some();
            let path = self.out.signals[*t as usize].path.clone();
            let declaration_span = self.out.signals[*t as usize].declaration_span;
            if !has_resolve {
                // Lead with the mistake (several sources driving one signal),
                // not its symptom (a missing `Resolve` impl) — the usual cause
                // is a miswired bus, e.g. two producers on one net. Point at
                // each contributing connection when we know where it came from.
                let sites: Vec<crate::diag::Span> = ctxs
                    .keys()
                    .filter_map(|c| self.ctx_span.get(c).copied())
                    .collect();
                let mut d = crate::diag::Diagnostic::error(format!(
                    "`{path}` is driven by {} conflicting sources",
                    ctxs.len()
                ))
                .with_code(crate::diag::codes::CONFLICTING_DRIVERS);
                if let Some((first, rest)) = sites.split_first() {
                    d = d.at(*first);
                    for (i, s) in rest.iter().enumerate() {
                        d = d.label(*s, format!("conflicting source {}", i + 2));
                    }
                    d = d.label(declaration_span, "signal declared here");
                } else {
                    d = d.at(declaration_span);
                }
                self.sink.emit(d.help(format!(
                    "only one source may drive `{path}`; a bus needs converse \
                     endpoints (one side driving each leaf). To have several \
                     drivers fold instead, `{ty}` needs an `impl Resolve` (as \
                     `Logic` has)"
                )));
                continue;
            }
            // A forwarded array Resolve operates per element and preserves the
            // separate value/discriminant planes.
            if let Some(element) = element_resolve {
                let width = self.out.signals[*t as usize].width;
                if let Some((value, meta)) = self.resolve_vector_contexts(ctxs, width, &element) {
                    replaced.push((*t, value, Some(meta)));
                } else {
                    self.sink.emit(
                        crate::diag::Diagnostic::error(format!(
                            "could not instantiate element-wise `impl Resolve for {element}[]` folding `{path}`"
                        ))
                        .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                        .at(declaration_span)
                        .help(
                            "the element Resolve implementation must have a finite hardware form",
                        ),
                    );
                }
                continue;
            }

            // Each context: fold its drivers (later overrides) over a 'Z' base.
            let mut contributions = Vec::new();
            for idxs in ctxs.values() {
                let mut acc = Expr::Logic('Z');
                for &i in idxs {
                    let d = &self.out.drivers[i];
                    acc = match &d.cond {
                        None => d.expr.clone(),
                        Some(c) => Expr::Select {
                            cond: Box::new(c.clone()),
                            then: Box::new(d.expr.clone()),
                            els: Box::new(acc),
                        },
                    };
                }
                contributions.push(acc);
            }
            // Pairwise resolve via the impl's inlined body.
            let mut it = contributions.into_iter();
            let mut folded = it.next().unwrap();
            for c in it {
                match self.inline_resolve(&ty, folded.clone(), c) {
                    Some(r) => folded = r,
                    None => {
                        self.sink.emit(
                            crate::diag::Diagnostic::error(format!(
                                "could not inline `impl Resolve for {ty}` folding `{path}`"
                            ))
                            .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                            .at(declaration_span)
                            .help("the Resolve implementation must have a finite hardware form"),
                        );
                        break;
                    }
                }
            }
            replaced.push((*t, folded, None));
        }
        for (t, expr, meta) in replaced {
            self.out.drivers.retain(|d| d.target.0 != t);
            self.out.drivers.push(Driver {
                span: self.cur_span,
                target: SignalId(t),
                cond: None,
                expr,
                meta,
                ctx: 0,
            });
        }
    }

    fn resolve_vector_contexts(
        &self,
        contexts: &std::collections::BTreeMap<u32, Vec<usize>>,
        width: u32,
        element: &str,
    ) -> Option<(Expr, Expr)> {
        let z = self.char_disc('Z', element)?;
        let mut contributions = Vec::new();
        for indices in contexts.values() {
            let mut value = Expr::Const(z & 1);
            let mut meta = Expr::Const(if z >= 2 { z } else { 0 });
            value = repeat_element_plane(value, width, 1);
            meta = repeat_element_plane(meta, width, 4);
            for &index in indices {
                let driver = &self.out.drivers[index];
                let next_value = driver.expr.clone();
                let next_meta = driver
                    .meta
                    .clone()
                    .or_else(|| self.lower_meta_ir(&driver.expr, width))
                    .unwrap_or(Expr::Const(0));
                match &driver.cond {
                    None => {
                        value = next_value;
                        meta = next_meta;
                    }
                    Some(condition) => {
                        value = Expr::Select {
                            cond: Box::new(condition.clone()),
                            then: Box::new(next_value),
                            els: Box::new(value),
                        };
                        meta = Expr::Select {
                            cond: Box::new(condition.clone()),
                            then: Box::new(next_meta),
                            els: Box::new(meta),
                        };
                    }
                }
            }
            contributions.push((value, meta));
        }

        let mut contributions = contributions.into_iter();
        let mut accumulated = contributions.next()?;
        for incoming in contributions {
            let mut value = Expr::Const(0);
            let mut meta = Expr::Const(0);
            for index in 0..width {
                let left = logic_element_disc(&accumulated.0, &accumulated.1, index);
                let right = logic_element_disc(&incoming.0, &incoming.1, index);
                let result = self.inline_resolve(element, left, right)?;
                let value_bit = Expr::Binary {
                    op: BinOp::And,
                    lhs: Box::new(result.clone()),
                    rhs: Box::new(Expr::Const(1)),
                };
                value = or_expr(
                    value,
                    Expr::Binary {
                        op: BinOp::Shl,
                        lhs: Box::new(value_bit),
                        rhs: Box::new(Expr::Const(index as u64)),
                    },
                );
                let is_meta = Expr::Binary {
                    op: BinOp::Ge,
                    lhs: Box::new(result.clone()),
                    rhs: Box::new(Expr::Const(2)),
                };
                let nibble = Expr::Select {
                    cond: Box::new(is_meta),
                    then: Box::new(result),
                    els: Box::new(Expr::Const(0)),
                };
                meta = or_expr(
                    meta,
                    Expr::Binary {
                        op: BinOp::Shl,
                        lhs: Box::new(nibble),
                        rhs: Box::new(Expr::Const((4 * index) as u64)),
                    },
                );
            }
            accumulated = (value, meta);
        }
        Some(accumulated)
    }

    /// Inline `impl Resolve for <ty>` over two already-lowered expressions.
    fn inline_resolve(&self, ty: &str, a: Expr, b: Expr) -> Option<Expr> {
        let fns = self
            .op_impls
            .get(&("Resolve".to_string(), ty.to_string()))?;
        let (f, _) = fns.first()?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        env.insert("self".to_string(), Val::Scalar(a));
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                env.insert(n.text.clone(), Val::Scalar(b));
            }
        }
        match self.inline_block(&body.stmts, &env)? {
            Val::Scalar(e) => Some(e),
            _ => None,
        }
    }

    /// A `read<T>("path")` initializer's requested type and literal path.
    fn fs_read_call(e: &ast::Expr) -> Option<(&ast::Type, &str)> {
        let ast::Expr::Call {
            callee,
            type_args,
            args,
            ..
        } = e
        else {
            return None;
        };
        let ast::Expr::Path(p) = callee.as_ref() else {
            return None;
        };
        if p.segments.len() != 1 || p.segments[0].text != "read" {
            return None;
        }
        match (type_args.as_slice(), args.as_slice()) {
            ([requested], [ast::Expr::StrLit { text, .. }]) => Some((requested, text)),
            _ => None,
        }
    }

    /// Seed a struct literal's leaves, recursing into a nested literal.
    ///
    /// `{ .p = { .x = 7 } }` names no leaf at `p` — the leaves are `p.x` and
    /// `p.y` — so the field loop used to `continue` past it and the inner
    /// values were dropped without a word, while a sibling scalar field on the
    /// same literal seeded correctly.
    /// Whether `value` is a call at all — `Pair::new()` or `Pair()`, once it is
    /// known to name no declared function, is a default construction rather
    /// than an initializer that failed to fold.
    fn is_default_construction(value: &ast::Expr) -> bool {
        matches!(value, ast::Expr::Call { .. })
    }

    /// Whether `value` is a call to a function this compilation declares —
    /// which separates "a body too complex to fold" from a default
    /// construction like `Pair::new()`, whose name resolves to no function at
    /// all and whose structural zeros are the right answer.
    fn resolves_to_declared_fn(&self, value: &ast::Expr) -> bool {
        let ast::Expr::Call { callee, .. } = value else {
            return false;
        };
        self.free_fns.get(callee).is_some()
    }

    /// The expression a call returns, with the arguments written at the call
    /// substituted for the parameters.
    ///
    /// `None` unless the callee is a declared function whose body is a single
    /// returned expression — anything else has no one expression to stand for
    /// the call, and the caller keeps its own handling.
    fn returned_expr_from_call(&self, value: &ast::Expr) -> Option<ast::Expr> {
        let ast::Expr::Call { callee, args, .. } = value else {
            return None;
        };
        let f = self.free_fns.get(callee)?;
        let [ast::Stmt::Return {
            value: Some(returned),
            ..
        }] = f.body.as_ref()?.stmts.as_slice()
        else {
            return None;
        };
        let mut bound: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(name) = param.name.as_ref() {
                bound.insert(name.text.clone(), arg.clone());
            }
        }
        Some(subst_expr_paths(returned, &bound))
    }

    /// The struct literal a call returns, with the arguments written at the
    /// call substituted for the parameters — so a struct-typed `let` can seed
    /// its fields from `let p: Pair = make(6)` the way it already does from
    /// `let p: Pair = { .a = 6, .b = 7 }`.
    ///
    /// An initializer is a power-on value folded at elaboration, and the scalar
    /// path folds a call: `let a: unsigned[8] = double(6)` is 12. The struct
    /// path folded nothing, because the block that does the folding is reached
    /// only through `locals[name]` and a struct local has no signal under its
    /// bare name — only `p.a`, `p.b`. So every field powered on at zero with no
    /// diagnostic, while the same call written as a separate assignment was
    /// right.
    ///
    /// `None` when the callee is not a known function (`Pair::new()` and
    /// `Pair()` are the structural default, not a call to fold) or its body is
    /// anything but a single returned literal — the caller reports those.
    fn struct_literal_from_call(
        &self,
        value: &ast::Expr,
    ) -> Option<(Vec<ast::ConnectArg>, Option<ast::Expr>)> {
        let ast::Expr::Call { callee, args, .. } = value else {
            return None;
        };
        let f = self.free_fns.get(callee)?;
        let body = f.body.as_ref()?;
        let [ast::Stmt::Return {
            value: Some(returned),
            ..
        }] = body.stmts.as_slice()
        else {
            return None;
        };
        let ast::Expr::Construct {
            args: fields,
            spread,
            ..
        } = returned
        else {
            return None;
        };
        // Bind each declared parameter to its argument. `self` takes no
        // argument, so it is skipped rather than consuming one.
        let mut bound: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(name) = param.name.as_ref() {
                bound.insert(name.text.clone(), arg.clone());
            }
        }
        let fields = fields
            .iter()
            .map(|field| ast::ConnectArg {
                field: field.field.clone(),
                value: field.value.as_ref().map(|v| subst_expr_paths(v, &bound)),
                span: field.span,
            })
            .collect();
        Some((
            fields,
            spread.as_deref().map(|s| subst_expr_paths(s, &bound)),
        ))
    }

    fn seed_struct_literal(
        &mut self,
        prefix: &str,
        struct_name: Option<&str>,
        args: &[ast::ConnectArg],
        spread: Option<&ast::Expr>,
        span: crate::diag::Span,
    ) {
        // `{ ..base, .x = v }` takes every leaf from `base` first, so the
        // explicit fields below overwrite what they name. Without this the
        // fields the literal did not mention powered on at 0 rather than at
        // the base's value, while the ones it did name seeded correctly.
        if let Some(base) = spread.and_then(expr_path) {
            let from = format!("{base}.");
            let leaves: Vec<(String, SignalId)> = self
                .locals
                .iter()
                .filter(|(name, _)| name.starts_with(&from))
                .map(|(name, id)| (name.clone(), *id))
                .collect();
            for (name, src) in leaves {
                let target = format!("{prefix}.{}", &name[from.len()..]);
                if let Some(&dst) = self.locals.get(&target) {
                    let init = self.out.signals[src.0 as usize].init.clone();
                    self.out.signals[dst.0 as usize].init = init;
                }
            }
        }
        let fields: Vec<(String, ast::Type)> = struct_name
            .and_then(|h| self.structs.get(h))
            .map(|sd| {
                sd.fields
                    .iter()
                    .map(|f| (f.name.text.clone(), f.ty.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (i, arg) in args.iter().enumerate() {
            // Named (`.a = 1`) or positional (bound by declaration order).
            let field = match &arg.field {
                Some(f) => Some(f.text.clone()),
                None => fields.get(i).map(|(n, _)| n.clone()),
            };
            let (Some(field), Some(value)) = (field, arg.value.as_ref()) else {
                continue;
            };
            let path = format!("{prefix}.{field}");
            // A struct-typed field's value is read against *that field's*
            // type, so a positional literal nested inside a named one
            // (`{ .inner = { 1, 2 }, .tag = 9 }`) is a struct literal too. It
            // stayed a concat and seeded nothing.
            let field_ty = fields.iter().find(|(n, _)| *n == field).map(|(_, t)| t);
            let value = &self.as_struct_literal(field_ty, value);
            // A field whose value is itself an aggregate names no leaf of its
            // own, so each of these used to fall through the lookup below and
            // seed nothing — silently, and without even reaching the
            // non-constant check.
            match value {
                ast::Expr::Construct {
                    args: inner,
                    spread: inner_spread,
                    ..
                } => {
                    let inner_ty = fields
                        .iter()
                        .find(|(n, _)| *n == field)
                        .and_then(|(_, ty)| self.free_fns.type_head_key(ty));
                    self.seed_struct_literal(
                        &path,
                        inner_ty.as_deref(),
                        inner,
                        inner_spread.as_deref(),
                        span,
                    );
                    continue;
                }
                // `{ .arr = [4, 5, 6] }` seeds `arr[0..2]`.
                ast::Expr::Array { elems, .. } => {
                    self.seed_elements(&path, elems.iter().collect::<Vec<_>>(), span);
                    continue;
                }
                // `{ .name = "abc" }` seeds one element per character.
                ast::Expr::StrLit { text, .. } => {
                    let indices = self.local_array.get(&path).cloned().unwrap_or_default();
                    for (c, i) in text.chars().zip(&indices) {
                        if let Some(&id) = self.locals.get(&format!("{path}[{i}]")) {
                            let en = self.out.signals[id.0 as usize].enum_type.clone();
                            let v = en
                                .and_then(|e| self.char_disc(c, &e))
                                .unwrap_or(c as u32 as u64);
                            self.out.signals[id.0 as usize].init = vec![v];
                        }
                    }
                    continue;
                }
                _ => {}
            }
            // Only constants seed an init. A non-constant is *not* lowered as
            // a driver here, whatever the comment used to claim: `{ .y = src +
            // 1 }` left `p.y` at 0 for every value of `src`, and the undriven
            // lint does not reach a struct leaf, so nothing said anything.
            let Some(&id) = self.locals.get(&path) else {
                continue;
            };
            // The field's own enum type resolves a character literal
            // (`.state = 'Z'`) to its variant.
            let en = self.out.signals[id.0 as usize].enum_type.clone();
            let is_char = self.out.signals[id.0 as usize].char;
            if let Some(v) = self.const_init_value(value, en.as_deref(), is_char) {
                self.out.signals[id.0 as usize].init = vec![v];
            } else {
                self.report_non_constant_init(&path, span);
            }
        }
    }

    /// Seed each element of an array-valued field or local from a literal.
    fn seed_elements(&mut self, prefix: &str, elems: Vec<&ast::Expr>, span: crate::diag::Span) {
        let indices = self.local_array.get(prefix).cloned().unwrap_or_default();
        for (elem, i) in elems.into_iter().zip(indices) {
            let path = format!("{prefix}[{i}]");
            let Some(&id) = self.locals.get(&path) else {
                // An element that is itself a struct has no scalar leaf of its
                // own — its fields are `ps[0].a` — so the lookup above found
                // nothing and the element was skipped in silence, leaving every
                // field of every element at zero. It is a struct literal in
                // element position, read against the element's type like any
                // other.
                if let Some(struct_name) = self.local_struct.get(&path).cloned() {
                    match elem {
                        ast::Expr::Construct { args, spread, .. } => self.seed_struct_literal(
                            &path,
                            Some(&struct_name),
                            args,
                            spread.as_deref(),
                            span,
                        ),
                        _ => {
                            if let Some(args) = self.positional_struct_args(&struct_name, elem) {
                                self.seed_struct_literal(
                                    &path,
                                    Some(&struct_name),
                                    &args,
                                    None,
                                    span,
                                );
                            }
                        }
                    }
                }
                continue;
            };
            let en = self.out.signals[id.0 as usize].enum_type.clone();
            let is_char = self.out.signals[id.0 as usize].char;
            if let Some(v) = self.const_init_value(elem, en.as_deref(), is_char) {
                self.out.signals[id.0 as usize].init = vec![v];
            } else {
                self.report_non_constant_init(&path, span);
            }
        }
    }

    /// An initializer is a power-on value, folded at elaboration (spec 3.29).
    /// One that reads another signal cannot fold, and every site that seeds an
    /// init — scalar, struct field, array element — used to drop it in
    /// silence, leaving the signal at its type's default. A driver is the
    /// spelling that computes from other signals, and is a different thing:
    /// continuous rather than once.
    /// Seed a struct local's leaves from a whole struct constant
    /// (`let p: Pair = K`). `false` when `value` names no struct constant, so
    /// the caller can fall through to its diagnostic.
    fn seed_from_struct_const(&mut self, prefix: &str, value: &ast::Expr) -> bool {
        let Some(fields) = expr_path(value).and_then(|name| self.const_struct_value(&name)) else {
            return false;
        };
        for (field, folded) in fields {
            let Expr::Const(word) = folded else { continue };
            let Some(&id) = self.locals.get(&format!("{prefix}.{field}")) else {
                continue;
            };
            let width = self.out.signals[id.0 as usize].width;
            let masked = if width > 0 && width < 64 {
                word & ((1u64 << width) - 1)
            } else {
                word
            };
            self.out.signals[id.0 as usize].init = vec![masked];
        }
        true
    }

    /// A *positional* struct literal, as the named form's arguments.
    ///
    /// `{ 6, 7 }` carries no field names, so it lexes as a bit concatenation
    /// and every struct-typed use of it saw a concat where `{ .a = 6, .b = 7 }`
    /// gives a construction. Nothing bound the parts to fields, so a `let`
    /// initialized this way seeded nothing and an assignment written this way
    /// was dropped entirely — its leaves then reported as never driven.
    /// Binding part *i* to declared field *i* is what the named form means.
    ///
    /// Which reading applies is decided by the *assigned type*, not by the
    /// shape of the braces: against a struct these braces are a struct
    /// literal, against an array or packed vector they stay a concatenation.
    /// So this is keyed on the target's struct name, and a part count that
    /// does not match the field count binds what it can — the fields left
    /// unbound are then reported by the checks that already look for them.
    fn positional_struct_args(
        &self,
        struct_name: &str,
        value: &ast::Expr,
    ) -> Option<Vec<ast::ConnectArg>> {
        let ast::Expr::Concat { parts, span } = value else {
            return None;
        };
        let declared = self.structs.get(struct_name)?;
        Some(
            parts
                .iter()
                .zip(&declared.fields)
                .map(|(part, field)| ast::ConnectArg {
                    field: Some(field.name.clone()),
                    value: Some(part.clone()),
                    span: *span,
                })
                .collect(),
        )
    }

    /// `value` as a struct literal when the type it is being assigned to is a
    /// struct: a positional `{ 6, 7 }` becomes the named form, and anything
    /// else is returned unchanged.
    ///
    /// Every position that knows its destination's type goes through here — a
    /// field of an enclosing literal, an instance's port connection, a
    /// function's parameter and its return — so the one rule ("the assigned
    /// type decides how to read the braces") is applied in one way rather than
    /// re-derived per site.
    fn as_struct_literal(&self, ty: Option<&ast::Type>, value: &ast::Expr) -> ast::Expr {
        let Some(args) = ty
            .and_then(|ty| self.free_fns.type_head_key(ty))
            .and_then(|head| self.positional_struct_args(&head, value))
        else {
            return value.clone();
        };
        ast::Expr::Construct {
            ty: None,
            args,
            spread: None,
            span: ast::expr_span(value),
        }
    }

    /// A struct constant's folded fields, keyed off the dotted entries
    /// `fold_const` left in the constant table. `None` when `name` names no
    /// struct constant.
    fn const_struct_value(&self, name: &str) -> Option<Vec<(String, Expr)>> {
        let prefix = format!("{name}.");
        let mut fields: Vec<(String, Expr)> = self
            .const_values
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix(&prefix)
                    .map(|field| (field.to_string(), value.clone()))
            })
            .collect();
        if fields.is_empty() {
            return None;
        }
        // Leaves are matched by name downstream; a stable order only keeps the
        // emitted IR reproducible.
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Some(fields)
    }

    /// A struct constant's `(field, value)` pairs. A named argument binds by
    /// name; a positional one binds to the declared field at that position, so
    /// both spellings of a literal fold the same way. `None` when the type is
    /// not a known struct or an argument carries no value.
    fn const_struct_fields(
        &self,
        ty: &ast::Type,
        args: &[ast::ConnectArg],
    ) -> Option<Vec<(String, ast::Expr)>> {
        let declared = self.structs.get(&self.free_fns.type_head_key(ty)?)?;
        let mut out = Vec::new();
        for (position, arg) in args.iter().enumerate() {
            let field = match &arg.field {
                Some(name) => name.text.clone(),
                None => declared.fields.get(position)?.name.text.clone(),
            };
            out.push((field, arg.value.clone()?));
        }
        Some(out)
    }

    fn report_non_constant_init(&mut self, name: &str, span: crate::diag::Span) {
        self.sink.emit(
            crate::diag::Diagnostic::error(format!(
                "the initializer for `{name}` is not a constant"
            ))
            .with_code(crate::diag::codes::NON_CONSTANT_INITIALIZER)
            .at(span)
            .help(format!(
                "an initializer is the signal's power-on value and is folded at \
                 elaboration. To compute it from other signals, drive it instead: \
                 declare `{name}` without a value, then assign it"
            )),
        );
    }

    /// The initial value of a constant `let` initializer, folded at compile
    /// time into a signal's power-on `init`. Two shapes reach here:
    /// **literals** — a real's f64 bits, or a character's position in its enum
    /// (a `'g'` has no intrinsic value; its type gives it one) — and **enum
    /// variants** — `Color::Red`, or `Bool`'s `true`/`false`, resolved to their
    /// discriminant. An integer or const-fn expression folds through
    /// `eval_const_fns`. A string initializer is a `Char` array, not a scalar,
    /// so it is written element-wise elsewhere, not here.
    fn const_init_value(&self, e: &ast::Expr, target: Option<&str>, is_char: bool) -> Option<u64> {
        match e {
            // --- literals: a value read as bits ---
            ast::Expr::Int { text, .. } if text.contains('.') => {
                text.replace('_', "").parse::<f64>().ok().map(f64::to_bits)
            }
            // A character reads by its position in the target enum (VHDL
            // `T'pos`), else std's default logic type. No value table here.
            // A `Char` target reads it through the Unicode table (its code
            // point), as `typed_char_literal` does for an operand. Only the
            // enum paths were tried here, and `Char` is a kernel type with no
            // variants, so `let c: Char = 'A';` folded to nothing — and once
            // a non-constant initializer became an error, that turned into
            // "the initializer for `c` is not a constant" on an obviously
            // constant character.
            ast::Expr::CharLit { ch, .. } if is_char => Some(*ch as u32 as u64),
            ast::Expr::CharLit { ch, .. } => target
                .and_then(|en| self.char_disc(*ch, en))
                .or_else(|| self.char_disc(*ch, DEFAULT_LOGIC_TYPE)),
            // --- enum variants: a name resolved to its discriminant ---
            // Includes `Bool`'s `true`/`false` (desugared to `Bool::true` etc.).
            ast::Expr::Path(p) if p.segments.len() >= 2 => self.enum_variant_path(p),
            // A radix bit-string initializer (`let v: unsigned[8] = x"AB"`) —
            // its value bits (metavalue positions carried separately, stage 1b).
            ast::Expr::BitStrLit { base, digits, .. } => {
                Some(self.decode_bit_string(*base, digits).0)
            }
            // A plain string on a logic-vector target reads as a logic array —
            // each character is a `std_ulogic` (no prefix needed). Only
            // reached for a single-signal (packed vector) target; a `Char[]`
            // flattens and never lands here.
            ast::Expr::StrLit { text, .. } => Some(self.decode_bit_string('b', text).0),
            // --- integer / const-fn arithmetic ---
            // A newtype constructor is value-transparent, so `let b: Byte =
            // Byte(200);` seeds 200. `eval_const_fns` has no rule for a call,
            // so the signal kept its default and read 0.
            ast::Expr::Call { callee, args, .. }
                if (match callee.as_ref() {
                    ast::Expr::Path(path) => self.free_fns.struct_path_key(path),
                    _ => None,
                })
                .and_then(|name| self.structs.get(&name).cloned())
                .is_some_and(|s| s.fields.is_empty() && s.base.is_some()) =>
            {
                self.const_init_value(args.first()?, target, is_char)
            }
            _ => eval_const_fns(e, &self.cur_env, &self.free_fns, 0).map(|v| v as u64),
        }
    }

    /// Fold each `impl New for T { fn new() -> T { return <const>; } }` to the
    /// type's uninitialized default value. Runs after `impls`/`enum_variants`
    /// are collected.
    fn compute_new_defaults(&self) -> HashMap<String, u64> {
        let mut out = HashMap::new();
        // Trait impls land in `op_impls` keyed by (trait, type); `impl New for T`
        // has a `new()` whose constant body is `T`'s uninitialized default.
        for ((tr, ty), fns) in &self.op_impls {
            if tr != "New" {
                continue;
            }
            for (f, _) in fns {
                if f.name.text != "new" {
                    continue;
                }
                if let Some(body) = &f.body {
                    for st in &body.stmts {
                        if let ast::Stmt::Return { value: Some(e), .. } = st {
                            let is_char = ty == "Char"
                                || struct_derives_kernel(ty, "Char", &self.structs, &self.free_fns);
                            if let Some(v) = self.const_init_value(e, Some(ty), is_char) {
                                out.insert(ty.clone(), v);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn next_ctx(&mut self) -> u32 {
        self.cur_ctx += 1;
        self.cur_ctx
    }

    /// A fresh driver context tied to the source site that created it, so a
    /// later conflict can point at the connection rather than just the signal.
    fn next_ctx_at(&mut self, span: crate::diag::Span) -> u32 {
        let ctx = self.next_ctx();
        self.ctx_span.insert(ctx, span);
        ctx
    }

    fn add_signal(
        &mut self,
        entity: &str,
        name: &str,
        width: u32,
        declaration_span: crate::diag::Span,
    ) {
        let id = SignalId(self.out.signals.len() as u32);
        self.out.signals.push(Signal {
            path: format!("{entity}.{name}"),
            declaration_span,
            width,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        });
        self.locals.insert(name.to_string(), id);
    }

    /// Ensure a `Logic`-vector signal has its metavalue companion — an extra
    /// signal, 4 bits per element, holding each element's full `std_ulogic`
    /// discriminant (nibble *i* = element *i*'s 9-value), so a read reconstructs
    /// the exact value (`'X'` vs `'Z'` vs …). The companion is a normal signal,
    /// so the engines store/reset it for free. Created only where a metavalue
    /// appears, so metavalue-free designs are untouched. Initial discriminants
    /// are arbitrary-width word vectors, so the companion scales with the
    /// source vector rather than stopping at one ABI word.
    fn ensure_meta_companion(&mut self, id: SignalId, discs: Vec<u64>) {
        if let Some(&cid) = self.out.meta_of.get(&id.0) {
            self.out.signals[cid as usize].init = discs;
            return;
        }
        let sig = &self.out.signals[id.0 as usize];
        let companion = Signal {
            path: format!("{}$meta", sig.path),
            declaration_span: sig.declaration_span,
            width: sig.width * 4,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: discs,
            enum_type: None,
        };
        let cid = self.out.signals.len() as u32;
        self.out.signals.push(companion);
        self.out.meta_of.insert(id.0, cid);
    }

    /// The metavalue companion for a *driven* vector: existing one, or a fresh
    /// all-clean (`init = 0`) companion. Never overwrites an init companion.
    fn driven_companion(&mut self, id: SignalId) -> u32 {
        if let Some(&c) = self.out.meta_of.get(&id.0) {
            return c;
        }
        let sig = &self.out.signals[id.0 as usize];
        let companion = Signal {
            path: format!("{}$meta", sig.path),
            declaration_span: sig.declaration_span,
            width: sig.width * 4,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        };
        let cid = self.out.signals.len() as u32;
        self.out.signals.push(companion);
        self.out.meta_of.insert(id.0, cid);
        cid
    }

    /// The metavalue disc-array a driver expression produces, or `None` if it is
    /// provably clean. `width` is the expression's result element count (for
    /// the poison pattern); recursive operands derive their own widths from the
    /// IR rather than inheriting a narrowed destination width. Covers: a signal
    /// read (its companion — copies and port
    /// connections carry the metavalue), `numeric_std` arithmetic (any metavalue
    /// operand poisons the whole result to `'X'`), and a mux (per branch).
    /// Logical/relational is a follow-on.
    fn lower_meta_ir(&self, e: &Expr, width: u32) -> Option<Expr> {
        match e {
            Expr::Current(id) => self
                .out
                .meta_of
                .get(&id.0)
                .map(|&c| Expr::Current(SignalId(c))),
            Expr::Old(id) => self.out.meta_of.get(&id.0).map(|&c| Expr::Old(SignalId(c))),
            Expr::Binary {
                op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div,
                lhs,
                rhs,
            } => {
                let lhs_width = self.meta_expr_width(lhs, width);
                let rhs_width = self.meta_expr_width(rhs, width);
                let cond = [
                    (self.lower_meta_ir(lhs, lhs_width), lhs_width),
                    (self.lower_meta_ir(rhs, rhs_width), rhs_width),
                ]
                .into_iter()
                .filter_map(|(meta, operand_width)| {
                    meta.map(|meta| any_unknown(&meta, operand_width))
                })
                .reduce(|a, b| Expr::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                })?;
                let mut all_x = vec![0u64; (width as usize).div_ceil(16)];
                for i in 0..width {
                    all_x[i as usize / 16] |= self.x_disc() << (4 * (i % 16));
                }
                Some(Expr::Select {
                    cond: Box::new(cond),
                    then: Box::new(words_const(all_x)),
                    els: Box::new(Expr::Const(0)),
                })
            }
            Expr::Select { cond, then, els } => {
                let (mt, me) = (
                    self.lower_meta_ir(then, width),
                    self.lower_meta_ir(els, width),
                );
                if mt.is_none() && me.is_none() {
                    return None;
                }
                Some(Expr::Select {
                    cond: cond.clone(),
                    then: Box::new(mt.unwrap_or(Expr::Const(0))),
                    els: Box::new(me.unwrap_or(Expr::Const(0))),
                })
            }
            Expr::Binary { op, lhs, rhs } if matches!(op, BinOp::And | BinOp::Or | BinOp::Xor) => {
                self.logical_meta(*op, lhs, rhs, width)
            }
            // A slice selects elements, so it selects their discriminants: the
            // companion holds four bits per element, so the nibble range is
            // four times the element range. Without this a slice had no
            // companion and `"0000X100"[3..0]` came back `1100`, the metavalue
            // replaced by the value plane's bit for it.
            Expr::Slice { base, hi, lo } => {
                let base_width = self.meta_expr_width(base, width.max(hi + 1));
                let m = self.lower_meta_ir(base, base_width)?;
                Some(Expr::Slice {
                    base: Box::new(m),
                    hi: hi * 4 + 3,
                    lo: lo * 4,
                })
            }
            // A shift moves elements, so it moves their discriminants with
            // them: the companion holds four bits per element, so it shifts by
            // four times as much. Without this the result had no companion at
            // all and `"0000X100" << 2` came back `00110000` -- the metavalue
            // silently replaced by the value plane's bit for it -- where VHDL
            // gives `00X10000`. Elements shifted in are `'0'`, which is a zero
            // nibble, exactly what shifting in zeroes produces.
            Expr::Binary {
                op: op @ (BinOp::Shl | BinOp::Shr),
                lhs,
                rhs,
            } => {
                let lhs_width = self.meta_expr_width(lhs, width);
                let m = self.lower_meta_ir(lhs, lhs_width)?;
                Some(Expr::Binary {
                    op: *op,
                    lhs: Box::new(m),
                    rhs: Box::new(Expr::Binary {
                        op: BinOp::Mul,
                        lhs: Box::new(rhs.as_ref().clone()),
                        rhs: Box::new(Expr::Const(4)),
                    }),
                })
            }
            // `not` as an IR unary. A vector `not` no longer arrives here --
            // it lowers to `x xor all-ones` and is handled as an `Xor` above --
            // so nothing in the corpus or the nvc differential reaches this,
            // and its `'U'` handling below is consistent by construction rather
            // than by test. It is kept because the shape is still a valid one
            // for the IR to hold, and a handler that disagreed with the `Xor`
            // path would be worse than one that is merely unused.
            Expr::Unary { op: UnOp::Not, rhs } => {
                // The operand's metavalue positions become `'U'` or `'X'`,
                // clean positions clean.
                let rhs_width = self.meta_expr_width(rhs, width);
                let m = self.lower_meta_ir(rhs, rhs_width)?;
                let (xd, ud) = (self.x_disc(), self.u_disc());
                let mut acc = Expr::Const(0);
                for i in 0..width {
                    let some = Some(m.clone());
                    let disc = Expr::Select {
                        cond: Box::new(u_bit(&some, i, ud)),
                        then: Box::new(Expr::Const(ud)),
                        els: Box::new(Expr::Const(xd)),
                    };
                    acc = or_expr(acc, meta_nibble(meta_bit(&some, i), i, disc));
                }
                Some(acc)
            }
            _ => None,
        }
    }

    /// Best available element width for a lowered value expression. Constants
    /// inherit their contextual fallback; signal reads and slices are exact.
    /// Keeping this structural avoids baking type-family widths into the IR and
    /// is enough to prevent a narrow outer slice from hiding unknown elements
    /// in a wider computed operand.
    fn meta_expr_width(&self, expr: &Expr, fallback: u32) -> u32 {
        match expr {
            Expr::Current(id) | Expr::Old(id) => self
                .out
                .signals
                .get(id.0 as usize)
                .map(|signal| signal.width)
                .unwrap_or(fallback),
            Expr::Slice { hi, lo, .. } => hi.saturating_sub(*lo) + 1,
            Expr::Select { then, els, .. } => self
                .meta_expr_width(then, fallback)
                .max(self.meta_expr_width(els, fallback)),
            Expr::Unary { rhs, .. } => self.meta_expr_width(rhs, fallback),
            Expr::Binary {
                op: BinOp::Shl | BinOp::Shr | BinOp::AShr,
                lhs,
                ..
            } => self.meta_expr_width(lhs, fallback),
            Expr::Binary { lhs, rhs, .. } => self
                .meta_expr_width(lhs, fallback)
                .max(self.meta_expr_width(rhs, fallback)),
            _ => fallback,
        }
    }

    /// The metavalue companion of a per-element logical op (`std_logic_1164`),
    /// unrolled per element: a result element is `'X'` when an operand is
    /// a metavalue *and* no operand forces the output — `0 and X = 0`,
    /// `1 or X = 1`, `X xor _ = X`.
    fn logical_meta(&self, op: BinOp, lhs: &Expr, rhs: &Expr, width: u32) -> Option<Expr> {
        let lhs_width = self.meta_expr_width(lhs, width);
        let rhs_width = self.meta_expr_width(rhs, width);
        let (ma, mb) = (
            self.lower_meta_ir(lhs, lhs_width),
            self.lower_meta_ir(rhs, rhs_width),
        );
        if ma.is_none() && mb.is_none() {
            return None;
        }
        let (x_disc, u_disc) = (self.x_disc(), self.u_disc());
        let mut acc = Expr::Const(0);
        for i in 0..width {
            let (am, bm) = (meta_bit(&ma, i), meta_bit(&mb, i));
            let (av, bv) = (bit(lhs, i), bit(rhs, i));
            let anymeta = or_expr(am.clone(), bm.clone());
            // `'U'` dominates: an unforced result with a `'U'` operand is
            // `'U'`, not `'X'`.
            let unresolved = or_expr(u_bit(&ma, i, u_disc), u_bit(&mb, i, u_disc));
            // A "forcing" operand (whose value alone fixes the result) clears
            // the metavalue: `and`'s forcing value is 0, `or`'s is 1, `xor` has
            // none.
            let forced = match op {
                BinOp::And => or_expr(and_expr(not1(am), not1(av)), and_expr(not1(bm), not1(bv))),
                BinOp::Or => or_expr(and_expr(not1(am), av), and_expr(not1(bm), bv)),
                _ => Expr::Const(0), // xor: no forcing
            };
            let meta_i = and_expr(anymeta, not1(forced));
            let disc = Expr::Select {
                cond: Box::new(unresolved),
                then: Box::new(Expr::Const(u_disc)),
                els: Box::new(Expr::Const(x_disc)),
            };
            acc = or_expr(acc, meta_nibble(meta_i, i, disc));
        }
        Some(acc)
    }

    /// Propagate metavalues through operators: drive each vector target's
    /// companion from [`lower_meta_ir`] of its value. Runs after drivers are
    /// lowered.
    fn propagate_metavalues(&mut self) {
        // First discover the complete set of signals that need companions.
        // A newly discovered companion can make a downstream copy discoverable,
        // so this is a fixed point over the finite set of value signals. No
        // companion drivers are added during discovery: that keeps companion
        // expressions terminal and makes the bound explicit.
        loop {
            let companion_ids: std::collections::HashSet<u32> =
                self.out.meta_of.values().copied().collect();
            let mut discovered = Vec::new();
            for d in &self.out.drivers {
                if companion_ids.contains(&d.target.0) || self.out.meta_of.contains_key(&d.target.0)
                {
                    continue;
                }
                let n = self.out.signals[d.target.0 as usize].width;
                if n != 0 && (d.meta.is_some() || self.lower_meta_ir(&d.expr, n).is_some()) {
                    discovered.push(d.target);
                }
            }
            for block in &self.out.event_blocks {
                for update in &block.updates {
                    if companion_ids.contains(&update.target.0)
                        || self.out.meta_of.contains_key(&update.target.0)
                    {
                        continue;
                    }
                    let n = self.out.signals[update.target.0 as usize].width;
                    if n != 0
                        && (update.meta.is_some() || self.lower_meta_ir(&update.expr, n).is_some())
                    {
                        discovered.push(update.target);
                    }
                }
            }
            discovered.sort_by_key(|signal| signal.0);
            discovered.dedup();
            if discovered.is_empty() {
                break;
            }
            for target in discovered {
                self.driven_companion(target);
            }
        }

        // Then emit exactly one companion write beside every write of a signal
        // that has a companion. Clean writes deliberately emit zero: without
        // that write a later clean override changed the value plane while an
        // earlier `X`/`Z` remained stale in the discriminant plane.
        let companion_ids: std::collections::HashSet<u32> =
            self.out.meta_of.values().copied().collect();
        let mut drivers = Vec::with_capacity(self.out.drivers.len() * 2);
        for mut driver in std::mem::take(&mut self.out.drivers) {
            if companion_ids.contains(&driver.target.0) {
                driver.meta = None;
                drivers.push(driver);
                continue;
            }
            let companion = self.out.meta_of.get(&driver.target.0).copied();
            let meta = companion.map(|_| {
                let width = self.out.signals[driver.target.0 as usize].width;
                driver
                    .meta
                    .take()
                    .or_else(|| self.lower_meta_ir(&driver.expr, width))
                    .unwrap_or(Expr::Const(0))
            });
            let cond = driver.cond.clone();
            let ctx = driver.ctx;
            let span = driver.span;
            drivers.push(driver);
            if let (Some(companion), Some(expr)) = (companion, meta) {
                drivers.push(Driver {
                    target: SignalId(companion),
                    cond,
                    expr,
                    meta: None,
                    ctx,
                    span,
                });
            }
        }
        self.out.drivers = drivers;

        for block_index in 0..self.out.event_blocks.len() {
            let mut updates =
                Vec::with_capacity(self.out.event_blocks[block_index].updates.len() * 2);
            for mut update in std::mem::take(&mut self.out.event_blocks[block_index].updates) {
                if companion_ids.contains(&update.target.0) {
                    update.meta = None;
                    updates.push(update);
                    continue;
                }
                let companion = self.out.meta_of.get(&update.target.0).copied();
                let meta = companion.map(|_| {
                    let width = self.out.signals[update.target.0 as usize].width;
                    update
                        .meta
                        .take()
                        .or_else(|| self.lower_meta_ir(&update.expr, width))
                        .unwrap_or(Expr::Const(0))
                });
                let cond = update.cond.clone();
                let span = update.span;
                updates.push(update);
                if let (Some(companion), Some(expr)) = (companion, meta) {
                    updates.push(NextUpdate {
                        target: SignalId(companion),
                        cond,
                        expr,
                        meta: None,
                        span,
                    });
                }
            }
            self.out.event_blocks[block_index].updates = updates;
        }
    }

    /// Rewrite each single-element read of a metavalue vector into its 9-value
    /// reconstruction (companion nibble when a metavalue, else the value bit).
    /// A post-pass so it sees driven companions (created in propagation), not
    /// just init ones.
    fn reconstruct_reads(&mut self) {
        let meta_of = self.out.meta_of.clone();
        // Companion id -> how many elements it describes, so the comparison
        // guard can ask its per-element question without the signal table.
        let elems: HashMap<u32, u32> = meta_of
            .iter()
            .map(|(&base, &companion)| (companion, self.out.signals[base as usize].width))
            .collect();
        for d in &mut self.out.drivers {
            if let Some(c) = &mut d.cond {
                reconstruct_expr(c, &meta_of, &elems);
            }
            reconstruct_expr(&mut d.expr, &meta_of, &elems);
        }
        for b in &mut self.out.event_blocks {
            reconstruct_expr(&mut b.condition, &meta_of, &elems);
            for u in &mut b.updates {
                if let Some(c) = &mut u.cond {
                    reconstruct_expr(c, &meta_of, &elems);
                }
                reconstruct_expr(&mut u.expr, &meta_of, &elems);
            }
        }
    }

    /// Add a signal for `name: ty`, flattening composites into scalar leaves: a
    /// struct into one signal per field (`s.valid`), an array into one per
    /// element (`a[0]`). Nested composites recurse. An integer vector
    /// (`unsigned[8]`) stays a single scalar signal.
    fn add_typed_signal(
        &mut self,
        entity: &str,
        name: &str,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
        declaration_span: crate::diag::Span,
    ) {
        // A generic entity's type parameters (`T -> unsigned[8]`) substitute first,
        // so a port/signal typed `T` becomes its concrete type here.
        let subst_ty;
        let ty = if self.cur_type_env.is_empty() {
            ty
        } else {
            subst_ty = subst_type_params(ty, &self.cur_type_env);
            &subst_ty
        };
        // Substitute `using X = T;` aliases transitively; an index applied to an alias of
        // an unconstrained array fills its hole (`string[5]` = `Char[5]`).
        let resolved;
        let ty = match ty {
            ast::Type::Path(_) => {
                let terminal = self.resolve_alias(ty);
                if std::ptr::eq(terminal, ty) {
                    ty
                } else {
                    resolved = terminal.clone();
                    &resolved
                }
            }
            ast::Type::Indexed {
                base,
                index: Some(i),
                span,
            } => {
                let inner = self.resolve_alias(base);
                match inner {
                    ast::Type::Indexed {
                        base: elem,
                        index: None,
                        ..
                    } => {
                        resolved = ast::Type::Indexed {
                            base: elem.clone(),
                            index: Some(i.clone()),
                            span: *span,
                        };
                        &resolved
                    }
                    _ => ty,
                }
            }
            _ => ty,
        };
        // An unconstrained array (`Char[]`) has no length to flatten with.
        if let ast::Type::Indexed { index: None, .. } = ty {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "unconstrained array type for `{name}`: the range must be set here                      (e.g. an explicit length)"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(declaration_span),
            );
            return;
        }
        let layout = self.source_layout(ty, env);
        self.add_layout_signal(entity, name, &layout, declaration_span);
    }

    /// Persist and flatten one already-resolved layout. This is now the sole
    /// recursive storage traversal; AST declarations are consulted only while
    /// constructing the root `SourceLayout` above.
    fn add_layout_signal(
        &mut self,
        entity: &str,
        name: &str,
        layout: &SourceLayout,
        declaration_span: crate::diag::Span,
    ) {
        self.out
            .source_layouts
            .insert(format!("{entity}.{name}"), layout.clone());
        match &layout.kind {
            LayoutKind::Struct {
                name: representation,
                view,
                fields,
            } => {
                self.local_struct.insert(
                    name.to_string(),
                    view.clone().unwrap_or_else(|| representation.clone()),
                );
                self.local_struct_repr
                    .insert(name.to_string(), representation.clone());
                for field in fields {
                    self.add_layout_signal(
                        entity,
                        &format!("{name}.{}", field.name),
                        &field.layout,
                        declaration_span,
                    );
                }
            }
            LayoutKind::Array { range, element } => {
                let Some(range) = range else {
                    return;
                };
                let indices = loop_range(range.left, range.right);
                self.local_array.insert(name.to_string(), indices.clone());
                for index in indices {
                    self.add_layout_signal(
                        entity,
                        &format!("{name}[{index}]"),
                        element,
                        declaration_span,
                    );
                }
            }
            LayoutKind::Packed {
                width,
                family,
                element_enum,
                ..
            } => {
                self.add_signal(entity, name, *width, declaration_span);
                self.local_numeric.insert(name.to_string(), family.clone());
                if let Some(&id) = self.locals.get(name) {
                    self.sig_type.insert(id.0, family.clone());
                    if let Some(element) = element_enum {
                        self.out.vector_element_enums.insert(id.0, element.clone());
                    }
                }
            }
            LayoutKind::Scalar {
                width,
                domain,
                value_range,
                ..
            } => {
                self.add_signal(entity, name, *width, declaration_span);
                let Some(&id) = self.locals.get(name) else {
                    return;
                };
                let signal = &mut self.out.signals[id.0 as usize];
                signal.range = *value_range;
                match domain {
                    ScalarDomain::Bits => {}
                    ScalarDomain::Integer => signal.integer = true,
                    ScalarDomain::Real => signal.real = true,
                    ScalarDomain::Character => {
                        signal.char = true;
                        self.local_char.insert(name.to_string());
                    }
                    ScalarDomain::Enum(enum_name) => {
                        self.local_enum.insert(name.to_string(), enum_name.clone());
                        self.sig_type.insert(id.0, enum_name.clone());
                        signal.enum_type = Some(enum_name.clone());
                        if let Some(&default) = self
                            .new_defaults
                            .get(enum_name)
                            .or_else(|| self.enum_first_disc.get(enum_name))
                        {
                            signal.init = vec![default];
                        }
                    }
                }
            }
            LayoutKind::Opaque { width, .. } => {
                self.add_signal(entity, name, width.unwrap_or(0), declaration_span);
            }
        }
    }

    /// Build the language-neutral recursive layout persisted on `Design`.
    /// Alias expansion, generic substitution, inherited fields, and concrete
    /// ranges happen here once; consumers must not need the source AST to
    /// recover them again.
    fn source_layout(&self, ty: &ast::Type, env: &HashMap<String, i64>) -> SourceLayout {
        self.source_layout_at(ty, env, &mut HashSet::new())
    }

    fn source_layout_at(
        &self,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
        expanding: &mut HashSet<String>,
    ) -> SourceLayout {
        let ty = self.normalize_layout_type(ty);
        let span = source_type_span(&ty);
        let rendered = crate::syntax::pretty::type_str(&ty);

        if let Some(fields) = self.struct_fields(&ty).filter(|fields| !fields.is_empty()) {
            let recursion_key = rendered.clone();
            if !expanding.insert(recursion_key.clone()) {
                return SourceLayout {
                    span,
                    kind: LayoutKind::Opaque {
                        name: rendered,
                        width: None,
                    },
                };
            }
            let (name, view) = match &ty {
                ast::Type::View { view, target, .. } => (
                    self.free_fns
                        .type_head_key(target)
                        .unwrap_or_else(|| "<anonymous>".to_string()),
                    view.segments.last().map(|name| name.text.clone()),
                ),
                ast::Type::Generic { base, .. } => (
                    self.free_fns
                        .type_head_key(base)
                        .unwrap_or_else(|| "<anonymous>".to_string()),
                    None,
                ),
                _ => (
                    self.free_fns
                        .type_head_key(&ty)
                        .unwrap_or_else(|| "<anonymous>".to_string()),
                    None,
                ),
            };
            let directions = self
                .view_of(&ty)
                .and_then(|view_key| self.view_dirs.get(&view_key));
            let fields = fields
                .into_iter()
                .map(|(name, ty)| {
                    let direction =
                        directions
                            .and_then(|directions| directions.get(&name))
                            .map(|direction| match direction {
                                ast::Direction::In => LayoutDirection::In,
                                ast::Direction::Out => LayoutDirection::Out,
                                ast::Direction::Inout => LayoutDirection::InOut,
                            });
                    LayoutField {
                        name,
                        direction,
                        layout: self.source_layout_at(&ty, env, expanding),
                    }
                })
                .collect();
            expanding.remove(&recursion_key);
            return SourceLayout {
                span,
                kind: LayoutKind::Struct { name, view, fields },
            };
        }

        if let Some((element, indices)) = array_of(
            &ty,
            env,
            &self.const_ranges,
            &self.vector_families,
            &self.free_fns,
        ) {
            let range = indices
                .first()
                .copied()
                .zip(indices.last().copied())
                .map(|(left, right)| LayoutRange { left, right });
            return SourceLayout {
                span,
                kind: LayoutKind::Array {
                    range,
                    element: Box::new(self.source_layout_at(element, env, expanding)),
                },
            };
        }

        if let Some(family) = self.packed_family(&ty, env) {
            let width = type_width(&ty, env, &self.free_fns, &self.structs, &self.const_ranges);
            return SourceLayout {
                span,
                kind: LayoutKind::Packed {
                    width,
                    range: self.layout_range(&ty, env, &mut HashSet::new()),
                    element_enum: self.vector_element_enum(&family),
                    family,
                },
            };
        }

        if let Some((width, enum_name)) = self.enum_representation(&ty) {
            return SourceLayout {
                span,
                kind: LayoutKind::Scalar {
                    width,
                    domain: ScalarDomain::Enum(enum_name.clone()),
                    nominal: Some(enum_name),
                    value_range: None,
                },
            };
        }

        if let Some((width, real, value_range)) = self.ranged_numeric(&ty) {
            return SourceLayout {
                span,
                kind: LayoutKind::Scalar {
                    width,
                    domain: if real {
                        ScalarDomain::Real
                    } else {
                        ScalarDomain::Integer
                    },
                    nominal: None,
                    value_range,
                },
            };
        }

        let width = type_width(&ty, env, &self.free_fns, &self.structs, &self.const_ranges);
        let head = self.free_fns.type_head_key(&ty);
        let domain = match head.as_deref() {
            Some("real") => Some(ScalarDomain::Real),
            Some("integer") => Some(ScalarDomain::Integer),
            Some("Char") => Some(ScalarDomain::Character),
            Some(name) if struct_derives_kernel(name, "real", &self.structs, &self.free_fns) => {
                Some(ScalarDomain::Real)
            }
            Some(name) if struct_derives_kernel(name, "integer", &self.structs, &self.free_fns) => {
                Some(ScalarDomain::Integer)
            }
            Some(name) if struct_derives_kernel(name, "Char", &self.structs, &self.free_fns) => {
                Some(ScalarDomain::Character)
            }
            Some(_) if width != 0 => Some(ScalarDomain::Bits),
            _ => None,
        };
        match domain {
            Some(domain) => SourceLayout {
                span,
                kind: LayoutKind::Scalar {
                    width,
                    domain,
                    nominal: head
                        .filter(|name| !matches!(name.as_str(), "integer" | "real" | "Char")),
                    value_range: None,
                },
            },
            None => SourceLayout {
                span,
                kind: LayoutKind::Opaque {
                    name: rendered,
                    width: (width != 0).then_some(width),
                },
            },
        }
    }

    /// Apply the same concrete alias/type-parameter rules signal lowering uses
    /// before building layout metadata for nested fields.
    fn normalize_layout_type(&self, ty: &ast::Type) -> ast::Type {
        let substituted = if self.cur_type_env.is_empty() {
            ty.clone()
        } else {
            subst_type_params(ty, &self.cur_type_env)
        };
        match &substituted {
            ast::Type::Path(path) if path.segments.len() == 1 => {
                self.resolve_alias(&substituted).clone()
            }
            ast::Type::Indexed {
                base,
                index: Some(index),
                span,
            } => match self.resolve_alias(base) {
                ast::Type::Indexed {
                    base: element,
                    index: None,
                    ..
                } => ast::Type::Indexed {
                    base: element.clone(),
                    index: Some(index.clone()),
                    span: *span,
                },
                _ => substituted,
            },
            _ => substituted,
        }
    }

    fn packed_family(&self, ty: &ast::Type, env: &HashMap<String, i64>) -> Option<String> {
        match ty {
            ast::Type::Indexed { base, .. } => {
                let name = self.free_fns.type_head_key(base)?;
                self.vector_families.contains(&name).then_some(name)
            }
            ast::Type::Path(_) => {
                let name = self.free_fns.type_head_key(ty)?;
                (self.vector_families.contains(&name)
                    && type_width(ty, env, &self.free_fns, &self.structs, &self.const_ranges) != 0)
                    .then_some(name)
            }
            _ => None,
        }
    }

    fn layout_range(
        &self,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
        seen: &mut HashSet<String>,
    ) -> Option<LayoutRange> {
        if let Some((left, right)) = self.declared_range(ty, env) {
            return Some(LayoutRange { left, right });
        }
        let name = self.free_fns.type_head_key(ty)?;
        if !seen.insert(name.clone()) {
            return None;
        }
        let base = self.structs.get(&name)?.base.as_ref()?;
        self.layout_range(base, env, seen)
    }

    fn vector_element_enum(&self, family: &str) -> Option<String> {
        let mut current = family.to_string();
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            let base = self.structs.get(&current)?.base.as_ref()?;
            let element = match base {
                ast::Type::Indexed { base, .. } => base.as_ref(),
                ast::Type::Path(_) => base,
                _ => return None,
            };
            if let ast::Type::Path(path) = element {
                if let Some(key) = self.free_fns.enum_path_key(path) {
                    if self.enum_reprs.contains_key(&key) {
                        return Some(key);
                    }
                }
            }
            let head = self.free_fns.type_head_key(element)?;
            if self.enum_reprs.contains_key(&head) {
                return Some(head);
            }
            current = head;
        }
        None
    }

    /// The storage width of a value-range-constrained numeric type
    /// (`integer<left..right>` / `real<left..right>`), if `ty` is one. Returns
    /// `(width, is_real)`.
    fn ranged_numeric(&self, ty: &ast::Type) -> Option<NumericRangeInfo> {
        let ast::Type::Generic { base, args, .. } = ty else {
            return None;
        };
        let ast::Type::Path(p) = base.as_ref() else {
            return None;
        };
        let kind = p.segments.last().map(|s| s.text.as_str())?;
        if kind != "integer" && kind != "real" {
            return None;
        }
        let [ast::GenericArg::Positional(arg)] = args.as_slice() else {
            return None;
        };
        if kind == "real" {
            return Some((64, true, None)); // range is a constraint, storage is f64
        }
        let (a, b) = match arg {
            ast::Expr::Range { lo, hi, .. } => (
                self.eval_const(lo, &self.cur_env)?,
                self.eval_const(hi, &self.cur_env)?,
            ),
            ast::Expr::Path(path) => self
                .free_fns
                .constant_path_key(path)
                .and_then(|key| self.const_ranges.get(&key).copied())?,
            _ => return None,
        };
        let (lo, hi) = (a.min(b), a.max(b));
        // Smallest width whose (signed, when lo < 0) domain covers [lo, hi].
        for w in 1..=64u32 {
            let fits = if lo < 0 {
                let half = 1i128 << (w - 1);
                (lo as i128) >= -half && (hi as i128) < half
            } else {
                (hi as i128) < (1i128 << w.min(63)) || w >= 64
            };
            if fits {
                return Some((w, false, Some((lo, hi))));
            }
        }
        Some((64, false, None))
    }

    /// The bit width of `ty` if it names a known enum or a fieldless nominal
    /// type whose representation ultimately derives from one.
    fn enum_representation(&self, ty: &ast::Type) -> Option<(u32, String)> {
        let ast::Type::Path(_) = ty else {
            return None;
        };
        let mut name = self.free_fns.type_head_key(ty)?;
        let mut seen = HashSet::new();
        while seen.insert(name.clone()) {
            if let Some(width) = self.enum_reprs.get(&name) {
                return Some((*width, name));
            }
            let declaration = self.structs.get(&name)?;
            if !declaration.fields.is_empty() {
                return None;
            }
            let base = declaration.base.as_ref()?;
            // A newtype over an *array* is a derived vector, not an enum
            // wearing a new name: `struct Byte(unsigned[8])` is eight bits.
            // Walking on reached `unsigned` -> `Logic[]` -> `Logic` and took
            // the element's four, so every `Byte` signal silently truncated.
            if matches!(base, ast::Type::Indexed { .. }) {
                return None;
            }
            name = self.free_fns.type_head_key(base)?;
        }
        None
    }

    /// The `(field name, field type)` list if `ty` names a known struct —
    /// resolving generic applications (`Pair<unsigned[8]>`) and bus-mode views
    /// (`Stream::Source`, `Stream<unsigned[8]>::Source`, spec 3.19).
    /// Normalize an instance's connection args into `(port, value)` pairs:
    /// positional args (`Inv { a, b }`) bind by the sub-entity's port order,
    /// explicit args (`.clk = clk`) bind by name. Every arg carries a value, so
    /// downstream sites don't special-case the connection shape.
    fn norm_conns(&self, conns: &[ast::ConnectArg], entity_id: DefId) -> Vec<(String, ast::Expr)> {
        let order: Vec<String> = self
            .entities
            .get(&entity_id)
            .map(|d| d.ports.iter().map(|p| p.name.text.clone()).collect())
            .unwrap_or_default();
        conns
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let port = match &c.field {
                    Some(f) => f.text.clone(),
                    None => order.get(i).cloned()?,
                };
                // The connected value is read against the *port's* declared
                // type, so a positional literal on a struct port
                // (`.p = { 7, 8 }`) is a struct literal, not the concat it
                // lexes as. It connected nothing and the child read zeros.
                let port_ty = self
                    .entities
                    .get(&entity_id)
                    .and_then(|d| d.ports.iter().find(|p| p.name.text == port))
                    .map(|p| p.ty.clone());
                let value = self.as_struct_literal(port_ty.as_ref(), &c.value.clone()?);
                Some((port, value))
            })
            .collect()
    }

    fn struct_fields(&self, ty: &ast::Type) -> Option<Vec<(String, ast::Type)>> {
        match ty {
            // A generic application: substitute the type parameters into the
            // base struct's field types.
            ast::Type::Generic { base, args, .. } => {
                let sname = self.free_fns.type_head_key(base)?;
                let s = self.structs.get(&sname)?;
                let mut subst: HashMap<String, ast::Type> = HashMap::new();
                for (index, arg) in args.iter().enumerate() {
                    let Some(ty) = (match arg {
                        ast::GenericArg::Positional(e) => expr_to_type(e),
                        ast::GenericArg::PositionalType(ty) => Some(ty.clone()),
                        ast::GenericArg::Named { value, .. } => expr_to_type(value),
                        ast::GenericArg::NamedType { ty, .. } => Some(ty.clone()),
                    }) else {
                        continue;
                    };
                    let parameter = match arg {
                        ast::GenericArg::Named { name, .. }
                        | ast::GenericArg::NamedType { name, .. } => name.text.clone(),
                        _ => s
                            .params
                            .params
                            .get(index)
                            .map(|parameter| parameter.name.text.clone())
                            .unwrap_or_default(),
                    };
                    if !parameter.is_empty() {
                        subst.insert(parameter, ty);
                    }
                }
                let fields = self.raw_struct_fields(&sname)?;
                Some(
                    fields
                        .into_iter()
                        .map(|(n, ft)| (n, subst_type_params(&ft, &subst)))
                        .collect(),
                )
            }
            // An applied view reuses the backing struct's representation.
            ast::Type::View { target, .. } => self.struct_fields(target),
            ast::Type::Path(p) => self
                .free_fns
                .struct_path_key(p)
                .and_then(|name| self.raw_struct_fields(&name)),
            _ => None,
        }
    }

    /// The base-first field list of a struct named directly (no generics/mode).
    /// Every leaf of a struct as a dotted path (`a.x`, `a.y`, `z`), descending
    /// through fields that are themselves structs. Composition makes a struct
    /// value a tree while the signal table holds only its leaves, so anything
    /// copying a whole struct value has to work in these terms.
    fn struct_leaf_names(&self, name: &str) -> Vec<String> {
        self.struct_leaf_names_at(name, &mut HashSet::new())
    }

    fn struct_leaf_names_at(&self, name: &str, seen: &mut HashSet<String>) -> Vec<String> {
        if !seen.insert(name.to_string()) {
            return Vec::new();
        }
        let Some(fields) = self.raw_struct_fields(name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (f, ty) in fields {
            let nested = self
                .free_fns
                .type_head_key(&ty)
                .map(|h| self.struct_leaf_names_at(&h, seen))
                .unwrap_or_default();
            if nested.is_empty() {
                out.push(f);
            } else {
                out.extend(nested.into_iter().map(|n| format!("{f}.{n}")));
            }
        }
        seen.remove(name);
        out
    }

    fn raw_struct_fields(&self, name: &str) -> Option<Vec<(String, ast::Type)>> {
        let s = self.structs.get(name)?;
        // Derived struct: inherited base fields come first (spec: derivation).
        // A cyclic derivation is reported by resolve, but lowering still runs
        // (best-effort) — and `struct_fields` calls straight back here, so
        // without this guard the pair recursed until the stack overflowed and
        // the process aborted.
        let mut fields = match &s.base {
            Some(_) if !self.expanding_structs.borrow_mut().insert(name.to_string()) => Vec::new(),
            Some(b) => {
                let inherited = self.struct_fields(b).unwrap_or_default();
                self.expanding_structs.borrow_mut().remove(name);
                inherited
            }
            None => Vec::new(),
        };
        fields.extend(s.fields.iter().map(|f| (f.name.text.clone(), f.ty.clone())));
        Some(fields)
    }

    fn view_of(&self, ty: &ast::Type) -> Option<String> {
        if let ast::Type::View { view, target, .. } = ty {
            return Some(format!(
                "{}@{}",
                view.segments.last()?.text,
                self.free_fns.type_head_key(target)?
            ));
        }
        let head = self.free_fns.type_head_key(ty)?;
        self.views.contains_key(&head).then_some(head)
    }

    /// Warn when specialization makes two unconditional assignments target
    /// the same concrete signal. The type checker handles direct source
    /// sequences, but cannot know a generic entity's parameter values or
    /// substitute generate-loop variables. Do that here, beside the actual
    /// unroller, and emit only when at least one assignment was generated so
    /// ordinary source warnings are not duplicated.
    fn lint_generated_dead_assignments<'s>(
        &mut self,
        statements: impl Iterator<Item = &'s ast::Stmt>,
    ) {
        let mut seen = HashMap::new();
        for statement in statements {
            self.lint_generated_dead_statement(statement, false, &mut seen);
        }
    }

    fn lint_generated_dead_statement(
        &mut self,
        statement: &ast::Stmt,
        generated: bool,
        seen: &mut HashMap<String, (crate::diag::Span, bool)>,
    ) {
        match statement {
            ast::Stmt::Assign { target, span, .. } => {
                let key = crate::syntax::pretty::expr_string(target);
                if let Some((previous, previous_generated)) =
                    seen.insert(key.clone(), (*span, generated))
                {
                    let frontend_reported = self.sink.diagnostics().iter().any(|diagnostic| {
                        diagnostic.code == Some(crate::diag::codes::DEAD_ASSIGNMENT)
                            && diagnostic.primary == Some(*span)
                            && diagnostic.labels.iter().any(|label| label.span == previous)
                    });
                    if !frontend_reported
                        && (generated || previous_generated)
                        && self.reported_generated_dead_assignments.insert((
                            previous,
                            *span,
                            key.clone(),
                        ))
                    {
                        self.sink.emit(
                            crate::diag::Diagnostic::warning(format!(
                                "`{key}` is assigned again here; the earlier assignment has no effect"
                            ))
                            .with_code(crate::diag::codes::DEAD_ASSIGNMENT)
                            .at(*span)
                            .label(previous, "this generated assignment is overridden")
                            .help(
                                "remove the overlapping generated assignment, or make one of them conditional",
                            ),
                        );
                    }
                }
            }
            ast::Stmt::For {
                var,
                range: ast::Expr::Range { lo, hi, .. },
                body,
                ..
            } => {
                let (Some(left), Some(right)) = (
                    self.eval_const(lo, &self.cur_env),
                    self.eval_const(hi, &self.cur_env),
                ) else {
                    seen.clear();
                    return;
                };
                for index in loop_range(left, right) {
                    for nested in &body.stmts {
                        let nested = subst_stmt(nested, &var.text, index);
                        self.lint_generated_dead_statement(&nested, true, seen);
                    }
                }
            }
            ast::Stmt::If(conditional) => {
                let Some(selected) = self.eval_const(&conditional.cond, &self.cur_env) else {
                    seen.clear();
                    return;
                };
                if selected != 0 {
                    for nested in &conditional.then.stmts {
                        self.lint_generated_dead_statement(nested, true, seen);
                    }
                } else {
                    match conditional.else_.as_deref() {
                        Some(ast::ElseBranch::Block(block)) => {
                            for nested in &block.stmts {
                                self.lint_generated_dead_statement(nested, true, seen);
                            }
                        }
                        Some(ast::ElseBranch::If(nested)) => self.lint_generated_dead_statement(
                            &ast::Stmt::If(nested.clone()),
                            true,
                            seen,
                        ),
                        None => {}
                    }
                }
            }
            // Any runtime control flow or statement with effects we do not
            // flatten ends the unconditional run, matching the frontend lint.
            _ => seen.clear(),
        }
    }

    /// Lower a top-level (combinational-context) statement. `cond` accumulates
    /// the enclosing combinational conditions.
    fn lower_combinational_block(&mut self, block: &ast::Block, cond: Option<Expr>) {
        self.block_scopes.borrow_mut().push(HashMap::new());
        for statement in &block.stmts {
            self.lower_stmt(statement, cond.clone());
        }
        self.block_scopes.borrow_mut().pop();
    }

    /// Find the innermost block-local binding addressed by a source path.
    /// The suffix is empty for the whole value and is a flattened field name
    /// (`inner.valid`) for a struct leaf.
    fn block_local_path(&self, expression: &ast::Expr) -> Option<(usize, String, String)> {
        let path = self
            .folded_elem_path(expression)
            .or_else(|| expr_path(expression))?;
        let scopes = self.block_scopes.borrow();
        for (scope_index, scope) in scopes.iter().enumerate().rev() {
            for name in scope.keys() {
                let suffix = if path == *name {
                    ""
                } else if let Some(rest) = path.strip_prefix(name) {
                    if rest.starts_with('.') || rest.starts_with('[') {
                        rest
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };
                return Some((
                    scope_index,
                    name.clone(),
                    suffix.strip_prefix('.').unwrap_or(suffix).to_string(),
                ));
            }
        }
        None
    }

    fn block_local_binding(&self, expression: &ast::Expr) -> Option<BlockLocal> {
        let (scope, name, _) = self.block_local_path(expression)?;
        self.block_scopes.borrow().get(scope)?.get(&name).cloned()
    }

    fn block_local_named(&self, name: &str) -> Option<(usize, BlockLocal)> {
        self.block_scopes
            .borrow()
            .iter()
            .enumerate()
            .rev()
            .find_map(|(scope, bindings)| {
                bindings.get(name).cloned().map(|binding| (scope, binding))
            })
    }

    /// The declared type at a local path. Aggregate roots retain their own
    /// type while a selected struct field or array element carries the leaf's
    /// type for width attributes and operator dispatch.
    fn block_local_type(&self, expression: &ast::Expr) -> Option<ast::Type> {
        match expression {
            ast::Expr::Path(_) => self
                .block_local_binding(expression)
                .map(|binding| binding.ty),
            ast::Expr::Field { base, field, .. } => {
                let base_type = self.block_local_type(base)?;
                self.struct_fields(&base_type)?
                    .into_iter()
                    .find(|(name, _)| *name == field.text)
                    .map(|(_, ty)| ty)
            }
            ast::Expr::Index { base, .. } => {
                let base_type = self.block_local_type(base)?;
                array_of(
                    &base_type,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.vector_families,
                    &self.free_fns,
                )
                .map(|(element, _)| element.clone())
            }
            _ => None,
        }
    }

    fn block_local_value(&self, expression: &ast::Expr) -> Option<Val> {
        let (scope, name, suffix) = self.block_local_path(expression)?;
        let binding = self.block_scopes.borrow().get(scope)?.get(&name)?.clone();
        if suffix.is_empty() {
            return Some(binding.value);
        }
        let Val::Fields(fields) = binding.value else {
            return None;
        };
        fields
            .into_iter()
            .find(|(field, _)| *field == suffix)
            .map(|(_, value)| Val::Scalar(value))
    }

    /// Runtime aggregate access into a storage-free block local. Its leaves
    /// use the same flattened suffixes as signals (`[0][1]`, `[0].data`), but
    /// live as expressions inside `Val::Fields` rather than `SignalId`s.
    fn lower_block_dynamic_access(&self, expression: &ast::Expr) -> Option<Expr> {
        let (root, steps) = access_steps(expression)?;
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return None;
        }
        let (_, binding) = self.block_local_named(&root)?;
        match binding.value {
            Val::Scalar(value) => {
                let [AccessStep::Index(index)] = steps.as_slice() else {
                    return None;
                };
                self.lower_block_packed_read(&binding.ty, value, index)
            }
            Val::Fields(fields) => {
                self.lower_block_dynamic_access_from(&binding.ty, "", &steps, &fields)
            }
        }
    }

    fn lower_block_packed_read(
        &self,
        ty: &ast::Type,
        value: Expr,
        index: &ast::Expr,
    ) -> Option<Expr> {
        if matches!(
            index,
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return None;
        }
        let positions = self.block_packed_positions(ty)?;
        if let Some(logical) = self.eval_const(index, &self.cur_env) {
            let physical = positions
                .iter()
                .find_map(|&(label, position)| (label == logical).then_some(position))?;
            return Some(Expr::Slice {
                base: Box::new(value),
                hi: physical,
                lo: physical,
            });
        }
        let lowered_index = self.lower_expr(index);
        let mut result = Expr::Const(0);
        for (logical, physical) in positions.into_iter().rev() {
            result = Expr::Select {
                cond: Box::new(eq(lowered_index.clone(), Expr::Const(logical as u64))),
                then: Box::new(Expr::Slice {
                    base: Box::new(value.clone()),
                    hi: physical,
                    lo: physical,
                }),
                els: Box::new(result),
            };
        }
        Some(result)
    }

    fn lower_block_dynamic_access_from(
        &self,
        ty: &ast::Type,
        prefix: &str,
        steps: &[AccessStep<'_>],
        fields: &[(String, Expr)],
    ) -> Option<Expr> {
        let Some((step, rest)) = steps.split_first() else {
            return fields
                .iter()
                .find(|(name, _)| name == prefix)
                .map(|(_, value)| value.clone());
        };
        match step {
            AccessStep::Field(field) => {
                let field_ty = self
                    .struct_fields(ty)?
                    .into_iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, field_ty)| field_ty)?;
                let separator = if prefix.is_empty() { "" } else { "." };
                self.lower_block_dynamic_access_from(
                    &field_ty,
                    &format!("{prefix}{separator}{field}"),
                    rest,
                    fields,
                )
            }
            AccessStep::Index(index) => {
                if let Some((element_ty, indices)) = array_of(
                    ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.vector_families,
                    &self.free_fns,
                ) {
                    let (&last, earlier) = indices.split_last()?;
                    let lowered_index = self.lower_expr(index);
                    let element = |position: i64| {
                        self.lower_block_dynamic_access_from(
                            element_ty,
                            &format!("{prefix}[{position}]"),
                            rest,
                            fields,
                        )
                    };
                    let mut result = element(last)?;
                    for &position in earlier.iter().rev() {
                        result = Expr::Select {
                            cond: Box::new(eq(lowered_index.clone(), Expr::Const(position as u64))),
                            then: Box::new(element(position)?),
                            els: Box::new(result),
                        };
                    }
                    return Some(result);
                }
                if !rest.is_empty() {
                    return None;
                }
                let value = fields
                    .iter()
                    .find(|(name, _)| name == prefix)
                    .map(|(_, value)| value.clone())?;
                self.lower_block_packed_read(ty, value, index)
            }
        }
    }

    fn block_local_width(&self, ty: &ast::Type) -> u32 {
        self.enum_representation(ty)
            .map(|(width, _)| width)
            .or_else(|| self.ranged_numeric(ty).map(|(width, _, _)| width))
            .unwrap_or_else(|| {
                type_width(
                    ty,
                    &self.cur_env,
                    &self.free_fns,
                    &self.structs,
                    &self.const_ranges,
                )
            })
    }

    fn block_local_default(&self, ty: &ast::Type) -> Val {
        if let Some((element, indices)) = array_of(
            ty,
            &self.cur_env,
            &self.const_ranges,
            &self.vector_families,
            &self.free_fns,
        ) {
            let mut fields = Vec::new();
            for index in indices {
                Self::prefix_block_value(
                    &format!("[{index}]"),
                    self.block_local_default(element),
                    &mut fields,
                );
            }
            return Val::Fields(fields);
        }
        let Some(head) = self.free_fns.type_head_key(ty) else {
            return Val::Scalar(Expr::Const(0));
        };
        if let Some(fields) = self.struct_default_leaves(&head, "") {
            return Val::Fields(fields);
        }
        let value = self
            .new_defaults
            .get(&head)
            .or_else(|| self.enum_first_disc.get(&head))
            .copied()
            .unwrap_or(0);
        Val::Scalar(Expr::Const(value))
    }

    fn prefix_block_value(prefix: &str, value: Val, out: &mut Vec<(String, Expr)>) {
        match value {
            Val::Scalar(value) => out.push((prefix.to_string(), value)),
            Val::Fields(fields) => {
                for (field, value) in fields {
                    let separator = if field.starts_with('[') { "" } else { "." };
                    out.push((format!("{prefix}{separator}{field}"), value));
                }
            }
        }
    }

    /// Lower a value with the declaration's aggregate shape available. The
    /// general expression inliner deliberately has no contextual type, while
    /// an array literal needs exactly that context to name its flattened
    /// elements.
    fn lower_block_value(&self, value: &ast::Expr, ty: &ast::Type) -> Val {
        if let Some((element, indices)) = array_of(
            ty,
            &self.cur_env,
            &self.const_ranges,
            &self.vector_families,
            &self.free_fns,
        ) {
            if let ast::Expr::Array { elems, .. } = value {
                let mut fields = Vec::new();
                for (index, expression) in indices.into_iter().zip(elems) {
                    Self::prefix_block_value(
                        &format!("[{index}]"),
                        self.lower_block_value(expression, element),
                        &mut fields,
                    );
                }
                return Val::Fields(fields);
            }
            if let ast::Expr::StrLit { text, .. } = value {
                let mut fields = Vec::new();
                for (index, character) in indices.into_iter().zip(text.chars()) {
                    let scalar =
                        self.coerce_block_local(element, Val::Scalar(Expr::Logic(character)));
                    Self::prefix_block_value(&format!("[{index}]"), scalar, &mut fields);
                }
                return Val::Fields(fields);
            }
        }
        self.coerce_block_local(ty, self.lower_val_env(value, &HashMap::new()))
    }

    /// Apply the representation boundary a signal store would provide. A
    /// block local has no storage of its own, so width, enum-character and
    /// real coercions must be made explicit in the substituted expression.
    fn coerce_block_local(&self, ty: &ast::Type, value: Val) -> Val {
        let Val::Scalar(mut expression) = value else {
            return value;
        };
        if let Some((_, enum_name)) = self.enum_representation(ty) {
            if let Expr::Logic(character) = expression {
                if let Some(discriminant) = self.char_disc(character, &enum_name) {
                    expression = Expr::Const(discriminant);
                } else {
                    expression = Expr::Logic(character);
                }
            }
        }
        let head = self.free_fns.type_head_key(ty).unwrap_or_default();
        if head == "Char" || struct_derives_kernel(&head, "Char", &self.structs, &self.free_fns) {
            if let Expr::Logic(character) = expression {
                expression = Expr::Const(character as u32 as u64);
            }
        }
        if head == "real" || struct_derives_kernel(&head, "real", &self.structs, &self.free_fns) {
            return Val::Scalar(self.coerce_real(expression));
        }
        let width = self.block_local_width(ty);
        if width > 0 {
            expression = Expr::Slice {
                base: Box::new(expression),
                hi: width - 1,
                lo: 0,
            };
        }
        Val::Scalar(expression)
    }

    fn declare_block_local(&self, declaration: &ast::LetDecl) {
        let Some(ty) = declaration.ty.clone() else {
            return;
        };
        let value = declaration
            .value
            .as_ref()
            .map(|value| self.lower_block_value(value, &ty))
            .unwrap_or_else(|| self.block_local_default(&ty));
        if let Some(scope) = self.block_scopes.borrow_mut().last_mut() {
            scope.insert(declaration.name.text.clone(), BlockLocal { value, ty });
        }
    }

    /// An assignment to a block-local value is an immediate expression
    /// rewrite, not a driver or next-state update. Return whether the target
    /// named such a local so signal lowering does not see it too.
    fn assign_block_local(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> bool {
        if self.assign_block_dynamic_access(target, value, cond) {
            return true;
        }
        // A packed local vector is one scalar expression. Its indexed writes
        // are immediate read-modify-writes over that expression, unlike an
        // array local whose elements live in `Val::Fields` below.
        if let ast::Expr::Index { base, index, .. } = target {
            if let Some((scope_index, name, suffix)) = self.block_local_path(base) {
                if suffix.is_empty() {
                    let previous = {
                        let scopes = self.block_scopes.borrow();
                        scopes
                            .get(scope_index)
                            .and_then(|scope| scope.get(&name))
                            .cloned()
                    };
                    if let Some(previous) = previous {
                        if let (Val::Scalar(old), Some((left, right))) = (
                            previous.value.clone(),
                            self.storage_slice_bounds(base, index),
                        ) {
                            let next = self.merge_slice(
                                old.clone(),
                                left.max(right),
                                left.min(right),
                                self.lower_expr(value),
                                self.block_local_width(&previous.ty),
                            );
                            let next = match cond {
                                Some(condition) => Expr::Select {
                                    cond: Box::new(condition.clone()),
                                    then: Box::new(next),
                                    els: Box::new(old),
                                },
                                None => next,
                            };
                            if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index)
                            {
                                scope.insert(
                                    name,
                                    BlockLocal {
                                        value: Val::Scalar(next),
                                        ty: previous.ty,
                                    },
                                );
                            }
                            return true;
                        }
                        if let Val::Scalar(old) = previous.value.clone() {
                            if !matches!(
                                index.as_ref(),
                                ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                            ) && self.eval_const(index, &self.cur_env).is_none()
                            {
                                if let Some(positions) = self.block_packed_positions(&previous.ty) {
                                    let lowered_index = self.lower_expr(index);
                                    let replacement = self.lower_expr(value);
                                    let width = self.block_local_width(&previous.ty);
                                    let mut next = old.clone();
                                    for (logical, physical) in positions.into_iter().rev() {
                                        let fire = and(
                                            cond.clone(),
                                            eq(lowered_index.clone(), Expr::Const(logical as u64)),
                                        );
                                        next = Expr::Select {
                                            cond: Box::new(fire),
                                            then: Box::new(self.merge_slice(
                                                old.clone(),
                                                physical,
                                                physical,
                                                replacement.clone(),
                                                width,
                                            )),
                                            els: Box::new(next),
                                        };
                                    }
                                    if let Some(scope) =
                                        self.block_scopes.borrow_mut().get_mut(scope_index)
                                    {
                                        scope.insert(
                                            name,
                                            BlockLocal {
                                                value: Val::Scalar(next),
                                                ty: previous.ty,
                                            },
                                        );
                                    }
                                    return true;
                                }
                            }
                        }
                        if let (Val::Fields(mut fields), Some((element_ty, indices))) = (
                            previous.value.clone(),
                            array_of(
                                &previous.ty,
                                &self.cur_env,
                                &self.const_ranges,
                                &self.vector_families,
                                &self.free_fns,
                            ),
                        ) {
                            let new_element = self.lower_block_value(value, element_ty);
                            let lowered_index = self.lower_expr(index);
                            for position in indices {
                                let prefix = format!("[{position}]");
                                let hit = Expr::Binary {
                                    op: BinOp::Eq,
                                    lhs: Box::new(lowered_index.clone()),
                                    rhs: Box::new(Expr::Const(position as u64)),
                                };
                                let fire = and(cond.clone(), hit);
                                for (field, old) in &mut fields {
                                    let suffix = if *field == prefix {
                                        Some("")
                                    } else {
                                        field
                                            .strip_prefix(&prefix)
                                            .and_then(|rest| rest.strip_prefix('.'))
                                    };
                                    let Some(suffix) = suffix else {
                                        continue;
                                    };
                                    let replacement = match &new_element {
                                        Val::Scalar(value) if suffix.is_empty() => {
                                            Some(value.clone())
                                        }
                                        Val::Fields(values) => values
                                            .iter()
                                            .find(|(name, _)| *name == suffix)
                                            .map(|(_, value)| value.clone()),
                                        _ => None,
                                    };
                                    if let Some(replacement) = replacement {
                                        *old = Expr::Select {
                                            cond: Box::new(fire.clone()),
                                            then: Box::new(replacement),
                                            els: Box::new(old.clone()),
                                        };
                                    }
                                }
                            }
                            if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index)
                            {
                                scope.insert(
                                    name,
                                    BlockLocal {
                                        value: Val::Fields(fields),
                                        ty: previous.ty,
                                    },
                                );
                            }
                            return true;
                        }
                    }
                }
            }
        }
        let Some((scope_index, name, suffix)) = self.block_local_path(target) else {
            return false;
        };
        let Some(previous) = self
            .block_scopes
            .borrow()
            .get(scope_index)
            .and_then(|scope| scope.get(&name))
            .cloned()
        else {
            return false;
        };

        let next = if suffix.is_empty() {
            self.lower_block_value(value, &previous.ty)
        } else {
            let Val::Fields(mut fields) = previous.value.clone() else {
                // A suffix on a scalar that was not recognized as a packed
                // bit/slice above has no place representation. Do not
                // misclassify it as a signal write.
                self.unsupported_exprs.borrow_mut().push((
                    crate::syntax::pretty::expr_string(target),
                    ast::expr_span(target),
                ));
                return true;
            };
            let Some((_, old)) = fields.iter_mut().find(|(field, _)| *field == suffix) else {
                self.unsupported_exprs.borrow_mut().push((
                    crate::syntax::pretty::expr_string(target),
                    ast::expr_span(target),
                ));
                return true;
            };
            let new = self
                .block_local_type(target)
                .map(|ty| self.lower_block_value(value, &ty))
                .and_then(|value| match value {
                    Val::Scalar(value) => Some(value),
                    Val::Fields(_) => None,
                })
                .unwrap_or_else(|| self.lower_expr(value));
            *old = match cond {
                Some(condition) => Expr::Select {
                    cond: Box::new(condition.clone()),
                    then: Box::new(new),
                    els: Box::new(old.clone()),
                },
                None => new,
            };
            Val::Fields(fields)
        };
        let next = if suffix.is_empty() {
            match cond {
                Some(condition) => select_val(condition.clone(), next, previous.value),
                None => next,
            }
        } else {
            next
        };
        if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index) {
            scope.insert(
                name,
                BlockLocal {
                    value: next,
                    ty: previous.ty,
                },
            );
        }
        true
    }

    /// Immediate assignment through one or more runtime indices of a block
    /// local. Each possible flattened target leaf becomes a `Select` guarded
    /// by the conjunction of its index matches.
    fn assign_block_dynamic_access(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> bool {
        let Some((root, steps)) = access_steps(target) else {
            return false;
        };
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return false;
        }
        let Some((scope_index, previous)) = self.block_local_named(&root) else {
            return false;
        };
        let Val::Fields(mut fields) = previous.value.clone() else {
            return false;
        };
        let mut targets = Vec::new();
        let Some(target_ty) =
            self.block_dynamic_targets(&previous.ty, "", &steps, None, &mut targets)
        else {
            return false;
        };
        let replacement = self.lower_block_value(value, &target_ty);
        for (prefix, hit) in targets {
            let fire = and(cond.clone(), hit);
            for (field, old) in &mut fields {
                let suffix = if *field == prefix {
                    Some("")
                } else if let Some(rest) = field.strip_prefix(&prefix) {
                    if let Some(rest) = rest.strip_prefix('.') {
                        Some(rest)
                    } else if rest.starts_with('[') {
                        Some(rest)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let Some(suffix) = suffix else {
                    continue;
                };
                let new = match &replacement {
                    Val::Scalar(value) if suffix.is_empty() => Some(value.clone()),
                    Val::Fields(values) => values
                        .iter()
                        .find(|(name, _)| name == suffix)
                        .map(|(_, value)| value.clone()),
                    _ => None,
                };
                if let Some(new) = new {
                    *old = Expr::Select {
                        cond: Box::new(fire.clone()),
                        then: Box::new(new),
                        els: Box::new(old.clone()),
                    };
                }
            }
        }
        if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index) {
            scope.insert(
                root,
                BlockLocal {
                    value: Val::Fields(fields),
                    ty: previous.ty,
                },
            );
        }
        true
    }

    fn block_dynamic_targets(
        &self,
        ty: &ast::Type,
        prefix: &str,
        steps: &[AccessStep<'_>],
        hit: Option<Expr>,
        out: &mut Vec<(String, Expr)>,
    ) -> Option<ast::Type> {
        let Some((step, rest)) = steps.split_first() else {
            out.push((prefix.to_string(), hit.unwrap_or(Expr::Const(1))));
            return Some(ty.clone());
        };
        match step {
            AccessStep::Field(field) => {
                let field_ty = self
                    .struct_fields(ty)?
                    .into_iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, field_ty)| field_ty)?;
                let separator = if prefix.is_empty() { "" } else { "." };
                self.block_dynamic_targets(
                    &field_ty,
                    &format!("{prefix}{separator}{field}"),
                    rest,
                    hit,
                    out,
                )
            }
            AccessStep::Index(index) => {
                let (element_ty, indices) = array_of(
                    ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.vector_families,
                    &self.free_fns,
                )?;
                let lowered_index = self.lower_expr(index);
                let mut target_ty = None;
                for position in indices {
                    let matches = eq(lowered_index.clone(), Expr::Const(position as u64));
                    let found = self.block_dynamic_targets(
                        element_ty,
                        &format!("{prefix}[{position}]"),
                        rest,
                        Some(and(hit.clone(), matches)),
                        out,
                    )?;
                    target_ty.get_or_insert(found);
                }
                target_ty
            }
        }
    }

    /// Lower one statement, with `cur_span` pinned to it for the duration so
    /// the drivers it pushes carry their source line. It is restored on the
    /// way out because `if`/`match` recurse through here: without that, a
    /// driver pushed by the outer statement after an inner one returned would
    /// be attributed to the inner statement's line.
    fn lower_stmt(&mut self, stmt: &ast::Stmt, cond: Option<Expr>) {
        let outer = self.cur_span.replace(ast::stmt_span(stmt));
        self.lower_stmt_at(stmt, cond);
        self.cur_span = outer;
    }

    fn lower_stmt_at(&mut self, stmt: &ast::Stmt, cond: Option<Expr>) {
        // Every index in this statement is as constant as it will ever be: a
        // generate `for` substitutes its variable before re-dispatching here.
        //
        // Only unconditioned statements are walked. A statement arriving with
        // a condition is one branch of an `if` this walk already visited, and
        // visiting it again reports it out of context: the walk applies branch
        // selection, so the dead half of `if i == 0 { s[0] = d } else { s[i] =
        // s[i - 1] }` is skipped at `i = 0`, but on re-entry that `s[i - 1]`
        // arrives as a bare statement with nothing left to skip it.
        if cond.is_none() {
            let mut bad = Vec::new();
            self.collect_stmt_bad_indices(stmt, &mut bad);
            self.report_bad_indices(bad);
        }
        // A sub-instance declared inside a generate block (`for i in .. { let
        // s: Sub = { .. } }`) is lowered structurally by `gather_generate`, so
        // this walk must leave it alone. Only the *assignment* spelling was
        // skipped below, and a connection value with no scalar form -- an
        // element of a struct array, `w[i]` where `w: Beat[N]` -- was then
        // lowered as an ordinary expression and reported "`w[0]` has no
        // hardware form". The same instance written at the entity's root
        // worked, and so did a scalar connection in a loop, which merely
        // lowered to a value nobody used.
        if let ast::Stmt::Let(l) = stmt {
            if instance_let_parts(l, &self.entities, self.resolved).is_some() {
                return;
            }
        }
        // An instance-array element (`stage[i] = Sub { .. }`, Sub an entity) is
        // lowered structurally by `gather_generate`, not as a behavioral driver
        // — skip it so unrolling a `for` doesn't mistake it for an assignment. A
        // struct-construct assignment (`y = Point { .. }`) is real data and
        // flows through normally.
        if let ast::Stmt::Assign {
            value: ast::Expr::Construct { ty: Some(t), .. },
            ..
        } = stmt
        {
            if type_def_id(t, self.resolved).is_some_and(|id| self.entities.contains_key(&id)) {
                return;
            }
        }
        match stmt {
            // `for i in left..right { .. }`: a generate loop — unroll over the static
            // range, substituting the index, so per-iteration drivers (and
            // nested generate-`if`s) are lowered concretely.
            ast::Stmt::For {
                var,
                range: ast::Expr::Range { lo, hi, .. },
                body,
                ..
            } => {
                if let (Some(a), Some(b)) = (
                    self.eval_const(lo, &self.cur_env),
                    self.eval_const(hi, &self.cur_env),
                ) {
                    let saved = self.cur_env.get(&var.text).copied();
                    for i in loop_range(a, b) {
                        self.cur_env.insert(var.text.clone(), i);
                        let unrolled = ast::Block {
                            stmts: body
                                .stmts
                                .iter()
                                .map(|statement| subst_stmt(statement, &var.text, i))
                                .collect(),
                            span: body.span,
                        };
                        self.lower_combinational_block(&unrolled, cond.clone());
                    }
                    match saved {
                        Some(v) => {
                            self.cur_env.insert(var.text.clone(), v);
                        }
                        None => {
                            self.cur_env.remove(&var.text);
                        }
                    }
                }
            }
            ast::Stmt::Assign {
                target,
                value,
                after,
                span,
            } => {
                // `after` delays are testbench stimulus, not synthesizable
                // hardware (Phase 1): reject rather than silently drop.
                if after.is_some() {
                    self.sink.emit(
                        crate::diag::Diagnostic::error(
                            "`after` delays are only allowed in #[test] testbenches (Phase 1)"
                                .to_string(),
                        )
                        .with_code(crate::diag::codes::TYPE_MISMATCH)
                        .at(*span),
                    );
                }
                if self.assign_block_local(target, value, &cond) {
                    return;
                }
                if let ast::Expr::Index { base, index, .. } = target {
                    if expr_path(base)
                        .as_deref()
                        .is_some_and(|path| self.local_struct.contains_key(path))
                    {
                        if let Some(index) = self.index_argument(index) {
                            if self.lower_method_stmt(
                                base,
                                "index_assign",
                                &[index, value.clone()],
                                cond.clone(),
                            ) {
                                return;
                            }
                        }
                    }
                }
                // Strict assignment width: a scalar signal target and a direct
                // signal-reference value must have equal, both-known widths
                // (spec 3.17 — no implicit resize). Arithmetic and conversions
                // are exempt (see `ref_width`); array/struct targets aren't in
                // `locals` so they fall through untouched.
                if let Some(tpath) = expr_path(target) {
                    if let Some(&tid) = self.locals.get(&tpath) {
                        let tw = self.out.signals[tid.0 as usize].width;
                        if let Some(sw) = self.ref_width(value) {
                            // Ranged kernel integers are constraints/subtypes
                            // of one numeric type, not packed-vector families.
                            // Their storage widths may differ across a normal
                            // assignment; range checking (static for constants,
                            // runtime for dynamic values) governs validity.
                            let integer_to_integer = self.out.signals[tid.0 as usize].integer
                                && expr_path(value)
                                    .and_then(|path| self.locals.get(&path))
                                    .is_some_and(|id| self.out.signals[id.0 as usize].integer);
                            if tw > 0 && sw > 0 && tw != sw && !integer_to_integer {
                                self.sink.emit(
                                    crate::diag::Diagnostic::error(format!(
                                        "width mismatch: `{tpath}` is {tw} bits but the \
                                         assigned value is {sw} bits"
                                    ))
                                    .with_code(crate::diag::codes::TYPE_MISMATCH)
                                    .at(*span)
                                    .help(
                                        "widths must match; use a conversion \
                                           (`unsigned[N](x)` / `resize(x, N)`) to change width",
                                    ),
                                );
                            }
                        }
                    }
                }
                // A struct-typed target takes one driver per flattened field
                // (struct copy, struct literal, or an inlined operator impl).
                if let Some(tpath) = expr_path(target) {
                    // Whole-array assignment: a string literal fills a Char
                    // array per element; an array of the same shape copies.
                    if let Some(indices) = self.local_array.get(&tpath).cloned() {
                        if let Some(binding) = self.block_local_binding(value) {
                            if let (Val::Fields(fields), Some((_, source_indices))) = (
                                binding.value,
                                array_of(
                                    &binding.ty,
                                    &self.cur_env,
                                    &self.const_ranges,
                                    &self.vector_families,
                                    &self.free_fns,
                                ),
                            ) {
                                for (target_index, source_index) in
                                    indices.iter().zip(source_indices)
                                {
                                    let source = fields
                                        .iter()
                                        .find(|(name, _)| *name == format!("[{source_index}]"));
                                    let target =
                                        self.locals.get(&format!("{tpath}[{target_index}]"));
                                    if let (Some((_, expression)), Some(&target)) = (source, target)
                                    {
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target,
                                            cond: cond.clone(),
                                            expr: self.coerce_to_target(target, expression.clone()),
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                        }
                        // An array-returning call has no array form of its own —
                        // the inliner's result is a scalar or named fields, and
                        // an array is neither — so `g = gives()` reported that
                        // `gives()` had no element-wise form. Reducing the call
                        // to the expression it returns, with the arguments
                        // substituted, hands it to the arms below: the literal
                        // it returns is driven element by element exactly as a
                        // literal written at the assignment would be.
                        let reduced = self.returned_expr_from_call(value);
                        let value = reduced.as_ref().unwrap_or(value);
                        match value {
                            ast::Expr::StrLit { text, .. } => {
                                let chars: Vec<char> = text.chars().collect();
                                if chars.len() != indices.len() {
                                    self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "string literal length {} does not match `{tpath}` length {}",
                                            chars.len(),
                                            indices.len()
                                        ))
                                        .with_code(crate::diag::codes::TYPE_MISMATCH)
                                        .at(ast::expr_span(value)),
                                    );
                                    return;
                                }
                                for (c, i) in chars.iter().zip(&indices) {
                                    if let Some(&sig) = self.locals.get(&format!("{tpath}[{i}]")) {
                                        // A char-enum element (`Color[3] = "rgb"`)
                                        // takes the variant's discriminant; a
                                        // plain `Char` array takes the code point.
                                        let val = self.out.signals[sig.0 as usize]
                                            .enum_type
                                            .clone()
                                            .and_then(|en| self.char_disc(*c, &en))
                                            .unwrap_or(*c as u32 as u64);
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target: sig,
                                            cond: cond.clone(),
                                            expr: Expr::Const(val),
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                            // `a = [e0, e1, ...];` drives one element per value.
                            ast::Expr::Array { elems, .. } => {
                                if elems.len() != indices.len() {
                                    self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "array literal length {} does not match `{tpath}` length {}",
                                            elems.len(),
                                            indices.len()
                                        ))
                                        .with_code(crate::diag::codes::TYPE_MISMATCH)
                                        .at(ast::expr_span(value)),
                                    );
                                    return;
                                }
                                for (e, i) in elems.iter().zip(&indices) {
                                    if let Some(&sig) = self.locals.get(&format!("{tpath}[{i}]")) {
                                        let expr = self.coerce_to_target(sig, self.lower_expr(e));
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target: sig,
                                            cond: cond.clone(),
                                            expr,
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                            v => {
                                if let Some(vpath) = expr_path(v) {
                                    if let Some(vidx) = self.local_array.get(&vpath).cloned() {
                                        for (ti, vi) in indices.iter().zip(&vidx) {
                                            let t = self.locals.get(&format!("{tpath}[{ti}]"));
                                            let sv = self.locals.get(&format!("{vpath}[{vi}]"));
                                            if let (Some(&t), Some(&sv)) = (t, sv) {
                                                self.out.drivers.push(Driver {
                                                    span: self.cur_span,
                                                    target: t,
                                                    cond: cond.clone(),
                                                    expr: Expr::Current(sv),
                                                    meta: None,
                                                    ctx: self.cur_ctx,
                                                });
                                            }
                                        }
                                        return;
                                    }
                                }
                                // An elementwise operator over arrays
                                // (`y = a and b`, `y = not a`). std declares
                                // these as blanket impls over `T[]`, and
                                // lowering had no form for them: the
                                // assignment fell through to the scalar path,
                                // which reported the *target* as unassignable
                                // even though `y = a` is fine.
                                let mut lowered = Vec::with_capacity(indices.len());
                                for (k, i) in indices.iter().enumerate() {
                                    let element = self.elementwise_at(v, k, indices.len());
                                    let signal = self.locals.get(&format!("{tpath}[{i}]"));
                                    match (element, signal) {
                                        (Some(element), Some(&signal)) => {
                                            let expr = self.coerce_to_target(
                                                signal,
                                                self.lower_expr(&element),
                                            );
                                            lowered.push((signal, expr));
                                        }
                                        // Not elementwise after all; leave the
                                        // existing paths to diagnose it.
                                        _ => {
                                            lowered.clear();
                                            break;
                                        }
                                    }
                                }
                                if !lowered.is_empty() {
                                    for (target, expr) in lowered {
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target,
                                            cond: cond.clone(),
                                            expr,
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                    return;
                                }
                                // `tpath` is a perfectly good target — the
                                // *value* has no array form. Falling through
                                // reached the scalar path, which failed on the
                                // target and reported it as unassignable,
                                // naming the innocent half of the statement.
                                self.sink.emit(
                                    crate::diag::Diagnostic::error(format!(
                                        "`{}` has no element-wise form, so `{tpath}` \
                                         cannot be driven from it",
                                        crate::syntax::pretty::expr_string(v)
                                    ))
                                    .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                                    .at(ast::expr_span(v))
                                    .help(
                                        "an array is driven by another array, an array \
                                         literal, or an element-wise expression over \
                                         arrays of the same length",
                                    ),
                                );
                                return;
                            }
                        }
                    }
                    if self.local_struct.contains_key(&tpath) {
                        // Same expansion the clocked path uses — one write per
                        // leaf. Keeping two copies is how the clocked one came
                        // to be missing in the first place.
                        for (sig, expr) in
                            self.struct_assign_leaves(target, value).unwrap_or_default()
                        {
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: sig,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                                ctx: self.cur_ctx,
                            });
                        }
                        return;
                    }
                }
                if let Some(target) = self.target_signal(target) {
                    let expr = self.coerce_to_target(target, self.lower_expr(value));
                    self.out.drivers.push(Driver {
                        span: self.cur_span,
                        target,
                        cond,
                        expr,
                        meta: self.bit_string_meta(value),
                        ctx: self.cur_ctx,
                    });
                } else if let Some(ups) = self.dynamic_write(target, value, &cond, false, &[]) {
                    for u in ups {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: u.target,
                            cond: u.cond,
                            expr: u.expr,
                            meta: u.meta,
                            ctx: self.cur_ctx,
                        });
                    }
                } else if let Some(ups) = self.dynamic_struct_write(target, value, &cond) {
                    for u in ups {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: u.target,
                            cond: u.cond,
                            expr: u.expr,
                            meta: u.meta,
                            ctx: self.cur_ctx,
                        });
                    }
                } else if let Some((sig, hi, lo)) = self.slice_target(target) {
                    // Partial write: merge over what this context has already
                    // driven (`y = base; y[3..0] = a;`), else over 0.
                    let v = self.lower_expr(value);
                    let width = self.out.signals[sig.0 as usize].width;
                    let base = self.slice_write_base(sig, false, &[]);
                    let merged = self.merge_slice(base, hi, lo, v, width);
                    // `merged` already folds in every driver this context has
                    // for `sig`, so it may *replace* the last one — but only
                    // when that one is unconditional and this write is too.
                    // Otherwise it has to be a new driver, or a guarded write
                    // would be applied unconditionally.
                    let last = self
                        .out
                        .drivers
                        .iter()
                        .rposition(|d| d.target == sig && d.ctx == self.cur_ctx);
                    match last {
                        Some(i) if cond.is_none() && self.out.drivers[i].cond.is_none() => {
                            self.out.drivers[i].expr = merged;
                        }
                        _ => self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: sig,
                            cond,
                            expr: merged,
                            meta: None,
                            ctx: self.cur_ctx,
                        }),
                    }
                } else if let ast::Expr::Concat { parts, span: cspan } = target {
                    // `{hi, lo} = w;` unpacks the value MSB-first: each part
                    // takes its width's slice of the RHS.
                    self.check_concat_target_width(parts, value, *cspan);
                    let v = self.lower_expr(value);
                    let mut off: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
                    for part in parts {
                        let w = self.ast_width(part);
                        let Some(t) = self.target_signal(part) else {
                            self.sink.emit(
                                crate::diag::Diagnostic::error(
                                    "each part of a concat assignment target must be a signal"
                                        .to_string(),
                                )
                                .with_code(crate::diag::codes::INVALID_ASSIGN_TARGET)
                                .at(ast::expr_span(part)),
                            );
                            continue;
                        };
                        let expr = Expr::Slice {
                            base: Box::new(v.clone()),
                            hi: off - 1,
                            lo: off - w,
                        };
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: t,
                            cond: cond.clone(),
                            expr,
                            meta: None,
                            ctx: self.cur_ctx,
                        });
                        off -= w;
                    }
                } else if self.record_unelaborated_instance_use(target) {
                    // The concrete child does not exist, so there is no port
                    // signal to drive. The queued E-P022 names the real cause.
                } else {
                    self.report_bad_assign_target(target);
                }
            }
            ast::Stmt::If(iff) => {
                if expr_is_event(&iff.cond) {
                    // Event-controlled block (spec 3.11): the body's assignments
                    // become next-state updates (spec 3.13).
                    let condition = self.lower_expr(&iff.cond);
                    let mut updates = Vec::new();
                    self.lower_event_block(&iff.then, None, &mut updates);
                    // An `else` on an event block is unusual; lower it under the
                    // negated event for completeness.
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(not(self.lower_expr(&iff.cond)));
                        self.lower_event_else(eb, neg, &mut updates);
                    }
                    self.out.event_blocks.push(EventBlock {
                        condition,
                        updates,
                        ctx: self.cur_ctx,
                    });
                } else if let Some(k) = self.eval_const(&iff.cond, &self.cur_env) {
                    // A generate-if: the condition is a compile-time constant
                    // (a parameter/const), so only the taken branch is lowered.
                    // Its instances were gathered structurally; lowering the
                    // untaken branch too would add a spurious driver that
                    // collides with a conditionally-instantiated block.
                    if k != 0 {
                        self.lower_combinational_block(&iff.then, cond.clone());
                    } else {
                        match iff.else_.as_deref() {
                            Some(ast::ElseBranch::Block(b)) => {
                                self.lower_combinational_block(b, cond.clone());
                            }
                            Some(ast::ElseBranch::If(inner)) => {
                                self.lower_stmt(&ast::Stmt::If(inner.clone()), cond.clone());
                            }
                            None => {}
                        }
                    }
                } else {
                    // A signal assigned on every path through this if/else (a
                    // terminal `else` supplies the complement) is fully covered
                    // — not a latch — even though each driver is conditional.
                    // Mark it like a wildcard match arm so the possible-latch
                    // lint skips it.
                    for id in self.if_covered_targets(iff) {
                        self.lint_defaulted.insert(id);
                    }
                    // Combinational conditional: assignments become conditional
                    // drivers; the `else` adds the negated condition.
                    let c = self.lower_expr(&iff.cond);
                    let then_cond = Some(and(cond.clone(), c.clone()));
                    self.lower_combinational_block(&iff.then, then_cond);
                    if let Some(eb) = iff.else_.as_deref() {
                        let else_cond = Some(and(cond, not(c)));
                        self.lower_combinational_else(eb, else_cond);
                    }
                }
            }
            ast::Stmt::Match(m) => {
                // Combinational match: each arm becomes conditional drivers
                // guarded by `scrutinee == variant` with first-match priority.
                let scrut = self.lower_expr(&m.scrutinee);
                // A match naming every variant of its scrutinee's enum is as
                // complete as one ending in `_`, so a signal every arm assigns
                // is driven on every path and is not a latch. Only the wildcard
                // half was implemented, so the natural spelling of an
                // exhaustive FSM decode drew an inferred-latch warning and the
                // fix people apply to it is a redundant `_` arm. The `if`
                // walker has had the general form all along
                // (`if_covered_targets`).
                for id in self.match_covered_targets(m) {
                    self.lint_defaulted.insert(id);
                }
                let mut remaining = cond;
                for arm in &m.arms {
                    let mc =
                        self.arm_match_cond(&arm.pattern, &m.scrutinee, &scrut, &HashMap::new());
                    // A wildcard arm is the match's default branch: its direct
                    // assignments cover "everything else", so those targets are
                    // not latches even though the lowered driver is conditional.
                    if mc.is_none() {
                        for s in &arm.body.stmts {
                            if let ast::Stmt::Assign { target, .. } = s {
                                if let Some(id) = self.target_signal(target) {
                                    self.lint_defaulted.insert(id.0);
                                }
                            }
                        }
                    }
                    let fire = match &mc {
                        Some(c) => Some(and(remaining.clone(), c.clone())),
                        None => remaining.clone(),
                    };
                    self.lower_combinational_block(&arm.body, fire);
                    remaining = match mc {
                        Some(c) => Some(and(remaining, not(c))),
                        None => Some(Expr::Const(0)),
                    };
                }
            }
            // A method call used as a statement (`s.send(v)`): inline the
            // method body as drivers on the receiver's signals (spec 3.20).
            ast::Stmt::Expr(ast::Expr::Call { callee, args, .. })
                if matches!(callee.as_ref(), ast::Expr::Field { .. }) =>
            {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    self.lower_method_stmt(base, &field.text, args, cond);
                }
            }
            // A free function used as a statement (`write(bus, value)`) may
            // itself contain method calls or assignments. Inline its body with
            // the concrete arguments just like a value-returning free call.
            ast::Stmt::Expr(ast::Expr::Call { callee, args, .. }) => {
                self.lower_free_stmt(callee, args, cond);
            }
            ast::Stmt::Let(declaration) => self.declare_block_local(declaration),
            // Other statement forms (bare expr and return) are not hardware
            // statements; the frontend diagnoses them when applicable.
            _ => {}
        }
    }

    /// The condition under which a match arm fires: `scrut == <variant value>`
    /// for an enum path, `(scrut & mask) == value` for a bit pattern with `?`
    /// don't-cares (spec 3.22), or always (`None`) for a wildcard.
    /// Lower a match-*expression* to a first-match `Select` chain: the wildcard
    /// arm's value is the base `els`, and each earlier arm wraps it under its
    /// `scrutinee == pattern` guard.
    fn lower_match_expr(&self, scrutinee: &ast::Expr, arms: &[ast::MatchArm]) -> Expr {
        let scrut = self.lower_expr(scrutinee);
        // Every match needs a base case. With no `_`, the last arm is it: its
        // guard is redundant when the arms cover the scrutinee, and when they
        // do not this is at least a defined value rather than an `Unknown` no
        // engine can run. The checker warns about the uncovered case.
        //
        // This was computed from enum variants alone, so the exhaustive
        // spelling of a *numeric* match — `0 | 1 => a, 2..3 => b` on
        // `unsigned[2]`, and even `0..3 => a` — lowered to an expression that
        // could not execute, while the same shape over an enum was fine.
        let exhaustive = !arms.iter().any(|a| pattern_has_wildcard(&a.pattern));
        let mut result: Option<Expr> = None;
        for (i, arm) in arms.iter().enumerate().rev() {
            let val = arm
                .value_expr()
                .map(|v| self.lower_expr(v))
                .unwrap_or(Expr::Unknown);
            let last = i + 1 == arms.len();
            match self.arm_match_cond(&arm.pattern, scrutinee, &scrut, &HashMap::new()) {
                None => result = Some(val), // wildcard: the default branch
                Some(_) if exhaustive && last => result = Some(val),
                Some(cond) => {
                    let els = result.take().unwrap_or(Expr::Unknown);
                    result = Some(Expr::Select {
                        cond: Box::new(cond),
                        then: Box::new(val),
                        els: Box::new(els),
                    });
                }
            }
        }
        result.unwrap_or(Expr::Unknown)
    }

    /// [`Self::lower_match_expr`] at [`Val`] level: the same first-match
    /// chain and the same exhaustiveness rule, folded with `select_val` so
    /// struct-valued arms combine field by field.
    fn lower_match_val(
        &self,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
        env: &HashMap<String, Val>,
    ) -> Val {
        let scrut = self.lower_scalar_env(scrutinee, env);
        let exhaustive = !arms.iter().any(|a| pattern_has_wildcard(&a.pattern));
        let mut result: Option<Val> = None;
        for (i, arm) in arms.iter().enumerate().rev() {
            let val = arm
                .value_expr()
                .map(|v| self.lower_val_env(v, env))
                .unwrap_or(Val::Scalar(Expr::Unknown));
            let last = i + 1 == arms.len();
            match self.arm_match_cond(&arm.pattern, scrutinee, &scrut, env) {
                None => result = Some(val),
                Some(_) if exhaustive && last => result = Some(val),
                Some(cond) => {
                    let els = result.take().unwrap_or(Val::Scalar(Expr::Unknown));
                    result = Some(select_val(cond, val, els));
                }
            }
        }
        result.unwrap_or(Val::Scalar(Expr::Unknown))
    }

    fn arm_match_cond(
        &self,
        pattern: &ast::Pattern,
        scrutinee: &ast::Expr,
        scrut: &Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        match pattern {
            ast::Pattern::Path(p) if p.segments.len() >= 2 => {
                let disc = self.enum_variant_path(p).unwrap_or(0);
                Some(eq(scrut.clone(), Expr::Const(disc)))
            }
            ast::Pattern::BitPattern { text, .. } => {
                let (mask, value) = crate::syntax::bit_pattern_mask(text)?;
                Some(eq(
                    Expr::Binary {
                        op: BinOp::And,
                        lhs: Box::new(scrut.clone()),
                        rhs: Box::new(words_const(mask)),
                    },
                    words_const(value),
                ))
            }
            // `A | B`: matches if any alternative matches (their conditions
            // OR-ed; a wildcard alternative makes the whole arm unconditional).
            ast::Pattern::Or { alts, .. } => {
                let mut acc: Option<Expr> = None;
                for a in alts {
                    match self.arm_match_cond(a, scrutinee, scrut, env) {
                        None => return None,
                        Some(c) => {
                            acc = Some(match acc {
                                Some(prev) => Expr::Binary {
                                    op: BinOp::Or,
                                    lhs: Box::new(prev),
                                    rhs: Box::new(c),
                                },
                                None => c,
                            })
                        }
                    }
                }
                acc
            }
            // An integer literal or inclusive range: `scrut == lo`, or
            // `lo <= scrut <= hi`. Reuse ordinary comparison selection so a
            // signed-vector `<=>` implementation, kernel-integer signedness,
            // and real coercion all remain identical to expression syntax.
            ast::Pattern::Range { lo, hi, span } => {
                let (low, high) = if lo <= hi { (*lo, *hi) } else { (*hi, *lo) };
                let compare = |op: ast::BinOp, value: i64| {
                    let magnitude = ast::Expr::Int {
                        text: value.unsigned_abs().to_string(),
                        span: *span,
                    };
                    let rhs = if value < 0 {
                        ast::Expr::Unary {
                            op: ast::UnOp::Neg,
                            rhs: Box::new(magnitude),
                            span: *span,
                        }
                    } else {
                        magnitude
                    };
                    let spelling = crate::syntax::pretty::bin_op(&op);
                    if let Some(derived) = self.inline_cmp(spelling, scrutinee, &rhs, env) {
                        return derived;
                    }
                    self.make_binary(
                        op,
                        scrut.clone(),
                        self.lower_scalar_env(&rhs, env),
                        self.binary_uses_kernel_integer(scrutinee, &rhs),
                        self.declares_kernel_integer(scrutinee),
                    )
                };
                if low == high {
                    Some(compare(ast::BinOp::Eq, low))
                } else {
                    let ge = compare(ast::BinOp::Ge, low);
                    let le = compare(ast::BinOp::Le, high);
                    Some(and(Some(ge), le))
                }
            }
            // A character literal names a variant of a char-valued enum
            // (`Logic` above all). `Expr::Logic` carries the character and is
            // resolved against the scrutinee's type downstream — exactly what
            // `l == '0'` in expression position already lowers to, so the two
            // spellings cannot disagree.
            ast::Pattern::CharLit { ch, .. } => Some(eq(scrut.clone(), Expr::Logic(*ch))),
            // A wildcard matches anything.
            _ => None,
        }
    }

    fn lower_combinational_else(&mut self, eb: &ast::ElseBranch, cond: Option<Expr>) {
        match eb {
            ast::ElseBranch::Block(b) => self.lower_combinational_block(b, cond),
            ast::ElseBranch::If(inner) => {
                self.lower_stmt(&ast::Stmt::If(inner.clone()), cond);
            }
        }
    }

    /// Lower the body of an event-controlled block into next-state updates,
    /// accumulating the priority condition through nested `if`/`else`.
    fn lower_event_block(
        &mut self,
        block: &ast::Block,
        cond: Option<Expr>,
        out: &mut Vec<NextUpdate>,
    ) {
        let outer = self.cur_span;
        self.block_scopes.borrow_mut().push(HashMap::new());
        for s in &block.stmts {
            // Same reason as `lower_stmt`: pin the statement being lowered so
            // its next-state updates can name their source line.
            self.cur_span = Some(ast::stmt_span(s));
            // The clocked path unrolls its own `for` (below) rather than going
            // through `lower_stmt`, so the same check has to be made here —
            // under the same unconditioned-only rule, which on this path is
            // what keeps a clocked generate chain's dead `else` unreported.
            if cond.is_none() {
                let mut bad = Vec::new();
                self.collect_stmt_bad_indices(s, &mut bad);
                self.report_bad_indices(bad);
            }
            match s {
                ast::Stmt::Assign {
                    target,
                    value,
                    after,
                    span,
                } => {
                    if after.is_some() {
                        self.sink.emit(
                            crate::diag::Diagnostic::error(
                                "`after` delays are only allowed in #[test] testbenches (Phase 1)"
                                    .to_string(),
                            )
                            .with_code(crate::diag::codes::TYPE_MISMATCH)
                            .at(*span),
                        );
                    }
                    if self.assign_block_local(target, value, &cond) {
                        continue;
                    }
                    if let Some(leaves) = self.struct_assign_leaves(target, value) {
                        // A registered bus: one next-state update per leaf.
                        for (sig, expr) in leaves {
                            out.push(NextUpdate {
                                span: self.cur_span,
                                target: sig,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                            });
                        }
                    } else if let Some(leaves) = self.array_assign_leaves(target, value) {
                        // A registered array: one next-state update per element.
                        for (sig, expr) in leaves {
                            out.push(NextUpdate {
                                span: self.cur_span,
                                target: sig,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                            });
                        }
                    } else if let Some(target) = self.target_signal(target) {
                        let expr = self.lower_expr(value);
                        out.push(NextUpdate {
                            span: self.cur_span,
                            target,
                            cond: cond.clone(),
                            expr,
                            meta: self.bit_string_meta(value),
                        });
                    } else if let Some(ups) = self.dynamic_write(target, value, &cond, true, out) {
                        out.extend(ups);
                    } else if let Some(ups) = self.dynamic_struct_write(target, value, &cond) {
                        out.extend(ups);
                    } else if let Some((sig, hi, lo)) = self.slice_target(target) {
                        // Register bit-field update: next(y) holds the other
                        // bits (read-modify-write on the value this block has
                        // produced so far, which is `Current` until something
                        // in it writes the signal).
                        let v = self.lower_expr(value);
                        let width = self.out.signals[sig.0 as usize].width;
                        let base = self.slice_write_base(sig, true, out);
                        let expr = self.merge_slice(base, hi, lo, v, width);
                        out.push(NextUpdate {
                            span: self.cur_span,
                            target: sig,
                            cond: cond.clone(),
                            expr,
                            meta: None,
                        });
                    } else if let ast::Expr::Concat { parts, span: cspan } = target {
                        // `{hi, lo} = w;` in a clocked block: each part takes
                        // its width's slice of the RHS, MSB-first.
                        self.check_concat_target_width(parts, value, *cspan);
                        let v = self.lower_expr(value);
                        let mut off: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
                        for part in parts {
                            let w = self.ast_width(part);
                            let Some(t) = self.target_signal(part) else {
                                self.sink.emit(
                                    crate::diag::Diagnostic::error(
                                        "each part of a concat assignment target must be a signal"
                                            .to_string(),
                                    )
                                    .with_code(crate::diag::codes::INVALID_ASSIGN_TARGET)
                                    .at(ast::expr_span(part)),
                                );
                                continue;
                            };
                            let expr = Expr::Slice {
                                base: Box::new(v.clone()),
                                hi: off - 1,
                                lo: off - w,
                            };
                            out.push(NextUpdate {
                                span: self.cur_span,
                                target: t,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                            });
                            off -= w;
                        }
                    } else if self.record_unelaborated_instance_use(target) {
                        // As above, suppress the generic assign-target error;
                        // E-P022 identifies the absent concrete child.
                    } else {
                        self.report_bad_assign_target(target);
                    }
                }
                ast::Stmt::If(iff) => {
                    let c = self.lower_expr(&iff.cond);
                    self.lower_event_block(&iff.then, Some(and(cond.clone(), c.clone())), out);
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(and(cond.clone(), not(c)));
                        self.lower_event_else(eb, neg, out);
                    }
                }
                ast::Stmt::Match(m) => {
                    let scrut = self.lower_expr(&m.scrutinee);
                    let mut remaining = cond.clone();
                    for arm in &m.arms {
                        let mc = self.arm_match_cond(
                            &arm.pattern,
                            &m.scrutinee,
                            &scrut,
                            &HashMap::new(),
                        );
                        let fire = match &mc {
                            Some(c) => Some(and(remaining.clone(), c.clone())),
                            None => remaining.clone(),
                        };
                        self.lower_event_block(&arm.body, fire, out);
                        remaining = match mc {
                            Some(c) => Some(and(remaining, not(c))),
                            None => Some(Expr::Const(0)),
                        };
                    }
                }
                // A call in statement position — `a.bump(step)`, or a free
                // `write(bus, v)` — inlines its body under the edge condition,
                // so its assignments become register updates. The
                // combinational walker has always inlined these; the
                // sequential one dropped them, so `if clk.rising() {
                // a.bump(step); }` left the register at its reset value with
                // no diagnostic. The body itself is shared, so a call means
                // the same thing in both positions.
                ast::Stmt::Expr(ast::Expr::Call {
                    callee, args, span, ..
                }) => {
                    let inlined = match callee.as_ref() {
                        ast::Expr::Field { base, field, .. } => {
                            let (base, field) = (base.clone(), field.text.clone());
                            self.method_stmt_body(&base, &field, args)
                        }
                        _ => self.free_stmt_body(callee, args),
                    };
                    if let Some(stmts) = inlined {
                        let block = ast::Block { stmts, span: *span };
                        self.lower_event_block(&block, cond.clone(), out);
                    }
                }
                // A generate `for` unrolls here exactly as it does in
                // combinational position (spec: a loop unrolls over a static
                // range, "instances *and* per-iteration drivers"). The
                // combinational walker has always done this; the sequential
                // one fell through its catch-all, so every driver a loop
                // wrote inside a clocked block was dropped in silence —
                // `if clk.rising() { for i in 0..2 { v = i; } }` left `v`
                // at its initial value with no diagnostic.
                ast::Stmt::For {
                    var,
                    range: ast::Expr::Range { lo, hi, .. },
                    body,
                    ..
                } => {
                    let (Some(a), Some(b)) = (
                        self.eval_const(lo, &self.cur_env),
                        self.eval_const(hi, &self.cur_env),
                    ) else {
                        continue;
                    };
                    let saved = self.cur_env.get(&var.text).copied();
                    for i in loop_range(a, b) {
                        self.cur_env.insert(var.text.clone(), i);
                        let unrolled = ast::Block {
                            stmts: body
                                .stmts
                                .iter()
                                .map(|st| subst_stmt(st, &var.text, i))
                                .collect(),
                            span: body.span,
                        };
                        self.lower_event_block(&unrolled, cond.clone(), out);
                    }
                    match saved {
                        Some(v) => {
                            self.cur_env.insert(var.text.clone(), v);
                        }
                        None => {
                            self.cur_env.remove(&var.text);
                        }
                    }
                }
                ast::Stmt::Let(declaration) => self.declare_block_local(declaration),
                _ => {}
            }
        }
        self.block_scopes.borrow_mut().pop();
        self.cur_span = outer;
    }

    fn lower_event_else(
        &mut self,
        eb: &ast::ElseBranch,
        cond: Option<Expr>,
        out: &mut Vec<NextUpdate>,
    ) {
        match eb {
            ast::ElseBranch::Block(b) => self.lower_event_block(b, cond, out),
            ast::ElseBranch::If(inner) => {
                let c = self.lower_expr(&inner.cond);
                self.lower_event_block(&inner.then, Some(and(cond.clone(), c.clone())), out);
                if let Some(eb) = inner.else_.as_deref() {
                    self.lower_event_else(eb, Some(and(cond, not(c))), out);
                }
            }
        }
    }

    /// Lower a scalar leaf reached through one or more runtime array indices.
    /// Arrays and structs are flattened (`m[0][1]`, `pack[0].data`), so each
    /// index expands over the concrete paths registered in `local_array` and
    /// each field simply extends the path. Like the original one-dimensional
    /// mux, an out-of-range read selects the last element at that dimension.
    fn lower_dynamic_access(&self, access: &ast::Expr) -> Option<Expr> {
        let (root, steps) = access_steps(access)?;
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return None;
        }
        self.lower_dynamic_access_from(&root, &steps)
    }

    fn lower_dynamic_access_from(&self, path: &str, steps: &[AccessStep<'_>]) -> Option<Expr> {
        let Some((step, rest)) = steps.split_first() else {
            return self.locals.get(path).copied().map(Expr::Current);
        };
        match step {
            AccessStep::Field(field) => {
                self.lower_dynamic_access_from(&format!("{path}.{field}"), rest)
            }
            AccessStep::Index(index) => {
                if let Some(indices) = self.local_array.get(path) {
                    let (&last, earlier) = indices.split_last()?;
                    let lowered_index = self.lower_expr(index);
                    let element = |position: i64| {
                        self.lower_dynamic_access_from(&format!("{path}[{position}]"), rest)
                    };
                    let mut result = element(last)?;
                    for &position in earlier.iter().rev() {
                        result = Expr::Select {
                            cond: Box::new(Expr::Binary {
                                op: BinOp::Eq,
                                lhs: Box::new(lowered_index.clone()),
                                rhs: Box::new(Expr::Const(position as u64)),
                            }),
                            then: Box::new(element(position)?),
                            els: Box::new(result),
                        };
                    }
                    return Some(result);
                }
                if !rest.is_empty()
                    || matches!(
                        index,
                        ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                    )
                {
                    return None;
                }
                let signal = *self.locals.get(path)?;
                let positions = self.packed_positions(path)?;
                if let Some(logical) = self.eval_const(index, &self.cur_env) {
                    let physical = positions
                        .iter()
                        .find_map(|&(label, position)| (label == logical).then_some(position))?;
                    return Some(Expr::Slice {
                        base: Box::new(Expr::Current(signal)),
                        hi: physical,
                        lo: physical,
                    });
                }
                let lowered_index = self.lower_expr(index);
                let mut result = Expr::Const(0);
                for (logical, physical) in positions.into_iter().rev() {
                    result = Expr::Select {
                        cond: Box::new(eq(lowered_index.clone(), Expr::Const(logical as u64))),
                        then: Box::new(Expr::Slice {
                            base: Box::new(Expr::Current(signal)),
                            hi: physical,
                            lo: physical,
                        }),
                        els: Box::new(result),
                    };
                }
                Some(result)
            }
        }
    }

    /// A dynamic aggregate write (`mem[addr] = v`, `m[row][col] = v`, or
    /// `pack[slot].data = v`): enumerate every concrete scalar leaf and gate it
    /// by all runtime index comparisons. An out-of-range write matches no leaf.
    fn dynamic_write(
        &mut self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
        sequential: bool,
        pending: &[NextUpdate],
    ) -> Option<Vec<NextUpdate>> {
        let (root, steps) = access_steps(target)?;
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return None;
        }
        let mut targets = Vec::new();
        self.dynamic_write_targets(&root, &steps, None, &mut targets)?;
        let expr = self.lower_expr(value);
        let mut updates = Vec::new();
        for target in targets {
            match target {
                DynamicWriteTarget::Whole { signal, hit } => updates.push(NextUpdate {
                    span: self.cur_span,
                    target: signal,
                    cond: write_guard(cond, hit),
                    expr: self.coerce_to_target(signal, expr.clone()),
                    meta: None,
                }),
                DynamicWriteTarget::PackedBit {
                    signal,
                    position,
                    hit,
                } => {
                    let width = self.out.signals[signal.0 as usize].width;
                    let base = self.slice_write_base(signal, sequential, pending);
                    let meta = if self.out.vector_element_enums.contains_key(&signal.0) {
                        let companion = SignalId(self.driven_companion(signal));
                        let meta_width = self.out.signals[companion.0 as usize].width;
                        let meta_base =
                            self.slice_meta_write_base(signal, companion, sequential, pending);
                        let meta_value = Expr::Select {
                            cond: Box::new(Expr::Binary {
                                op: BinOp::Ge,
                                lhs: Box::new(expr.clone()),
                                rhs: Box::new(Expr::Const(2)),
                            }),
                            then: Box::new(expr.clone()),
                            els: Box::new(Expr::Const(0)),
                        };
                        Some(self.merge_slice(
                            meta_base,
                            position * 4 + 3,
                            position * 4,
                            meta_value,
                            meta_width,
                        ))
                    } else {
                        None
                    };
                    updates.push(NextUpdate {
                        span: self.cur_span,
                        target: signal,
                        cond: write_guard(cond, hit.clone()),
                        expr: self.merge_slice(base, position, position, expr.clone(), width),
                        meta,
                    });
                }
            }
        }
        Some(updates)
    }

    /// A whole struct written to an array element chosen at runtime
    /// (`slots[i] = { .tag = t, .val = v }`).
    ///
    /// The element has no signal of its own — its fields are `slots[0].tag`,
    /// `slots[0].val` — so the leaf lookup that the runtime-index expansion
    /// depends on found nothing and the statement was reported as an
    /// unassignable target. Everything around it lowered: the same write at a
    /// *constant* index, a single *field* of it at a runtime index, and a
    /// runtime index into an array of scalars.
    ///
    /// One update per element per field, each gated on the index matching that
    /// element, which is the same shape the scalar expansion produces.
    fn dynamic_struct_write(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> Option<Vec<NextUpdate>> {
        let ast::Expr::Index { base, index, .. } = target else {
            return None;
        };
        if matches!(
            index.as_ref(),
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return None;
        }
        // A constant index already resolves to one element's leaves.
        if self.eval_const(index, &self.cur_env).is_some() {
            return None;
        }
        let base_path = expr_path(base)?;
        let indices = self.local_array.get(&base_path)?.clone();
        // The elements must be structs; an array of scalars is the existing
        // expansion's business.
        self.local_struct
            .get(&format!("{base_path}[{}]", indices.first()?))?;
        let Val::Fields(fields) = self.lower_val_env(value, &HashMap::new()) else {
            return None;
        };
        let lowered_index = self.lower_expr(index);
        let mut updates = Vec::new();
        for position in indices {
            let hit = eq(lowered_index.clone(), Expr::Const(position as u64));
            for (field, expr) in &fields {
                let Some(&signal) = self.locals.get(&format!("{base_path}[{position}].{field}"))
                else {
                    continue;
                };
                updates.push(NextUpdate {
                    span: self.cur_span,
                    target: signal,
                    cond: Some(and(cond.clone(), hit.clone())),
                    expr: self.coerce_to_target(signal, expr.clone()),
                    meta: None,
                });
            }
        }
        (!updates.is_empty()).then_some(updates)
    }

    /// The discriminant plane that preceding writes in this context have
    /// produced. It mirrors [`Self::slice_write_base`] but reads the metadata
    /// retained on each value write instead of relying on independently ordered
    /// companion writes.
    fn slice_meta_write_base(
        &self,
        signal: SignalId,
        companion: SignalId,
        sequential: bool,
        pending: &[NextUpdate],
    ) -> Expr {
        let write_meta = |expr: &Expr, explicit: &Option<Expr>| {
            explicit
                .clone()
                .or_else(|| self.lower_meta_ir(expr, self.out.signals[signal.0 as usize].width))
                .unwrap_or(Expr::Const(0))
        };
        if sequential {
            let seed = self
                .out
                .event_blocks
                .iter()
                .flat_map(|block| {
                    block
                        .updates
                        .iter()
                        .filter(|update| update.target == signal)
                        .map(move |update| {
                            let guard = match &update.cond {
                                Some(cond) => and_expr(block.condition.clone(), cond.clone()),
                                None => block.condition.clone(),
                            };
                            (guard, write_meta(&update.expr, &update.meta))
                        })
                })
                .fold(Expr::Current(companion), |acc, (guard, expr)| {
                    Expr::Select {
                        cond: Box::new(guard),
                        then: Box::new(expr),
                        els: Box::new(acc),
                    }
                });
            return pending
                .iter()
                .filter(|update| update.target == signal)
                .fold(seed, |acc, update| match &update.cond {
                    Some(cond) => Expr::Select {
                        cond: Box::new(cond.clone()),
                        then: Box::new(write_meta(&update.expr, &update.meta)),
                        els: Box::new(acc),
                    },
                    None => write_meta(&update.expr, &update.meta),
                });
        }
        self.out
            .drivers
            .iter()
            .filter(|driver| driver.target == signal && driver.ctx == self.cur_ctx)
            .fold(Expr::Const(0), |acc, driver| match &driver.cond {
                Some(cond) => Expr::Select {
                    cond: Box::new(cond.clone()),
                    then: Box::new(write_meta(&driver.expr, &driver.meta)),
                    els: Box::new(acc),
                },
                None => write_meta(&driver.expr, &driver.meta),
            })
    }

    /// What `signal` already holds where this write appears — the base a
    /// read-modify-write must merge over.
    ///
    /// It is not enough to start from the signal's *prior* value. Each write
    /// produces a whole new value for the signal, and the backend keeps only
    /// the last one that fires: event-block updates are all staged from the
    /// pre-commit state and committed in order, and combinational drivers fold
    /// as `val = cond ? expr : val`. So a second partial write that merged over
    /// `Current(sig)` (or over nothing) silently threw the first one away —
    /// `word[1] = '1'; word[3] = '1';` set bit 3 alone. Folding the writes
    /// already lowered in this context gives each one the value its
    /// predecessors left behind, which is what the source says in both engines.
    fn slice_write_base(&self, signal: SignalId, sequential: bool, pending: &[NextUpdate]) -> Expr {
        if sequential {
            // A clocked block reads the pre-commit value, so an unwritten
            // signal keeps `Current`. Earlier *blocks* of the same driver
            // context count too: an impl may write one signal from several
            // events, and each of those blocks contributes only when its own
            // event fires. Their updates are staged from the same pre-commit
            // state, so folding them symbolically is exactly what the backend
            // computes.
            let seed = self
                .out
                .event_blocks
                .iter()
                .filter(|block| block.ctx == self.cur_ctx)
                .flat_map(|block| {
                    block
                        .updates
                        .iter()
                        .filter(|update| update.target == signal)
                        .map(move |update| {
                            let guard = match &update.cond {
                                Some(cond) => and_expr(block.condition.clone(), cond.clone()),
                                None => block.condition.clone(),
                            };
                            (guard, update.expr.clone())
                        })
                })
                .fold(Expr::Current(signal), |acc, (guard, expr)| Expr::Select {
                    cond: Box::new(guard),
                    then: Box::new(expr),
                    els: Box::new(acc),
                });
            // Then this block's own updates: each expression is a complete
            // next value, so a later one supersedes exactly when it fires.
            return pending
                .iter()
                .filter(|update| update.target == signal)
                .fold(seed, |acc, update| match &update.cond {
                    Some(cond) => Expr::Select {
                        cond: Box::new(cond.clone()),
                        then: Box::new(update.expr.clone()),
                        els: Box::new(acc),
                    },
                    None => update.expr.clone(),
                });
        }
        // Combinational: undriven bits read as zero, which is the seed the
        // single-driver form has always used.
        self.out
            .drivers
            .iter()
            .filter(|driver| driver.target == signal && driver.ctx == self.cur_ctx)
            .fold(Expr::Const(0), |acc, driver| match &driver.cond {
                Some(cond) => Expr::Select {
                    cond: Box::new(cond.clone()),
                    then: Box::new(driver.expr.clone()),
                    els: Box::new(acc),
                },
                None => driver.expr.clone(),
            })
    }

    fn dynamic_write_targets(
        &self,
        path: &str,
        steps: &[AccessStep<'_>],
        hit: Option<Expr>,
        out: &mut Vec<DynamicWriteTarget>,
    ) -> Option<()> {
        let Some((step, rest)) = steps.split_first() else {
            let signal = *self.locals.get(path)?;
            out.push(DynamicWriteTarget::Whole {
                signal,
                hit: hit.unwrap_or(Expr::Const(1)),
            });
            return Some(());
        };
        match step {
            AccessStep::Field(field) => {
                self.dynamic_write_targets(&format!("{path}.{field}"), rest, hit, out)
            }
            AccessStep::Index(index) => {
                if let Some(indices) = self.local_array.get(path) {
                    let lowered_index = self.lower_expr(index);
                    for &position in indices {
                        let matches = eq(lowered_index.clone(), Expr::Const(position as u64));
                        self.dynamic_write_targets(
                            &format!("{path}[{position}]"),
                            rest,
                            Some(and(hit.clone(), matches)),
                            out,
                        )?;
                    }
                    return Some(());
                }
                if !rest.is_empty()
                    || matches!(
                        index,
                        ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                    )
                {
                    return None;
                }
                let signal = *self.locals.get(path)?;
                let positions = self.packed_positions(path)?;
                if let Some(logical) = self.eval_const(index, &self.cur_env) {
                    let position = positions
                        .into_iter()
                        .find_map(|(label, position)| (label == logical).then_some(position))?;
                    out.push(DynamicWriteTarget::PackedBit {
                        signal,
                        position,
                        hit: hit.unwrap_or(Expr::Const(1)),
                    });
                    return Some(());
                }
                let lowered_index = self.lower_expr(index);
                for (logical, position) in positions {
                    out.push(DynamicWriteTarget::PackedBit {
                        signal,
                        position,
                        hit: and(
                            hit.clone(),
                            eq(lowered_index.clone(), Expr::Const(logical as u64)),
                        ),
                    });
                }
                Some(())
            }
        }
    }

    /// A slice-assignment target `y[hi..lo]`: the base signal and the
    /// (normalized) bit range.
    fn slice_target(&self, target: &ast::Expr) -> Option<(SignalId, u32, u32)> {
        let ast::Expr::Index { base, index, .. } = target else {
            return None;
        };
        let (a, b) = self.storage_slice_bounds(base, index)?;
        let sig = *self.locals.get(&expr_path(base)?)?;
        Some((sig, a.max(b), a.min(b)))
    }

    /// A partial (bit-slice) write as a read-modify-write over `base`:
    /// `(base & keep) | ((value & slice_mask) << lo)`, where `keep` clears the
    /// [hi..lo] window. `width` is the target signal's width.
    fn merge_slice(&self, base: Expr, hi: u32, lo: u32, value: Expr, width: u32) -> Expr {
        let slice_w = hi - lo + 1;
        let ones = |bits: u32| {
            let mut words = vec![u64::MAX; (bits as usize).div_ceil(64)];
            if let Some(last) = words.last_mut() {
                let used = bits % 64;
                if used != 0 {
                    *last = (1u64 << used) - 1;
                }
            }
            words
        };
        let mut keep = ones(width);
        for bit in lo..=hi {
            if let Some(word) = keep.get_mut(bit as usize / 64) {
                *word &= !(1u64 << (bit % 64));
            }
        }
        let kept = Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(base),
            rhs: Box::new(words_const(keep)),
        };
        let masked = Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(value),
            rhs: Box::new(words_const(ones(slice_w))),
        };
        let shifted = Expr::Binary {
            op: BinOp::Shl,
            lhs: Box::new(masked),
            rhs: Box::new(Expr::Const(lo as u64)),
        };
        Expr::Binary {
            op: BinOp::Or,
            lhs: Box::new(kept),
            rhs: Box::new(shifted),
        }
    }

    /// Expand a whole-struct assignment (`bus = Bus { .. }`, `mem[0] = E { .. }`)
    /// into one write per leaf field.
    ///
    /// A struct signal is many leaves and no single id, so `target_signal`
    /// returns `None` for it. The combinational path had this expansion and
    /// the clocked path did not, so registering a bus — the ordinary way to
    /// write a pipeline stage — failed with "`e` cannot be assigned to",
    /// which reads as though the signal were an input.
    ///
    /// `Some` whenever the target *is* a struct path, so the caller stops
    /// rather than falling through to a diagnostic about a different problem.
    fn struct_assign_leaves(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> Option<Vec<(SignalId, Expr)>> {
        let tpath = self
            .folded_elem_path(target)
            .or_else(|| expr_path(target))?;
        let struct_name = self.local_struct.get(&tpath).cloned()?;
        // The target's type decides how to read the braces: against a struct
        // `{ 6, 7 }` is a positional literal, not the bit concatenation it
        // lexes as. Without this the whole assignment produced no fields and
        // was dropped, leaving its leaves reported as never driven.
        let positional = self.positional_struct_args(&struct_name, value);
        let value = &match positional {
            Some(args) => ast::Expr::Construct {
                ty: None,
                args,
                spread: None,
                span: ast::expr_span(value),
            },
            None => value.clone(),
        };
        let mut out = Vec::new();
        if let Val::Fields(fields) = self.lower_val_env(value, &HashMap::new()) {
            for (fname, expr) in fields {
                if let Some(&sig) = self.locals.get(&format!("{tpath}.{fname}")) {
                    out.push((sig, self.coerce_to_target(sig, expr)));
                }
            }
        }
        Some(out)
    }

    /// Expand a whole-array assignment (`g = src`, `g = [3, 4]`, `g = f()`)
    /// into one write per element.
    ///
    /// An array signal is many element signals and no single id, so
    /// `target_signal` returns `None` for it. The combinational path has had
    /// this expansion; the clocked path had none, so *every* array assignment
    /// in an event block — from another array, from a literal, from a call —
    /// fell through to the target check and reported `g` as something that
    /// cannot be assigned to. Registering an array is the ordinary way to
    /// write a pipeline, and it named the innocent half of the statement.
    fn array_assign_leaves(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> Option<Vec<(SignalId, Expr)>> {
        let tpath = self
            .folded_elem_path(target)
            .or_else(|| expr_path(target))?;
        let indices = self.local_array.get(&tpath).cloned()?;
        // A call stands for the expression it returns, as it does
        // combinationally.
        let reduced = self.returned_expr_from_call(value);
        let value = reduced.as_ref().unwrap_or(value);
        let leaf = |index: &i64| self.locals.get(&format!("{tpath}[{index}]")).copied();
        let mut out = Vec::new();
        match value {
            ast::Expr::Array { elems, .. } if elems.len() == indices.len() => {
                for (element, index) in elems.iter().zip(&indices) {
                    let signal = leaf(index)?;
                    out.push((
                        signal,
                        self.coerce_to_target(signal, self.lower_expr(element)),
                    ));
                }
            }
            // Another array signal: element for element, from its pre-commit
            // value like every other read in an event block.
            value
                if expr_path(value)
                    .and_then(|p| self.local_array.get(&p))
                    .is_some_and(|source| source.len() == indices.len()) =>
            {
                let source_path = expr_path(value)?;
                let source = self.local_array.get(&source_path)?;
                for (target_index, source_index) in indices.iter().zip(source) {
                    let signal = leaf(target_index)?;
                    let from = self
                        .locals
                        .get(&format!("{source_path}[{source_index}]"))
                        .copied()?;
                    out.push((signal, self.coerce_to_target(signal, Expr::Current(from))));
                }
            }
            // An element-wise expression over arrays (`g = a and b`).
            value => {
                for (position, index) in indices.iter().enumerate() {
                    let element = self.elementwise_at(value, position, indices.len())?;
                    let signal = leaf(index)?;
                    out.push((
                        signal,
                        self.coerce_to_target(signal, self.lower_expr(&element)),
                    ));
                }
            }
        }
        Some(out)
    }

    fn target_signal(&self, target: &ast::Expr) -> Option<SignalId> {
        // Prefer a constant-folded element path (`w[i+1]` with `i` bound in a
        // generate loop -> `w[3]`), so an unrolled constant index resolves to a
        // static element rather than falling through to a dynamic array write.
        if let Some(p) = self.folded_elem_path(target) {
            if let Some(&id) = self.locals.get(&p) {
                return Some(id);
            }
        }
        expr_path(target).and_then(|p| self.locals.get(&p).copied())
    }

    /// Render an element/field path with every index constant-folded through
    /// the current generate-loop environment (`w[i+1]` -> `w[3]`). `None` if any
    /// index is not a compile-time constant.
    fn folded_elem_path(&self, e: &ast::Expr) -> Option<String> {
        match e {
            ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
            ast::Expr::Field { base, field, .. } => {
                Some(format!("{}.{}", self.folded_elem_path(base)?, field.text))
            }
            ast::Expr::Index { base, index, .. } => {
                let i = self.eval_const(index, &self.cur_env)?;
                Some(format!("{}[{}]", self.folded_elem_path(base)?, i))
            }
            _ => None,
        }
    }

    /// Signals assigned on *every* path through an if/else — a terminal `else`
    /// supplies the complement, so these are fully covered and are not latches
    /// even though each driver is conditional. Without a terminal `else` the
    /// fall-through path assigns nothing, so nothing is covered. An
    /// event-controlled branch is sequential (not a combinational latch).
    fn if_covered_targets(&self, iff: &ast::IfStmt) -> std::collections::BTreeSet<u32> {
        use std::collections::BTreeSet;
        if expr_is_event(&iff.cond) {
            return BTreeSet::new();
        }
        let then = self.block_covered_targets(&iff.then);
        let els = match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(b)) => self.block_covered_targets(b),
            Some(ast::ElseBranch::If(inner)) => self.if_covered_targets(inner),
            None => return BTreeSet::new(),
        };
        then.intersection(&els).copied().collect()
    }

    /// Signals assigned by *every* arm of a match that names every variant of
    /// its scrutinee's enum — the match equivalent of a terminal `else`. Empty
    /// when the scrutinee is not an enum, when a variant is unmatched, or when
    /// the match already has a wildcard (which the caller handles).
    fn match_covered_targets(&self, m: &ast::MatchStmt) -> std::collections::BTreeSet<u32> {
        use std::collections::BTreeSet;
        let Some(ty) = self.operand_type_name(&m.scrutinee) else {
            return BTreeSet::new();
        };
        let Some(variants) = self.enum_variants.get(&ty) else {
            return BTreeSet::new();
        };
        let mut named: std::collections::HashSet<String> = std::collections::HashSet::new();
        for arm in &m.arms {
            collect_named_variants(&arm.pattern, &mut named);
        }
        if !variants.keys().all(|v| named.contains(v)) {
            return BTreeSet::new();
        }
        let mut covered: Option<BTreeSet<u32>> = None;
        for arm in &m.arms {
            let here = self.block_covered_targets(&arm.body);
            covered = Some(match covered {
                Some(prev) => prev.intersection(&here).copied().collect(),
                None => here,
            });
        }
        covered.unwrap_or_default()
    }

    /// Signals a block assigns on every path: its direct assignment targets,
    /// plus any target fully covered by a nested if/else.
    fn block_covered_targets(&self, b: &ast::Block) -> std::collections::BTreeSet<u32> {
        let mut out = std::collections::BTreeSet::new();
        for s in &b.stmts {
            match s {
                ast::Stmt::Assign { target, .. } => {
                    if let Some(id) = self.target_signal(target) {
                        out.insert(id.0);
                    }
                }
                ast::Stmt::If(inner) => out.extend(self.if_covered_targets(inner)),
                _ => {}
            }
        }
        out
    }

    fn lower_expr(&self, e: &ast::Expr) -> Expr {
        match e {
            ast::Expr::Call { callee, args, .. } => {
                // `T()` — the nullary constructor — resolves to the type's
                // default before any free-fn/conversion lookup (scalar context).
                if let Some(Val::Scalar(v)) = self.lower_new(callee, args) {
                    return v;
                }
                self.lower_conversion(callee, args, &HashMap::new())
                    .or_else(
                        || match self.lower_free_call(callee, args, &HashMap::new()) {
                            Some(Val::Scalar(v)) => Some(v),
                            _ => None,
                        },
                    )
                    .or_else(
                        || match self.lower_method_call(callee, args, &HashMap::new()) {
                            Some(Val::Scalar(v)) => Some(v),
                            _ => None,
                        },
                    )
                    .or_else(|| match self.lower_from(callee, args, &HashMap::new()) {
                        Some(Val::Scalar(v)) => Some(v),
                        _ => None,
                    })
                    .unwrap_or(Expr::Unknown)
            }
            // `if c { a } else { b }` is a mux: lower to a select.
            ast::Expr::IfExpr {
                cond, then, els, ..
            } => Expr::Select {
                cond: Box::new(self.lower_expr(cond)),
                then: Box::new(self.lower_expr(then)),
                els: Box::new(self.lower_expr(els)),
            },
            // A match-expression is a first-match `Select` chain over the arms.
            ast::Expr::Match {
                scrutinee, arms, ..
            } => self.lower_match_expr(scrutinee, arms),
            // A decimal point makes it a `real` literal (`1.5`).
            ast::Expr::Int { text, .. } if text.contains('.') => {
                Expr::Real(text.replace('_', "").parse().unwrap_or(0.0))
            }
            ast::Expr::Int { text, .. } => integer_const(text).unwrap_or(Expr::Const(0)),
            // A suffix with an `impl Suffix` fn inlines it (scalar results
            // only here; struct results flow through `lower_val_env`).
            // Otherwise `1ns` / `10MHz` scale by the fixed fs/Hz table.
            ast::Expr::SuffixLit { text, suffix, .. } => match self.inline_suffix(e) {
                Some(Val::Scalar(v)) => v,
                Some(Val::Fields(_)) => Expr::Unknown,
                None => Expr::Const(
                    parse_int(text)
                        .map(|v| {
                            v.saturating_mul(ast::suffix_scale(&suffix.text).unwrap_or(1) as u64)
                        })
                        .unwrap_or(0),
                ),
            },
            // Keep every word: `decode_bit_string` returns the low one, so a
            // literal past 64 bits used to lose its top half in driver
            // position while the same literal as an initializer (which
            // already decoded to words) kept it — `w = x"DEADBEEF0123..."`
            // drove `0x0123...` and `let w: unsigned[96] = x"DEADBEEF0123..."`
            // did not.
            ast::Expr::BitStrLit { base, digits, .. } => {
                words_const(self.decode_bit_string_words(*base, digits).0)
            }
            // A plain string in value position is a logic-value vector
            // (`out = "1X10"`) — decode it per-char like a binary bit string.
            // (Char/enum arrays are filled element-wise before reaching here.)
            ast::Expr::StrLit { text, .. } => {
                words_const(self.decode_bit_string_words('b', text).0)
            }
            ast::Expr::CharLit { ch, .. } => Expr::Logic(*ch),
            ast::Expr::Path(path) => {
                let leaf = path
                    .segments
                    .last()
                    .map(|name| name.text.as_str())
                    .unwrap_or("");
                if path.segments.len() == 1 {
                    if let Some(Val::Scalar(value)) = self.block_local_value(e) {
                        return value;
                    }
                    if let Some(id) = self.locals.get(leaf) {
                        return Expr::Current(*id);
                    }
                }
                // Module constants use their resolved qualified identity;
                // implementation constants and parameters retain a local leaf
                // key. This lookup intentionally precedes enum variants so
                // `a::VALUE` is not mistaken for `Enum::Variant`.
                if let Some(key) = self.free_fns.constant_path_key(path) {
                    if let Some(value) = self.const_values.get(&key) {
                        return value.clone();
                    }
                    if let Some(&v) = self.cur_env.get(&key) {
                        return Expr::Const(v as u64);
                    }
                    if let Some(&f) = self.consts_real.get(&key) {
                        return Expr::Real(f);
                    }
                }
                if path.segments.len() >= 2 {
                    return self
                        .enum_variant_path(path)
                        .map(Expr::Const)
                        .unwrap_or(Expr::Unknown);
                }
                // A generic parameter of the entity being lowered has no
                // value when that entity is analysed on its own rather than
                // through an instantiation — `check` roots every
                // uninstantiated entity so library code is analysed too. That
                // is parametric, not unknown, and the same parameter in *type*
                // position (`unsigned[N]`) has always been tolerated this way;
                // only the value position reported the author's own parameter
                // as an undeclared name.
                let parametric = self
                    .lower_stack
                    .last()
                    .and_then(|entity| self.entities.get(entity))
                    .is_some_and(|decl| decl.params.params.iter().any(|q| q.name.text == leaf));
                if !parametric {
                    // Nothing declares this name. Every signal, constant and
                    // in-scope parameter is known here, so record it rather
                    // than lowering to a silent `Unknown` that `check` called
                    // ok.
                    self.unresolved_names
                        .borrow_mut()
                        .push((leaf.to_string(), path.span));
                }
                Expr::Unknown
            }
            // An element of a constant lookup table (`TAB[2]`, `TAB[addr]`).
            // A signal array has had both forms since dynamic indexing landed;
            // a `const` array had neither, because constants are stored one
            // scalar per name. Both lowered to `Unknown` and were reported as
            // a driver index with no name attached.
            ast::Expr::Index { base, index, .. }
                if self
                    .free_fns
                    .constant_expr_key(base)
                    .is_some_and(|key| self.const_arrays.contains_key(&key)) =>
            {
                let key = self.free_fns.constant_expr_key(base).unwrap();
                let values = &self.const_arrays[&key];
                if let Some(i) = self.eval_const(index, &self.consts) {
                    return usize::try_from(i)
                        .ok()
                        .and_then(|i| values.get(i).cloned())
                        // Out of range reads 0, as a dynamic index does.
                        .unwrap_or(Expr::Const(0));
                }
                // A runtime index selects between the elements, the same mux
                // chain `lower_dynamic_read` builds over a signal array.
                let idx = self.lower_expr(index);
                let mut acc = Expr::Const(0);
                for (i, value) in values.iter().enumerate().rev() {
                    acc = Expr::Select {
                        cond: Box::new(Expr::Binary {
                            op: BinOp::Eq,
                            lhs: Box::new(idx.clone()),
                            rhs: Box::new(Expr::Const(i as u64)),
                        }),
                        then: Box::new(value.clone()),
                        els: Box::new(acc),
                    };
                }
                acc
            }
            // A bit slice `base[a..b]` (constant bounds, possibly a named
            // range constant). Direction follows the written order: `7..4`
            // (descending) extracts MSB-first — the natural bit order —
            // while `4..7` (ascending) extracts with the bit order reversed.
            ast::Expr::Index { base, index, .. }
                if self.storage_slice_bounds(base, index).is_some() =>
            {
                let (a, b) = self.storage_slice_bounds(base, index).unwrap();
                let lowered = self.lower_expr(base);
                if a >= b {
                    Expr::Slice {
                        base: Box::new(lowered),
                        hi: a,
                        lo: b,
                    }
                } else {
                    // Ascending: reassemble bits a..=b with significance
                    // reversed: source bit (a+k) lands at result bit (w-1-k).
                    let w = b - a + 1;
                    let mut acc = Expr::Const(0);
                    for k in 0..w {
                        let bit = Expr::Slice {
                            base: Box::new(lowered.clone()),
                            hi: a + k,
                            lo: a + k,
                        };
                        let shifted = Expr::Binary {
                            op: BinOp::Shl,
                            lhs: Box::new(bit),
                            rhs: Box::new(Expr::Const((w - 1 - k) as u64)),
                        };
                        acc = Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(acc),
                            rhs: Box::new(shifted),
                        };
                    }
                    acc
                }
            }
            // A struct-field (`s.data`) or constant array-element (`a[2]`) access
            // resolves to its flattened signal; a *dynamic* array index
            // (`mem[addr]`) becomes a mux tree over the element signals.
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                if let Some(Val::Scalar(value)) = self.block_local_value(e) {
                    return value;
                }
                // `p'old.valid` / `xs'old[0]`: a struct or array is stored as
                // leaf signals, so there is no one signal to take the previous
                // value of. The attribute belongs on the leaf, and
                // `p.valid'old` means the same thing (spec 3.9 writes the
                // first form).
                if let Some(sunk) = sunk_sysattr(e) {
                    return self.lower_expr(&sunk);
                }
                if let Some(id) = expr_path(e).and_then(|p| self.locals.get(&p).copied()) {
                    return Expr::Current(id);
                }
                // A struct constant's field (`K.a`). It has no signal — a
                // constant is a value, not storage — so the dotted path is
                // looked up in the constant table the same way a plain `N`
                // is, one entry per field.
                if let Some(value) = self
                    .free_fns
                    .constant_expr_key(e)
                    .and_then(|key| self.const_values.get(&key))
                {
                    return value.clone();
                }
                // A constant index into an array literal (`[3, 4][0]`). This is
                // what an array-literal argument becomes once the parameter is
                // substituted, and it is a value with no storage behind it, so
                // there is no signal to find — the element is simply picked.
                if let ast::Expr::Index { base, index, .. } = e {
                    if let ast::Expr::Array { elems, .. } = base.as_ref() {
                        if let Some(element) = self
                            .eval_const(index, &self.cur_env)
                            .and_then(|i| usize::try_from(i).ok())
                            .and_then(|i| elems.get(i))
                        {
                            return self.lower_expr(element);
                        }
                    }
                }
                if let Some(v) = self.lower_block_dynamic_access(e) {
                    return v;
                }
                if let Some(v) = self.lower_dynamic_access(e) {
                    return v;
                }
                if let ast::Expr::Index { base, index, .. } = e {
                    if let Some(v) = self.lower_custom_index(base, index) {
                        return v;
                    }
                }
                if self.record_unelaborated_instance_use(e) {
                    return Expr::Unknown;
                }
                if self.is_unresolved_instance_array_reference(e) {
                    return Expr::Unknown;
                }
                // No signal, no mux tree, no `Index` impl. Record the shape
                // while the source is still in hand: from the IR this was an
                // anonymous `Unknown`, and the reader was told only which
                // signal's driver contained one.
                self.unsupported_exprs
                    .borrow_mut()
                    .push((crate::syntax::pretty::expr_string(e), ast::expr_span(e)));
                Expr::Unknown
            }
            ast::Expr::SysAttr { base, attr, .. } => self.lower_sysattr(base, &attr.text),
            ast::Expr::Unary { op, rhs, .. } => {
                // `not` on an enum-typed operand inlines its impl (`impl
                // "not" for Logic`), like binary operators.
                if *op == ast::UnOp::Not {
                    if let Some(Val::Scalar(v)) = self.inline_unary("not", rhs) {
                        return v;
                    }
                    // "Boolean per bit": `not` on a vector-valued signal
                    // reference (name, field, element, slice) inverts every
                    // bit — lower to `x xor mask` so the engines need no
                    // width knowledge. A 1-bit operand keeps the boolean
                    // form (same 0<->1 either way), as do compound
                    // expressions (`not (a == b)`) and enum-typed signals
                    // (their `not` is the impl above, or undefined).
                    let is_vector_ref = match rhs.as_ref() {
                        // A slice is always a bit vector.
                        ast::Expr::Index { base, index, .. }
                            if self.slice_bounds(base, index).is_some() =>
                        {
                            true
                        }
                        ast::Expr::Path(_) | ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                            expr_path(rhs)
                                .and_then(|p| self.locals.get(&p))
                                .map(|&id| self.out.signals[id.0 as usize].enum_type.is_none())
                                .unwrap_or(false)
                        }
                        _ => false,
                    };
                    let _ = is_vector_ref;
                    if let Some(v) = self.vector_not(rhs, |e| self.lower_expr(e)) {
                        return v;
                    }
                }
                self.make_unary(*op, self.lower_expr(rhs))
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // An operator on an enum/struct-typed operand inlines its
                // operator-trait impl body (spec 3.25); `==`/`!=` stay
                // built-in discriminant comparison unless `<=>` derives them.
                let op_str = crate::syntax::pretty::bin_op(op);
                if let Some(native) =
                    self.native_vector_logical(op_str, lhs, rhs, &|e| self.lower_expr(e))
                {
                    return native;
                }
                // Every route to a comparison is marked, because they all owe
                // the same answer: an `Operator` impl, the `<=>` derivation,
                // and the built-in below.
                if !matches!(op_str, "==" | "!=") {
                    if let Some(Val::Scalar(inlined)) =
                        self.inline_op(op_str, lhs, rhs, &HashMap::new())
                    {
                        return self.mark_vector_compare(op, lhs, rhs, inlined);
                    }
                }
                if let Some(derived) = self.inline_cmp(op_str, lhs, rhs, &HashMap::new()) {
                    return self.mark_vector_compare(op, lhs, rhs, derived);
                }
                let (mut l, mut r) = (self.lower_expr(lhs), self.lower_expr(rhs));
                // A character literal's identity comes from its counterpart's
                // type (`c == 'x'` with c: Char reads 'x' as Unicode).
                if let ast::Expr::CharLit { ch, .. } = lhs.as_ref() {
                    if let Some(v) = self.typed_char_literal(*ch, rhs) {
                        l = v;
                    }
                }
                if let ast::Expr::CharLit { ch, .. } = rhs.as_ref() {
                    if let Some(v) = self.typed_char_literal(*ch, lhs) {
                        r = v;
                    }
                }
                let built = self.make_binary(
                    op.clone(),
                    l,
                    r,
                    self.binary_uses_kernel_integer(lhs, rhs),
                    self.declares_kernel_integer(lhs) || self.declares_kernel_integer(rhs),
                );
                self.mark_vector_compare(op, lhs, rhs, built)
            }
            // `{a, b, c}`: fold into `(((0 << w_a) or a) << w_b) or b ...`.
            // First part is the MSBs.
            //
            // `or` rather than `+`: the parts do not overlap, so the two agree
            // bit for bit, but the metavalue companion reads a `+` as
            // arithmetic and poisons the whole result. Joining two fields is
            // not arithmetic, and `"X100" & "1101"` should keep its `'X'` in
            // place rather than turn eight elements unknown.
            ast::Expr::Concat { parts, .. } => {
                let mut acc = Expr::Const(0);
                for part in parts {
                    let w = self.ast_width(part);
                    let e = self.lower_expr(part);
                    let shifted = Expr::Binary {
                        op: BinOp::Shl,
                        lhs: Box::new(acc),
                        rhs: Box::new(Expr::Const(w as u64)),
                    };
                    acc = Expr::Binary {
                        op: BinOp::Or,
                        lhs: Box::new(shifted),
                        rhs: Box::new(e),
                    };
                }
                acc
            }
            _ => Expr::Unknown,
        }
    }

    /// The bit width of a source expression, for sizing concatenations. A nested
    /// concat sums its parts; a slice is its span; a signal/field/element is its
    /// declared width; a literal is its minimal width.
    /// The width of a *direct width-bearing reference* on the RHS of an
    /// assignment — a signal name, struct field, constant array element, bit
    /// slice, or concatenation — for the strict assignment-width check. Returns
    /// `None` for everything else (arithmetic, literals, conversions, muxes,
    /// calls): those are exempt because operator results are not auto-widened
    /// (overflow wraps at the operand width; a different width is an explicit
    /// `resize`), so only signal-to-signal width equality is enforced.
    /// A concat assignment target has an exact width (the sum of its parts),
    /// so the source must match it — the same strict rule scalar targets
    /// follow (spec 3.17). Without this the lowering just slices whatever it
    /// is given: an 8-bit `{y, z}` fed 4 bits silently zero-filled `y`.
    fn check_concat_target_width(
        &mut self,
        parts: &[ast::Expr],
        value: &ast::Expr,
        span: crate::diag::Span,
    ) {
        let want: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
        let Some(have) = self.ref_width(value) else {
            return;
        };
        if want > 0 && have > 0 && want != have {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "width mismatch: this concatenation target is {want} bits but the \
                     assigned value is {have} bits"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(span)
                .help(
                    "widths must match; use a conversion (`unsigned[N](x)` / \
                     `resize(x, N)`) to change width",
                ),
            );
        }
    }

    fn ref_width(&self, e: &ast::Expr) -> Option<u32> {
        if let Some(ty) = self.block_local_type(e) {
            return Some(self.block_local_width(&ty));
        }
        // Indexing is precisely where syntax-only lowering cannot distinguish
        // a vector family from its scalar element. Literal and match types are
        // intentionally contextual, so their best-effort Stage-4 default
        // (`integer`) must not override the assignment target's width here.
        if matches!(e, ast::Expr::Index { .. }) {
            if let Some(width) = self
                .expr_types
                .get(&ast::expr_span(e))
                .and_then(crate::types::Ty::bit_width)
            {
                return Some(width);
            }
        }
        match e {
            ast::Expr::Path(_) | ast::Expr::Field { .. } => {
                let p = expr_path(e)?;
                self.locals
                    .get(&p)
                    .map(|&id| self.out.signals[id.0 as usize].width)
            }
            ast::Expr::Index { base, index, .. } if self.slice_bounds(base, index).is_some() => {
                let (a, b) = self.slice_bounds(base, index)?;
                // A single element of a `Logic`-vector *is* a `Logic` — its
                // width is the element's (a 4-bit disc), not one value bit — so
                // `s: Logic = v[i]` matches, and a metavalue reconstructs.
                if a == b
                    && expr_path(base)
                        .and_then(|p| self.locals.get(&p))
                        .is_some_and(|&id| self.out.signals[id.0 as usize].width > 1)
                {
                    return Some(4);
                }
                Some((a.max(b) - a.min(b) + 1) as u32)
            }
            ast::Expr::Index { .. } => {
                // A constant element index (`v[2]`) reads its element signal.
                let p = expr_path(e)?;
                self.locals
                    .get(&p)
                    .map(|&id| self.out.signals[id.0 as usize].width)
            }
            ast::Expr::Concat { parts, .. } => Some(parts.iter().map(|p| self.ast_width(p)).sum()),
            _ => None,
        }
    }

    /// The width to bind for an operand of an inlined impl. A bare integer
    /// literal has no width of its own — `2` is two bits, and its top bit is
    /// set — so `rhs'length` made every positive literal look negative:
    /// `s / 2` took signed division's both-operands-negative branch and
    /// returned |s| / |2| with the sign dropped, 28 where -28 was meant. A
    /// literal operand is as wide as the value it is used with, the same rule
    /// its *family* already follows.
    fn literal_aware_width(&self, e: &ast::Expr, other: u32) -> u32 {
        // Any constant expression, not just a bare literal: `0 - 2` is two
        // bits by its operands' own reckoning, so a negative literal divisor
        // failed the sign test the same way a positive one passed it.
        if other > 0 && self.eval_const(e, &self.cur_env).is_some() {
            return other;
        }
        self.ast_width(e)
    }

    fn ast_width(&self, e: &ast::Expr) -> u32 {
        if let Some(ty) = self.block_local_type(e) {
            return self.block_local_width(&ty);
        }
        // A bound parameter carries the caller's width, recorded at the
        // inline; without it a nested inline sees no width at all.
        if let Some(p) = expr_path(e) {
            if let Some(&w) = self.param_widths.borrow().get(&p) {
                return w;
            }
        }
        match e {
            ast::Expr::IfExpr { then, .. } => self.ast_width(then),
            ast::Expr::Match { arms, .. } => arms
                .iter()
                .filter_map(|a| a.value_expr())
                .map(|v| self.ast_width(v))
                .max()
                .unwrap_or(1),
            // `not x` is as wide as `x`. Without this the fallback gave 1, so
            // `sext(not s)` bound `x'length` to 1 and returned 0 where the
            // testbench — which does look through the unary — said 55.
            ast::Expr::Unary { rhs, .. } => self.ast_width(rhs),
            // Arithmetic and shifts are as wide as their operands. Without
            // this the fallback below gave 1, so `sext(s + 0)` bound
            // `x'length` to 1 and tested bit 0 for the sign: -56 came back
            // as 200, while `sext(s)` — the same value, named — was right.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                self.ast_width(lhs).max(self.ast_width(rhs))
            }
            // A conversion is as wide as its target (64 for kernel integer).
            ast::Expr::Call { callee, args, .. } => match callee.as_ref() {
                ast::Expr::Index { base, index, .. }
                    if expr_path(base)
                        .as_deref()
                        .is_some_and(|h| self.vector_families.contains(h)) =>
                {
                    self.eval_const(index, &self.cur_env)
                        .map(|w| w as u32)
                        .unwrap_or(64)
                }
                ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "resize" => {
                    args.get(1)
                        .and_then(|n| self.eval_const(n, &self.cur_env))
                        .map(|w| w as u32)
                        .unwrap_or(64)
                }
                // An ordinary call is as wide as its declared return type. The
                // 64 below is the kernel-integer default; taking it for a
                // `signed[8]` result made a nested inline read `self'length`
                // as 64 and test bit 63 for the sign.
                _ => self
                    .free_fns
                    .get(callee)
                    .and_then(|f| f.ret.as_ref())
                    .map(|ret| {
                        type_width(
                            ret,
                            &self.cur_env,
                            &self.free_fns,
                            &self.structs,
                            &self.const_ranges,
                        )
                    })
                    .filter(|w| *w > 0)
                    .unwrap_or(64),
            },
            ast::Expr::Concat { parts, .. } => parts.iter().map(|p| self.ast_width(p)).sum(),
            ast::Expr::Index { base, index, .. } if self.slice_bounds(base, index).is_some() => {
                let (a, b) = self.slice_bounds(base, index).unwrap();
                (a.max(b) - a.min(b) + 1) as u32
            }
            ast::Expr::Int { text, .. } => {
                (u64::BITS - parse_int(text).unwrap_or(0).leading_zeros()).max(1)
            }
            // A bit-string literal has an explicit digit-count width.
            ast::Expr::BitStrLit { base, digits, .. } => (crate::syntax::radix_digits(digits)
                .count() as u32
                * crate::syntax::bits_per_digit(*base))
            .max(1),
            // A signal reference (name, struct field, constant array element).
            _ => expr_path(e)
                .and_then(|p| self.locals.get(&p))
                .map(|&id| self.out.signals[id.0 as usize].width)
                .unwrap_or(1),
        }
    }

    /// Inline the operator-trait impl body for `lhs OP rhs` when the left
    /// operand is an enum- or struct-typed local with a matching impl. The
    /// body must be a pure expression tree: `return e;` or `if c { .. } else
    /// { .. }` chains ending in returns (which become [`Expr::Select`], per
    /// field for struct values). `None` falls back to built-in lowering.
    // ponytail: operand types come from the outer locals, so `self + rhs`
    // nested *inside* an impl body doesn't re-inline; loops/match in bodies
    // unsupported until needed.
    /// Lower a derived logical operator on a *packed* vector natively, as
    /// `and`/`or` already are, instead of inlining std's body.
    ///
    /// std spells these arithmetically -- `xor` is `(a or b) - (a and b)`, and
    /// the complements subtract from an all-ones mask. That agrees on
    /// two-valued data, but the metavalue companion then sees a subtraction and
    /// poisons the whole vector, where `std_logic_1164` applies its table per
    /// element: `not "0000X100"` came back all `'X'` rather than `1111X011`.
    ///
    /// std cannot express the fix itself. A packed vector has no per-element
    /// signals, so the per-element blanket impls over `T[]` do not lower for
    /// one, and a complement needs `not` of a compound value -- which is the
    /// boolean form, not a bitwise one. That is the same reason `and` and `or`
    /// are core operators rather than library ones.
    ///
    /// A `Logic` scalar keeps its impl: `xor` on a nine-value discriminant is a
    /// table lookup, and `^` of two discriminants would be nonsense.
    fn native_vector_logical(
        &self,
        op: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        lower: &dyn Fn(&ast::Expr) -> Expr,
    ) -> Option<Expr> {
        if !matches!(op, "xor" | "nand" | "nor" | "xnor") {
            return None;
        }
        if ![lhs, rhs].iter().all(|e| {
            self.operand_type_name(e)
                .is_some_and(|f| self.out.vector_element_of_family.contains_key(&f))
        }) {
            return None;
        }
        let (a, b) = (lower(lhs), lower(rhs));
        let bin = |op, lhs, rhs| Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let inner = match op {
            "xor" | "xnor" => bin(BinOp::Xor, a, b),
            "nand" => bin(BinOp::And, a, b),
            _ => bin(BinOp::Or, a, b),
        };
        if op == "xor" {
            return Some(inner);
        }
        // The complement is `x xor all-ones`, which the companion lowering
        // reads per element the same way it reads any other `xor`.
        let width = self.ast_width(lhs);
        let mut ones = vec![0u64; (width as usize).max(1).div_ceil(64)];
        for i in 0..width {
            ones[i as usize / 64] |= 1u64 << (i % 64);
        }
        Some(bin(BinOp::Xor, inner, words_const(ones)))
    }

    fn inline_op(
        &self,
        op: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let lhs_ty = self.operand_type_name(lhs)?;
        let rhs_ty = self.operand_type_name(rhs);
        // `a + b` dispatches to the Rust-style trait (`Add`), spec 3.25.
        let tr = op;
        let fns = self.op_impls.get(&(tr.to_string(), lhs_ty.clone()))?;

        // Overload selection. Each candidate's declared rhs type is the
        // impl's trait argument (`impl Add<integer>`) or the fn's rhs
        // parameter type, with `Self` reading as the impl target. Pass 1:
        // exact rhs match. Pass 2: an `integer` operand (a literal) coerces
        // to a Self-typed rhs (`a + 1`). A sole candidate is accepted only
        // when the rhs operand's type is unknown — never on a known mismatch
        // (so `10 + x` with x: unsigned does not inline a Complex impl).
        let declared = |f: &ast::FnDecl, rhs_arg: &Option<String>| -> Option<String> {
            let d = rhs_arg.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(type_head_name)
                    .map(str::to_string)
            })?;
            Some(if d == "Self" { lhs_ty.clone() } else { d })
        };
        let f = match &rhs_ty {
            Some(r) => fns
                .iter()
                .find(|(f, a)| declared(f, a).as_deref() == Some(r.as_str()))
                .or_else(|| {
                    if r == "integer" {
                        fns.iter()
                            .find(|(f, a)| declared(f, a).as_deref() == Some(lhs_ty.as_str()))
                    } else {
                        None
                    }
                }),
            None => {
                if fns.len() == 1 {
                    fns.first()
                } else {
                    None
                }
            }
        };
        // No candidate accepted this right operand. For a *vector family* that
        // is fine — the caller falls back to builtin arithmetic on the packed
        // word. For an aggregate struct there is nothing to fall back to: the
        // expression yields no fields, and the assignment it feeds is dropped
        // without a word, leaving only a downstream "never driven" warning
        // that names the symptom rather than the operator.
        // An *aggregate* struct is the test, not "not a vector family": a
        // multi-field struct may opt into `Vector`, and it is still many
        // signals with no packed-word arithmetic to fall back on, so excluding
        // vector families here would have left exactly this case silent. A
        // field-less newtype (`struct Q(unsigned[8])`) is a word and is
        // correctly left to builtin arithmetic.
        // Only an *aggregate* struct. A field-less newtype is one word with
        // builtin arithmetic behind it, and std's families are exactly that
        // shape (`pub struct unsigned(Logic[])`), so `contains_key` alone
        // would report on ordinary vector expressions. Testing for "not a
        // vector family" instead would be wrong the other way: an aggregate
        // may opt into `Vector` and is still many signals with nothing to fall
        // back on.
        if f.is_none()
            && self
                .structs
                .get(lhs_ty.as_str())
                .is_some_and(|st| !st.fields.is_empty())
        {
            self.bad_operators.borrow_mut().push((
                op.to_string(),
                lhs_ty.clone(),
                rhs_ty.clone(),
                ast::expr_span(lhs).to(ast::expr_span(rhs)),
            ));
        }
        let (f, _) = f?;
        let body = f.body.as_ref()?;

        // Bind `self` to the left operand and the first named param to the
        // right — plus each operand's bit width, so a body can say
        // `self::length` (needed for e.g. sign-aware `signed` comparison).
        let mut fenv: HashMap<String, Val> = HashMap::new();
        fenv.insert("self".to_string(), self.lower_val_env(lhs, env));
        fenv.insert(
            "self::length".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(lhs) as u64)),
        );
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(rhs, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(
                        self.literal_aware_width(rhs, self.ast_width(lhs)) as u64,
                    )),
                );
            }
        }
        self.inline_block(&body.stmts, &fenv)
    }

    /// The written (left, right) constant bounds of a slice index: a range
    /// expression with const-evaluable bounds, or a named range constant.
    fn slice_bounds(&self, base: &ast::Expr, index: &ast::Expr) -> Option<(i64, i64)> {
        let path = expr_path(base)?;
        let block_binding = self.block_local_binding(base);
        let declared = block_binding
            .as_ref()
            .and_then(|binding| self.declared_range(&binding.ty, &self.cur_env))
            .or_else(|| self.persisted_range(&path))?;
        match index {
            ast::Expr::Range { lo, hi, .. } => Some((
                self.eval_const(lo, &self.cur_env)?,
                self.eval_const(hi, &self.cur_env)?,
            )),
            ast::Expr::PartialRange { lo, hi, .. } => {
                let (left, right) = declared;
                Some((
                    match lo {
                        Some(lo) => self.eval_const(lo, &self.cur_env)?,
                        None => left,
                    },
                    match hi {
                        Some(hi) => self.eval_const(hi, &self.cur_env)?,
                        None => right,
                    },
                ))
            }
            ast::Expr::Path(constant) => {
                if let Some(bounds) = self
                    .free_fns
                    .constant_path_key(constant)
                    .and_then(|key| self.const_ranges.get(&key).copied())
                {
                    Some(bounds)
                } else if self.locals.contains_key(&path)
                    || block_binding
                        .as_ref()
                        .is_some_and(|binding| matches!(binding.value, Val::Scalar(_)))
                {
                    let n = self.eval_const(index, &self.cur_env)?;
                    Some((n, n))
                } else {
                    None
                }
            }
            // A single constant index is the one-bit slice `w[n..n]`, but only
            // on a packed vector — which is one signal. An array's elements are
            // signals of their own and `a[2]` resolves through those, so the
            // base having its own entry in `locals` is what tells them apart.
            _ if self.locals.contains_key(&path)
                || block_binding.is_some_and(|binding| matches!(binding.value, Val::Scalar(_))) =>
            {
                let n = self.eval_const(index, &self.cur_env)?;
                Some((n, n))
            }
            _ => None,
        }
    }

    /// Map a vector's declared index labels onto its zero-based storage bits.
    /// A nonzero range keeps numeric significance: `unsigned[15..8]` stores
    /// label 8 in bit 0 and label 15 in bit 7. Direction still controls slice
    /// ordering, but does not waste storage below the declared low bound.
    fn packed_positions(&self, path: &str) -> Option<Vec<(i64, u32)>> {
        if matches!(
            self.persisted_layout(path).map(|layout| &layout.kind),
            Some(LayoutKind::Array { .. })
        ) {
            return None;
        }
        let (left, right) = self.persisted_range(path)?;
        let low = left.min(right);
        let high = left.max(right);
        let signal = *self.locals.get(path)?;
        let width = self.out.signals[signal.0 as usize].width;
        let span = i128::from(high) - i128::from(low) + 1;
        if span != i128::from(width) {
            return None;
        }
        (low..=high)
            .map(|logical| {
                u32::try_from(i128::from(logical) - i128::from(low))
                    .ok()
                    .map(|physical| (logical, physical))
            })
            .collect()
    }

    fn block_packed_positions(&self, ty: &ast::Type) -> Option<Vec<(i64, u32)>> {
        let (left, right) = self.declared_range(ty, &self.cur_env)?;
        let low = left.min(right);
        let high = left.max(right);
        let width = self.block_local_width(ty);
        let span = i128::from(high) - i128::from(low) + 1;
        if span != i128::from(width) {
            return None;
        }
        (low..=high)
            .map(|logical| {
                u32::try_from(i128::from(logical) - i128::from(low))
                    .ok()
                    .map(|physical| (logical, physical))
            })
            .collect()
    }

    /// Constant source bounds translated from declared labels to packed
    /// storage positions. Arrays are excluded because their elements are
    /// separate signals rather than bits of one scalar.
    fn storage_slice_bounds(&self, base: &ast::Expr, index: &ast::Expr) -> Option<(u32, u32)> {
        let named = expr_path(base).and_then(|path| {
            let range = self
                .block_local_binding(base)
                .and_then(|binding| self.declared_range(&binding.ty, &self.cur_env))
                .or_else(|| self.persisted_range(&path))?;
            Some((self.slice_bounds(base, index)?, range))
        });
        let ((a, b), (left, right)) = if let Some(named) = named {
            named
        } else {
            // A computed packed value has no declaration path from which to
            // recover labels. Its result uses the vector family's canonical
            // zero-based storage labels, so an explicit/partial slice can still
            // select it (`(a + b)[3..0]`) instead of lowering to `Unknown`.
            if !matches!(
                self.expr_types.get(&ast::expr_span(base)),
                Some(crate::types::Ty::Array {
                    family: Some(_),
                    ..
                })
            ) {
                return None;
            }
            let width = self.ast_width(base);
            if width == 0 {
                return None;
            }
            let declared = (i64::from(width - 1), 0);
            let bounds = match index {
                ast::Expr::Range { lo, hi, .. } => (
                    self.eval_const(lo, &self.cur_env)?,
                    self.eval_const(hi, &self.cur_env)?,
                ),
                ast::Expr::PartialRange { lo, hi, .. } => (
                    match lo.as_deref() {
                        Some(lo) => self.eval_const(lo, &self.cur_env)?,
                        None => declared.0,
                    },
                    match hi.as_deref() {
                        Some(hi) => self.eval_const(hi, &self.cur_env)?,
                        None => declared.1,
                    },
                ),
                ast::Expr::Path(_) => {
                    let value = self.eval_const(index, &self.cur_env)?;
                    (value, value)
                }
                _ => {
                    let value = self.eval_const(index, &self.cur_env)?;
                    (value, value)
                }
            };
            (bounds, declared)
        };
        let low = left.min(right);
        let high = left.max(right);
        if a < low || a > high || b < low || b > high {
            return None;
        }
        let to_storage = |label: i64| {
            let offset = i128::from(label) - i128::from(low);
            u32::try_from(offset).ok()
        };
        Some((to_storage(a)?, to_storage(b)?))
    }

    fn lower_custom_index(&self, base: &ast::Expr, index: &ast::Expr) -> Option<Expr> {
        let arg = self.index_argument(index)?;
        let span = ast::expr_span(index);
        let callee = ast::Expr::Field {
            base: Box::new(base.clone()),
            field: ast::Ident {
                text: "index".to_string(),
                span,
            },
            span,
        };
        match self.lower_method_call(&callee, &[arg], &HashMap::new())? {
            Val::Scalar(value) => Some(value),
            Val::Fields(_) => None,
        }
    }

    fn index_argument(&self, index: &ast::Expr) -> Option<ast::Expr> {
        let ast::Expr::Range { lo, hi, span } = index else {
            return (!matches!(index, ast::Expr::PartialRange { .. })).then(|| index.clone());
        };
        let path = ast::Path {
            segments: vec![ast::Ident {
                text: "Range".to_string(),
                span: *span,
            }],
            span: *span,
        };
        let field = |name: &str, value: ast::Expr| ast::ConnectArg {
            field: Some(ast::Ident {
                text: name.to_string(),
                span: *span,
            }),
            value: Some(value),
            span: *span,
        };
        Some(ast::Expr::Construct {
            ty: Some(ast::Type::Path(path)),
            args: vec![
                field("left", lo.as_ref().clone()),
                field("right", hi.as_ref().clone()),
            ],
            spread: None,
            span: *span,
        })
    }

    /// Resolve an exact `using` alias chain without looping on malformed
    /// cyclic declarations (which resolution diagnoses separately).
    fn resolve_alias<'t>(&'t self, ty: &'t ast::Type) -> &'t ast::Type {
        let mut ty = ty;
        let mut seen = std::collections::HashSet::new();
        loop {
            let ast::Type::Path(path) = ty else {
                return ty;
            };
            let Some(name) = self.free_fns.type_alias_path_key(path) else {
                return ty;
            };
            if !seen.insert(name.clone()) {
                return ty;
            }
            let Some(alias) = self.aliases.get(&name) else {
                return ty;
            };
            ty = alias;
        }
    }

    fn type_resolves_to(&self, ty: &ast::Type, expected: &str) -> bool {
        let mut ty = ty;
        let mut seen = std::collections::HashSet::new();
        loop {
            let Some(name) = type_head_name(ty) else {
                return false;
            };
            if name == expected {
                return true;
            }
            let ast::Type::Path(path) = ty else {
                return false;
            };
            let Some(key) = self.free_fns.type_alias_path_key(path) else {
                return false;
            };
            if !seen.insert(key.clone()) {
                return false;
            }
            let Some(alias) = self.aliases.get(&key) else {
                return false;
            };
            ty = alias;
        }
    }

    /// Declare `name` as a `Char[n]` array (string-literal inference).
    fn add_char_array(
        &mut self,
        entity: &str,
        name: &str,
        n: usize,
        declaration_span: crate::diag::Span,
    ) {
        self.local_array
            .insert(name.to_string(), (0..n as i64).collect());
        for i in 0..n {
            let elem = format!("{name}[{i}]");
            self.add_signal(entity, &elem, 32, declaration_span);
            if let Some(&id) = self.locals.get(&elem) {
                self.out.signals[id.0 as usize].char = true;
            }
            self.local_char.insert(elem);
        }
    }

    /// Resolve a character literal against its counterpart's type (the
    /// literal has no identity of its own): a `Char` counterpart reads it
    /// through the Unicode table (code point); an enum counterpart reads it
    /// as the matching variant. `None` keeps the default logic-literal form.
    fn typed_char_literal(&self, c: char, other: &ast::Expr) -> Option<Expr> {
        let t = self.operand_type_name(other)?;
        if t == "Char" {
            return Some(Expr::Const(c as u32 as u64));
        }
        let vars = self.enum_variants.get(&t)?;
        vars.get(&format!("'{c}'")).map(|&d| Expr::Const(d))
    }

    /// A char literal's value in enum `en` — its position in that enum's own
    /// declaration (VHDL `T'pos`), from `enum_variants`. Char variants are keyed
    /// with quotes (`'g'`). `None` if `en` has no such variant.
    fn char_disc(&self, ch: char, en: &str) -> Option<u64> {
        self.enum_variant(en, &format!("'{ch}'"))
    }

    /// The discriminant of `variant` in enum `en`, from std's declaration —
    /// the one place enum values come from. `None` if either is unknown.
    fn enum_variant(&self, en: &str, variant: &str) -> Option<u64> {
        self.enum_variants
            .get(en)
            .and_then(|m| m.get(variant))
            .copied()
    }

    fn enum_variant_path(&self, path: &ast::Path) -> Option<u64> {
        let (enumeration, variant) = self.free_fns.enum_variant_key(path)?;
        self.enum_variant(&enumeration, &variant)
    }

    /// Decode a bit-string literal into `(value, discs)`, MSB-first: `value` is
    /// the per-element 0/1 bit (element *i* at bit *i*); `discs` is the full
    /// per-element `std_ulogic` discriminant packed 4 bits each (element *i* at
    /// nibble *i*), so a metavalue's exact value survives. A hex string (`x"…"`)
    /// is pure 2-value. This is the front-end half of X/Z vector support (see
    /// "X/Z propagation through vectors" in `docs/simulation.md`); `discs` is
    /// stored in the element-container companion.
    fn decode_bit_string(&self, base: char, digits: &str) -> (u64, u64) {
        let (value, discs) = self.decode_bit_string_words(base, digits);
        (
            value.first().copied().unwrap_or(0),
            discs.first().copied().unwrap_or(0),
        )
    }

    /// Arbitrary-width counterpart of [`Self::decode_bit_string`]. Both
    /// results are low-word-first, with one value bit and one discriminant
    /// nibble per source element respectively.
    fn decode_bit_string_words(&self, base: char, digits: &str) -> (Vec<u64>, Vec<u64>) {
        // Radix-expanded 2-value strings: hex is 4 bits/digit, octal 3.
        if let Some(bits) = crate::syntax::is_radix_prefix(base)
            .then(|| crate::syntax::bits_per_digit(base) as usize)
        {
            // `_` is a separator, not a digit: it contributes no bits and
            // must not shift the ones after it.
            let width = crate::syntax::radix_digits(digits)
                .count()
                .saturating_mul(bits);
            let mut value = vec![0u64; width.div_ceil(64).max(1)];
            let mut discs = vec![0u64; width.saturating_mul(4).div_ceil(64).max(1)];
            for (digit_index, ch) in crate::syntax::radix_digits(digits).rev().enumerate() {
                let digit = ch.to_digit(crate::syntax::radix_of(base)).unwrap_or(0);
                for bit in 0..bits {
                    let pos = digit_index * bits + bit;
                    let value_bit = u64::from((digit & (1 << bit)) != 0);
                    value[pos / 64] |= value_bit << (pos % 64);
                    discs[(4 * pos) / 64] |= value_bit << ((4 * pos) % 64);
                }
            }
            return (value, discs);
        }
        let n = digits.len();
        let mut value = vec![0u64; n.div_ceil(64).max(1)];
        let mut discs = vec![0u64; n.saturating_mul(4).div_ceil(64).max(1)];
        for (i, ch) in digits.chars().enumerate() {
            let pos = n - 1 - i; // MSB-first: first digit is the top bit
            let disc = self.char_disc(ch, DEFAULT_LOGIC_TYPE).unwrap_or(0);
            value[pos / 64] |= (disc & 1) << (pos % 64);
            discs[(4 * pos) / 64] |= (disc & 0xF) << ((4 * pos) % 64);
        }
        (value, discs)
    }

    /// The `(base, digits)` of a bit-string-like value: an explicit radix
    /// literal `x"…"`/`o"…"`, or a plain string (internal base `'b'`, per-char
    /// binary) — a string of logic values reads as a logic array, no prefix.
    fn bit_string_parts(e: &ast::Expr) -> Option<(char, &str)> {
        match e {
            ast::Expr::BitStrLit { base, digits, .. } => Some((*base, digits)),
            ast::Expr::StrLit { text, .. } => Some(('b', text)),
            _ => None,
        }
    }

    /// Preserve the discriminant plane of a metavalue-carrying bit string
    /// alongside its value driver until [`Self::propagate_metavalues`] creates
    /// the target's companion. A raw `Const`/`WideConst` retains only the low
    /// value bit of each element and cannot recover whether that bit was `X`,
    /// `Z`, or another nine-value symbol.
    fn bit_string_meta(&self, e: &ast::Expr) -> Option<Expr> {
        let (base, digits) = Self::bit_string_parts(e)?;
        let (_, discs) = self.decode_bit_string_words(base, digits);
        Self::has_metavalue(&discs).then(|| words_const(discs))
    }

    /// A nibble of this mask is nonzero exactly when its element's discriminant
    /// is `>= 2` — i.e. a metavalue (`'0'`/`'1'` are discs 0/1 in std's
    /// `Bit`-first `ULogic`, everything above is a metavalue).
    /// `discs & META_MASK != 0` ⇔ "has a metavalue".
    const META_MASK: u64 = 0xEEEE_EEEE_EEEE_EEEE;

    fn has_metavalue(discs: &[u64]) -> bool {
        discs.iter().any(|word| word & Self::META_MASK != 0)
    }

    /// `'X'`'s discriminant, from std's logic enum (not a baked-in `3`), so the
    /// poison value tracks the declaration.
    fn x_disc(&self) -> u64 {
        self.char_disc('X', DEFAULT_LOGIC_TYPE).unwrap_or(3)
    }

    fn u_disc(&self) -> u64 {
        self.char_disc('U', DEFAULT_LOGIC_TYPE).unwrap_or(4)
    }

    /// Rewrite every `Expr::Logic(c)` left in the design — those a typed context
    /// (enum signal, comparison counterpart) did not already resolve — to its
    /// position in std's [`DEFAULT_LOGIC_TYPE`]. After this the backends see
    /// only `Const`s, so no engine hardcodes what `'0'`/`'Z'`/… mean.
    fn normalize_logic_literals(&mut self) {
        let lut = self
            .enum_variants
            .get(DEFAULT_LOGIC_TYPE)
            .cloned()
            .unwrap_or_default();
        for d in &mut self.out.drivers {
            if let Some(c) = &mut d.cond {
                resolve_logic_expr(c, &lut);
            }
            resolve_logic_expr(&mut d.expr, &lut);
        }
        for b in &mut self.out.event_blocks {
            resolve_logic_expr(&mut b.condition, &lut);
            for u in &mut b.updates {
                if let Some(c) = &mut u.cond {
                    resolve_logic_expr(c, &lut);
                }
                resolve_logic_expr(&mut u.expr, &lut);
            }
        }
    }

    /// Coerce a driven value to the target's representation: integer
    /// constants become f64 bits when the target signal is `real`.
    fn coerce_to_target(&self, target: SignalId, expr: Expr) -> Expr {
        let sig = &self.out.signals[target.0 as usize];
        // A char literal assigned to an enum-typed signal takes that variant's
        // position in the enum's *own* declaration (VHDL `T'pos`) — data-driven
        // from `enum_variants`, not a hardcoded Logic map, so a user char enum
        // (`enum Color { 'r','g','b' }`) resolves correctly.
        if let (Some(en), Expr::Logic(c)) = (&sig.enum_type, &expr) {
            if let Some(d) = self.char_disc(*c, en) {
                return Expr::Const(d);
            }
        }
        if sig.char {
            if let Expr::Logic(c) = expr {
                return Expr::Const(c as u32 as u64);
            }
        }
        if sig.real {
            self.coerce_real(expr)
        } else {
            expr
        }
    }

    /// Whether a lowered expression produces f64-bit (`real`) values.
    fn is_real_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Real(_) => true,
            Expr::Current(id) | Expr::Old(id) => self.out.signals[id.0 as usize].real,
            Expr::Binary { op, .. } => {
                matches!(op, BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv)
            }
            Expr::Select { then, els, .. } => self.is_real_expr(then) || self.is_real_expr(els),
            Expr::CCall { f64_ret, .. } => *f64_ret,
            _ => false,
        }
    }

    /// Reinterpret an integer value flowing into a real context (`.re = 10`,
    /// `self.re + 3`, a constant-folded `10 + 0`) as its f64 form: constants
    /// convert, integer arithmetic becomes float arithmetic, selects recurse.
    fn coerce_real(&self, e: Expr) -> Expr {
        if self.is_real_expr(&e) {
            return e;
        }
        match e {
            Expr::Const(v) => Expr::Real(v as f64),
            Expr::Unary { op: UnOp::Neg, rhs } => Expr::Binary {
                op: BinOp::FSub,
                lhs: Box::new(Expr::Real(0.0)),
                rhs: Box::new(self.coerce_real(*rhs)),
            },
            Expr::Select { cond, then, els } => Expr::Select {
                cond,
                then: Box::new(self.coerce_real(*then)),
                els: Box::new(self.coerce_real(*els)),
            },
            Expr::Binary { op, lhs, rhs } => {
                let fop = match op {
                    BinOp::Add | BinOp::SAdd => Some(BinOp::FAdd),
                    BinOp::Sub | BinOp::SSub => Some(BinOp::FSub),
                    BinOp::Mul | BinOp::SMul => Some(BinOp::FMul),
                    BinOp::Div | BinOp::SDiv => Some(BinOp::FDiv),
                    _ => None,
                };
                match fop {
                    Some(f) => Expr::Binary {
                        op: f,
                        lhs: Box::new(self.coerce_real(*lhs)),
                        rhs: Box::new(self.coerce_real(*rhs)),
                    },
                    None => Expr::Binary { op, lhs, rhs },
                }
            }
            e => e,
        }
    }

    /// A unary node, switching negation to float form when the operand is
    /// real. `UnOp::Neg` negates a *word*, and a real carries f64 bits, so
    /// `-2.5` produced the two's-complement of the bit pattern — a different
    /// number entirely, and one that compared unequal to `0.0 - 2.5`.
    fn make_unary(&self, op: ast::UnOp, rhs: Expr) -> Expr {
        if matches!(op, ast::UnOp::Neg) && self.is_real_expr(&rhs) {
            return Expr::Binary {
                op: BinOp::FSub,
                lhs: Box::new(Expr::Real(0.0)),
                rhs: Box::new(rhs),
            };
        }
        Expr::Unary {
            op: lower_unop(op),
            rhs: Box::new(rhs),
        }
    }

    /// Build a binary node, switching `+ - * /` to float arithmetic (and
    /// coercing integer constants) when either operand is real. `==`/`!=`
    /// compare f64 bits exactly, which is right once constants are coerced.
    /// Mark a comparison whose operands are metavalue-capable vectors, so the
    /// `numeric_std` rule can be applied once companions exist. Returns the
    /// value unchanged for anything else -- a scalar, an integer, a comparison
    /// on a type that has no metavalue plane.
    fn mark_vector_compare(
        &self,
        op: &ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        built: Expr,
    ) -> Expr {
        use ast::BinOp as A;
        if !matches!(op, A::Eq | A::Ne | A::Lt | A::Le | A::Gt | A::Ge) {
            return built;
        }
        let vector = |e: &ast::Expr| {
            self.operand_type_name(e)
                .is_some_and(|f| self.out.vector_element_of_family.contains_key(&f))
        };
        if !vector(lhs) && !vector(rhs) {
            return built;
        }
        Expr::MetaCmp {
            ne: matches!(op, A::Ne),
            operands: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
            inner: Box::new(built),
        }
    }

    fn make_binary(
        &self,
        op: ast::BinOp,
        lhs: Expr,
        rhs: Expr,
        integer: bool,
        declared: bool,
    ) -> Expr {
        if self.is_real_expr(&lhs) || self.is_real_expr(&rhs) {
            let (lhs, rhs) = (self.coerce_real(lhs), self.coerce_real(rhs));
            let op = match op {
                ast::BinOp::Add => BinOp::FAdd,
                ast::BinOp::Sub => BinOp::FSub,
                ast::BinOp::Mul => BinOp::FMul,
                ast::BinOp::Div => BinOp::FDiv,
                // Comparisons need ordered float semantics, not integer compare
                // on the bit patterns (which misorders negatives / `±0.0`).
                ast::BinOp::Eq => BinOp::FEq,
                ast::BinOp::Ne => BinOp::FNe,
                ast::BinOp::Lt => BinOp::FLt,
                ast::BinOp::Le => BinOp::FLe,
                ast::BinOp::Gt => BinOp::FGt,
                ast::BinOp::Ge => BinOp::FGe,
                other => match lower_binop(other) {
                    Some(op) => op,
                    None => return Expr::Unknown,
                },
            };
            return Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        // Generic/library impl bodies can leave their parameter expressions
        // typed as the kernel default even after substitution. The concrete
        // lowered signal is authoritative: a `signed[N]`, `unsigned[N]`, Bit,
        // enum, or user-newtype signal must keep the implementation's raw
        // vector operations rather than inherit kernel-integer signedness.
        // A declared kernel-integer operand overrides the signal test: what
        // its body happened to read does not change the type it returns.
        let integer = integer
            && (declared
                || !self.has_non_integer_signal(&lhs) && !self.has_non_integer_signal(&rhs));
        match lower_binop(op) {
            Some(op) => {
                let op = if integer {
                    match op {
                        BinOp::Add => BinOp::SAdd,
                        BinOp::Sub => BinOp::SSub,
                        BinOp::Mul => BinOp::SMul,
                        BinOp::Div => BinOp::SDiv,
                        BinOp::Shr => BinOp::AShr,
                        BinOp::Lt => BinOp::SLt,
                        BinOp::Le => BinOp::SLe,
                        BinOp::Gt => BinOp::SGt,
                        BinOp::Ge => BinOp::SGe,
                        op => op,
                    }
                } else {
                    op
                };
                Expr::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                }
            }
            None => Expr::Unknown,
        }
    }

    fn has_non_integer_signal(&self, e: &Expr) -> bool {
        match e {
            Expr::MetaCmp { inner, .. } => self.has_non_integer_signal(inner),
            Expr::Current(id) | Expr::Old(id) => !self.out.signals[id.0 as usize].integer,
            Expr::Event(_) => true,
            Expr::Unary { rhs, .. } | Expr::Slice { base: rhs, .. } => {
                self.has_non_integer_signal(rhs)
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.has_non_integer_signal(lhs) || self.has_non_integer_signal(rhs)
            }
            // A select's condition controls which value flows but does not
            // participate in that value's numeric representation.
            Expr::Select { then, els, .. } => {
                self.has_non_integer_signal(then) || self.has_non_integer_signal(els)
            }
            // Argument representations do not determine a foreign call's
            // declared return type.
            Expr::CCall { .. } => false,
            Expr::Const(_)
            | Expr::WideConst(_)
            | Expr::Real(_)
            | Expr::Logic(_)
            | Expr::Unknown => false,
        }
    }

    /// Whether an operand *declares* itself a kernel integer, so the
    /// representation of the signals inside it says nothing about the
    /// operation's signedness.
    ///
    /// `sext(x)` exists to turn a `signed[8]` into a negative kernel value,
    /// and `integer(r)` to truncate a `real` to one. Both are inlined, so the
    /// `signed[8]` (or `real`) signal they read is still visible in the
    /// lowered tree — and the guard below, seeing a non-integer signal, forced
    /// the comparison unsigned. `if sext(x) < 0` was therefore false for every
    /// negative `x`, and `abs(sext(x))` returned 251 for -5.
    ///
    /// This is the reasoning the `CCall` arm of `has_non_integer_signal`
    /// already carries for a foreign call: argument representations do not
    /// determine a declared return type. An inlined siox call is no different.
    fn declares_kernel_integer(&self, e: &ast::Expr) -> bool {
        if self
            .block_local_type(e)
            .and_then(|ty| self.free_fns.type_head_key(&ty))
            .is_some_and(|name| name == "integer")
        {
            return true;
        }
        // A parameter of the function being inlined, declared `integer`. The
        // value bound to it may read a `signed[N]` signal (`abs(sext(x))`),
        // and that must not decide the body's signedness either.
        if expr_path(e).is_some_and(|n| self.param_integers.borrow().contains(&n)) {
            return true;
        }
        let ast::Expr::Call { callee, .. } = e else {
            return false;
        };
        // The kernel conversion `integer(x)`, whose whole purpose is to produce
        // a signed kernel value (`integer(r)` truncates a `real`, which may be
        // negative). It reads the real/vector signal it converts, so the signal
        // scan would otherwise force the comparison unsigned. (This needs the
        // matching `fit_signed` on the LLVM side of `RealToInt`: the two are
        // interdependent — a signed compare that zero-extends its operand is no
        // better than an unsigned one.)
        if let ast::Expr::Path(p) = callee.as_ref() {
            if p.segments.len() == 1 && p.segments[0].text == "integer" {
                return true;
            }
        }
        // A module function whose declared return type is `integer`.
        self.free_fns
            .get(callee)
            .and_then(|f| f.ret.as_ref())
            .and_then(type_head_name)
            == Some("integer")
    }

    fn binary_uses_kernel_integer(&self, lhs_ast: &ast::Expr, rhs_ast: &ast::Expr) -> bool {
        // An explicit kernel-integer declaration is authoritative. In
        // particular, the type table may describe `integer(real_value)` with
        // the source family after inlining; rejecting arrays first made a
        // direct negative comparison unsigned even though assigning the same
        // conversion to an integer local worked.
        if self.declares_kernel_integer(lhs_ast) || self.declares_kernel_integer(rhs_ast) {
            return true;
        }
        let lhs = self.expr_types.get(&ast::expr_span(lhs_ast));
        let rhs = self.expr_types.get(&ast::expr_span(rhs_ast));
        // Literals retain their default `integer` type even when they occur
        // inside an inlined library-vector implementation. A concrete
        // array/newtype operand owns that operation through std; it must not be
        // reinterpreted as a signed kernel-integer operation merely because
        // its other operand happens to be an integer literal.
        if matches!(lhs, Some(crate::types::Ty::Array { .. }))
            || matches!(rhs, Some(crate::types::Ty::Array { .. }))
        {
            return false;
        }
        // A parameter of the function being inlined, declared `integer`. Its
        // recorded type is `Error` (the body is checked without parameters in
        // scope), so without this the declaration is simply not consulted.
        let declared_integer =
            |e: &ast::Expr| expr_path(e).is_some_and(|n| self.param_integers.borrow().contains(&n));
        matches!(lhs, Some(crate::types::Ty::Integer))
            || matches!(rhs, Some(crate::types::Ty::Integer))
            || declared_integer(lhs_ast)
            || declared_integer(rhs_ast)
    }

    /// Derive a comparison from the three-way `<=>` impl (spaceship, spec
    /// 3.25): `a < b` becomes `(a <=> b) == Ordering::Less`, etc. The impl
    /// returns std::ops' `Ordering { Less, Equal, Greater }` (0/1/2), so no
    /// signed arithmetic is needed. `None` when the operand type has no
    /// `<=>` impl — built-in comparison applies.
    fn inline_cmp(
        &self,
        op_str: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        // (`Ordering` variant to compare against, negate?). The discriminant
        // comes from std's `Ordering` enum, not a baked-in 0/1/2 — the fallback
        // is only the conventional layout for a std-less unit test.
        let (variant, fallback, ne) = match op_str {
            "<" => ("Less", 0u64, false),
            "==" => ("Equal", 1, false),
            ">" => ("Greater", 2, false),
            ">=" => ("Less", 0, true),
            "!=" => ("Equal", 1, true),
            "<=" => ("Greater", 2, true),
            _ => return None,
        };
        let want = self.enum_variant("Ordering", variant).unwrap_or(fallback);
        let Val::Scalar(cmp) = self.inline_op("<=>", lhs, rhs, env)? else {
            return None;
        }; // -> Ord::cmp
        Some(Expr::Binary {
            op: if ne { BinOp::Ne } else { BinOp::Eq },
            lhs: Box::new(cmp),
            rhs: Box::new(Expr::Const(want)),
        })
    }

    /// Inline a unary operator impl (`not a`): binds only `self`.
    fn inline_unary(&self, op: &str, rhs: &ast::Expr) -> Option<Val> {
        let ty = self.operand_type_name(rhs)?;
        let tr = op;
        let fns = self.op_impls.get(&(tr.to_string(), ty))?;
        let (f, _) = fns.first()?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        env.insert("self".to_string(), self.lower_val_env(rhs, &HashMap::new()));
        env.insert(
            "self::length".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(rhs) as u64)),
        );
        self.inline_block(&body.stmts, &env)
    }

    /// Synthesize a total derivation conversion `target(x)` when no explicit
    /// `From` impl exists (spec: derived types §14). Two total cases:
    ///  - enums connected by a derivation chain where every source variant
    ///    exists in the target — representation-identity (base-first
    ///    discriminants), so the value passes through unchanged;
    ///  - a source struct that derives (transitively) from the target struct
    ///    — project onto the inherited fields.
    fn derived_conversion(
        &self,
        target: &str,
        src: Option<&str>,
        arg: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let src = src?;
        // Enum case: chain-connected and source variants subset of target.
        if let (Some(sv), Some(tv)) = (self.enum_variants.get(src), self.enum_variants.get(target))
        {
            let connected = self.enum_ancestor(src, target) || self.enum_ancestor(target, src);
            let total = sv.keys().all(|v| tv.contains_key(v));
            if connected && total {
                return Some(self.lower_val_env(arg, env)); // identity
            }
            return None;
        }
        // Struct case: project a derived struct onto its base fields by
        // reading the source's per-field signals (a bare struct path isn't
        // itself a Val::Fields).
        if self.struct_derives_from(src, target) {
            let base = expr_path(arg)?;
            let fields = self
                .struct_field_names(target)
                .into_iter()
                .map(|n| {
                    let expr = self
                        .locals
                        .get(&format!("{base}.{n}"))
                        .map(|&id| Expr::Current(id))
                        .unwrap_or(Expr::Unknown);
                    (n, expr)
                })
                .collect();
            return Some(Val::Fields(fields));
        }
        None
    }

    /// Whether `anc` is a (transitive) enum-derivation ancestor of `name`.
    fn enum_ancestor(&self, anc: &str, name: &str) -> bool {
        let mut cur = name.to_string();
        let mut seen = HashSet::new();
        while let Some(b) = self.enum_bases.get(&cur) {
            if !seen.insert(cur.clone()) {
                break;
            }
            if b == anc {
                return true;
            }
            cur = b.clone();
        }
        false
    }

    /// Whether struct `name` derives (transitively) from struct `base`.
    fn struct_derives_from(&self, name: &str, base: &str) -> bool {
        let mut cur = name.to_string();
        let mut seen = HashSet::new();
        while let Some(s) = self.structs.get(&cur) {
            if !seen.insert(cur.clone()) {
                break;
            }
            let Some(b) = s
                .base
                .as_ref()
                .and_then(|ty| self.free_fns.type_head_key(ty))
            else {
                return false;
            };
            if b == base {
                return true;
            }
            cur = b;
        }
        false
    }

    /// A struct type's full (inherited + own) field names, base chain first.
    fn struct_field_names(&self, name: &str) -> Vec<String> {
        self.struct_field_names_at(name, &mut HashSet::new())
    }

    /// Cycle-safe, like [`Self::struct_derives_from`]: a cyclic derivation is
    /// reported by resolve, but lowering still runs best-effort.
    fn struct_field_names_at(&self, name: &str, seen: &mut HashSet<String>) -> Vec<String> {
        if !seen.insert(name.to_string()) {
            return Vec::new();
        }
        let Some(s) = self.structs.get(name) else {
            return Vec::new();
        };
        let mut out = match s
            .base
            .as_ref()
            .and_then(|ty| self.free_fns.type_head_key(ty))
        {
            Some(b) => self.struct_field_names_at(&b, seen),
            None => Vec::new(),
        };
        out.extend(s.fields.iter().map(|f| f.name.text.clone()));
        seen.remove(name);
        out
    }

    /// `T(x)` on a named type: dispatch to `impl From<Source> for T`,
    /// selected by the argument's type (sole impl accepted for an unknown
    /// source). Struct-valued results come back as per-field values.
    fn lower_from(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let target = match callee {
            ast::Expr::Path(p) => self
                .free_fns
                .enum_path_key(p)
                .or_else(|| (p.segments.len() == 1).then(|| p.segments[0].text.clone()))?,
            _ => return None,
        };
        let arg = args.first()?;
        let src = self.operand_type_name(arg);
        let found = self.lower_from_inner(&target, arg, src.as_deref(), env);
        // This is the last conversion strategy tried, so a `None` here is an
        // `Unknown` in the driver. Record it while the target, the source and
        // a span are all still in hand.
        if found.is_none()
            && (self.structs.contains_key(&target) || self.enum_variants.contains_key(&target))
        {
            self.bad_conversions
                .borrow_mut()
                .push((target, src.clone(), ast::expr_span(callee)));
        }
        found
    }

    fn lower_from_inner(
        &self,
        target: &str,
        arg: &ast::Expr,
        src: Option<&str>,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let src = src.map(str::to_string);
        // No explicit `impl From<src> for target`: try a derivation-total
        // conversion (spec: T(x) is auto for total derivations).
        let Some(fns) = self.op_impls.get(&("From".to_string(), target.to_string())) else {
            return self.derived_conversion(target, src.as_deref(), arg, env);
        };
        let declared = |f: &ast::FnDecl, a: &Option<String>| -> Option<String> {
            a.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(type_head_name)
                    .map(str::to_string)
            })
        };
        let chosen = match &src {
            Some(sty) => fns
                .iter()
                .find(|(f, a)| declared(f, a).as_deref() == Some(sty)),
            None => (fns.len() == 1).then(|| &fns[0]),
        };
        let (f, _) = match chosen {
            Some(c) => c,
            None => return self.derived_conversion(target, src.as_deref(), arg, env),
        };
        let body = f.body.as_ref()?;
        let mut fenv: HashMap<String, Val> = HashMap::new();
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(arg, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(arg) as u64)),
                );
            }
        }
        self.inline_block(&body.stmts, &fenv)
    }

    /// `T()` / `T[N]()` — the nullary constructor: the structural `new()`
    /// default of a named type, the explicit spelling of the value an
    /// uninitialized signal already powers on to (§3.29), and the zero-argument
    /// member of the same `T(...)` family whose one-argument form `T(x)` is the
    /// conversion of §3.28. An enum yields its first variant (`T'LEFT`); a
    /// numeric / vector / `Char` / `real` / kernel `integer` yields `0`; a
    /// struct yields its fields defaulted the same way. The `impl New for T`
    /// *override* waits on trait resolution; this is the derived default only.
    /// (An array `T[N]()` of a composite defaults through its per-element signal
    /// inits, not an expression value.)
    fn lower_new(&self, callee: &ast::Expr, args: &[ast::Expr]) -> Option<Val> {
        if !args.is_empty() {
            return None;
        }
        // The head type name — a bare `T` or the family of a sized `T[N]` (the
        // width is irrelevant to a zero default).
        let name = match callee {
            ast::Expr::Path(p) => self
                .free_fns
                .type_owner_key(p)
                .or_else(|| (p.segments.len() == 1).then(|| p.segments[0].text.clone()))?,
            ast::Expr::Index { base, .. } => match base.as_ref() {
                ast::Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.clone(),
                _ => return None,
            },
            _ => return None,
        };
        if let Some(&d) = self.enum_first_disc.get(&name) {
            return Some(Val::Scalar(Expr::Const(d)));
        }
        if let Some(fields) = self.struct_default_leaves(&name, "") {
            return Some(Val::Fields(fields));
        }
        (self.vector_families.contains(&name)
            || matches!(name.as_str(), "integer" | "Char" | "real"))
        .then_some(Val::Scalar(Expr::Const(0)))
    }

    /// A struct's derived default as flattened `(leaf-dotted-name, expr)` pairs
    /// (the shape `Val::Fields` assignment consumes), each field defaulted
    /// structurally and nested structs recursed. `None` for a non-aggregate (a
    /// scalar newtype like `struct unsigned : Logic[]`, which has no fields).
    fn struct_default_leaves(&self, sname: &str, prefix: &str) -> Option<Vec<(String, Expr)>> {
        let fields = self.raw_struct_fields(sname).filter(|f| !f.is_empty())?;
        let mut out = Vec::new();
        for (fname, fty) in fields {
            let path = if prefix.is_empty() {
                fname.clone()
            } else {
                format!("{prefix}.{fname}")
            };
            if let ast::Type::Path(enum_path) = &fty {
                if let Some(enum_key) = self.free_fns.enum_path_key(enum_path) {
                    if let Some(&d) = self.enum_first_disc.get(&enum_key) {
                        out.push((path, Expr::Const(d)));
                        continue;
                    }
                }
            }
            if let Some(h) = self.free_fns.type_head_key(&fty) {
                if let Some(nested) = self.struct_default_leaves(&h, &path) {
                    out.extend(nested);
                    continue;
                }
                if let Some(&d) = self.enum_first_disc.get(&h) {
                    out.push((path, Expr::Const(d)));
                    continue;
                }
            }
            out.push((path, Expr::Const(0)));
        }
        Some(out)
    }

    /// Lower a call to a module-level `fn`: const-fold when every argument
    /// const-evaluates (so `clog2(DEPTH)` is a constant), else inline the
    /// body like an operator impl (params bound positionally, with
    /// `param::length` available). Depth-guarded against runaway recursion.
    /// Inline a module-level function call.
    ///
    /// Returns a [`Val`], not an `Expr`: a function may return a struct, and
    /// discarding the `Val::Fields` the body produced left the call with no
    /// value at all. The assignment it fed was then dropped for want of
    /// fields, so `s = twice(a)` read as zero with only a "never driven"
    /// warning — while a *method* with the identical body worked, because the
    /// method path always kept the `Val`.
    fn lower_free_call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let display_name = call_fn_key(callee)?;
        let f = self.free_fns.get(callee)?;
        // A bodyless declaration is a foreign C function (`extern "C"`).
        if f.body.is_none() {
            let is_type = |t: &Option<ast::Type>, expected: &str| {
                t.as_ref()
                    .is_some_and(|ty| self.type_resolves_to(ty, expected))
            };
            let f64_args = f
                .params
                .iter()
                .filter(|p| !p.is_self)
                .map(|p| is_type(&p.ty, "real"))
                .collect();
            let integer_args = f
                .params
                .iter()
                .filter(|p| !p.is_self)
                .map(|p| is_type(&p.ty, "integer"))
                .collect();
            let f64_ret = is_type(&f.ret, "real");
            let integer_ret = is_type(&f.ret, "integer");
            let args = args.iter().map(|a| self.lower_scalar_env(a, env)).collect();
            return Some(Val::Scalar(Expr::CCall {
                name: f.name.text.clone(),
                args,
                f64_args,
                integer_args,
                f64_ret,
                integer_ret,
            }));
        }
        // Constant arguments: run the body statically.
        let consts: Option<Vec<i64>> = args
            .iter()
            .map(|a| eval_const_fns(a, &self.cur_env, &self.free_fns, 0))
            .collect();
        if let Some(cs) = consts {
            let mut fenv = self.cur_env.clone();
            for (p, v) in f.params.iter().filter(|p| !p.is_self).zip(cs) {
                if let Some(n) = &p.name {
                    fenv.insert(n.text.clone(), v);
                }
            }
            if let Some(v) = eval_const_stmts(&f.body.as_ref()?.stmts, &fenv, &self.free_fns, 0) {
                return Some(Val::Scalar(Expr::Const(v as u64)));
            }
        }
        // Dynamic arguments: inline the body as an expression tree.
        if self.inline_depth.get() > 16 {
            // Bailing here leaves an `Unknown` in the middle of a driver, so
            // record it — otherwise lowering "succeeds" and the design only
            // fails much later with a generic engine message.
            self.depth_exceeded
                .borrow_mut()
                .push((display_name, ast::expr_span(callee)));
            return None;
        }
        self.inline_depth.set(self.inline_depth.get() + 1);
        let mut fenv: HashMap<String, Val> = HashMap::new();
        // Saved param-family bindings to restore after this inline (nesting).
        let mut saved: Vec<(String, Option<String>)> = Vec::new();
        let mut saved_widths: Vec<(String, Option<u32>)> = Vec::new();
        // Names this inline added to `param_integers`, removed on the way out
        // so a nested or later inline does not inherit them.
        let mut added_integers: Vec<String> = Vec::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                // An argument is read against the *parameter's* declared type,
                // so a positional literal for a struct parameter is a struct
                // literal. Left as the concatenation it lexes as, the
                // parameter bound no fields and the body's `p.a` reported
                // having no hardware form.
                let a = &self.as_struct_literal(p.ty.as_ref(), a);
                fenv.insert(n.text.clone(), self.lower_val_env(a, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(a) as u64)),
                );
                // Propagate the argument's family so the body dispatches
                // operators on the caller's concrete type.
                if let Some(fam) = self.operand_type_name(a) {
                    let prev = self.param_types.borrow_mut().insert(n.text.clone(), fam);
                    saved.push((n.text.clone(), prev));
                }
                // A parameter declared `integer` makes the body's operations
                // signed, which its recorded types cannot say.
                if p.ty.as_ref().and_then(type_head_name) == Some("integer")
                    && self.param_integers.borrow_mut().insert(n.text.clone())
                {
                    added_integers.push(n.text.clone());
                }
                // The width travels with the family: a nested inline (e.g.
                // `signed`'s Ord inside this body) reads `self'length` off the
                // parameter, and without this it saw none.
                let w = self.ast_width(a);
                if w > 0 {
                    saved_widths.push((
                        n.text.clone(),
                        self.param_widths.borrow_mut().insert(n.text.clone(), w),
                    ));
                }
            }
        }
        // An array-typed parameter has no `Val` to bind to: a `Val` is a scalar
        // or a set of named fields, and an array is neither — its elements are
        // separate signals. So `fenv` held nothing useful for it and the body's
        // `v[0]` resolved to nothing, reporting "has no hardware form" with
        // help about runtime indices, pointing inside the callee at a line the
        // caller never wrote. Substituting the parameter's *name* with the
        // argument turns `v[0]` into `d[0]`, an ordinary element read.
        //
        // When one parameter is an array, *every* parameter is substituted:
        // the body's `v[i]` has to become `q[idx]`, and an index left bound in
        // the value environment instead reports `i` as an unknown name — that
        // environment is consulted for a value, not for the index of an
        // element read. A function with no array parameter keeps its value
        // bindings, which carry the width and family that a substituted
        // expression does not.
        let has_array_param = f.params.iter().filter(|p| !p.is_self).any(|p| {
            p.ty.as_ref().is_some_and(|ty| {
                array_of(
                    ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.vector_families,
                    &self.free_fns,
                )
                .is_some()
            })
        });
        let array_args: HashMap<String, ast::Expr> = if has_array_param {
            f.params
                .iter()
                .filter(|p| !p.is_self)
                .zip(args)
                .filter_map(|(p, a)| Some((p.name.as_ref()?.text.clone(), a.clone())))
                .collect()
        } else {
            HashMap::new()
        };
        let out = f.body.as_ref().and_then(|b| {
            let stmts = self.normalize_struct_returns(&b.stmts, f.ret.as_ref());
            let stmts: Vec<ast::Stmt> = if array_args.is_empty() {
                stmts
            } else {
                stmts
                    .iter()
                    .map(|s| subst_stmt_paths(s, &array_args))
                    .collect()
            };
            self.inline_block(&stmts, &fenv)
        });
        // The result is a value of the declared return type, so it wraps to
        // that width. Assigning it to a signal masked it anyway, which hid
        // this — but used in place (`neg(x) < 0`) the extra bits survived and
        // signed's Ord tested the wrong one.
        // Only a scalar result has a declared width to wrap to; a struct's
        // leaves were already masked field by field as the body built them.
        let out = match (out, f.ret.as_ref()) {
            (Some(Val::Scalar(v)), Some(ret)) => Some(Val::Scalar(self.mask_to_type_width(v, ret))),
            (v, _) => v,
        };
        for (name, prev) in saved.into_iter().rev() {
            match prev {
                Some(v) => self.param_types.borrow_mut().insert(name, v),
                None => self.param_types.borrow_mut().remove(&name),
            };
        }
        for (name, prev) in saved_widths.into_iter().rev() {
            match prev {
                Some(w) => self.param_widths.borrow_mut().insert(name, w),
                None => self.param_widths.borrow_mut().remove(&name),
            };
        }
        for name in added_integers {
            self.param_integers.borrow_mut().remove(&name);
        }
        self.inline_depth.set(self.inline_depth.get() - 1);
        out
    }

    /// Wrap `v` to the width of declared type `ret`, when that is a bounded
    /// vector. A `real`, a kernel `integer` or an unknown width is left alone.
    fn mask_to_type_width(&self, v: Expr, ret: &ast::Type) -> Expr {
        if type_head_name(ret).is_some_and(|h| matches!(h, "real" | "integer")) {
            return v;
        }
        let w = type_width(
            ret,
            &self.cur_env,
            &self.free_fns,
            &self.structs,
            &self.const_ranges,
        );
        if w == 0 || w >= 64 {
            return v;
        }
        Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(v),
            rhs: Box::new(Expr::Const((1u64 << w) - 1)),
        }
    }

    /// Find a method `name` on type `ty`: an inherent-impl method
    /// (`impl T { fn name(self, ..) }`) or a trait-impl method
    /// (`impl Tr for T { fn name(self, ..) }`, held in `op_impls` keyed by
    /// trait+type). Inherent impls win; first match otherwise.
    fn find_method(&self, ty: &str, name: &str, input: Option<&str>) -> Option<&'a ast::FnDecl> {
        if let Some(impls) = self.nominal_id(ty).and_then(|id| self.impls.get(&id)) {
            for im in impls {
                for it in &im.items {
                    if let ast::ImplItem::Fn(f) = it {
                        if f.name.text == name {
                            return Some(f);
                        }
                    }
                }
            }
        }
        if let Some(f) = self
            .op_impls
            .iter()
            .filter(|((_, t), _)| t == ty)
            .flat_map(|(_, fns)| fns.iter())
            .find(|(f, rhs)| {
                f.name.text == name
                    && input.is_none_or(|input| rhs.as_deref().is_none_or(|rhs| rhs == input))
            })
            .map(|(f, _)| *f)
        {
            return Some(f);
        }
        // Last: a defaulted method the type inherits from a trait it
        // implements. The impl's own methods are found above, so an override
        // always wins; this only supplies what the impl omitted.
        self.implemented_traits
            .get(ty)?
            .iter()
            .filter_map(|tr| self.trait_decls.get(tr.as_str()))
            .flat_map(|t| t.items.iter())
            .find(|f| f.name.text == name && f.body.is_some())
    }

    /// Lower a method call `recv.method(args)` (spec 3.20) by inlining the
    /// impl method's body: `self` binds to the receiver, each named parameter
    /// to its argument (mirroring [`Self::lower_free_call`]), and the receiver
    /// type is stashed under `param_types["self"]` so operators inside the body
    /// dispatch on the concrete type. Value-returning methods (`a.cmp(b)`,
    /// `s.can_send()`) inline to a [`Val`]; a body the inliner cannot express
    /// as a value (a statement method that drives signals) yields `None`.
    fn lower_method_call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let ast::Expr::Field { base, field, .. } = callee else {
            return None;
        };
        let ty = self.operand_type_name(base)?;
        let input = args.first().and_then(|arg| match arg {
            ast::Expr::Construct { ty: Some(ty), .. } if type_head_name(ty) == Some("Range") => {
                Some("Range".to_string())
            }
            _ => self.operand_type_name(arg),
        });
        let f = self.find_method(&ty, &field.text, input.as_deref())?;
        let body = f.body.as_ref()?;
        if self.inline_depth.get() > 16 {
            self.depth_exceeded
                .borrow_mut()
                .push((format!("{ty}.{}", field.text), ast::expr_span(callee)));
            return None;
        }
        self.inline_depth.set(self.inline_depth.get() + 1);
        // Bind `self` to the receiver's signal so a `self'event`/`self'old`
        // sysattr in the body (the std `ClockLike` edge methods) resolves to it.
        let saved_self = self.self_signal.replace(self.base_signal(base));
        let mut fenv: HashMap<String, Val> = HashMap::new();
        fenv.insert("self".to_string(), self.lower_val_env(base, env));
        fenv.insert(
            "self::length".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(base) as u64)),
        );
        // Family bindings to restore after the inline (nesting-safe).
        let mut saved: Vec<(String, Option<String>)> = Vec::new();
        let mut saved_widths: Vec<(String, Option<u32>)> = Vec::new();
        // Names this inline added to `param_integers`, removed on the way out
        // so a nested or later inline does not inherit them.
        let mut added_integers: Vec<String> = Vec::new();
        let self_prev = self
            .param_types
            .borrow_mut()
            .insert("self".to_string(), ty.clone());
        saved.push(("self".to_string(), self_prev));
        let receiver_width = self.ast_width(base);
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                // An argument is read against the *parameter's* declared type,
                // so a positional literal for a struct parameter is a struct
                // literal. Left as the concatenation it lexes as, the
                // parameter bound no fields and the body's `p.a` reported
                // having no hardware form.
                let a = &self.as_struct_literal(p.ty.as_ref(), a);
                fenv.insert(n.text.clone(), self.lower_val_env(a, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(
                        self.literal_aware_width(a, receiver_width) as u64
                    )),
                );
                if let Some(fam) = self.operand_type_name(a) {
                    let prev = self.param_types.borrow_mut().insert(n.text.clone(), fam);
                    saved.push((n.text.clone(), prev));
                }
                // A parameter declared `integer` makes the body's operations
                // signed, which its recorded types cannot say.
                if p.ty.as_ref().and_then(type_head_name) == Some("integer")
                    && self.param_integers.borrow_mut().insert(n.text.clone())
                {
                    added_integers.push(n.text.clone());
                }
                // The width travels with the family: a nested inline (e.g.
                // `signed`'s Ord inside this body) reads `self'length` off the
                // parameter, and without this it saw none.
                let w = self.ast_width(a);
                if w > 0 {
                    saved_widths.push((
                        n.text.clone(),
                        self.param_widths.borrow_mut().insert(n.text.clone(), w),
                    ));
                }
            }
        }
        // A method's array parameter needs the same substitution a free
        // function's does — the value environment has no array case, so the
        // body's `v[0]` resolved to nothing.
        let array_args: HashMap<String, ast::Expr> =
            if f.params.iter().filter(|p| !p.is_self).any(|p| {
                p.ty.as_ref().is_some_and(|ty| {
                    array_of(
                        ty,
                        &self.cur_env,
                        &self.const_ranges,
                        &self.vector_families,
                        &self.free_fns,
                    )
                    .is_some()
                })
            }) {
                f.params
                    .iter()
                    .filter(|p| !p.is_self)
                    .zip(args)
                    .filter_map(|(p, a)| Some((p.name.as_ref()?.text.clone(), a.clone())))
                    .collect()
            } else {
                HashMap::new()
            };
        let stmts: Vec<ast::Stmt> = if array_args.is_empty() {
            body.stmts.clone()
        } else {
            body.stmts
                .iter()
                .map(|s| subst_stmt_paths(s, &array_args))
                .collect()
        };
        let out = self.inline_block(&stmts, &fenv);
        for (name, prev) in saved.into_iter().rev() {
            match prev {
                Some(v) => self.param_types.borrow_mut().insert(name, v),
                None => self.param_types.borrow_mut().remove(&name),
            };
        }
        for (name, prev) in saved_widths.into_iter().rev() {
            match prev {
                Some(w) => self.param_widths.borrow_mut().insert(name, w),
                None => self.param_widths.borrow_mut().remove(&name),
            };
        }
        self.self_signal.set(saved_self);
        for name in added_integers {
            self.param_integers.borrow_mut().remove(&name);
        }
        self.inline_depth.set(self.inline_depth.get() - 1);
        out
    }

    /// Lower a method call used as a *statement* (`s.send(v)`): inline the
    /// method's body as drivers, substituting `self` -> receiver and each
    /// parameter -> its argument, so a body of `self.valid = '1'; self.data =
    /// value;` drives the receiver's flattened field signals. Returns `false`
    /// when the receiver's type or the method can't be resolved (the caller
    /// then leaves the statement to the existing fall-through).
    /// The body of a method call in statement position, with `self` and the
    /// parameters substituted — shared by the combinational and sequential
    /// walkers so a call means the same thing in both. `None` when the call is
    /// not a known method with a body.
    fn method_stmt_body(
        &mut self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
    ) -> Option<Vec<ast::Stmt>> {
        let ty = self.operand_type_name(recv)?;
        // `f` borrows the AST (`'a`), not `self`, so it survives the `&mut self`
        // lowering calls below.
        let input = args.first().and_then(|arg| match arg {
            ast::Expr::Construct { ty: Some(ty), .. } if type_head_name(ty) == Some("Range") => {
                Some("Range".to_string())
            }
            _ => self.operand_type_name(arg),
        });
        let f = self.find_method(&ty, method, input.as_deref())?;
        let body = f.body.as_ref()?;
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        map.insert("self".to_string(), recv.clone());
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                map.insert(n.text.clone(), a.clone());
            }
        }
        Some(
            body.stmts
                .iter()
                .map(|s| subst_stmt_paths(s, &map))
                .collect(),
        )
    }

    /// Inline a method call in statement position as combinational drivers.
    fn lower_method_stmt(
        &mut self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
        cond: Option<Expr>,
    ) -> bool {
        let Some(stmts) = self.method_stmt_body(recv, method, args) else {
            return false;
        };
        let span = ast::expr_span(recv);
        self.lower_combinational_block(&ast::Block { stmts, span }, cond);
        true
    }

    /// Inline a free function called in statement position. This is the
    /// procedure-shaped counterpart of `lower_free_call`: parameters are
    /// substituted with their concrete expressions, then assignments and
    /// nested method calls are lowered as ordinary drivers.
    fn free_stmt_body(&mut self, callee: &ast::Expr, args: &[ast::Expr]) -> Option<Vec<ast::Stmt>> {
        let f = self.free_fns.get(callee)?;
        let body = f.body.as_ref()?;
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|param| !param.is_self).zip(args) {
            if let Some(name) = &param.name {
                map.insert(name.text.clone(), arg.clone());
            }
        }
        Some(
            body.stmts
                .iter()
                .map(|stmt| subst_stmt_paths(stmt, &map))
                .collect(),
        )
    }

    /// Inline a free call in statement position as combinational drivers.
    fn lower_free_stmt(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        cond: Option<Expr>,
    ) -> bool {
        let Some(stmts) = self.free_stmt_body(callee, args) else {
            return false;
        };
        let span = ast::expr_span(callee);
        self.lower_combinational_block(&ast::Block { stmts, span }, cond);
        true
    }

    /// Lower a conversion expression (spec 3.17): `unsigned[16](x)` resizes,
    /// `signed[8](x)` truncates, `integer(x)` crosses to the kernel word, and
    /// `resize(x, n)` is the family-preserving spelling (n const-evaluable —
    /// the language is static, so a value argument in width position is a
    /// generic argument). Semantics on the word IR: an `signed`-family source
    /// sign-extends into the full word first (`v - 2^w` when the sign bit is
    /// set); the target width truncates via a slice; widening to `unsigned`
    /// zero-extends implicitly. `None` when `callee` is not a conversion.
    fn lower_conversion(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        // A field-less struct derived from a scalar kernel type is a nominal
        // newtype with the same representation. Its constructor is therefore
        // value-transparent (`time(v)`, `frequency(v)`), just as derivation is;
        // the target signal supplies any required real coercion.
        if let ast::Expr::Path(p) = callee {
            if let Some(target) = self.free_fns.struct_path_key(p) {
                let scalar_newtype = self.structs.get(&target).is_some_and(|s| {
                    s.fields.is_empty()
                        && ["integer", "real", "Char"].iter().any(|kernel| {
                            struct_derives_kernel(&target, kernel, &self.structs, &self.free_fns)
                        })
                });
                if scalar_newtype {
                    return Some(self.lower_scalar_env(args.first()?, env));
                }
                // A field-less struct over a *vector* (`struct Byte(unsigned[8])`)
                // is the same idea at a width: the constructor keeps the value
                // and the type fixes how many bits of it there are. Without
                // this `Byte(200)` matched no conversion shape at all and
                // lowered to `Unknown`.
                if let Some(base) = self
                    .structs
                    .get(&target)
                    .filter(|s| s.fields.is_empty())
                    .and_then(|s| s.base.as_ref())
                {
                    if matches!(base, ast::Type::Indexed { .. }) {
                        let w = type_width(
                            base,
                            &self.cur_env,
                            &self.free_fns,
                            &self.structs,
                            &self.const_ranges,
                        );
                        let v = self.lower_scalar_env(args.first()?, env);
                        return Some(if w > 0 && w < 64 {
                            Expr::Slice {
                                base: Box::new(v),
                                hi: w - 1,
                                lo: 0,
                            }
                        } else {
                            v
                        });
                    }
                }
            }
        }
        // Target: (is_resize, family, width). Width None = kernel integer.
        let head = |e: &ast::Expr| match e {
            ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
            _ => None,
        };
        let (target_w, resize) = match callee {
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "integer" => {
                (None, false)
            }
            // `Char(n)`: a code point becomes a symbol (32-bit storage).
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "Char" => {
                (Some(32), false)
            }
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "resize" => {
                let n = args.get(1)?;
                let w = match self.lower_scalar_env(n, env) {
                    Expr::Const(c) => c as u32,
                    _ => self.eval_const(n, &self.cur_env)? as u32,
                };
                (Some(w), true)
            }
            ast::Expr::Index { base, index, .. }
                if head(base)
                    .as_deref()
                    .is_some_and(|h| self.vector_families.contains(h)) =>
            {
                let w = match self.lower_scalar_env(index, env) {
                    Expr::Const(c) => c as u32,
                    _ => self.eval_const(index, &self.cur_env)? as u32,
                };
                (Some(w), false)
            }
            _ => return None,
        };
        let _ = resize;
        let arg = args.first()?;
        // Conversions are a raw resize (zero-extend / truncate). Signed
        // widening is the library `std::bits::sext`, not the compiler's job.
        let mut v = self.lower_scalar_env(arg, env);
        // ...except crossing out of `real`, which is a value conversion: the
        // operand carries f64 bits, and resizing them keeps a mantissa slice
        // rather than the number.
        if self.is_real_expr(&v) {
            v = Expr::Unary {
                op: UnOp::RealToInt,
                rhs: Box::new(v),
            };
        }
        Some(match target_w {
            Some(w) if w > 0 && w < 64 => Expr::Slice {
                base: Box::new(v),
                hi: w - 1,
                lo: 0,
            },
            _ => v,
        })
    }

    /// The type name an operand contributes to operator-impl lookup: a local's
    /// declared enum/struct, a suffix literal's target type, an enum variant's
    /// enum, or `integer` for a bare numeric literal.
    fn operand_type_name(&self, e: &ast::Expr) -> Option<String> {
        if let Some(ty) = self.block_local_type(e) {
            return self.free_fns.type_head_key(&ty);
        }
        match e {
            // A branch-valued expression is whatever its branches are; the
            // checker has already made them agree. Without this an `if`/`match`
            // over `signed` values had no family and compared unsigned, while
            // a struct field or a conversion in the same position did not.
            ast::Expr::IfExpr { then, els, .. } => self
                .operand_type_name(then)
                .or_else(|| self.operand_type_name(els)),
            ast::Expr::Match { arms, .. } => arms
                .iter()
                .filter_map(|a| a.value_expr())
                .find_map(|v| self.operand_type_name(v)),
            // An arithmetic or shift expression is whatever its operands are;
            // the checker has already required them to agree. Comparisons and
            // the logical operators yield `Bool` and so carry no numeric
            // family, and a custom operator's result comes from its impl.
            //
            // Without this a binary expression had no family at all, so
            // `print!("{}", a / b)` rendered a `signed` result as unsigned
            // (-3 came out as 253) while `let q: signed[8] = a / b;` — the
            // same value, merely bound first — printed correctly.
            // `not x` is whatever `x` is, in hardware as in the testbench.
            ast::Expr::Unary { rhs, .. } => self.operand_type_name(rhs),
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                let l = self.operand_type_name(lhs);
                let r = self.operand_type_name(rhs);
                // An integer literal takes the family of the other side, so
                // `0 - q` reads as `q`'s family rather than plain `integer`.
                match (l, r) {
                    (Some(l), _) if l != "integer" => Some(l),
                    (_, Some(r)) if r != "integer" => Some(r),
                    (l, r) => l.or(r),
                }
            }
            ast::Expr::Int { .. } => Some("integer".to_string()),
            ast::Expr::SuffixLit { suffix, .. } => self
                .suffix_impls
                .get(&suffix.text)
                .map(|(ty, _)| ty.clone()),
            // A conversion expression `F[N](x)` / `F(x)` reads as its target
            // family, so operators on it dispatch correctly (`signed[32](a) < ..`
            // uses signed's signed Ord).
            ast::Expr::Call { callee, .. } => {
                let head = match callee.as_ref() {
                    ast::Expr::Index { base, .. } => expr_path(base),
                    ast::Expr::Path(p) => self
                        .free_fns
                        .type_owner_key(p)
                        .or_else(|| (p.segments.len() == 1).then(|| p.segments[0].text.clone())),
                    _ => None,
                }?;
                // A conversion reads as its target: a vector family
                // (`signed[32](a)`) or an enum (`ULogic(b)` inside
                // `Logic(ULogic(b))`).
                if self.vector_families.contains(&head) || self.enum_variants.contains_key(&head) {
                    return Some(head);
                }
                // Otherwise it is an ordinary call, and its declared return
                // type is the family. Without this a call had none, so
                // `neg(x) < 0` never dispatched signed's Ord and compared
                // unsigned.
                let ret = self
                    .free_fns
                    .get(callee)
                    .and_then(|f| f.ret.as_ref())
                    .and_then(|ty| self.free_fns.type_head_key(ty))?;
                // A struct return counts too: `twice(v) + v` needs a type for
                // its left operand before any `Operator` impl can be found,
                // and without one the whole expression produced nothing.
                (self.vector_families.contains(&ret)
                    || self.enum_variants.contains_key(&ret)
                    || self.structs.contains_key(&ret))
                .then_some(ret)
            }
            ast::Expr::Path(p) if p.segments.len() >= 2 => self
                .free_fns
                .enum_variant_key(p)
                .map(|(enumeration, _)| enumeration),
            // An *array* element is a signal in its own right and resolves by
            // its flattened name. A *bit* of a packed vector is not, so it
            // reads as the vector's element type — otherwise it had no type
            // at all and no operator impl could be found for it: `v[7] xor
            // v[5]` did not lower, while `v[7] and v[5]` did, because `and`
            // is a built-in with its own lowering and needs no impl.
            ast::Expr::Index { base, .. } => {
                if let Some(name) = expr_path(e) {
                    if let Some(found) = self
                        .local_enum
                        .get(&name)
                        .or_else(|| self.local_struct.get(&name))
                        .or_else(|| self.local_numeric.get(&name))
                    {
                        return Some(found.clone());
                    }
                }
                let family = self.operand_type_name(base)?;
                self.vector_element_enum(&family)
            }
            _ => {
                let p = expr_path(e)?;
                // A generic-fn parameter reads as its caller's concrete family.
                if let Some(fam) = self.param_types.borrow().get(&p) {
                    return Some(fam.clone());
                }
                if self.local_char.contains(&p) {
                    return Some("Char".to_string());
                }
                self.local_enum
                    .get(&p)
                    .or_else(|| self.local_struct.get(&p))
                    .or_else(|| self.local_numeric.get(&p))
                    .cloned()
            }
        }
    }

    /// Read every `return` in `stmts` against the function's declared return
    /// type, so a positional literal returned from a struct-returning function
    /// (`return { 3, 4 }`) is a struct literal rather than the concatenation it
    /// lexes as. Returned as the concat it produced no fields, and the caller's
    /// destination was left undriven.
    ///
    /// Only the shapes the inliner itself understands are walked; anything else
    /// is carried through unchanged.
    fn normalize_struct_returns(
        &self,
        stmts: &[ast::Stmt],
        ret: Option<&ast::Type>,
    ) -> Vec<ast::Stmt> {
        stmts
            .iter()
            .map(|stmt| match stmt {
                ast::Stmt::Return {
                    value: Some(value),
                    span,
                } => ast::Stmt::Return {
                    value: Some(self.as_struct_literal(ret, value)),
                    span: *span,
                },
                ast::Stmt::If(iff) => {
                    let mut iff = iff.clone();
                    iff.then.stmts = self.normalize_struct_returns(&iff.then.stmts, ret);
                    iff.else_ = iff.else_.map(|branch| {
                        Box::new(match *branch {
                            ast::ElseBranch::Block(mut b) => {
                                b.stmts = self.normalize_struct_returns(&b.stmts, ret);
                                ast::ElseBranch::Block(b)
                            }
                            ast::ElseBranch::If(inner) => {
                                let rewritten = self.normalize_struct_returns(
                                    std::slice::from_ref(&ast::Stmt::If(inner.clone())),
                                    ret,
                                );
                                match rewritten.into_iter().next() {
                                    Some(ast::Stmt::If(inner)) => ast::ElseBranch::If(inner),
                                    _ => ast::ElseBranch::If(inner),
                                }
                            }
                        })
                    });
                    ast::Stmt::If(iff)
                }
                ast::Stmt::Match(m) => {
                    let mut m = m.clone();
                    for arm in &mut m.arms {
                        arm.body.stmts = self.normalize_struct_returns(&arm.body.stmts, ret);
                    }
                    ast::Stmt::Match(m)
                }
                other => other.clone(),
            })
            .collect()
    }

    /// The value a straight-line `return`/`if-else` block produces, or `None`
    /// if the block has statements the inliner cannot express as a value.
    fn inline_block(&self, stmts: &[ast::Stmt], env: &HashMap<String, Val>) -> Option<Val> {
        match stmts {
            [ast::Stmt::Return { value: Some(v), .. }, ..] => Some(self.lower_val_env(v, env)),
            [ast::Stmt::If(iff), rest @ ..] => {
                let cond = self.lower_scalar_env(&iff.cond, env);
                let then = self.inline_block(&iff.then.stmts, env)?;
                // The else value: an explicit else branch, or the statements
                // after the if.
                let els = match &iff.else_ {
                    Some(e) => match e.as_ref() {
                        ast::ElseBranch::Block(b) => self.inline_block(&b.stmts, env)?,
                        ast::ElseBranch::If(i) => {
                            self.inline_block(std::slice::from_ref(&ast::Stmt::If(i.clone())), env)?
                        }
                    },
                    None => self.inline_block(rest, env)?,
                };
                Some(select_val(cond, then, els))
            }
            // A `match` whose arms return is the same shape as an `if`
            // chain, and only the `if` form was handled — the two share
            // `MatchArm` and have drifted apart repeatedly. First-match
            // priority comes from folding the arms in reverse.
            [ast::Stmt::Match(m), rest @ ..] => {
                let scrut = self.lower_scalar_env(&m.scrutinee, env);
                // What the body yields when no arm returns.
                let after = self.inline_block(rest, env);
                let mut acc: Option<Val> = after.clone();
                for arm in m.arms.iter().rev() {
                    let value = match self.inline_block(&arm.body.stmts, env) {
                        Some(value) => value,
                        // An arm that returns nothing (`_ => {}`) falls
                        // through to the statements after the match.
                        None => after.clone()?,
                    };
                    acc = Some(
                        match (
                            self.arm_match_cond(&arm.pattern, &m.scrutinee, &scrut, env),
                            acc,
                        ) {
                            // A wildcard covers everything that follows it.
                            (None, _) => value,
                            // Nothing follows: an exhaustive match ends here, so
                            // this arm is the fallback.
                            (Some(_), None) => value,
                            (Some(cond), Some(otherwise)) => select_val(cond, value, otherwise),
                        },
                    );
                }
                acc
            }
            // `let t: T = expr;` names a value for the statements that
            // follow. Without this arm the body matched neither shape and the
            // whole call lowered to an `Unknown` — and silently, because
            // `check` and `--emit ir` both pass on it and only code
            // generation reports the unlowered driver.
            [ast::Stmt::Let(l), rest @ ..] => {
                let value = l.value.as_ref()?;
                let mut scoped = env.clone();
                scoped.insert(l.name.text.clone(), self.lower_val_env(value, env));
                scoped.insert(
                    format!("{}::length", l.name.text),
                    Val::Scalar(Expr::Const(self.ast_width(value) as u64)),
                );
                self.inline_block(rest, &scoped)
            }
            _ => None,
        }
    }

    /// Lower an expression to a [`Val`], with fn parameters substituted from
    /// `env`. Struct-typed locals and struct literals become per-field values.
    /// `not` over a bit vector: every bit inverted, lowered as `mask - x` so
    /// no engine needs width knowledge. `None` when the operand is not a
    /// vector reference — a 1-bit operand keeps the boolean form (identical
    /// either way), and a compound expression or enum-typed signal has its
    /// own `not`.
    fn vector_not(&self, rhs: &ast::Expr, lower: impl Fn(&ast::Expr) -> Expr) -> Option<Expr> {
        let is_vector_ref = match rhs {
            // A slice is always a bit vector.
            ast::Expr::Index { base, index, .. } if self.slice_bounds(base, index).is_some() => {
                true
            }
            ast::Expr::Path(_) | ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                self.block_local_type(rhs)
                    .and_then(|ty| self.free_fns.type_head_key(&ty))
                    .is_some_and(|family| self.vector_families.contains(&family))
                    || expr_path(rhs)
                        .and_then(|p| self.locals.get(&p))
                        .map(|&id| self.out.signals[id.0 as usize].enum_type.is_none())
                        .unwrap_or(false)
            }
            _ => false,
        };
        if !is_vector_ref {
            return None;
        }
        let w = self.ast_width(rhs);
        (w > 1 && w <= 64).then(|| {
            let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
            // `x xor all-ones`, not `all-ones - x`. The two agree bit for bit
            // on two-valued data, but a subtraction makes the metavalue
            // companion poison the whole vector, so `not "0000X100"` came back
            // all `'X'` where `std_logic_1164` inverts per element and leaves
            // `1111X011`.
            Expr::Binary {
                op: BinOp::Xor,
                lhs: Box::new(lower(rhs)),
                rhs: Box::new(Expr::Const(mask)),
            }
        })
    }

    fn lower_val_env(&self, e: &ast::Expr, env: &HashMap<String, Val>) -> Val {
        match e {
            // `self::length` inside an operator-impl body: the bound operand's
            // width (inline_op stashes it under the "param::attr" key).
            ast::Expr::SysAttr { base, attr, .. } => {
                if let Some(v) =
                    expr_path(base).and_then(|p| env.get(&format!("{p}::{}", attr.text)))
                {
                    return v.clone();
                }
                Val::Scalar(self.lower_expr(e))
            }
            ast::Expr::IfExpr {
                cond, then, els, ..
            } => {
                let c = self.lower_scalar_env(cond, env);
                select_val(
                    c,
                    self.lower_val_env(then, env),
                    self.lower_val_env(els, env),
                )
            }
            // A match *expression* whose arms are struct values, folded per
            // field the way `IfExpr` above is. Without this it fell through to
            // the scalar lowering, which builds one `Select` chain over whole
            // values and has nowhere to put a struct — so a decoder written as
            // `c = match op { .. => Ctrl { .. } }` drove nothing at all, and
            // said so only as "never driven" on each field. The equivalent
            // `if`-expression and the statement form both worked.
            ast::Expr::Match {
                scrutinee, arms, ..
            } => self.lower_match_val(scrutinee, arms, env),
            ast::Expr::Call { callee, args, .. } => {
                // `T()` — the nullary constructor — resolves to the type's
                // default (a struct yields per-field values) before any
                // free-fn/conversion lookup.
                if let Some(v) = self.lower_new(callee, args) {
                    return v;
                }
                match self
                    .lower_conversion(callee, args, env)
                    .map(Val::Scalar)
                    .or_else(|| self.lower_free_call(callee, args, env))
                {
                    Some(v) => v,
                    None => match self
                        .lower_method_call(callee, args, env)
                        .or_else(|| self.lower_from(callee, args, env))
                    {
                        Some(v) => v,
                        None => Val::Scalar(self.lower_expr(e)),
                    },
                }
            }
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let name = &p.segments[0].text;
                if let Some(v) = env.get(name) {
                    return v.clone();
                }
                if let Some(value) = self.block_local_value(e) {
                    return value;
                }
                if let Some(v) = self.aggregate_signal_val(name) {
                    return v;
                }
                // A struct constant read whole (`p = K`). Its fields live in
                // the constant table under dotted paths, so it has no signal
                // and no scalar form — as a value it is the fields themselves,
                // which is what an assignment expands per leaf.
                if let Some(fields) = self.const_struct_value(name) {
                    return Val::Fields(fields);
                }
                Val::Scalar(self.lower_expr(e))
            }
            // `self.re` where `self` is an env-bound struct value.
            ast::Expr::Field { base, field, .. } => {
                if let Some(value) = self.block_local_value(e) {
                    return value;
                }
                if let ast::Expr::Path(p) = base.as_ref() {
                    if p.segments.len() == 1 {
                        if let Some(Val::Fields(fs)) = env.get(&p.segments[0].text) {
                            let v = fs
                                .iter()
                                .find(|(n, _)| *n == field.text)
                                .map(|(_, e)| e.clone())
                                .unwrap_or(Expr::Unknown);
                            return Val::Scalar(v);
                        }
                    }
                }
                if let Some(value) = self.nested_aggregate_val(e) {
                    return value;
                }
                Val::Scalar(self.lower_expr(e))
            }
            // A struct literal (named or name-less): one value per field.
            // Explicit `.re = v` binds by name; a positional arg binds to the
            // struct's field at that position (needs a named struct type).
            ast::Expr::Construct {
                ty,
                args,
                spread,
                span,
            } => {
                // Field order comes from the construct's type, or (for a
                // name-less `{ ..base, .. }`) from the spread base's struct type.
                let struct_name: Option<String> = ty
                    .as_ref()
                    .and_then(|ty| self.free_fns.type_head_key(ty))
                    .or_else(|| {
                        spread
                            .as_ref()
                            .and_then(|b| expr_path(b))
                            .and_then(|p| self.local_struct_repr.get(&p).cloned())
                    });
                let field_order: Option<Vec<String>> = struct_name
                    .as_deref()
                    .and_then(|n| self.raw_struct_fields(n))
                    .map(|fs| fs.into_iter().map(|(n, _)| n).collect());
                let mut fields: Vec<(String, Expr)> = Vec::new();
                // `{ ..base, .. }`: seed every field from `base` before overrides.
                if let (Some(base), Some(sname)) = (spread, struct_name.as_deref()) {
                    // Copy per *leaf*, not per top-level field: a field that is
                    // itself a struct has no scalar form, so reading it whole
                    // would yield `Unknown` and silently drop everything nested
                    // that the spread was supposed to carry over.
                    for leaf in self.struct_leaf_names(sname) {
                        let mut fe = (**base).clone();
                        for part in leaf.split('.') {
                            fe = ast::Expr::Field {
                                base: Box::new(fe),
                                field: ast::Ident {
                                    text: part.to_string(),
                                    span: *span,
                                },
                                span: *span,
                            };
                        }
                        let v = self.lower_scalar_env(&fe, env);
                        fields.push((leaf, v));
                    }
                }
                for (i, a) in args.iter().enumerate() {
                    let fname = match &a.field {
                        Some(f) => f.text.clone(),
                        None => field_order
                            .as_ref()
                            .and_then(|o| o.get(i).cloned())
                            .unwrap_or_default(),
                    };
                    // Every field carries a value; a value-less arg only reaches
                    // here on parser recovery (already diagnosed).
                    // A field holding a struct of its own (composition:
                    // `Outer { .inner = Inner { .a = v } }`) lowers to its own
                    // `Fields`, which has no scalar form. Splice those in under
                    // a dotted name so the flat map still addresses one leaf per
                    // entry — `inner.a` then resolves against the flattened
                    // signal `…o.inner.a` exactly as a top-level field does.
                    // A struct-typed field reads its value against that
                    // field's type, so a positional literal nested in a named
                    // one is itself a struct literal rather than the concat it
                    // lexes as.
                    let field_ty = struct_name
                        .as_deref()
                        .and_then(|n| self.structs.get(n))
                        .and_then(|sd| sd.fields.iter().find(|f| f.name.text == fname))
                        .map(|f| f.ty.clone());
                    let vals: Vec<(String, Expr)> = match &a
                        .value
                        .as_ref()
                        .map(|v| self.as_struct_literal(field_ty.as_ref(), v))
                    {
                        Some(v) => match self.lower_val_env(v, env) {
                            Val::Scalar(e) => vec![(fname, e)],
                            Val::Fields(inner) => inner
                                .into_iter()
                                .map(|(n, e)| (format!("{fname}.{n}"), e))
                                .collect(),
                        },
                        None => vec![(fname, Expr::Unknown)],
                    };
                    for (fname, v) in vals {
                        match fields.iter_mut().find(|(n, _)| *n == fname) {
                            Some(slot) => slot.1 = v,
                            None => fields.push((fname, v)),
                        }
                    }
                }
                Val::Fields(fields)
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                let op_str = crate::syntax::pretty::bin_op(op);
                if let Some(native) =
                    self.native_vector_logical(op_str, lhs, rhs, &|e| self.lower_scalar_env(e, env))
                {
                    return Val::Scalar(native);
                }
                if !matches!(op_str, "==" | "!=") {
                    if let Some(v) = self.inline_op(op_str, lhs, rhs, env) {
                        return v;
                    }
                }
                if let Some(derived) = self.inline_cmp(op_str, lhs, rhs, env) {
                    return Val::Scalar(derived);
                }
                let (l, r) = (
                    self.lower_scalar_env(lhs, env),
                    self.lower_scalar_env(rhs, env),
                );
                Val::Scalar(self.make_binary(
                    op.clone(),
                    l,
                    r,
                    self.binary_uses_kernel_integer(lhs, rhs),
                    self.declares_kernel_integer(lhs) || self.declares_kernel_integer(rhs),
                ))
            }
            ast::Expr::Unary { op, rhs, .. } => {
                // `not x` on an enum operand inlines its impl, as it does in
                // `lower_expr`. Building a raw unary here negated the
                // discriminant instead of consulting `Logic`'s table, so
                // `(not a) and b` gave '1' where 'X' was meant — while
                // `let t = not a; t and b`, the same thing named, was right.
                if *op == ast::UnOp::Not {
                    if let Some(v) = self.inline_unary("not", rhs) {
                        return v;
                    }
                    // `not` on a vector is `mask - x`, not a bitwise
                    // complement. `lower_expr` knew that and this path did
                    // not, so `unsigned[8](not s)` — the same operand inside
                    // a conversion — lowered to a raw `not` and read 0 where
                    // the bare `not s` gave 55.
                    if let Some(v) = self.vector_not(rhs, |e| self.lower_scalar_env(e, env)) {
                        return Val::Scalar(v);
                    }
                }
                Val::Scalar(self.make_unary(*op, self.lower_scalar_env(rhs, env)))
            }
            ast::Expr::SuffixLit { .. } => self.inline_suffix(e).unwrap_or_else(|| {
                Val::Scalar(self.lower_expr(e)) // fixed fs/Hz table fallback
            }),
            _ => self
                .nested_aggregate_val(e)
                .unwrap_or_else(|| Val::Scalar(self.lower_expr(e))),
        }
    }

    /// Inline the `impl Suffix<sym, _> for T` `suffix` fn for a suffixed
    /// literal (`5i` -> the `"i"` impl's body): its parameter binds to the
    /// literal value.
    fn inline_suffix(&self, e: &ast::Expr) -> Option<Val> {
        let ast::Expr::SuffixLit { text, suffix, .. } = e else {
            return None;
        };
        let (_, f) = self.suffix_impls.get(&suffix.text)?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                // A `real` parameter takes the literal's float value.
                let is_real = p.ty.as_ref().and_then(type_head_name) == Some("real");
                let v = if is_real {
                    Expr::Real(text.replace('_', "").parse().unwrap_or(0.0))
                } else {
                    Expr::Const(parse_int(text).unwrap_or(0))
                };
                env.insert(n.text.clone(), Val::Scalar(v));
            }
        }
        self.inline_block(&body.stmts, &env)
    }

    fn lower_scalar_env(&self, e: &ast::Expr, env: &HashMap<String, Val>) -> Expr {
        match self.lower_val_env(e, env) {
            Val::Scalar(e) => e,
            Val::Fields(_) => Expr::Unknown, // a struct value has no scalar context
        }
    }

    /// The per-field value of a struct-typed local (`p` -> `p.re`, `p.im`).
    fn struct_local_val(&self, name: &str) -> Option<Val> {
        let sname = self.local_struct_repr.get(name)?;
        let s = self.structs.get(sname)?;
        Some(Val::Fields(
            s.fields
                .iter()
                .map(|f| {
                    let sig = self.locals.get(&format!("{name}.{}", f.name.text));
                    (
                        f.name.text.clone(),
                        sig.map(|&id| Expr::Current(id)).unwrap_or(Expr::Unknown),
                    )
                })
                .collect(),
        ))
    }

    /// A flattened struct or array signal as one aggregate value. Scanning
    /// leaf names also handles nested structs/arrays, where no signal exists
    /// for an intermediate field.
    /// A *nested* aggregate read whole: `outer.inner`, `w[0]` where `w` is an
    /// array of structs. Only a bare name reached `aggregate_signal_val`, so
    /// these were lowered as ordinary scalars -- which an aggregate has none of
    /// -- and reported "has no hardware form", a message about runtime indices
    /// on a path whose indices are literal. Writing one already worked, so a
    /// struct array element could be assigned but not read back.
    fn nested_aggregate_val(&self, e: &ast::Expr) -> Option<Val> {
        let path = self.folded_elem_path(e)?;
        self.aggregate_signal_val(&path)
    }

    fn aggregate_signal_val(&self, name: &str) -> Option<Val> {
        if !self.local_struct_repr.contains_key(name) && !self.local_array.contains_key(name) {
            return None;
        }
        let field_prefix = format!("{name}.");
        let element_prefix = format!("{name}[");
        let mut fields: Vec<(String, Expr)> = self
            .locals
            .iter()
            .filter(|(path, _)| {
                path.starts_with(&field_prefix) || path.starts_with(&element_prefix)
            })
            .map(|(path, &signal)| {
                (
                    path.strip_prefix(name)
                        .unwrap_or(path)
                        .trim_start_matches('.')
                        .to_string(),
                    Expr::Current(signal),
                )
            })
            .collect();
        if fields.is_empty() {
            return self.struct_local_val(name);
        }
        fields.sort_by(|left, right| left.0.cmp(&right.0));
        Some(Val::Fields(fields))
    }

    /// The declared index range `(left, right)` in written order of a vector or
    /// array type — `Logic[7..0]` -> `(7, 0)`, a named `range` const keeps its
    /// direction, a width-only `Bit[4]` -> `(0, 3)` (ascending). `None` for a
    /// non-indexed type.
    fn declared_range(&self, ty: &ast::Type, env: &HashMap<String, i64>) -> Option<(i64, i64)> {
        let ast::Type::Indexed {
            index: Some(idx), ..
        } = ty
        else {
            return None;
        };
        match idx.as_ref() {
            // A written range keeps its direction (`[7..0]` is descending); the
            // `Range` fields are first/second as written, not numerically sorted.
            ast::Expr::Range { lo, hi, .. } => {
                Some((self.eval_const(lo, env)?, self.eval_const(hi, env)?))
            }
            // A named range constant (`const BYTE: range = 7..0;`).
            ast::Expr::Path(path) => {
                if let Some(bounds) = self
                    .free_fns
                    .constant_path_key(path)
                    .and_then(|key| self.const_ranges.get(&key).copied())
                {
                    Some(bounds)
                } else {
                    let n = self.eval_const(idx, env)?;
                    Some((0, (n - 1).max(0)))
                }
            }
            // A width-only index (`Bit[4]`, `unsigned[8]`) is ascending `0..N-1`.
            _ => {
                let n = self.eval_const(idx, env)?;
                Some((0, (n - 1).max(0)))
            }
        }
    }

    /// Lower a system attribute. `clk.rising()`/`falling`/`edge` expand into
    /// `Event`/`Old`/`Current` so the scheduler needs no special knowledge.
    fn persisted_layout(&self, local_path: &str) -> Option<&SourceLayout> {
        self.out
            .source_layouts
            .get(&format!("{}.{}", self.cur_instance_path, local_path))
    }

    fn persisted_range(&self, local_path: &str) -> Option<(i64, i64)> {
        self.persisted_layout(local_path)
            .and_then(SourceLayout::index_range)
            .map(|range| (range.left, range.right))
    }

    fn lower_sysattr(&self, base: &ast::Expr, attr: &str) -> Expr {
        // `::length` is elaboration-time metadata: an array's element count,
        // else a signal's bit width (they coincide for a flat vector, so one
        // attribute serves both — VHDL's `'length`).
        if attr == "length" {
            if let Some(ty) = self.block_local_type(base) {
                if let Some((_, indices)) = array_of(
                    &ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.vector_families,
                    &self.free_fns,
                ) {
                    return Expr::Const(indices.len() as u64);
                }
                return Expr::Const(self.block_local_width(&ty) as u64);
            }
            if let Some(layout) = expr_path(base).and_then(|path| self.persisted_layout(&path)) {
                let length = match &layout.kind {
                    LayoutKind::Array { range, .. } => range.and_then(LayoutRange::len),
                    LayoutKind::Packed { width, .. } | LayoutKind::Scalar { width, .. } => {
                        Some(u64::from(*width))
                    }
                    LayoutKind::Opaque { width, .. } => width.map(u64::from),
                    LayoutKind::Struct { .. } => None,
                };
                if let Some(length) = length {
                    return Expr::Const(length);
                }
            }
            if let Some(sig) = self.base_signal(base) {
                return Expr::Const(self.out.signals[sig.0 as usize].width as u64);
            }
            return Expr::Unknown;
        }
        // Range bounds from the declared index range (VHDL `'left`/`'right`/
        // `'high`/`'low`/`'ascending`): `left`/`right` in written order,
        // `high`/`low` numeric, `ascending` the direction (`to` vs `downto`).
        if matches!(attr, "left" | "right" | "high" | "low" | "ascending") {
            let local_declared = self
                .block_local_type(base)
                .and_then(|ty| self.declared_range(&ty, &self.cur_env));
            if let Some((l, r)) = local_declared
                .or_else(|| expr_path(base).and_then(|path| self.persisted_range(&path)))
            {
                let v = match attr {
                    "left" => l,
                    "right" => r,
                    "high" => l.max(r),
                    "low" => l.min(r),
                    "ascending" => (l <= r) as i64,
                    _ => unreachable!(),
                };
                return Expr::Const(v as u64);
            }
            return Expr::Unknown;
        }
        let Some(sig) = self.base_signal(base) else {
            // An aggregate has no signal of its own — elaboration flattens it
            // into one leaf per field or element. `'old` still lands on a leaf
            // (`p'old.data` sinks to `p.data'old`, `a'old[0]` indexes first),
            // but `'event` has nothing to sink to and returned `Unknown`, so
            // `p'event` and `a'event` failed to lower at all. The spec defines
            // both: "any field changed" / "any element changed" — which is the
            // OR over the leaves.
            if attr == "event" {
                if let Some(leaves) = self.aggregate_leaves(base) {
                    return leaves
                        .into_iter()
                        .map(Expr::Event)
                        .reduce(or_expr)
                        .unwrap_or(Expr::Const(0));
                }
            }
            return Expr::Unknown;
        };
        match attr {
            // `::event`/`::old` are the primitives; the edge helpers are the
            // std `ClockLike` methods, which inline to these plus a comparison.
            "event" => Expr::Event(sig),
            "old" => Expr::Old(sig),
            _ => Expr::Unknown,
        }
    }

    /// The leaf signals a struct or array path flattens into, in signal order
    /// so the lowered expression is stable. `None` when the path names no
    /// aggregate (an unknown name, or a scalar handled by `base_signal`).
    fn aggregate_leaves(&self, base: &ast::Expr) -> Option<Vec<SignalId>> {
        let path = expr_path(base)?;
        let (field, element) = (format!("{path}."), format!("{path}["));
        let mut leaves: Vec<SignalId> = self
            .locals
            .iter()
            .filter(|(name, _)| name.starts_with(&field) || name.starts_with(&element))
            .map(|(_, id)| *id)
            .collect();
        if leaves.is_empty() {
            return None;
        }
        leaves.sort_by_key(|id| id.0);
        Some(leaves)
    }

    /// One position of an elementwise array expression: every path naming an
    /// array of the same length becomes that array's `k`-th element, so
    /// `a and b` at position 0 is `a[a0] and b[b0]` — paired by position, so
    /// a descending range keeps its own indices. `None` when an operand is
    /// not such an array, which leaves the existing paths to report it.
    fn elementwise_at(&self, e: &ast::Expr, k: usize, len: usize) -> Option<ast::Expr> {
        match e {
            ast::Expr::Path(_) => {
                let indices = self.local_array.get(&expr_path(e)?)?;
                let index = *indices.get(k).filter(|_| indices.len() == len)?;
                if index < 0 {
                    return None;
                }
                let span = ast::expr_span(e);
                Some(ast::Expr::Index {
                    base: Box::new(e.clone()),
                    index: Box::new(ast::Expr::Int {
                        text: index.to_string(),
                        span,
                    }),
                    span,
                })
            }
            ast::Expr::Binary { op, lhs, rhs, span } => Some(ast::Expr::Binary {
                op: op.clone(),
                lhs: Box::new(self.elementwise_at(lhs, k, len)?),
                rhs: Box::new(self.elementwise_at(rhs, k, len)?),
                span: *span,
            }),
            // The condition is a scalar, so it is shared by every element;
            // only the branches are per-element. `y = if c { a } else { b }`
            // on an array had no form at all and reported the *target* as
            // unassignable, the same misleading shape the operators had.
            ast::Expr::IfExpr {
                cond,
                then,
                els,
                span,
            } => Some(ast::Expr::IfExpr {
                cond: cond.clone(),
                then: Box::new(self.elementwise_at(then, k, len)?),
                els: Box::new(self.elementwise_at(els, k, len)?),
                span: *span,
            }),
            // `match` selects a whole branch the way `if` does; the two
            // share `MatchArm` and have drifted apart before, so they are
            // lifted together here.
            ast::Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                let mut lifted = Vec::with_capacity(arms.len());
                for a in arms {
                    let value = self.elementwise_at(a.value_expr()?, k, len)?;
                    lifted.push(ast::MatchArm {
                        pattern: a.pattern.clone(),
                        body: ast::Block {
                            stmts: vec![ast::Stmt::Expr(value)],
                            span: a.body.span,
                        },
                        span: a.span,
                    });
                }
                Some(ast::Expr::Match {
                    scrutinee: scrutinee.clone(),
                    arms: lifted,
                    span: *span,
                })
            }
            ast::Expr::Unary { op, rhs, span } => Some(ast::Expr::Unary {
                op: *op,
                rhs: Box::new(self.elementwise_at(rhs, k, len)?),
                span: *span,
            }),
            _ => None,
        }
    }

    fn base_signal(&self, base: &ast::Expr) -> Option<SignalId> {
        if let ast::Expr::Path(p) = base {
            if p.segments.len() == 1 {
                // `self` inside an inlined method body binds to the receiver.
                if p.segments[0].text == "self" {
                    if let Some(sig) = self.self_signal.get() {
                        return Some(sig);
                    }
                }
                return self.locals.get(&p.segments[0].text).copied();
            }
        }
        // A struct field or array element is a signal in its own right, named
        // by its flattened path (`p.valid`, `xs[0]`).
        self.locals.get(&expr_path(base)?).copied()
    }
}

/// Collect every signal an IR expression reads (`Current`/`Old`/`Event`
/// leaves) into `out`, in first-seen order.
pub fn read_set(e: &Expr, out: &mut Vec<SignalId>) {
    match e {
        // The operands are read through `inner` too; walking both would list
        // them twice and `dedup` is not applied everywhere this feeds.
        Expr::MetaCmp { inner, .. } => read_set(inner, out),
        Expr::Current(id) | Expr::Old(id) | Expr::Event(id) => out.push(*id),
        Expr::CCall { args, .. } => {
            for a in args {
                read_set(a, out);
            }
        }
        Expr::Unary { rhs, .. } => read_set(rhs, out),
        Expr::Binary { lhs, rhs, .. } => {
            read_set(lhs, out);
            read_set(rhs, out);
        }
        Expr::Slice { base, .. } => read_set(base, out),
        Expr::Select { cond, then, els } => {
            read_set(cond, out);
            read_set(then, out);
            read_set(els, out);
        }
        Expr::Const(_) | Expr::WideConst(_) | Expr::Real(_) | Expr::Logic(_) | Expr::Unknown => {}
    }
}

/// Rewrite `Expr::Logic(c)` in place to `Const(position of c in `lut`)` — the
/// std-supplied variant map of the default logic type. Recurses into children.
fn resolve_logic_expr(e: &mut Expr, lut: &HashMap<String, u64>) {
    match e {
        Expr::MetaCmp {
            operands, inner, ..
        } => {
            for operand in operands {
                resolve_logic_expr(operand, lut);
            }
            resolve_logic_expr(inner, lut);
        }
        Expr::Logic(c) => {
            *e = Expr::Const(lut.get(&format!("'{c}'")).copied().unwrap_or(0));
        }
        Expr::Unary { rhs, .. } => resolve_logic_expr(rhs, lut),
        Expr::Binary { lhs, rhs, .. } => {
            resolve_logic_expr(lhs, lut);
            resolve_logic_expr(rhs, lut);
        }
        Expr::Slice { base, .. } => resolve_logic_expr(base, lut),
        Expr::Select { cond, then, els } => {
            resolve_logic_expr(cond, lut);
            resolve_logic_expr(then, lut);
            resolve_logic_expr(els, lut);
        }
        Expr::CCall { args, .. } => {
            for a in args {
                resolve_logic_expr(a, lut);
            }
        }
        Expr::Const(_)
        | Expr::WideConst(_)
        | Expr::Real(_)
        | Expr::Current(_)
        | Expr::Old(_)
        | Expr::Event(_)
        | Expr::Unknown => {}
    }
}

/// Rewrite `Slice(Current(v), i, i)` — one element of a metavalue vector — into
/// its 9-value reconstruction: the companion nibble when it is a metavalue
/// (disc >= 2), else the value bit. Recurses; does not descend into the node it
/// creates (the companion has no companion).
fn reconstruct_expr(e: &mut Expr, meta_of: &HashMap<u32, u32>, elems: &HashMap<u32, u32>) {
    // A marked comparison: apply the `numeric_std` rule now that every
    // companion exists. Done before the generic walk so the operands, which are
    // also present inside `inner`, are not rewritten twice.
    if let Expr::MetaCmp {
        ne,
        operands,
        inner,
    } = e
    {
        let ne = *ne;
        let mut resolved = inner.as_ref().clone();
        reconstruct_expr(&mut resolved, meta_of, elems);
        let unknown = operands
            .iter()
            .filter_map(|operand| companion_read(operand, meta_of))
            .map(|(companion, read)| {
                let n = elems.get(&companion).copied().unwrap_or(0);
                any_unknown(&read, n)
            })
            .reduce(or_expr);
        *e = match unknown {
            // `/=` is the one that goes the other way: unknown operands are
            // definitely not equal.
            Some(unknown) if ne => or_expr(resolved, unknown),
            Some(unknown) => and_expr(resolved, not1(unknown)),
            None => resolved,
        };
        return;
    }
    // A whole-vector comparison with a metavalue operand is false (numeric_std).
    if let Expr::Binary { op, lhs, rhs } = e {
        if matches!(
            op,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::SLt
                | BinOp::SLe
                | BinOp::SGt
                | BinOp::SGe
        ) {
            // Per element, not `companion != 0`: a nibble is non-zero for
            // `'L'` and `'H'` too, and those are a weak 0 and a weak 1 that
            // `numeric_std` compares like any other value. The coarse test made
            // a vector holding a pull-up's `'H'` compare false against
            // everything.
            let cond = [companion_read(lhs, meta_of), companion_read(rhs, meta_of)]
                .into_iter()
                .flatten()
                .map(|(companion, read)| {
                    let elems = elems.get(&companion).copied().unwrap_or(0);
                    any_unknown(&read, elems)
                })
                .reduce(|a, b| Expr::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                });
            if let Some(cond) = cond {
                let orig = e.clone();
                *e = Expr::Select {
                    cond: Box::new(cond),
                    then: Box::new(Expr::Const(0)),
                    els: Box::new(orig),
                };
                return;
            }
        }
    }
    if let Expr::Slice { base, hi, lo } = e {
        if hi == lo {
            if let Expr::Current(vid) | Expr::Old(vid) = base.as_ref() {
                if let Some(&cid) = meta_of.get(&vid.0) {
                    let companion = match base.as_ref() {
                        Expr::Current(_) => Expr::Current(SignalId(cid)),
                        Expr::Old(_) => Expr::Old(SignalId(cid)),
                        _ => unreachable!(),
                    };
                    let elem = *lo;
                    let nibble = Expr::Slice {
                        base: Box::new(companion),
                        hi: 4 * elem + 3,
                        lo: 4 * elem,
                    };
                    let valbit = (**base).clone();
                    let valbit = Expr::Slice {
                        base: Box::new(valbit),
                        hi: *hi,
                        lo: *lo,
                    };
                    *e = Expr::Select {
                        cond: Box::new(Expr::Binary {
                            op: BinOp::Ge,
                            lhs: Box::new(nibble.clone()),
                            rhs: Box::new(Expr::Const(2)),
                        }),
                        then: Box::new(nibble),
                        els: Box::new(valbit),
                    };
                    return;
                }
            }
        }
    }
    match e {
        Expr::Unary { rhs, .. } => reconstruct_expr(rhs, meta_of, elems),
        Expr::Binary { lhs, rhs, .. } => {
            reconstruct_expr(lhs, meta_of, elems);
            reconstruct_expr(rhs, meta_of, elems);
        }
        Expr::Slice { base, .. } => reconstruct_expr(base, meta_of, elems),
        Expr::Select { cond, then, els } => {
            reconstruct_expr(cond, meta_of, elems);
            reconstruct_expr(then, meta_of, elems);
            reconstruct_expr(els, meta_of, elems);
        }
        Expr::CCall { args, .. } => {
            for a in args {
                reconstruct_expr(a, meta_of, elems);
            }
        }
        _ => {}
    }
}

/// Preserve the temporal plane of a value read when looking up its metavalue
/// companion. `old(v)` must inspect `old(v$meta)`, not the current companion.
fn companion_read(expr: &Expr, meta_of: &HashMap<u32, u32>) -> Option<(u32, Expr)> {
    match expr {
        Expr::Current(id) => meta_of
            .get(&id.0)
            .copied()
            .map(|companion| (companion, Expr::Current(SignalId(companion)))),
        Expr::Old(id) => meta_of
            .get(&id.0)
            .copied()
            .map(|companion| (companion, Expr::Old(SignalId(companion)))),
        _ => None,
    }
}

/// The source spelling of a free or associated function path: a bare name
/// (`clog2`), a fully qualified module function (`math::bits::clog2`), or
/// `Type::name` for a static associated function (`Unicode::code`). Semantic
/// lookup of free functions uses [`FunctionIndex`] and never this string.
/// `None` only for an empty or non-path callee.
pub fn call_fn_key(callee: &ast::Expr) -> Option<String> {
    let ast::Expr::Path(p) = callee else {
        return None;
    };
    (!p.segments.is_empty()).then(|| {
        p.segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("::")
    })
}

/// The enum-variant names a pattern tree names directly (`State::Idle`, `'0'`),
/// flattening or-patterns. A wildcard names none — the caller treats it
/// separately.
fn collect_named_variants(p: &ast::Pattern, out: &mut std::collections::HashSet<String>) {
    match p {
        ast::Pattern::Path(path) if path.segments.len() >= 2 => {
            out.insert(path.segments.last().expect("variant path").text.clone());
        }
        ast::Pattern::CharLit { ch, .. } => {
            out.insert(format!("'{ch}'"));
        }
        ast::Pattern::Or { alts, .. } => {
            for a in alts {
                collect_named_variants(a, out);
            }
        }
        _ => {}
    }
}

// --- per-element logical metavalue builders (0/1-valued Exprs) --------------

/// `a & b` (bitwise; on 0/1 operands this is logical and).
fn and_expr(a: Expr, b: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(a),
        rhs: Box::new(b),
    }
}
/// `a | b`.
fn or_expr(a: Expr, b: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Or,
        lhs: Box::new(a),
        rhs: Box::new(b),
    }
}
/// Logical `not` of a 0/1 value: `x == 0`.
fn not1(x: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(x),
        rhs: Box::new(Expr::Const(0)),
    }
}
/// Bit `i` of a value expression.
fn bit(e: &Expr, i: u32) -> Expr {
    Expr::Slice {
        base: Box::new(e.clone()),
        hi: i,
        lo: i,
    }
}

fn logic_element_disc(value: &Expr, meta: &Expr, index: u32) -> Expr {
    let nibble = Expr::Slice {
        base: Box::new(meta.clone()),
        hi: 4 * index + 3,
        lo: 4 * index,
    };
    Expr::Select {
        cond: Box::new(Expr::Binary {
            op: BinOp::Ge,
            lhs: Box::new(nibble.clone()),
            rhs: Box::new(Expr::Const(2)),
        }),
        then: Box::new(nibble),
        els: Box::new(bit(value, index)),
    }
}

fn repeat_element_plane(element: Expr, count: u32, stride: u32) -> Expr {
    let mut result = Expr::Const(0);
    for index in 0..count {
        result = or_expr(
            result,
            Expr::Binary {
                op: BinOp::Shl,
                lhs: Box::new(element.clone()),
                rhs: Box::new(Expr::Const((index * stride) as u64)),
            },
        );
    }
    result
}

/// Whether element `i` of a metavalue disc-array is a metavalue (disc >= 2).
/// Is element `i` of the companion an *unknown*?
///
/// `'L'` and `'H'` are not: `std_logic_1164` defines them as a weak 0 and a
/// weak 1, and both its tables and `numeric_std` treat them as such -- `H and 0`
/// is `'0'`, and `"0000H100" + b` is ordinary arithmetic. Testing the whole
/// discriminant range at or above `'Z'` swept them in, so a vector holding a
/// pull-up's `'H'` poisoned every operation applied to it.
///
/// The unknowns are `'Z'`, `'X'`, `'U'`, `'W'` and `'-'`, which are contiguous
/// except for the last -- hence the two-part test rather than one comparison.
fn meta_bit(m: &Option<Expr>, i: u32) -> Expr {
    let Some(m) = m else { return Expr::Const(0) };
    let nibble = Expr::Slice {
        base: Box::new(m.clone()),
        hi: 4 * i + 3,
        lo: 4 * i,
    };
    let cmp = |op, rhs| Expr::Binary {
        op,
        lhs: Box::new(nibble.clone()),
        rhs: Box::new(Expr::Const(rhs)),
    };
    or_expr(
        and_expr(cmp(BinOp::Ge, 2), cmp(BinOp::Le, 5)),
        cmp(BinOp::Eq, 8),
    )
}

/// Does any element of `m` hold an unknown? The arithmetic rule is all-or-
/// nothing per vector, but it must ask the same question per element as the
/// logical rule -- a bare `companion != 0` also fires on `'L'`/`'H'`, whose
/// discriminants are non-zero but whose values are perfectly definite.
fn any_unknown(m: &Expr, width: u32) -> Expr {
    let mut acc = Expr::Const(0);
    for i in 0..width {
        acc = or_expr(acc, meta_bit(&Some(m.clone()), i));
    }
    acc
}
/// Place `'X'` (disc `x_disc`, from std's logic enum) in nibble `i` when
/// `meta_i` (0/1) is set, else 0.
/// Place `disc` in nibble `i` when `meta_i` holds, and nothing otherwise.
/// `disc` is an expression rather than a constant because the discriminant a
/// logical operator produces depends on its operands: `'U'` dominates `'X'`.
fn meta_nibble(meta_i: Expr, i: u32, disc: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Shl,
        lhs: Box::new(Expr::Binary {
            op: BinOp::Mul,
            lhs: Box::new(meta_i),
            rhs: Box::new(disc),
        }),
        rhs: Box::new(Expr::Const(4 * i as u64)),
    }
}

/// Is element `i` of the companion specifically `'U'`? `std_logic_1164` lets
/// uninitialised dominate: where an operand is `'U'` and nothing forces the
/// result, the result is `'U'` and not merely unknown. Reporting `'X'` there
/// loses the distinction between "never driven" and "driven to conflict",
/// which is the whole reason `'U'` exists.
fn u_bit(m: &Option<Expr>, i: u32, u_disc: u64) -> Expr {
    let Some(m) = m else { return Expr::Const(0) };
    Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(Expr::Slice {
            base: Box::new(m.clone()),
            hi: 4 * i + 3,
            lo: 4 * i,
        }),
        rhs: Box::new(Expr::Const(u_disc)),
    }
}

fn dedup(v: &mut Vec<SignalId>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|id| seen.insert(*id));
}

/// Validation walk over an expression (see [`Design::validate`]).
fn check_expr(e: &Expr, n: u32, issues: &mut Vec<String>, ctx: &str) {
    match e {
        // Every one is rewritten once companions are known; one surviving means
        // that pass did not reach it, and the backends have no meaning for it.
        Expr::MetaCmp { .. } => issues.push(format!(
            "{ctx}: contains an unresolved metavalue comparison"
        )),
        Expr::CCall { args, .. } => {
            for a in args {
                check_expr(a, n, issues, ctx);
            }
        }
        Expr::Current(id) | Expr::Old(id) | Expr::Event(id) => {
            if id.0 >= n {
                issues.push(format!("{ctx}: signal id {} out of range (n={n})", id.0));
            }
        }
        Expr::Unknown => issues.push(format!("{ctx}: contains an Unknown (unlowered) expression")),
        Expr::Unary { rhs, .. } => check_expr(rhs, n, issues, ctx),
        Expr::Binary { lhs, rhs, .. } => {
            check_expr(lhs, n, issues, ctx);
            check_expr(rhs, n, issues, ctx);
        }
        Expr::Slice { base, hi, lo } => {
            if lo > hi {
                issues.push(format!("{ctx}: slice bounds lo {lo} > hi {hi}"));
            }
            check_expr(base, n, issues, ctx);
        }
        Expr::Select { cond, then, els } => {
            check_expr(cond, n, issues, ctx);
            check_expr(then, n, issues, ctx);
            check_expr(els, n, issues, ctx);
        }
        Expr::Const(_) | Expr::WideConst(_) | Expr::Real(_) | Expr::Logic(_) => {}
    }
}

/// A unit of behaviour the scheduler dispatches, with its **sensitivity**
/// (the signals it reads) and **write set** (the signals it drives). This is
/// the process view the LLVM backend compiles and the interpreter dispatches
/// on (spec Stage 6 / the compiled-backend plan, B1).
#[derive(Clone, Debug)]
pub struct Process {
    pub kind: ProcessKind,
    /// Signals read by the process's conditions/expressions (sensitivity).
    pub reads: Vec<SignalId>,
    /// Signals the process drives.
    pub writes: Vec<SignalId>,
}

#[derive(Clone, Debug)]
pub enum ProcessKind {
    /// A combinational target, resolved from the drivers that target it, in
    /// source order (spec 3.14 last-writer-wins). `drivers` indexes
    /// `Design::drivers`.
    Comb {
        target: SignalId,
        drivers: Vec<usize>,
    },
    /// A clocked event block. `block` indexes `Design::event_blocks`.
    Event { block: usize },
}

impl Design {
    /// The assignment sites a dynamic range failure (spec 3.26) can be blamed
    /// on: the distinct source spans of the drivers and next-state updates
    /// that write a ranged signal, in lowering order.
    ///
    /// The runtime latches an index into this table plus one, so `0` keeps its
    /// meaning of "no site" and the report falls back to the signal's
    /// declaration. Both engines call *this* to build the table rather than
    /// walking the design themselves: the index is the whole contract between
    /// the value the hardware latches and the string the harness prints, and
    /// two walks that disagree by one entry would misattribute every failure
    /// after the first divergence.
    pub fn range_sites(&self) -> Vec<crate::diag::Span> {
        let mut sites = Vec::new();
        let mut seen = HashSet::new();
        let mut add = |target: SignalId, span: Option<crate::diag::Span>| {
            let ranged = self
                .signals
                .get(target.0 as usize)
                .is_some_and(|s| s.range.is_some());
            if let (true, Some(span)) = (ranged, span) {
                if seen.insert(span) {
                    sites.push(span);
                }
            }
        };
        for driver in &self.drivers {
            add(driver.target, driver.span);
        }
        for block in &self.event_blocks {
            for update in &block.updates {
                add(update.target, update.span);
            }
        }
        sites
    }

    /// Semantic width presented to backends for one flattened signal. When a
    /// persisted source layout exists it is the authority; hand-built IR used
    /// by backend tests remains valid without layout metadata.
    pub fn signal_width(&self, id: SignalId) -> Option<u32> {
        let signal = self.signals.get(id.0 as usize)?;
        match self.source_layouts.get(&signal.path) {
            Some(layout) => layout
                .bit_width()
                .and_then(|width| u32::try_from(width).ok())
                .or_else(|| (signal.width == 0).then_some(0)),
            None => Some(signal.width),
        }
    }

    /// Check the IR is well-formed enough for a backend to compile: signal
    /// ids in range, no `Unknown` (unlowered) expressions, concrete widths,
    /// and valid slice bounds. Returns a list of problems — empty means the
    /// design is safe to hand to codegen. Pure; callers decide how to react.
    pub fn validate(&self) -> Vec<String> {
        let n = self.signals.len() as u32;
        let mut issues = Vec::new();

        // Signals codegen actually touches (driven or read). An unreferenced
        // width-0 signal — e.g. an instance-binding `let` placeholder — is
        // harmless, so only flag unknown widths on referenced signals.
        let mut referenced: std::collections::HashSet<SignalId> = std::collections::HashSet::new();
        let collect = |e: &Expr| {
            let mut v = Vec::new();
            read_set(e, &mut v);
            v
        };
        for d in &self.drivers {
            referenced.insert(d.target);
            if let Some(c) = &d.cond {
                referenced.extend(collect(c));
            }
            referenced.extend(collect(&d.expr));
        }
        for eb in &self.event_blocks {
            referenced.extend(collect(&eb.condition));
            for u in &eb.updates {
                referenced.insert(u.target);
                if let Some(c) = &u.cond {
                    referenced.extend(collect(c));
                }
                referenced.extend(collect(&u.expr));
            }
        }
        for (i, s) in self.signals.iter().enumerate() {
            if s.width == 0 && referenced.contains(&SignalId(i as u32)) {
                issues.push(format!("signal `{}` has unknown width (0)", s.path));
            }
            let Some(layout) = self.source_layouts.get(&s.path) else {
                continue;
            };
            if matches!(
                &layout.kind,
                LayoutKind::Struct { .. } | LayoutKind::Array { .. }
            ) {
                issues.push(format!(
                    "signal `{}` still has an aggregate source layout instead of a flattened leaf",
                    s.path
                ));
                continue;
            }
            match layout
                .bit_width()
                .and_then(|width| u32::try_from(width).ok())
            {
                Some(width) if width != s.width => issues.push(format!(
                    "signal `{}` width {} disagrees with its source layout width {width}",
                    s.path, s.width
                )),
                None if s.width != 0 => issues.push(format!(
                    "signal `{}` has no concrete width in its source layout",
                    s.path
                )),
                _ => {}
            }
        }
        let target = |id: SignalId, what: &str, issues: &mut Vec<String>| {
            if id.0 >= n {
                issues.push(format!(
                    "{what}: target signal id {} out of range (n={n})",
                    id.0
                ));
            }
        };
        // A driver's position in this vector means nothing to the person who
        // wrote the design; the signal it drives is what they can go and look
        // at. "driver 0 expr: contains an Unknown" sent readers to an IR dump
        // to work out which line it meant.
        let name = |id: SignalId| -> String {
            self.signals
                .get(id.0 as usize)
                .map(|s| format!("`{}`", s.path))
                .unwrap_or_else(|| format!("signal id {}", id.0))
        };
        for d in &self.drivers {
            let ctx = format!("the driver for {}", name(d.target));
            target(d.target, &ctx, &mut issues);
            if d.meta.is_some() {
                issues.push(format!(
                    "{ctx}: still carries unexpanded metavalue metadata"
                ));
            }
            if let Some(c) = &d.cond {
                check_expr(c, n, &mut issues, &format!("{ctx} (condition)"));
            }
            check_expr(&d.expr, n, &mut issues, &ctx);
        }
        for (bi, eb) in self.event_blocks.iter().enumerate() {
            // An event block has no single target, so name it by what it
            // updates; the index is the fallback for an empty one.
            let block = match eb.updates.first() {
                Some(u) => format!("the event block updating {}", name(u.target)),
                None => format!("event block {bi}"),
            };
            check_expr(
                &eb.condition,
                n,
                &mut issues,
                &format!("{block} (condition)"),
            );
            for u in &eb.updates {
                let ctx = format!("{block}, update of {}", name(u.target));
                target(u.target, &ctx, &mut issues);
                if u.meta.is_some() {
                    issues.push(format!(
                        "{ctx}: still carries unexpanded metavalue metadata"
                    ));
                }
                if let Some(c) = &u.cond {
                    check_expr(c, n, &mut issues, &format!("{ctx} (condition)"));
                }
                check_expr(&u.expr, n, &mut issues, &ctx);
            }
        }
        issues
    }

    /// The process decomposition: one combinational process per driven signal
    /// (grouping its source-ordered drivers) and one per event block, each
    /// with its sensitivity and write set. Combinational targets keep their
    /// first-seen order so source-order override is preserved.
    pub fn processes(&self) -> Vec<Process> {
        let mut procs = Vec::new();

        // Group combinational drivers by target, first-seen order.
        let mut order: Vec<SignalId> = Vec::new();
        let mut by_target: std::collections::HashMap<SignalId, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, d) in self.drivers.iter().enumerate() {
            by_target.entry(d.target).or_insert_with(|| {
                order.push(d.target);
                Vec::new()
            });
            by_target.get_mut(&d.target).unwrap().push(i);
        }
        for target in order {
            let drivers = by_target.remove(&target).unwrap();
            let mut reads = Vec::new();
            for &di in &drivers {
                let d = &self.drivers[di];
                if let Some(c) = &d.cond {
                    read_set(c, &mut reads);
                }
                read_set(&d.expr, &mut reads);
            }
            dedup(&mut reads);
            procs.push(Process {
                kind: ProcessKind::Comb { target, drivers },
                reads,
                writes: vec![target],
            });
        }

        // One process per event block.
        for (bi, eb) in self.event_blocks.iter().enumerate() {
            let mut reads = Vec::new();
            read_set(&eb.condition, &mut reads);
            let mut writes = Vec::new();
            for u in &eb.updates {
                if let Some(c) = &u.cond {
                    read_set(c, &mut reads);
                }
                read_set(&u.expr, &mut reads);
                writes.push(u.target);
            }
            dedup(&mut reads);
            dedup(&mut writes);
            procs.push(Process {
                kind: ProcessKind::Event { block: bi },
                reads,
                writes,
            });
        }
        procs
    }

    /// Render normalized IR (backs `siox ir`).
    pub fn to_ir_string(&self) -> String {
        let mut out = String::new();
        for s in &self.signals {
            let w = if s.width == 0 {
                "?".to_string()
            } else {
                s.width.to_string()
            };
            out.push_str(&format!("signal {} : {w}\n", s.path));
        }
        for d in &self.drivers {
            let cond = match &d.cond {
                Some(c) => format!("  when {}", render(c, self)),
                None => String::new(),
            };
            out.push_str(&format!(
                "driver {} = {}{cond}\n",
                self.signals[d.target.0 as usize].path,
                render(&d.expr, self)
            ));
        }
        for eb in &self.event_blocks {
            out.push_str(&format!("event ({}):\n", render(&eb.condition, self)));
            for u in &eb.updates {
                let cond = match &u.cond {
                    Some(c) => format!("  when {}", render(c, self)),
                    None => String::new(),
                };
                out.push_str(&format!(
                    "    next {} = {}{cond}\n",
                    self.signals[u.target.0 as usize].path,
                    render(&u.expr, self)
                ));
            }
        }
        out
    }
}

// --- expression builders ----------------------------------------------------

fn not(e: Expr) -> Expr {
    Expr::Unary {
        op: UnOp::Not,
        rhs: Box::new(e),
    }
}

/// Whether a pattern matches everything, including a `_` written inside an
/// alternation (`A | _`).
fn pattern_has_wildcard(p: &ast::Pattern) -> bool {
    match p {
        ast::Pattern::Wildcard => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(pattern_has_wildcard),
        _ => false,
    }
}

fn eq(lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

/// The guard for one expanded write: the enclosing condition narrowed by the
/// index-match `hit`.
///
/// A constant index matches unconditionally, and saying so matters beyond tidy
/// IR. `Some(Const(1))` is still a *conditional* driver, so `word[1] = '1'` at
/// an entity's root drew an inferred-latch warning (W-P002) and never became
/// the unconditional driver that a following partial write merges over.
fn write_guard(cond: &Option<Expr>, hit: Expr) -> Option<Expr> {
    if matches!(hit, Expr::Const(1)) {
        return cond.clone();
    }
    Some(and(cond.clone(), hit))
}

/// `and` of an optional accumulated condition with a new one.
fn and(acc: Option<Expr>, c: Expr) -> Expr {
    match acc {
        Some(a) => Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(a),
            rhs: Box::new(c),
        },
        None => c,
    }
}

// --- rendering --------------------------------------------------------------

fn render(e: &Expr, d: &Design) -> String {
    match e {
        Expr::MetaCmp { inner, .. } => format!("metacmp({})", render(inner, d)),
        Expr::CCall { name, args, .. } => {
            let a = args
                .iter()
                .map(|x| render(x, d))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({a})")
        }
        Expr::Const(v) => v.to_string(),
        Expr::WideConst(words) => {
            let mut parts = words.iter().rev();
            let mut text = format!("0x{:x}", parts.next().copied().unwrap_or(0));
            for word in parts {
                text.push_str(&format!("{word:016x}"));
            }
            text
        }
        Expr::Real(x) => format!("{x}"),
        Expr::Logic(c) => format!("'{c}'"),
        Expr::Current(id) => d.signals[id.0 as usize].path.clone(),
        Expr::Old(id) => format!("Old({})", d.signals[id.0 as usize].path),
        Expr::Event(id) => format!("Event({})", d.signals[id.0 as usize].path),
        Expr::Unary { op, rhs } => format!("{}{}", un_sym(*op), paren(rhs, d)),
        Expr::Binary { op, lhs, rhs } => {
            format!("{} {} {}", paren(lhs, d), bin_sym(*op), paren(rhs, d))
        }
        Expr::Slice { base, hi, lo } => format!("{}[{hi}..{lo}]", paren(base, d)),
        Expr::Select { cond, then, els } => {
            format!(
                "{} ? {} : {}",
                paren(cond, d),
                paren(then, d),
                paren(els, d)
            )
        }
        Expr::Unknown => "?".to_string(),
    }
}

fn paren(e: &Expr, d: &Design) -> String {
    match e {
        Expr::Binary { .. } | Expr::Unary { .. } => format!("({})", render(e, d)),
        _ => render(e, d),
    }
}

fn un_sym(op: UnOp) -> &'static str {
    match op {
        UnOp::Not => "not ",
        UnOp::Neg => "-",
        UnOp::RealToInt => "integer",
    }
}

fn bin_sym(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::SAdd => "+",
        BinOp::SSub => "-",
        BinOp::SMul => "*",
        BinOp::SDiv => "/",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Xor => "xor",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::AShr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::SLt => "<",
        BinOp::SLe => "<=",
        BinOp::SGt => ">",
        BinOp::SGe => ">=",
        BinOp::FAdd => "+.",
        BinOp::FSub => "-.",
        BinOp::FMul => "*.",
        BinOp::FDiv => "/.",
        BinOp::FEq => "==.",
        BinOp::FNe => "!=.",
        BinOp::FLt => "<.",
        BinOp::FLe => "<=.",
        BinOp::FGt => ">.",
        BinOp::FGe => ">=.",
    }
}

// --- helpers ----------------------------------------------------------------

/// Whether an expression depends on a `::event`-family system attribute, which
/// makes an enclosing `if` an event-controlled block (spec 3.11).
fn expr_is_event(e: &ast::Expr) -> bool {
    match e {
        ast::Expr::SysAttr { base, attr, .. } => {
            // `::event` is the primitive that makes an `if` sequential; the edge
            // helpers are the `ClockLike` methods (handled by the Call arm).
            attr.text == "event" || expr_is_event(base)
        }
        // A `ClockLike` edge method (`clk.rising()`, `clk.falling()`,
        // `clk.edge()`) depends on `::event`, so it makes an `if` sequential.
        ast::Expr::Call { callee, .. } => match callee.as_ref() {
            ast::Expr::Field { field, .. } => {
                matches!(field.text.as_str(), "rising" | "falling" | "edge")
            }
            _ => false,
        },
        ast::Expr::Unary { rhs, .. } => expr_is_event(rhs),
        ast::Expr::Binary { lhs, rhs, .. } => expr_is_event(lhs) || expr_is_event(rhs),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => expr_is_event(base),
        _ => false,
    }
}

fn lower_unop(op: AstUnOp) -> UnOp {
    match op {
        AstUnOp::Not => UnOp::Not,
        AstUnOp::Neg => UnOp::Neg,
    }
}

fn lower_binop(op: AstBinOp) -> Option<BinOp> {
    Some(match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        AstBinOp::Div => BinOp::Div,
        AstBinOp::And => BinOp::And,
        AstBinOp::Or => BinOp::Or,
        AstBinOp::Custom { .. } => return None,
        AstBinOp::Shl => BinOp::Shl,
        AstBinOp::Shr => BinOp::Shr,
        AstBinOp::Eq => BinOp::Eq,
        AstBinOp::Ne => BinOp::Ne,
        AstBinOp::Lt => BinOp::Lt,
        AstBinOp::Le => BinOp::Le,
        AstBinOp::Gt => BinOp::Gt,
        AstBinOp::Ge => BinOp::Ge,
    })
}

fn parse_int(text: &str) -> Option<u64> {
    let normalized = text.trim().replace('_', "");
    let t = normalized.as_str();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(b, 2).ok()
    } else {
        t.parse().ok()
    }
}

fn integer_const(text: &str) -> Option<Expr> {
    let text = text.trim().replace('_', "");
    let (radix, digits) =
        if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (16, digits)
        } else if let Some(digits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (2, digits)
        } else {
            (10, text.as_str())
        };
    let mut words = Vec::<u64>::new();
    for digit in digits.chars() {
        let digit = digit.to_digit(radix)? as u64;
        let mut carry = digit as u128;
        for word in &mut words {
            let next = (*word as u128) * radix as u128 + carry;
            *word = next as u64;
            carry = next >> 64;
        }
        if carry != 0 || words.is_empty() {
            words.push(carry as u64);
        }
    }
    while words.last() == Some(&0) && words.len() > 1 {
        words.pop();
    }
    Some(words_const(words))
}

fn words_const(mut words: Vec<u64>) -> Expr {
    while words.last() == Some(&0) && words.len() > 1 {
        words.pop();
    }
    match words.as_slice() {
        [] => Expr::Const(0),
        [word] => Expr::Const(*word),
        _ => Expr::WideConst(words),
    }
}

fn lower_const_value(
    expression: &ast::Expr,
    exact: &HashMap<String, Expr>,
    narrow: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
) -> Option<Expr> {
    match expression {
        ast::Expr::Int { text, .. } if text.contains('.') => {
            text.replace('_', "").parse().ok().map(Expr::Real)
        }
        ast::Expr::Int { text, .. } => integer_const(text),
        ast::Expr::Path(path) => fns.constant_path_key(path).and_then(|key| {
            exact
                .get(&key)
                .cloned()
                .or_else(|| narrow.get(&key).map(|value| Expr::Const(*value as u64)))
        }),
        ast::Expr::Unary { op, rhs, .. } => Some(Expr::Unary {
            op: lower_unop(*op),
            rhs: Box::new(lower_const_value(rhs, exact, narrow, fns)?),
        }),
        ast::Expr::Binary { op, lhs, rhs, .. } => Some(Expr::Binary {
            op: lower_binop(op.clone())?,
            lhs: Box::new(lower_const_value(lhs, exact, narrow, fns)?),
            rhs: Box::new(lower_const_value(rhs, exact, narrow, fns)?),
        }),
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => Some(Expr::Select {
            cond: Box::new(lower_const_value(cond, exact, narrow, fns)?),
            then: Box::new(lower_const_value(then, exact, narrow, fns)?),
            els: Box::new(lower_const_value(els, exact, narrow, fns)?),
        }),
        ast::Expr::SuffixLit { text, suffix, .. } => Some(Expr::Binary {
            op: BinOp::Mul,
            lhs: Box::new(integer_const(text)?),
            rhs: Box::new(Expr::Const(
                ast::suffix_scale(&suffix.text).unwrap_or(1) as u64
            )),
        }),
        _ => None,
    }
}

/// Bit width from a type annotation, substituting parameters from `env` (so
/// `unsigned[W]` with `W=8` is width 8). `0` means parametric / not yet known.
fn source_type_span(ty: &ast::Type) -> crate::diag::Span {
    match ty {
        ast::Type::Path(path) => path.span,
        ast::Type::Indexed { span, .. }
        | ast::Type::Generic { span, .. }
        | ast::Type::View { span, .. } => *span,
    }
}

fn type_width(
    t: &ast::Type,
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    structs: &HashMap<String, &ast::StructDecl>,
    ranges: &HashMap<String, (i64, i64)>,
) -> u32 {
    type_width_at(t, env, fns, structs, ranges, &mut HashSet::new())
}

/// `type_width` with cycle detection. A cyclic derivation
/// (`struct A : B` / `struct B : A`) is reported by resolve, but lowering runs
/// anyway best-effort.
fn type_width_at(
    t: &ast::Type,
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    structs: &HashMap<String, &ast::StructDecl>,
    ranges: &HashMap<String, (i64, i64)>,
    seen: &mut HashSet<String>,
) -> u32 {
    match t {
        ast::Type::Path(_) => match fns.type_head_key(t).as_deref() {
            Some("integer") | Some("real") => 64, // native kernel word / f64 bits
            Some("Char") => 32,                   // symbol storage (implementation detail)
            // A derived type inherits its base array's size/range: `struct Byte
            // : Logic[8]` is 8 bits, `struct Word : unsigned[16]` is 16 (spec:
            // nominal derivation reuses the base representation).
            Some(name) => {
                if !seen.insert(name.to_string()) {
                    return 0;
                }
                let width = structs
                    .get(name)
                    .and_then(|s| s.base.as_ref())
                    .map(|b| type_width_at(b, env, fns, structs, ranges, seen))
                    .unwrap_or(0);
                seen.remove(name);
                width
            }
            None => 0,
        },
        // For `unsigned[8]` the index is the width; for `Logic[31..0]` it is the
        // span; unconstrained `T[]` stays width 0 ("set at use").
        ast::Type::Indexed { index: None, .. } => 0,
        ast::Type::Indexed {
            index: Some(index), ..
        } => match index.as_ref() {
            ast::Expr::Range { lo, hi, .. } => {
                match (
                    eval_const_fns(lo, env, fns, 0),
                    eval_const_fns(hi, env, fns, 0),
                ) {
                    (Some(a), Some(b)) => {
                        u32::try_from((i128::from(a) - i128::from(b)).unsigned_abs())
                            .ok()
                            .and_then(|width| width.checked_add(1))
                            .unwrap_or(0)
                    }
                    _ => 0,
                }
            }
            // A *range* constant used as the index (`unsigned[SPAN]` where
            // `const SPAN: range = 7..0`) states a span, not a width. Falling
            // through to the integer path found no integer and produced a
            // zero-width signal in silence — the literal `unsigned[7..0]`
            // spelling of the same thing was eight bits.
            ast::Expr::Path(path)
                if fns
                    .constant_path_key(path)
                    .is_some_and(|key| ranges.contains_key(&key)) =>
            {
                let key = fns.constant_path_key(path).expect("guarded range key");
                let (a, b) = ranges[&key];
                u32::try_from((i128::from(a) - i128::from(b)).unsigned_abs())
                    .ok()
                    .and_then(|width| width.checked_add(1))
                    .unwrap_or(0)
            }
            e => eval_const_fns(e, env, fns, 0)
                .map(|v| v.max(0) as u32)
                .unwrap_or(0),
        },
        ast::Type::Generic { base, .. } | ast::Type::View { target: base, .. } => {
            type_width(base, env, fns, structs, ranges)
        }
    }
}

/// Whether a field-less nominal struct ultimately derives from `kernel`.
/// Representation follows the declared base chain; names such as `time` and
/// `frequency` are not special to the compiler.
fn struct_derives_kernel(
    name: &str,
    kernel: &str,
    structs: &HashMap<String, &ast::StructDecl>,
    fns: &FunctionIndex<'_>,
) -> bool {
    if name == kernel {
        return true;
    }
    let mut current = name.to_string();
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        let Some(st) = structs.get(&current) else {
            return false;
        };
        if !st.fields.is_empty() {
            return false;
        }
        let Some(base) = st.base.as_ref().and_then(|ty| fns.type_head_key(ty)) else {
            return false;
        };
        if base == kernel {
            return true;
        }
        current = base;
    }
    false
}

/// Const-evaluate a width expression against a parameter environment.
fn eval_const(e: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
    let resolved = Resolved::default();
    let fns = FunctionIndex::new(&resolved);
    eval_const_fns(e, env, &fns, 0)
}

/// [`eval_const`] with module functions in scope: a call whose arguments
/// const-evaluate runs the function body statically (recursion allowed to a
/// bounded depth) — `clog2(DEPTH)` works in width positions.
pub fn eval_const_fns(
    e: &ast::Expr,
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    depth: u32,
) -> Option<i64> {
    if depth > 64 {
        return None;
    }
    match e {
        ast::Expr::Int { text, .. } => parse_int(text).map(|v| v as i64),
        ast::Expr::Path(path) => fns
            .constant_path_key(path)
            .and_then(|key| env.get(&key).copied()),
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_const_fns(cond, env, fns, depth + 1)? != 0 {
                eval_const_fns(then, env, fns, depth + 1)
            } else {
                eval_const_fns(els, env, fns, depth + 1)
            }
        }
        ast::Expr::Call { callee, args, .. } => {
            let name = call_fn_key(callee)?;
            // Kernel conversions are value-transparent in const context.
            if name == "integer" || name == "Char" {
                return eval_const_fns(args.first()?, env, fns, depth + 1);
            }
            let f = fns.get(callee)?;
            let body = f.body.as_ref()?;
            // Parameters shadow local leaves, while resolver-qualified module
            // constants remain available inside the called function body.
            // Starting from an empty map made `fn f() { return VALUE; }`
            // cease to be constant even though `VALUE` was in the caller's
            // compile-time environment.
            let mut fenv = env.clone();
            for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
                let n = p.name.as_ref()?;
                fenv.insert(n.text.clone(), eval_const_fns(a, env, fns, depth + 1)?);
            }
            eval_const_stmts(&body.stmts, &fenv, fns, depth + 1)
        }
        ast::Expr::Unary { op, rhs, .. } => {
            let v = eval_const_fns(rhs, env, fns, depth + 1)?;
            match op {
                ast::UnOp::Neg => v.checked_neg(),
                ast::UnOp::Not => Some((v == 0) as i64),
            }
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let (a, b) = (
                eval_const_fns(lhs, env, fns, depth + 1)?,
                eval_const_fns(rhs, env, fns, depth + 1)?,
            );
            match op {
                ast::BinOp::Add => a.checked_add(b),
                ast::BinOp::Sub => a.checked_sub(b),
                ast::BinOp::Mul => a.checked_mul(b),
                ast::BinOp::Div => a.checked_div(b),
                ast::BinOp::Shl => u32::try_from(b).ok().and_then(|shift| a.checked_shl(shift)),
                ast::BinOp::Shr => u32::try_from(b).ok().and_then(|shift| a.checked_shr(shift)),
                ast::BinOp::Eq => Some((a == b) as i64),
                ast::BinOp::Ne => Some((a != b) as i64),
                ast::BinOp::Lt => Some((a < b) as i64),
                ast::BinOp::Le => Some((a <= b) as i64),
                ast::BinOp::Gt => Some((a > b) as i64),
                ast::BinOp::Ge => Some((a >= b) as i64),
                ast::BinOp::And => Some((a != 0 && b != 0) as i64),
                ast::BinOp::Or => Some((a != 0 || b != 0) as i64),
                ast::BinOp::Custom { .. } => None,
            }
        }
        _ => None,
    }
}

/// Statically execute a const-fn body: `return`s and `if`/`else` chains.
pub fn eval_const_stmts(
    stmts: &[ast::Stmt],
    env: &HashMap<String, i64>,
    fns: &FunctionIndex<'_>,
    depth: u32,
) -> Option<i64> {
    for st in stmts {
        match st {
            ast::Stmt::Return { value, .. } => {
                return eval_const_fns(value.as_ref()?, env, fns, depth);
            }
            ast::Stmt::If(iff) => {
                if eval_const_fns(&iff.cond, env, fns, depth)? != 0 {
                    if let Some(v) = eval_const_stmts(&iff.then.stmts, env, fns, depth) {
                        return Some(v);
                    }
                } else {
                    match iff.else_.as_deref() {
                        Some(ast::ElseBranch::Block(b)) => {
                            if let Some(v) = eval_const_stmts(&b.stmts, env, fns, depth) {
                                return Some(v);
                            }
                        }
                        Some(ast::ElseBranch::If(inner)) => {
                            if let Some(v) = eval_const_stmts(
                                std::slice::from_ref(&ast::Stmt::If(inner.clone())),
                                env,
                                fns,
                                depth,
                            ) {
                                return Some(v);
                            }
                        }
                        None => {}
                    }
                }
            }
            _ => return None,
        }
    }
    None
}

/// Build `enum name -> variant name -> discriminant`. Explicit `= n` values are
/// honoured; unspecified variants continue from the previous discriminant + 1.
/// Index every enum declaration by name (for base-chain resolution).
/// Every derived type's inherited width: `struct Byte : Logic[8]` -> 8,
/// `struct Word : Byte` -> 8 (following the base chain). A derived type reuses
/// its base array's size/range (spec: nominal derivation). Testbench evaluators
/// consult this so a local of a derived vector type masks to the right width.
pub fn derived_widths(modules: &[Module], fns: &FunctionIndex<'_>) -> HashMap<String, u32> {
    let mut structs: HashMap<String, &ast::StructDecl> = HashMap::new();
    for m in modules {
        for it in &m.items {
            if let ast::Item::Struct(s) = it {
                structs.insert(fns.struct_decl_key(&s.name), s);
            }
        }
    }
    let empty_env = HashMap::new();
    structs
        .iter()
        .filter_map(|(name, s)| {
            let w = s
                .base
                .as_ref()
                .map(|b| type_width(b, &empty_env, fns, &structs, &HashMap::new()))
                .unwrap_or(0);
            (w > 0).then_some((name.clone(), w))
        })
        .collect()
}

pub fn vector_families(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> std::collections::HashSet<String> {
    // `impl Vector for F` opts a family into packed numeric storage. Compute
    // inheritance to a fixpoint so `struct Byte(unsigned[8])` joins it too.
    let structs: Vec<&ast::StructDecl> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|it| match it {
            ast::Item::Struct(st) => Some(st),
            _ => None,
        })
        .collect();
    let mut out: std::collections::HashSet<String> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Impl(im)
                if im
                    .trait_
                    .as_ref()
                    .and_then(|path| path.segments.last())
                    .is_some_and(|name| name.text == "Vector") =>
            {
                fns.type_head_key(&im.target)
            }
            _ => None,
        })
        .collect();
    loop {
        let mut changed = false;
        for st in &structs {
            let key = fns.struct_decl_key(&st.name);
            if !out.contains(&key) && is_bit_vector_struct(st, &out, fns) {
                out.insert(key);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// A field-less struct deriving from an already-known `Vector` family inherits
/// packed numeric storage.
fn is_bit_vector_struct(
    st: &ast::StructDecl,
    families: &std::collections::HashSet<String>,
    fns: &FunctionIndex<'_>,
) -> bool {
    if !st.fields.is_empty() {
        return false;
    }
    let elem = match &st.base {
        Some(ast::Type::Indexed { base, .. }) => fns.type_head_key(base),
        // A bare derived base (`struct Byte : unsigned`) reuses the base family.
        Some(ast::Type::Path(p)) => fns
            .struct_path_key(p)
            .or_else(|| p.segments.last().map(|segment| segment.text.clone())),
        _ => None,
    };
    elem.is_some_and(|head| families.contains(&head))
}

fn enum_index<'a>(
    modules: &'a [Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, &'a ast::EnumDecl> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Enum(e) = item {
                out.insert(fns.enum_decl_key(&e.name), e);
            }
        }
    }
    out
}

/// The `: Type` head name when it names another enum — i.e. a derivation
/// base rather than a numeric repr.
fn enum_base_name(
    e: &ast::EnumDecl,
    enums: &HashMap<String, &ast::EnumDecl>,
    fns: &FunctionIndex<'_>,
) -> Option<String> {
    let name = fns.type_head_key(e.repr.as_ref()?)?;
    enums.contains_key(&name).then_some(name)
}

/// An enum's effective variants, base chain first then its own declared ones
/// (spec: nominal derivation). `(name, explicit discriminant)`.
fn effective_variants(
    name: &str,
    enums: &HashMap<String, &ast::EnumDecl>,
    fns: &FunctionIndex<'_>,
    seen: &mut Vec<String>,
) -> Vec<(String, Option<i64>)> {
    let Some(e) = enums.get(name) else {
        return Vec::new();
    };
    if seen.iter().any(|n| n == name) {
        return Vec::new(); // cycle guard
    }
    seen.push(name.to_string());
    let mut out = match enum_base_name(e, enums, fns) {
        Some(base) => effective_variants(&base, enums, fns, seen),
        None => Vec::new(),
    };
    for v in &e.variants {
        let disc = match &v.value {
            Some(ast::Expr::Int { text, .. }) => parse_int(text).map(|n| n as i64),
            _ => None,
        };
        out.push((v.name.text.clone(), disc));
    }
    seen.pop();
    out
}

/// Every enum's `variant -> discriminant` map, *including inherited variants*
/// from a derivation base (`enum Extended : Base` gets Base's variants too).
/// Consumers (runner, native emitter) share this so derived-enum variant
/// references resolve identically.
pub fn enum_discriminants(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, HashMap<String, u64>> {
    let enums = enum_index(modules, fns);
    let mut out = HashMap::new();
    for name in enums.keys() {
        let mut vars = HashMap::new();
        let mut next = 0u64;
        for (v, disc) in effective_variants(name, &enums, fns, &mut Vec::new()) {
            let d = disc.map(|d| d as u64).unwrap_or(next);
            vars.insert(v, d);
            next = d + 1;
        }
        out.insert(name.clone(), vars);
    }
    out
}

/// Every enum's first-variant discriminant — the derived `new()` default
/// (`T'LEFT`). Mirrors `enum_discriminants`' running-counter numbering but keeps
/// only the first (declaration-order, base chain first) variant's value, so an
/// enum whose first variant carries a non-zero `= n` still defaults to a valid
/// member rather than a bare `0`.
pub fn enum_first_discriminants(
    modules: &[Module],
    fns: &FunctionIndex<'_>,
) -> HashMap<String, u64> {
    let enums = enum_index(modules, fns);
    let mut out = HashMap::new();
    for name in enums.keys() {
        if let Some((_, disc)) = effective_variants(name, &enums, fns, &mut Vec::new()).first() {
            out.insert(name.clone(), disc.map(|d| d as u64).unwrap_or(0));
        }
    }
    out
}

/// Flatten a struct literal into `suffix -> value` (".valid", ".inner.x"),
/// named the way a composite port's leaves are.
fn literal_leaves<'a>(
    args: &'a [ast::ConnectArg],
    prefix: &str,
    out: &mut HashMap<String, &'a ast::Expr>,
) {
    for a in args {
        let (Some(name), Some(value)) = (a.field.as_ref(), a.value.as_ref()) else {
            continue;
        };
        let key = format!("{prefix}.{}", name.text);
        match value {
            ast::Expr::Construct { args: inner, .. } => literal_leaves(inner, &key, out),
            _ => {
                out.insert(key, value);
            }
        }
    }
}

/// The instance type + connections a `let` declares, in either form:
/// - `let x: Entity = { .. }` (type on the construct),
/// - `let x: Entity = { .. }` (type from the annotation, name-less construct),
/// - `let x: Entity;` (type from the annotation, no connections).
///
/// `entities` decides whether an annotation names an entity.
fn instance_let_parts(
    l: &ast::LetDecl,
    entities: &HashMap<DefId, &ast::EntityDecl>,
    resolved: &Resolved,
) -> Option<(ast::Type, Vec<ast::ConnectArg>)> {
    // A *named* construction is a sub-instance only when the name is an
    // entity's. Every other branch below checks that; this one did not, so
    // `let p: Pair = Pair { .a = 1 }` — naming the struct, the way one
    // ordinarily writes a struct literal — was filed as an instance. No field
    // signals were ever created, and reading `p.a` came back as E-P017 "has no
    // hardware form", which describes a runtime-index problem the source does
    // not have. The same literal written `{ .a = 1 }` worked.
    if let Some(ast::Expr::Construct {
        ty: Some(cty),
        args,
        ..
    }) = &l.value
    {
        if type_def_id(cty, resolved).is_some_and(|id| entities.contains_key(&id)) {
            return Some((cty.clone(), args.clone()));
        }
    }
    let ann = l.ty.as_ref()?;
    // An entity *array* (`let stage: Inc[N]`) is built element-wise, not a
    // single instance.
    if matches!(ann, ast::Type::Indexed { .. }) {
        return None;
    }
    if !type_def_id(ann, resolved).is_some_and(|id| entities.contains_key(&id)) {
        return None;
    }
    match &l.value {
        // Dotted name-less construct `{ .a = a }`.
        Some(ast::Expr::Construct { ty: None, args, .. }) => Some((ann.clone(), args.clone())),
        // Positional/empty `{ a, b }` / `{}` lexes as a concat; its parts are
        // positional connections.
        Some(ast::Expr::Concat { parts, span }) => {
            let args = parts
                .iter()
                .map(|p| ast::ConnectArg {
                    field: None,
                    value: Some(p.clone()),
                    span: *span,
                })
                .collect();
            Some((ann.clone(), args))
        }
        None => Some((ann.clone(), Vec::new())),
        _ => None,
    }
}

/// Unroll a generate `for i in a..b { let s: Sub = {..} }` into concrete
/// sub-instances, substituting the loop index into each instance's name, type
/// arguments, and connection expressions. Plain `let` instances inside the
/// loop body are handled too; nested loops recurse. Non-instance statements
/// are left for the behavioural pass.
fn gather_generate(
    s: &ast::Stmt,
    env: &HashMap<String, i64>,
    loop_idx: &[i64],
    entities: &HashMap<DefId, &ast::EntityDecl>,
    resolved: &Resolved,
    fns: &FunctionIndex<'_>,
    out: &mut Vec<(String, ast::Type, Vec<ast::ConnectArg>)>,
) {
    match s {
        ast::Stmt::Let(l) => {
            if let Some((cty, args)) = instance_let_parts(l, entities, resolved) {
                // A generated instance (inside a loop) gets the enclosing loop
                // indices appended for a unique name, matching the elaborator's
                // `<name>_<i>` convention.
                let name = if loop_idx.is_empty() {
                    l.name.text.clone()
                } else {
                    let idx: Vec<String> = loop_idx.iter().map(|v| v.to_string()).collect();
                    format!("{}_{}", l.name.text, idx.join("_"))
                };
                out.push((name, cty, args));
            }
        }
        // Instance-array element: `stage[i] = Sub { .. }` (index already
        // substituted). The rendered target (`stage[1]`) is the instance name,
        // matching the elaborator so `stage[i].port` reads line up.
        ast::Stmt::Assign {
            target,
            value:
                ast::Expr::Construct {
                    ty: Some(cty),
                    args,
                    ..
                },
            ..
        } => {
            if let Some(name) = expr_path(target) {
                out.push((name, cty.clone(), args.clone()));
            }
        }
        ast::Stmt::For {
            var,
            range: ast::Expr::Range { lo, hi, .. },
            body,
            ..
        } => {
            if let (Some(a), Some(b)) = (
                eval_const_fns(lo, env, fns, 0),
                eval_const_fns(hi, env, fns, 0),
            ) {
                for i in loop_range(a, b) {
                    let mut e = env.clone();
                    e.insert(var.text.clone(), i);
                    let mut idx = loop_idx.to_vec();
                    idx.push(i);
                    for st in &body.stmts {
                        // Substitute the loop index throughout the statement so
                        // `Sub<W=i>` and `wires[i]` become concrete before the
                        // instance is recorded.
                        let st = subst_stmt(st, &var.text, i);
                        gather_generate(&st, &e, &idx, entities, resolved, fns, out);
                    }
                }
            }
        }
        // `if <const> { .. } else { .. }`: a generate-if — the condition is
        // constant-folded and only the taken branch's instances are gathered.
        // A non-constant condition is behavioral, not a generate-if.
        ast::Stmt::If(iff) => {
            if let Some(c) = eval_const_fns(&iff.cond, env, fns, 0) {
                if c != 0 {
                    for st in &iff.then.stmts {
                        gather_generate(st, env, loop_idx, entities, resolved, fns, out);
                    }
                } else {
                    match iff.else_.as_deref() {
                        Some(ast::ElseBranch::Block(b)) => {
                            for st in &b.stmts {
                                gather_generate(st, env, loop_idx, entities, resolved, fns, out);
                            }
                        }
                        Some(ast::ElseBranch::If(inner)) => {
                            gather_generate(
                                &ast::Stmt::If(inner.clone()),
                                env,
                                loop_idx,
                                entities,
                                resolved,
                                fns,
                                out,
                            );
                        }
                        None => {}
                    }
                }
            }
        }
        _ => {}
    }
}

/// Substitute a bound integer for a single-segment path variable throughout a
/// statement (used to unroll generate loops).
/// Read a generic argument expression as a type: `unsigned[8]` (parsed as an index
/// expression) becomes the type `unsigned[8]`, a bare name becomes a path type.
/// Used to substitute a struct's type parameters (`Pair<unsigned[8]>`).
fn expr_to_type(e: &ast::Expr) -> Option<ast::Type> {
    match e {
        ast::Expr::Path(p) => Some(ast::Type::Path(p.clone())),
        ast::Expr::Index { base, index, span } => Some(ast::Type::Indexed {
            base: Box::new(expr_to_type(base)?),
            index: Some(index.clone()),
            span: *span,
        }),
        _ => None,
    }
}

/// Substitute type parameters (`T -> unsigned[8]`) in a type, recursing through
/// array/generic/mode wrappers.
fn subst_type_params(ty: &ast::Type, subst: &HashMap<String, ast::Type>) -> ast::Type {
    match ty {
        ast::Type::Path(p) if p.segments.len() == 1 => subst
            .get(&p.segments[0].text)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        ast::Type::Indexed { base, index, span } => ast::Type::Indexed {
            base: Box::new(subst_type_params(base, subst)),
            index: index.clone(),
            span: *span,
        },
        ast::Type::Generic { base, args, span } => ast::Type::Generic {
            base: Box::new(subst_type_params(base, subst)),
            args: args
                .iter()
                .map(|arg| match arg {
                    ast::GenericArg::Positional(ast::Expr::Path(path))
                        if path.segments.len() == 1
                            && subst.contains_key(&path.segments[0].text) =>
                    {
                        ast::GenericArg::PositionalType(subst[&path.segments[0].text].clone())
                    }
                    ast::GenericArg::Named {
                        name,
                        value: ast::Expr::Path(path),
                    } if path.segments.len() == 1 && subst.contains_key(&path.segments[0].text) => {
                        ast::GenericArg::NamedType {
                            name: name.clone(),
                            ty: subst[&path.segments[0].text].clone(),
                        }
                    }
                    ast::GenericArg::PositionalType(ty) => {
                        ast::GenericArg::PositionalType(subst_type_params(ty, subst))
                    }
                    ast::GenericArg::NamedType { name, ty } => ast::GenericArg::NamedType {
                        name: name.clone(),
                        ty: subst_type_params(ty, subst),
                    },
                    _ => arg.clone(),
                })
                .collect(),
            span: *span,
        },
        ast::Type::View { view, target, span } => ast::Type::View {
            view: view.clone(),
            target: Box::new(subst_type_params(target, subst)),
            span: *span,
        },
        _ => ty.clone(),
    }
}

/// Deep-clone a statement, replacing every bare single-segment path named in
/// `map` with its expression. Used to inline a method body: `self` maps to the
/// receiver and each parameter to its argument, so `self.valid = '1'` in a
/// method becomes `<recv>.valid = '1'` at the call site (spec 3.20). Public so
/// the testbench evaluators (siox-run, the native emitter) inline method calls
/// the same way hardware lowering does.
pub fn subst_stmt_paths(s: &ast::Stmt, map: &HashMap<String, ast::Expr>) -> ast::Stmt {
    use ast::Stmt;
    match s {
        Stmt::Assign {
            target,
            value,
            after,
            span,
        } => Stmt::Assign {
            target: subst_expr_paths(target, map),
            value: subst_expr_paths(value, map),
            after: after.as_ref().map(|a| subst_expr_paths(a, map)),
            span: *span,
        },
        Stmt::If(iff) => Stmt::If(subst_if_paths(iff, map)),
        Stmt::Match(m) => Stmt::Match(ast::MatchStmt {
            scrutinee: subst_expr_paths(&m.scrutinee, map),
            arms: m
                .arms
                .iter()
                .map(|a| ast::MatchArm {
                    pattern: a.pattern.clone(),
                    body: subst_block_paths(&a.body, map),
                    span: a.span,
                })
                .collect(),
            span: m.span,
        }),
        Stmt::For {
            var,
            range,
            body,
            span,
        } => Stmt::For {
            var: var.clone(),
            range: subst_expr_paths(range, map),
            body: subst_block_paths(body, map),
            span: *span,
        },
        Stmt::Let(l) => {
            let mut l = l.clone();
            l.value = l.value.as_ref().map(|v| subst_expr_paths(v, map));
            Stmt::Let(l)
        }
        Stmt::Expr(e) => Stmt::Expr(subst_expr_paths(e, map)),
        Stmt::Return { value, span } => Stmt::Return {
            value: value.as_ref().map(|v| subst_expr_paths(v, map)),
            span: *span,
        },
    }
}

fn subst_block_paths(b: &ast::Block, map: &HashMap<String, ast::Expr>) -> ast::Block {
    ast::Block {
        stmts: b.stmts.iter().map(|s| subst_stmt_paths(s, map)).collect(),
        span: b.span,
    }
}

fn subst_if_paths(iff: &ast::IfStmt, map: &HashMap<String, ast::Expr>) -> ast::IfStmt {
    ast::IfStmt {
        cond: subst_expr_paths(&iff.cond, map),
        then: subst_block_paths(&iff.then, map),
        else_: iff.else_.as_ref().map(|e| {
            Box::new(match e.as_ref() {
                ast::ElseBranch::Block(b) => ast::ElseBranch::Block(subst_block_paths(b, map)),
                ast::ElseBranch::If(i) => ast::ElseBranch::If(subst_if_paths(i, map)),
            })
        }),
        span: iff.span,
    }
}

/// Deep-clone an expression, replacing every bare single-segment path named in
/// `map` with its mapped expression (the value-side counterpart of
/// [`subst_stmt_paths`]).
pub fn subst_expr_paths(e: &ast::Expr, map: &HashMap<String, ast::Expr>) -> ast::Expr {
    use ast::Expr;
    let sub = |x: &Expr| Box::new(subst_expr_paths(x, map));
    match e {
        Expr::Path(p) if p.segments.len() == 1 => map
            .get(&p.segments[0].text)
            .cloned()
            .unwrap_or_else(|| e.clone()),
        Expr::Field { base, field, span } => Expr::Field {
            base: sub(base),
            field: field.clone(),
            span: *span,
        },
        Expr::SysAttr { base, attr, span } => Expr::SysAttr {
            base: sub(base),
            attr: attr.clone(),
            span: *span,
        },
        Expr::Index { base, index, span } => Expr::Index {
            base: sub(base),
            index: sub(index),
            span: *span,
        },
        Expr::Range { lo, hi, span } => Expr::Range {
            lo: sub(lo),
            hi: sub(hi),
            span: *span,
        },
        Expr::PartialRange { lo, hi, span } => Expr::PartialRange {
            lo: lo.as_deref().map(sub),
            hi: hi.as_deref().map(sub),
            span: *span,
        },
        Expr::Unary { op, rhs, span } => Expr::Unary {
            op: *op,
            rhs: sub(rhs),
            span: *span,
        },
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: op.clone(),
            lhs: sub(lhs),
            rhs: sub(rhs),
            span: *span,
        },
        Expr::IfExpr {
            cond,
            then,
            els,
            span,
        } => Expr::IfExpr {
            cond: sub(cond),
            then: sub(then),
            els: sub(els),
            span: *span,
        },
        Expr::Call {
            callee,
            type_args,
            args,
            bang,
            span,
        } => Expr::Call {
            callee: sub(callee),
            type_args: type_args.clone(),
            args: args.iter().map(|a| subst_expr_paths(a, map)).collect(),
            bang: *bang,
            span: *span,
        },
        Expr::Concat { parts, span } => Expr::Concat {
            parts: parts.iter().map(|p| subst_expr_paths(p, map)).collect(),
            span: *span,
        },
        Expr::Array { elems, span } => Expr::Array {
            elems: elems.iter().map(|e| subst_expr_paths(e, map)).collect(),
            span: *span,
        },
        Expr::Construct {
            ty,
            args,
            spread,
            span,
        } => Expr::Construct {
            ty: ty.clone(),
            args: args
                .iter()
                .map(|a| ast::ConnectArg {
                    field: a.field.clone(),
                    value: a.value.as_ref().map(|v| subst_expr_paths(v, map)),
                    span: a.span,
                })
                .collect(),
            spread: spread.as_ref().map(|b| Box::new(subst_expr_paths(b, map))),
            span: *span,
        },
        other => other.clone(),
    }
}

fn subst_stmt(s: &ast::Stmt, var: &str, val: i64) -> ast::Stmt {
    match s {
        ast::Stmt::Let(l) => {
            let mut l = l.clone();
            l.value = l.value.as_ref().map(|v| subst_expr(v, var, val));
            ast::Stmt::Let(l)
        }
        ast::Stmt::For {
            var: v,
            range,
            body,
            span,
        } => ast::Stmt::For {
            var: v.clone(),
            range: subst_expr(range, var, val),
            body: {
                let mut b = body.clone();
                b.stmts = b.stmts.iter().map(|st| subst_stmt(st, var, val)).collect();
                b
            },
            span: *span,
        },
        // `stage[i] = Sub { .x = w[i] }`: substitute in both the indexed target
        // and the construct, so instance-array elements unroll concretely.
        ast::Stmt::Assign {
            target,
            value,
            after,
            span,
        } => ast::Stmt::Assign {
            target: subst_expr(target, var, val),
            value: subst_expr(value, var, val),
            after: after.as_ref().map(|a| subst_expr(a, var, val)),
            span: *span,
        },
        // Recurse into `if`/`match` so a generate loop's index is substituted
        // inside their branches too (`for i { if i<N { .. w[i] .. } }`).
        ast::Stmt::If(iff) => ast::Stmt::If(subst_if(iff, var, val)),
        ast::Stmt::Match(m) => {
            let mut m = m.clone();
            m.scrutinee = subst_expr(&m.scrutinee, var, val);
            for arm in &mut m.arms {
                arm.body.stmts = arm
                    .body
                    .stmts
                    .iter()
                    .map(|s| subst_stmt(s, var, val))
                    .collect();
            }
            ast::Stmt::Match(m)
        }
        other => other.clone(),
    }
}

fn subst_if(iff: &ast::IfStmt, var: &str, val: i64) -> ast::IfStmt {
    let mut n = iff.clone();
    n.cond = subst_expr(&iff.cond, var, val);
    n.then.stmts = iff
        .then
        .stmts
        .iter()
        .map(|s| subst_stmt(s, var, val))
        .collect();
    n.else_ = iff.else_.as_ref().map(|eb| {
        Box::new(match eb.as_ref() {
            ast::ElseBranch::Block(b) => {
                let mut b = b.clone();
                b.stmts = b.stmts.iter().map(|s| subst_stmt(s, var, val)).collect();
                ast::ElseBranch::Block(b)
            }
            ast::ElseBranch::If(inner) => ast::ElseBranch::If(subst_if(inner, var, val)),
        })
    });
    n
}

/// Deep-clone an expression, replacing every bare `var` reference with the
/// integer literal `val`. Also rewrites index/type-argument expressions.
fn subst_expr(e: &ast::Expr, var: &str, val: i64) -> ast::Expr {
    use ast::Expr;
    let sub = |x: &Expr| Box::new(subst_expr(x, var, val));
    match e {
        // A negative iteration has to stay a well-formed AST: the lexer never
        // produces an `Int` whose text carries a sign, so `Int { text: "-1" }`
        // failed `parse_int` and every const-folding path treated the index as
        // non-constant. `for i in 0..(N - 1)` with `N = 0` counts down through
        // -1 (ranges are directional), and that iteration silently folded to
        // nothing instead of to element -1.
        Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == var => {
            int_literal(val, p.span)
        }
        Expr::Field { base, field, span } => Expr::Field {
            base: sub(base),
            field: field.clone(),
            span: *span,
        },
        Expr::SysAttr { base, attr, span } => Expr::SysAttr {
            base: sub(base),
            attr: attr.clone(),
            span: *span,
        },
        Expr::Index { base, index, span } => Expr::Index {
            base: sub(base),
            index: sub(index),
            span: *span,
        },
        Expr::Range { lo, hi, span } => Expr::Range {
            lo: sub(lo),
            hi: sub(hi),
            span: *span,
        },
        Expr::PartialRange { lo, hi, span } => Expr::PartialRange {
            lo: lo.as_deref().map(sub),
            hi: hi.as_deref().map(sub),
            span: *span,
        },
        // Fold constant arithmetic so a substituted index like `wires[i+1]`
        // becomes the literal `wires[2]` that `expr_path` can resolve.
        Expr::Unary { op, rhs, span } => {
            let n = Expr::Unary {
                op: *op,
                rhs: sub(rhs),
                span: *span,
            };
            fold_const(n, *span)
        }
        Expr::Binary { op, lhs, rhs, span } => {
            let n = Expr::Binary {
                op: op.clone(),
                lhs: sub(lhs),
                rhs: sub(rhs),
                span: *span,
            };
            fold_const(n, *span)
        }
        Expr::IfExpr {
            cond,
            then,
            els,
            span,
        } => Expr::IfExpr {
            cond: sub(cond),
            then: sub(then),
            els: sub(els),
            span: *span,
        },
        Expr::Call {
            callee,
            type_args,
            args,
            bang,
            span,
        } => Expr::Call {
            callee: sub(callee),
            type_args: type_args.clone(),
            args: args.iter().map(|a| subst_expr(a, var, val)).collect(),
            bang: *bang,
            span: *span,
        },
        Expr::Concat { parts, span } => Expr::Concat {
            parts: parts.iter().map(|p| subst_expr(p, var, val)).collect(),
            span: *span,
        },
        Expr::Array { elems, span } => Expr::Array {
            elems: elems.iter().map(|e| subst_expr(e, var, val)).collect(),
            span: *span,
        },
        Expr::Construct {
            ty,
            args,
            spread,
            span,
        } => Expr::Construct {
            ty: ty.as_ref().map(|t| subst_type(t, var, val)),
            args: args
                .iter()
                .map(|a| ast::ConnectArg {
                    field: a.field.clone(),
                    value: a.value.as_ref().map(|v| subst_expr(v, var, val)),
                    span: a.span,
                })
                .collect(),
            spread: spread.as_ref().map(|b| Box::new(subst_expr(b, var, val))),
            span: *span,
        },
        other => other.clone(),
    }
}

/// The values a `for i in left..right` loop visits. Range endpoints are **inclusive
/// and directional**, matching bit slices and array ranges elsewhere in the
/// language: `0..2` yields 0,1,2 and `2..0` yields 2,1,0.
pub fn loop_range(a: i64, b: i64) -> Vec<i64> {
    if a <= b {
        (a..=b).collect()
    } else {
        (b..=a).rev().collect()
    }
}

/// Collapse a now-constant arithmetic node to an integer literal, so unrolled
/// index expressions resolve as plain `Int`s. Non-constant nodes pass through.
/// Build the AST for an integer value.
///
/// The lexer never produces an `Int` whose text carries a sign, so a negative
/// value has to be a negation over an unsigned literal — `Int { text: "-5" }`
/// is a node no other stage can read. `parse_int` rejects it, which silently
/// demotes the whole expression to non-constant, and the value it was carrying
/// reaches hardware as 0. Both places that turn a folded `i64` back into AST
/// go through here so a third one cannot drift.
fn int_literal(val: i64, span: crate::diag::Span) -> ast::Expr {
    let lit = ast::Expr::Int {
        text: val.unsigned_abs().to_string(),
        span,
    };
    if val < 0 {
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs: Box::new(lit),
            span,
        }
    } else {
        lit
    }
}

fn fold_const(e: ast::Expr, span: crate::diag::Span) -> ast::Expr {
    match eval_const(&e, &HashMap::new()) {
        Some(v) => int_literal(v, span),
        None => e,
    }
}

/// Substitute the loop index into a type's index/generic-argument expressions.
fn subst_type(t: &ast::Type, var: &str, val: i64) -> ast::Type {
    match t {
        ast::Type::Indexed { base, index, span } => ast::Type::Indexed {
            base: Box::new(subst_type(base, var, val)),
            index: index.as_ref().map(|i| Box::new(subst_expr(i, var, val))),
            span: *span,
        },
        ast::Type::Generic { base, args, span } => ast::Type::Generic {
            base: Box::new(subst_type(base, var, val)),
            args: args
                .iter()
                .map(|a| match a {
                    ast::GenericArg::Positional(e) => {
                        ast::GenericArg::Positional(subst_expr(e, var, val))
                    }
                    ast::GenericArg::PositionalType(ty) => {
                        ast::GenericArg::PositionalType(subst_type(ty, var, val))
                    }
                    ast::GenericArg::Named { name, value } => ast::GenericArg::Named {
                        name: name.clone(),
                        value: subst_expr(value, var, val),
                    },
                    ast::GenericArg::NamedType { name, ty } => ast::GenericArg::NamedType {
                        name: name.clone(),
                        ty: subst_type(ty, var, val),
                    },
                })
                .collect(),
            span: *span,
        },
        ast::Type::View { view, target, span } => ast::Type::View {
            view: view.clone(),
            target: Box::new(subst_type(target, var, val)),
            span: *span,
        },
        ast::Type::Path(_) => t.clone(),
    }
}

/// `p'old.valid` -> `p.valid'old`, `xs'old[0]` -> `xs[0]'old`. Only the two
/// value primitives move: `'length` and the range bounds describe the whole
/// aggregate, so pushing them at a leaf would change what is asked.
fn sunk_sysattr(e: &ast::Expr) -> Option<ast::Expr> {
    let (base, rebuild): (&ast::Expr, &dyn Fn(Box<ast::Expr>) -> ast::Expr) = match e {
        ast::Expr::Field { base, field, span } => (base, &|inner| ast::Expr::Field {
            base: inner,
            field: field.clone(),
            span: *span,
        }),
        ast::Expr::Index { base, index, span } => (base, &|inner| ast::Expr::Index {
            base: inner,
            index: index.clone(),
            span: *span,
        }),
        _ => return None,
    };
    let ast::Expr::SysAttr {
        base: inner,
        attr,
        span,
    } = base
    else {
        return None;
    };
    if attr.text != "old" && attr.text != "event" {
        return None;
    }
    Some(ast::Expr::SysAttr {
        base: Box::new(rebuild(inner.clone())),
        attr: attr.clone(),
        span: *span,
    })
}

/// The dotted signal path of a name, struct-field, or constant-index access:
/// `s` -> `"s"`, `s.data` -> `"s.data"`, `a[2]` -> `"a[2]"`. A dynamic index or
/// anything else (calls, slices) yields `None`.
fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", expr_path(base)?, field.text))
        }
        ast::Expr::Index { base, index, .. } => match index.as_ref() {
            ast::Expr::Int { text, .. } => {
                Some(format!("{}[{}]", expr_path(base)?, parse_int(text)?))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Split a flattened aggregate access into its root name and ordered field /
/// index suffixes. Unlike `expr_path`, indices remain as expressions so a
/// runtime access can be expanded over the concrete leaf paths.
fn access_steps(e: &ast::Expr) -> Option<(String, Vec<AccessStep<'_>>)> {
    fn walk<'e>(e: &'e ast::Expr, steps: &mut Vec<AccessStep<'e>>) -> Option<String> {
        match e {
            ast::Expr::Path(path) if path.segments.len() == 1 => {
                Some(path.segments[0].text.clone())
            }
            ast::Expr::Field { base, field, .. } => {
                let root = walk(base, steps)?;
                steps.push(AccessStep::Field(&field.text));
                Some(root)
            }
            ast::Expr::Index { base, index, .. } => {
                let root = walk(base, steps)?;
                steps.push(AccessStep::Index(index));
                Some(root)
            }
            _ => None,
        }
    }

    let mut steps = Vec::new();
    let root = walk(e, &mut steps)?;
    Some((root, steps))
}

/// The `(element type, length)` if `ty` is an array — an `Indexed` type whose
/// base is *not* an integer (`Bit[4]`), as opposed to a vector (`unsigned[8]`).
/// The element type and **ordered element indices** of an array type.
/// A width-only index (`Bit[4]`) is ascending `0..=3`; a range keeps its
/// written direction (`Logic[7..0]` yields 7,6,...,0). A single-segment path
/// as the index may name a range constant.
fn array_of<'t>(
    ty: &'t ast::Type,
    env: &HashMap<String, i64>,
    const_ranges: &HashMap<String, (i64, i64)>,
    families: &std::collections::HashSet<String>,
    fns: &FunctionIndex<'_>,
) -> Option<(&'t ast::Type, Vec<i64>)> {
    let ast::Type::Indexed {
        base,
        index: Some(index),
        ..
    } = ty
    else {
        return None;
    };
    // A Logic-vector family (unsigned/signed/user) `F[N]` is one N-bit signal, not an
    // N-element array — but only when the base is DIRECTLY the family (`unsigned`),
    // not when it is itself indexed (`unsigned[8][4]` is an array of vectors).
    let base_is_family = matches!(base.as_ref(), ast::Type::Path(_))
        && fns
            .type_head_key(base)
            .is_some_and(|head| families.contains(&head));
    if is_int_type(base) || base_is_family {
        return None;
    }
    let bounds = match index.as_ref() {
        ast::Expr::Range { lo, hi, .. } => Some((
            eval_const_fns(lo, env, fns, 0)?,
            eval_const_fns(hi, env, fns, 0)?,
        )),
        ast::Expr::Path(path) => fns
            .constant_path_key(path)
            .and_then(|key| const_ranges.get(&key).copied()),
        _ => None,
    };
    let indices = match bounds {
        Some((a, b)) if a <= b => (a..=b).collect(),
        Some((a, b)) => (b..=a).rev().collect(),
        None => (0..eval_const_fns(index, env, fns, 0).unwrap_or(0).max(0)).collect(),
    };
    Some((base, indices))
}

/// The kernel `integer` scalar (a bare word). unsigned/signed are NOT here — they
/// are `#[vector]` families recognized via the family set, not by name.
fn is_int_type(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Path(p)
        if p.segments.last().map(|s| s.text.as_str()) == Some("integer"))
}

/// Build `enum name -> bit width`: the `repr` width if given (`enum S: unsigned[2]`),
/// else the bits needed for the variant count.
fn enum_reprs(modules: &[Module], fns: &FunctionIndex<'_>) -> HashMap<String, u32> {
    let empty = HashMap::new();
    let enums = enum_index(modules, fns);
    let mut out = HashMap::new();
    for (name, e) in &enums {
        // A numeric `: repr` sets the width explicitly; otherwise the width is
        // derived, and must hold every *value* the enum can take — not just
        // one code per variant. An explicit discriminant can sit far above the
        // ordinal range (`enum Code { Lo = 1, Hi = 9 }` is two variants but
        // needs four bits), so the larger of the two bounds wins.
        let w = if let Some(repr) = e
            .repr
            .as_ref()
            .filter(|_| enum_base_name(e, &enums, fns).is_none())
        {
            type_width(repr, &empty, fns, &HashMap::new(), &HashMap::new())
        } else {
            let variants = effective_variants(name, &enums, fns, &mut Vec::new());
            let n = variants.len().max(1) as u32;
            let count_bits = if n <= 1 {
                1
            } else {
                u32::BITS - (n - 1).leading_zeros()
            };
            let max_disc = variants.iter().filter_map(|(_, d)| *d).max().unwrap_or(0);
            let disc_bits = if max_disc <= 0 {
                1
            } else {
                u64::BITS - (max_disc as u64).leading_zeros()
            };
            count_bits.max(disc_bits)
        };
        out.insert(name.clone(), w);
    }
    out
}

fn has_attr(e: &ast::EntityDecl, name: &str) -> bool {
    e.attrs
        .iter()
        .any(|a| a.name.segments.last().map(|s| s.text.as_str()) == Some(name))
}

fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::View { view, .. } => view.segments.last().map(|s| s.text.as_str()),
    }
}

fn type_def_id(ty: &ast::Type, resolved: &Resolved) -> Option<DefId> {
    match ty {
        ast::Type::Path(path) => resolved.resolved(path.span),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
            type_def_id(base, resolved)
        }
        ast::Type::View { view, .. } => resolved.resolved(view.span),
    }
}

fn is_blanket_array_impl(im: &ast::ImplDecl) -> bool {
    let ast::Type::Indexed {
        base, index: None, ..
    } = &im.target
    else {
        return false;
    };
    let Some(head) = type_head_name(base) else {
        return false;
    };
    im.params.params.iter().any(|param| param.name.text == head)
}

fn blanket_requirement(im: &ast::ImplDecl) -> Option<String> {
    let ast::Type::Indexed { base, .. } = &im.target else {
        return None;
    };
    let parameter = type_head_name(base)?;
    let bound = im
        .params
        .params
        .iter()
        .find(|candidate| candidate.name.text == parameter)?
        .bound
        .as_ref()?;
    match bound {
        ast::Type::Generic { base, args, .. } if type_head_name(base) == Some("Operator") => {
            args.first().and_then(|argument| match argument {
                ast::GenericArg::Positional(ast::Expr::StrLit { text, .. }) => Some(text.clone()),
                _ => None,
            })
        }
        _ => type_head_name(bound).map(str::to_string),
    }
}

fn declared_view_key(view: &ast::ViewDecl, fns: &FunctionIndex<'_>) -> String {
    let target = fns
        .type_head_key(&view.target)
        .unwrap_or_else(|| "<error>".to_string());
    format!("{}@{target}", view.name.text)
}

/// Pack one little-endian file integer into the compiler's ABI-word vector.
///
/// File integers use exactly `ceil(width / 8)` bytes. Missing bytes are zero,
/// and padding bits in the final byte never escape the declared type width.
fn file_integer_words(bytes: &[u8], offset: usize, byte_count: usize, width: u32) -> Vec<u64> {
    let word_count = width.max(1).div_ceil(64) as usize;
    let mut words = vec![0; word_count];
    for byte_index in 0..byte_count {
        let Some(index) = offset.checked_add(byte_index) else {
            break;
        };
        let Some(&byte) = bytes.get(index) else {
            break;
        };
        let bit = byte_index * 8;
        let word = bit / 64;
        if let Some(destination) = words.get_mut(word) {
            *destination |= u64::from(byte) << (bit % 64);
        }
    }
    if let Some(last) = words.last_mut() {
        let used = width % 64;
        if used != 0 {
            *last &= (1_u64 << used) - 1;
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    /// A minimal `ClockLike` impl so self-contained test sources can use the
    /// `clk.rising()` edge methods (std provides these for real designs).
    const CLK_PRELUDE: &str = "\n\
        enum Bool { false, true }\n\
        enum Bit { '0', '1' }\n\
        enum ULogic { '0', '1', 'Z', 'X', 'U', 'W', 'L', 'H', '-' }\n\
        enum Logic(ULogic);\n\
        trait Boolean { fn as_bool(self) -> Bool; }\n\
        trait Vector {}\n\
        impl Boolean for Bit { fn as_bool(self) -> Bool { return true; } }\n\
        impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }\n\
        impl Vector for unsigned {}\n\
        impl Vector for signed {}\n\
        impl Operator<\"and\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"or\", Bool, Bool> for Bool { fn apply(self, rhs: Bool) -> Bool { return self; } }\n\
        impl Operator<\"not\", Bool, Bool> for Bool { fn apply(self) -> Bool { return self; } }\n\
        trait ClockLike { fn rising(self) -> Bool; fn falling(self) -> Bool; fn edge(self) -> Bool; }\n\
        impl ClockLike for Bit { fn rising(self) -> Bool { return self'event and self'old == '0' and self == '1'; } fn falling(self) -> Bool { return self'event and self'old == '1' and self == '0'; } fn edge(self) -> Bool { return self'event; } }\n";

    /// A match naming every variant of its scrutinee is as complete as one
    /// ending in `_`, so a signal every arm assigns is not a latch. Only the
    /// wildcard half was implemented, so the natural spelling of an exhaustive
    /// decode drew an inferred-latch warning whose suggested fix is a
    /// redundant `_` arm.
    /// An initializer is a signal's power-on value, folded at elaboration. One
    /// that reads another signal cannot fold, and was dropped in silence: the
    /// signal kept its type's default, so `let i: unsigned[8] = a + 1;` read 0
    /// while `i = a + 1;` — a driver, and a different thing — read 201.
    #[test]
    fn a_non_constant_initializer_is_reported() {
        let count = |src: &str| {
            lower_diags(src)
                .into_iter()
                .filter(|d| d.contains("is not a constant"))
                .count()
        };
        assert_eq!(
            count(
                "module m;\n#[top] entity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let i: unsigned[8] = a + 1; y = i; }\n"
            ),
            1,
            "an initializer reading another signal"
        );
        // Driving it is the spelling that means "compute this", and is fine.
        assert_eq!(
            count(
                "module m;\n#[top] entity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let i: unsigned[8]; i = a + 1; y = i; }\n"
            ),
            0,
            "the driver spelling is not an initializer"
        );
        // Everything that can fold still seeds without complaint: a literal, a
        // module constant, an arithmetic fold, and a const-evaluable call.
        assert_eq!(
            count(
                "module m;\nconst K: unsigned[8] = 5;\n\
                 fn twice(n: unsigned[8]) -> unsigned[8] { return n * 2; }\n\
                 #[top] entity E { y: unsigned[8] out }\n\
                 impl E { let a: unsigned[8] = 200; let b: unsigned[8] = K;\n\
                 let c: unsigned[8] = 3 * 7; let d: unsigned[8] = twice(4);\n\
                 y = a + b + c + d; }\n"
            ),
            0,
            "literals, constants, folds and const calls all seed"
        );

        // The aggregate sites seed inits the same way and dropped a
        // non-constant the same way — and there, no undriven lint reaches a
        // struct leaf or an array element, so nothing was reported at all.
        assert_eq!(
            count(
                "module m;\nstruct P { x: unsigned[8], y: unsigned[8] }\n\
                 #[top] entity E { src: unsigned[8] in, y: unsigned[8] out }\n\
                 impl E { let p: P = { .x = 7, .y = src + 1 }; y = p.y; }\n"
            ),
            1,
            "a struct-field initializer reading a signal"
        );
        assert_eq!(
            count(
                "module m;\n#[top] entity E { src: unsigned[8] in, y: unsigned[8] out }\n\
                 impl E { let arr: unsigned[8][2] = [9, src + 2]; y = arr[1]; }\n"
            ),
            1,
            "an array-element initializer reading a signal"
        );
        // Constant aggregates keep seeding.
        assert_eq!(
            count(
                "module m;\nconst K: unsigned[8] = 5;\n\
                 struct P { x: unsigned[8], y: unsigned[8] }\n\
                 #[top] entity E { y: unsigned[8] out }\n\
                 impl E { let p: P = { .x = K + 2, .y = 3 };\n\
                 let arr: unsigned[8][2] = [1, 3 * 4]; y = p.x + arr[1]; }\n"
            ),
            0,
            "a constant struct literal and array literal still seed"
        );
    }

    #[test]
    fn an_exhaustive_match_is_not_an_inferred_latch() {
        let latches = |src: &str| {
            lower_diags(src)
                .into_iter()
                .filter(|d| d.contains("inferred latch"))
                .count()
        };
        const ENUM: &str = "module m;\nenum State { Idle, Run }\n";

        // Every variant named, every arm assigning `a`.
        assert_eq!(
            latches(&format!(
                "{ENUM}#[top] entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle => a = 10, State::Run => a = 20, }} }}\n"
            )),
            0,
            "a match over every variant drives on every path"
        );

        // The same over a character-valued enum.
        assert_eq!(
            latches(
                "module m;\n#[top] entity E { b: Bit in, a: unsigned[8] out }\n\
                 impl E { match b { '0' => a = 10, '1' => a = 20, } }\n"
            ),
            0,
            "and over `Bit`, whose variants are character literals"
        );

        // A variant left out is a genuine latch.
        assert_eq!(
            latches(&format!(
                "{ENUM}#[top] entity E {{ s: State in, a: unsigned[8] out }}\n\
                 impl E {{ match s {{ State::Idle => a = 10, }} }}\n"
            )),
            1,
            "an unmatched variant still holds the previous value"
        );

        // Exhaustive, but one arm does not assign the signal.
        assert_eq!(
            latches(&format!(
                "{ENUM}#[top] entity E {{ s: State in, a: unsigned[8] out, k: unsigned[8] out }}\n\
                 impl E {{ a = 0; match s {{ State::Idle => k = 1, State::Run => a = 2, }} }}\n"
            )),
            1,
            "a signal only one arm assigns is a latch even when the match is complete"
        );
    }

    fn lower_src(src: &str) -> Design {
        // unsigned/signed are library types (attribute-marked vectors), not seeded.
        let src =
            format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = crate::resolve::resolve(modules, &mut sink);
        let typed = crate::types::check(modules, &resolved, &mut sink);
        let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
        lower(modules, &resolved, &hier, &mut sink)
    }

    fn lower_diagnostics(src: &str) -> Vec<crate::diag::Diagnostic> {
        let src =
            format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        let modules = std::slice::from_ref(&module);
        let resolved = crate::resolve::resolve(modules, &mut sink);
        let typed = crate::types::check(modules, &resolved, &mut sink);
        let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
        let _ = lower(modules, &resolved, &hier, &mut sink);
        sink.diagnostics().to_vec()
    }

    fn lower_diags(src: &str) -> Vec<String> {
        lower_diagnostics(src)
            .iter()
            .map(|d| format!("{:?}: {}", d.code, d.message))
            .collect()
    }

    #[test]
    fn equal_entity_leaves_lower_the_resolved_bodies() {
        let sources = [
            (
                "module a; pub entity Cell { a: Bit in, y: Bit out } \
                 impl Cell { y = a; }",
                FileId(0),
            ),
            (
                "module b; pub entity Cell { b: Bit in, z: Bit out } \
                 impl Cell { z = b; }",
                FileId(1),
            ),
            (
                "module user; #[top] entity Top { a: Bit in, b: Bit in, y: Bit out, z: Bit out } \
                 impl Top { \
                   let left: a::Cell = { .a = a, .y = y }; \
                   let right: b::Cell = { .b = b, .z = z }; \
                 }",
                FileId(2),
            ),
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let design = lower(&modules, &resolved, &hierarchy, &mut sink);
        let names: HashSet<&str> = design
            .signals
            .iter()
            .map(|signal| signal.path.as_str())
            .collect();

        assert!(names.contains("Top.left.a"));
        assert!(names.contains("Top.left.y"));
        assert!(names.contains("Top.right.b"));
        assert!(names.contains("Top.right.z"));
        assert!(!names.contains("Top.left.z"));
        assert!(!names.contains("Top.right.y"));
    }

    #[test]
    fn equal_root_entity_leaves_get_distinct_qualified_paths() {
        let sources = [
            (
                "module a; #[top] pub entity Root { value: integer out } \
                 impl Root { value = 11; }",
                FileId(0),
            ),
            (
                "module b; #[top] pub entity Root { value: integer out } \
                 impl Root { value = 22; }",
                FileId(1),
            ),
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let design = lower(&modules, &resolved, &hierarchy, &mut sink);
        assert_eq!(
            sink.error_count(),
            0,
            "diagnostics: {:#?}",
            sink.diagnostics()
        );

        let driven = |path: &str| {
            let signal = design
                .signals
                .iter()
                .position(|signal| signal.path == path)
                .expect("missing root output") as u32;
            design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(signal))
                .map(|driver| driver.expr.clone())
        };
        assert!(matches!(driven("a::Root.value"), Some(Expr::Const(11))));
        assert!(matches!(driven("b::Root.value"), Some(Expr::Const(22))));
        assert_eq!(hierarchy.to_tree_string(), "a::Root\nb::Root\n");
    }

    #[test]
    fn equal_free_function_leaves_lower_the_resolved_bodies() {
        let sources = [
            (
                "module a::math; pub fn select() -> integer { return 11; }",
                FileId(0),
            ),
            (
                "module b::math; pub fn select() -> integer { return 22; }",
                FileId(1),
            ),
            (
                "module user; #[top] entity Top { left: integer out, right: integer out } \
                 impl Top { left = a::math::select(); right = b::math::select(); }",
                FileId(2),
            ),
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let design = lower(&modules, &resolved, &hierarchy, &mut sink);
        assert_eq!(
            sink.error_count(),
            0,
            "diagnostics: {:#?}",
            sink.diagnostics()
        );

        let driven = |path: &str| {
            let signal = design
                .signals
                .iter()
                .position(|signal| signal.path == path)
                .expect("missing output signal") as u32;
            design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(signal))
                .map(|driver| driver.expr.clone())
        };
        assert!(matches!(driven("Top.left"), Some(Expr::Const(11))));
        assert!(matches!(driven("Top.right"), Some(Expr::Const(22))));
    }

    #[test]
    fn equal_type_alias_leaves_keep_the_resolved_representation() {
        let sources = [
            (
                "module a; pub using Scalar = integer<-16..15>; pub using Value = Scalar;",
                FileId(0),
            ),
            (
                "module b; pub using Scalar = integer<-128..127>; pub using Value = Scalar;",
                FileId(1),
            ),
            (
                "module user; #[top] entity Top { left: a::Value in, right: b::Value in }",
                FileId(2),
            ),
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let design = lower(&modules, &resolved, &hierarchy, &mut sink);
        assert_eq!(
            sink.error_count(),
            0,
            "diagnostics: {:#?}",
            sink.diagnostics()
        );

        let signal = |suffix: &str| {
            design
                .signals
                .iter()
                .find(|signal| signal.path.ends_with(suffix))
                .expect("missing aliased signal")
        };
        assert_eq!(signal(".left").width, 5);
        assert_eq!(signal(".left").range, Some((-16, 15)));
        assert_eq!(signal(".right").width, 8);
        assert_eq!(signal(".right").range, Some((-128, 127)));
        assert!(signal(".left").integer && signal(".right").integer);
    }

    #[test]
    fn equal_enum_leaves_keep_variants_widths_and_symbols_distinct() {
        let sources = [
            (
                "module a; pub enum Base { Idle = 3, Run = 7 } pub enum State(Base);",
                FileId(0),
            ),
            (
                "module b; pub enum Base { Low = 1, High = 9 } pub enum State(Base);",
                FileId(1),
            ),
            (
                "module user; #[top] entity Top { left: a::State out, right: b::State out } \
                 impl Top { left = a::State::Run; right = b::State::High; }",
                FileId(2),
            ),
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let design = lower(&modules, &resolved, &hierarchy, &mut sink);
        assert_eq!(
            sink.error_count(),
            0,
            "diagnostics: {:#?}",
            sink.diagnostics()
        );

        let signal = |suffix: &str| {
            design
                .signals
                .iter()
                .find(|signal| signal.path.ends_with(suffix))
                .expect("missing enum signal")
        };
        assert_eq!(signal(".left").width, 3);
        assert_eq!(signal(".left").enum_type.as_deref(), Some("a::State"));
        assert_eq!(signal(".right").width, 4);
        assert_eq!(signal(".right").enum_type.as_deref(), Some("b::State"));
        assert_eq!(
            design.enum_syms["a::State"].get(&7).map(String::as_str),
            Some("Run")
        );
        assert_eq!(
            design.enum_syms["b::State"].get(&9).map(String::as_str),
            Some("High")
        );
        let driven = |suffix: &str| {
            let target = design
                .signals
                .iter()
                .position(|signal| signal.path.ends_with(suffix))
                .expect("missing driven enum signal") as u32;
            design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(target))
                .map(|driver| driver.expr.clone())
        };
        assert!(matches!(driven(".left"), Some(Expr::Const(7))));
        assert!(matches!(driven(".right"), Some(Expr::Const(9))));
    }

    #[test]
    fn equal_struct_leaves_keep_fields_layouts_and_drivers_distinct() {
        let sources = [
            (
                "module a; pub struct Pair { pub left: integer<0..7> }",
                FileId(0),
            ),
            (
                "module b; pub struct Pair { pub right: integer<0..31> }",
                FileId(1),
            ),
            (
                "module user; #[top] entity Top { a_pair: a::Pair out, b_pair: b::Pair out } \
                 impl Top { a_pair = { .left = 5 }; b_pair = { .right = 17 }; }",
                FileId(2),
            ),
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let design = lower(&modules, &resolved, &hierarchy, &mut sink);
        assert_eq!(
            sink.error_count(),
            0,
            "diagnostics: {:#?}",
            sink.diagnostics()
        );

        let left = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(".a_pair.left"))
            .expect("module a field");
        let right = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(".b_pair.right"))
            .expect("module b field");
        assert_eq!(left.width, 3);
        assert_eq!(right.width, 5);
        assert!(design
            .signals
            .iter()
            .all(|signal| !signal.path.ends_with(".a_pair.right")));
        assert!(design
            .signals
            .iter()
            .all(|signal| !signal.path.ends_with(".b_pair.left")));
        assert!(matches!(
            &design.source_layouts["Top.a_pair"].kind,
            LayoutKind::Struct { name, .. } if name == "a::Pair"
        ));
        assert!(matches!(
            &design.source_layouts["Top.b_pair"].kind,
            LayoutKind::Struct { name, .. } if name == "b::Pair"
        ));
        let driver_value = |signal: &Signal| {
            let id = design
                .signals
                .iter()
                .position(|candidate| std::ptr::eq(candidate, signal))
                .expect("signal index") as u32;
            design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(id))
                .map(|driver| driver.expr.clone())
        };
        assert!(matches!(driver_value(left), Some(Expr::Const(5))));
        assert!(matches!(driver_value(right), Some(Expr::Const(17))));
    }

    #[test]
    fn equal_module_constant_leaves_lower_the_resolved_values() {
        let sources = [
            ("module a; pub const VALUE: integer = 11;", FileId(0)),
            ("module b; pub const VALUE: integer = 22;", FileId(1)),
            (
                "module user; #[top] entity Top { left: integer out, right: integer out } \
                 impl Top { left = a::VALUE; right = b::VALUE; }",
                FileId(2),
            ),
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .map(|(source, file)| crate::syntax::parse_module(*file, source, &mut sink))
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let design = lower(&modules, &resolved, &hierarchy, &mut sink);
        assert_eq!(
            sink.error_count(),
            0,
            "diagnostics: {:#?}",
            sink.diagnostics()
        );

        let driven = |path: &str| {
            let signal = design
                .signals
                .iter()
                .position(|signal| signal.path == path)
                .expect("missing output signal") as u32;
            design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(signal))
                .map(|driver| driver.expr.clone())
        };
        assert!(matches!(driven("Top.left"), Some(Expr::Const(11))));
        assert!(matches!(driven("Top.right"), Some(Expr::Const(22))));
    }

    #[test]
    fn module_range_constant_keeps_its_qualified_width_identity() {
        let design = lower_src(
            "module widths; const SPAN: range = 7..0; \
             #[top] entity Top { len: integer out } \
             impl Top { let bits: unsigned[SPAN]; len = bits'length; }",
        );
        let bits = design
            .signals
            .iter()
            .find(|signal| signal.path == "Top.bits")
            .expect("missing range-sized signal");
        assert_eq!(bits.width, 8);

        let len = design
            .signals
            .iter()
            .position(|signal| signal.path == "Top.len")
            .expect("missing length output") as u32;
        assert!(matches!(
            design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(len))
                .map(|driver| &driver.expr),
            Some(Expr::Const(8))
        ));
    }

    #[test]
    fn late_ir_lints_point_at_the_signal_declaration() {
        let source = "module m;\n\
            entity L { c: Logic in, looped: unsigned[8] out, latched: unsigned[8] out, forgotten: unsigned[8] out }\n\
            impl L {\n\
              let discarded: unsigned[8];\n\
              looped = looped;\n\
              if c == '1' { latched = 1; }\n\
              discarded = 2;\n\
            }\n\
            #[top] entity Top {}\n\
            impl Top {\n\
              let c: Logic = '0';\n\
              let looped: unsigned[8]; let latched: unsigned[8]; let forgotten: unsigned[8];\n\
              let dut: L = { .c = c, .looped = looped, .latched = latched, .forgotten = forgotten };\n\
            }\n";
        let diagnostics = lower_diagnostics(source);
        let cases = [
            (crate::diag::codes::COMBINATIONAL_LOOP, "looped", "looped:"),
            (crate::diag::codes::POSSIBLE_LATCH, "latched", "latched:"),
            (
                crate::diag::codes::UNDRIVEN_OUTPUT,
                "forgotten",
                "forgotten:",
            ),
            (
                crate::diag::codes::UNUSED_SIGNAL,
                "discarded",
                "let discarded:",
            ),
        ];
        for (code, signal, declaration) in cases {
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| {
                    diagnostic.code == Some(code) && diagnostic.message.contains(signal)
                })
                .unwrap_or_else(|| panic!("missing {code} for {declaration}: {diagnostics:#?}"));
            let span = diagnostic
                .primary
                .unwrap_or_else(|| panic!("{code} has no primary span: {diagnostic:#?}"));
            let rendered = &source[span.start as usize..span.end as usize];
            assert!(
                rendered.contains(declaration),
                "{code} points at {rendered:?}, not declaration {declaration:?}"
            );
        }
    }

    #[test]
    fn late_ir_errors_keep_stable_codes_and_source_spans() {
        let cases = [
            (
                "module m;\n#[top] entity E {}\nimpl E { let data: unsigned[8][2] = read<unsigned[8]>(\"__siox_missing_span_fixture__.bin\"); }\n",
                crate::diag::codes::COMPILE_TIME_IO,
                "let data:",
            ),
            (
                "module m;\nfn recurse(v: unsigned[8]) -> unsigned[8] { return recurse(v); }\n#[top] entity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = recurse(a); }\n",
                crate::diag::codes::UNBOUNDED_RECURSION,
                "recurse",
            ),
            (
                "module m;\n#[top] entity E { a: unsigned[8] in, y: unsigned[8] out }\nimpl E { y = a after 1; }\n",
                crate::diag::codes::TYPE_MISMATCH,
                "y = a after 1",
            ),
        ];
        for (source, code, source_text) in cases {
            let diagnostics = lower_diagnostics(source);
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == Some(code))
                .unwrap_or_else(|| panic!("missing {code}: {diagnostics:#?}"));
            let span = diagnostic
                .primary
                .unwrap_or_else(|| panic!("{code} has no primary span: {diagnostic:#?}"));
            let rendered = &source[span.start as usize..span.end as usize];
            assert!(
                rendered.contains(source_text),
                "{code} points at {rendered:?}, not {source_text:?}"
            );
        }
    }

    /// Generate loops are unrolled after type checking, so the source-level
    /// dead-assignment lint could not see two iterations/loops resolve to the
    /// same concrete target.
    #[test]
    fn generated_dead_assignments_warn_after_specialization() {
        let warning_count = |source: &str| {
            lower_diags(source)
                .iter()
                .filter(|diagnostic| diagnostic.starts_with("Some(\"W-P014\")"))
                .count()
        };
        let overlapping = "module m;
             #[top] entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 for i in 0..2 { values[i] = 11; }
                 for i in 2..3 { values[i] = 22; }
                 y = values[2];
             }";
        assert_eq!(
            warning_count(overlapping),
            1,
            "the generated writes to values[2] overlap"
        );

        let repeated_instance = "module m;
             entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 for i in 0..2 { values[i] = 11; }
                 for i in 2..3 { values[i] = 22; }
                 y = values[2];
             }
             #[top] entity H { a: unsigned[8] out, b: unsigned[8] out, }
             impl H {
                 let first: E = { .y = a };
                 let second: E = { .y = b };
             }";
        assert_eq!(
            warning_count(repeated_instance),
            1,
            "a source warning must not repeat for every instance"
        );

        let direct = "module m;
             #[top] entity E { y: unsigned[8] out, }
             impl E { y = 1; y = 2; }";
        assert_eq!(
            warning_count(direct),
            1,
            "the IR lint must not duplicate the frontend warning"
        );

        let selected_block = "module m;
             #[top] entity E { y: unsigned[8] out, }
             impl E { if true { y = 1; y = 2; } }";
        assert_eq!(
            warning_count(selected_block),
            1,
            "specializing a block must not repeat its frontend warning"
        );

        let disjoint = "module m;
             #[top] entity E { y: unsigned[8] out, }
             impl E {
                 let values: unsigned[8][4];
                 for i in 0..1 { values[i] = 11; }
                 for i in 2..3 { values[i] = 22; }
                 y = values[2];
             }";
        assert_eq!(
            warning_count(disjoint),
            0,
            "disjoint generated targets are independent"
        );
    }

    #[test]
    fn integer_literals_keep_all_words() {
        let d = lower_src(
            "module m;
             #[top] entity E { y: unsigned[192] out, }
             impl E { y = 1020847100762815390427017310442723737601; }",
        );
        assert!(matches!(
            &d.drivers[0].expr,
            Expr::WideConst(words) if words == &[1, 2, 3]
        ));
    }

    #[test]
    fn signals_retain_kernel_integer_identity() {
        let design = lower_src(
            "module m;
             #[top] entity E {
                 plain: integer out,
                 constrained: integer<-10..10> out,
                 bits: unsigned[8] out,
             }
             impl E { plain = -8; constrained = -3; bits = 255; }",
        );
        let signal = |suffix: &str| {
            design
                .signals
                .iter()
                .find(|signal| signal.path.ends_with(suffix))
                .unwrap_or_else(|| panic!("no signal {suffix}"))
        };
        assert!(signal(".plain").integer);
        assert!(signal(".constrained").integer);
        assert!(!signal(".bits").integer);
    }

    #[test]
    fn deep_acyclic_type_derivation_has_no_magic_depth_limit() {
        let mut src = String::from("module m;\nstruct S0(Bit);\n");
        for i in 1..80 {
            src.push_str(&format!("struct S{i}(S{});\n", i - 1));
        }
        src.push_str(
            "#[top] entity E { y: S79 out, }
             impl E { y = S79(); }",
        );
        let d = lower_src(&src);
        assert_eq!(d.signals.iter().find(|s| s.path == "E.y").unwrap().width, 1);
    }

    /// A struct-literal initializer on an entity-level `let` silently powered
    /// on at 0 — the testbench interpreter honoured it, hardware lowering did
    /// not, so the two engines disagreed about the same declaration.
    #[test]
    fn struct_literal_initializer_seeds_field_inits() {
        let d = lower_src(
            "module m; struct P { a: unsigned[8], b: unsigned[8] }\n\
             entity E { x: unsigned[8] out, y: unsigned[8] out }\n\
             impl E { let p: P = { .a = 11, .b = 22 }; x = p.a; y = p.b; }\n\
             #[top] entity H { x: unsigned[8] out, y: unsigned[8] out, }\n\
             impl H { let d: E = { .x = x, .y = y }; }",
        );
        let init = |suffix: &str| {
            d.signals
                .iter()
                .find(|s| s.path.ends_with(suffix))
                .unwrap_or_else(|| panic!("no signal {suffix}"))
                .init
                .first()
                .copied()
                .unwrap_or(0)
        };
        assert_eq!(init(".p.a"), 11);
        assert_eq!(init(".p.b"), 22);
    }

    /// A concat target has an exact width, so the source must match it — the
    /// lowering otherwise just sliced whatever it was given and zero-filled.
    #[test]
    fn concat_assignment_target_width_must_match() {
        let src = |rhs: &str| {
            format!(
                "module m;\nentity E {{ a: unsigned[8] in, y: unsigned[4] out, z: unsigned[4] out, }}\n\
                 impl E {{ {{y, z}} = {rhs}; }}\n\
                 #[top] entity H {{ a: unsigned[8] in, y: unsigned[4] out, z: unsigned[4] out, }}\n\
                 impl H {{ let d: E = {{ .a = a, .y = y, .z = z }}; }}\n"
            )
        };
        let mismatched = lower_diags(&src("a[3..0]"));
        assert!(
            mismatched
                .iter()
                .any(|d| d.contains("concatenation target is 8 bits")),
            "expected a width mismatch, got: {mismatched:?}"
        );
        let exact = lower_diags(&src("a"));
        assert!(
            !exact.iter().any(|d| d.contains("concatenation target")),
            "an exact-width source is fine: {exact:?}"
        );
    }

    /// Two producers wired to one bus net is the classic miswiring. It must
    /// name the conflict (not the missing `Resolve` impl it happens to hit),
    /// carry a code, and point at each contributing connection — this is the
    /// only guard left now that views carry no coarse endpoint role.
    #[test]
    fn conflicting_drivers_name_the_conflict_and_its_sites() {
        let src = "module m;\n\
            struct Stream { valid: Bit, data: unsigned[8] }\n\
            view Source for Stream { valid out, data out }\n\
            entity Producer { bus: Stream Source, value: unsigned[8] in }\n\
            impl Producer { bus.valid = '1'; bus.data = value; }\n\
            #[top]\n\
            entity BadLink { a: unsigned[8] in, b: unsigned[8] in }\n\
            impl BadLink {\n\
              let wire: Stream;\n\
              let p1: Producer = { .bus = wire, .value = a };\n\
              let p2: Producer = { .bus = wire, .value = b };\n\
            }\n";
        let mut sink = DiagnosticSink::new();
        let full =
            format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n{CLK_PRELUDE}");
        let module = crate::syntax::parse_module(FileId(0), &full, &mut sink);
        let modules = std::slice::from_ref(&module);
        let resolved = crate::resolve::resolve(modules, &mut sink);
        let typed = crate::types::check(modules, &resolved, &mut sink);
        let hier = crate::elab::elaborate(modules, &resolved, &typed, &mut sink);
        let _ = lower(modules, &resolved, &hier, &mut sink);

        let conflicts: Vec<_> = sink
            .diagnostics()
            .iter()
            .filter(|d| d.code == Some(crate::diag::codes::CONFLICTING_DRIVERS))
            .collect();
        assert!(
            !conflicts.is_empty(),
            "expected a conflicting-drivers error"
        );
        for d in &conflicts {
            assert!(
                d.message.contains("conflicting sources"),
                "should name the conflict, got: {}",
                d.message
            );
            assert!(d.primary.is_some(), "should point at a connection site");
            assert!(!d.labels.is_empty(), "should label the other source(s)");
            assert!(d.help.is_some(), "should say how to fix it");
        }
    }

    #[test]
    fn resolved_parallel_drivers_are_legal_without_a_warning() {
        let diagnostics = lower_diags(
            "module m;\n\
             enum Wire { '0', '1' }\n\
             trait Resolve { fn resolve(self, rhs: Wire) -> Wire; }\n\
             impl Resolve for Wire {\n\
                 fn resolve(self, rhs: Wire) -> Wire { return self; }\n\
             }\n\
             #[top] entity Net { a: Wire in, b: Wire in, y: Wire out }\n\
             impl Net { y = a; }\n\
             impl Net { y = b; }\n",
        );
        assert!(
            !diagnostics.iter().any(|diagnostic| {
                diagnostic.contains("driver") || diagnostic.contains("W-P001")
            }),
            "a type-defined Resolve fold is intentional, not suspicious: {diagnostics:?}"
        );
    }

    #[test]
    fn bit_pattern_masks() {
        // Bare strings are per-bit with `-` as the don't-care.
        assert_eq!(
            crate::syntax::bit_pattern_mask("\"01--\""),
            Some((vec![0b1100], vec![0b0100]))
        );
        assert_eq!(
            crate::syntax::bit_pattern_mask("\"0000_11--\""),
            Some((vec![0b11111100], vec![0b00001100]))
        );
        // Radix prefixes mask a whole group with `?`.
        assert_eq!(
            crate::syntax::bit_pattern_mask("x\"A?\""),
            Some((vec![0xF0], vec![0xA0]))
        );
        assert_eq!(
            crate::syntax::bit_pattern_mask("x\"?3\""),
            Some((vec![0x0F], vec![0x03]))
        );
        assert_eq!(
            crate::syntax::bit_pattern_mask("o\"7?\""),
            Some((vec![0o70], vec![0o70]))
        );
        let wide = format!("\"1{}\"", "-".repeat(128));
        assert_eq!(
            crate::syntax::bit_pattern_mask(&wide),
            Some((vec![0, 0, 1], vec![0, 0, 1]))
        );
        assert_eq!(crate::syntax::bit_pattern_mask("\"2\""), None); // bad binary digit
    }

    #[test]
    fn applied_view_flattens_its_backing_struct_fields() {
        let d = lower_src(
            "module m;\n\
             struct HandshakeBus { valid: Bit, ready: Bit }\n\
             view Handshake for HandshakeBus { valid out, ready in }\n\
             impl HandshakeBus Handshake { fn assert_valid(self) { self.valid = '1'; } }\n\
             #[top] entity Producer { bus: HandshakeBus Handshake, observed: Bit out, }\n\
             impl Producer { bus.assert_valid(); observed = bus.ready; }",
        );
        assert!(d.signals.iter().any(|s| s.path.ends_with(".bus.valid")));
        assert!(d.signals.iter().any(|s| s.path.ends_with(".bus.ready")));
        assert!(matches!(
            d.source_layouts.get("Producer.bus").map(|layout| &layout.kind),
            Some(LayoutKind::Struct {
                name,
                view: Some(view),
                fields,
            }) if name == "HandshakeBus"
                && view == "Handshake"
                && fields.iter().map(|field| field.direction.clone()).collect::<Vec<_>>()
                    == [Some(LayoutDirection::Out), Some(LayoutDirection::In)]
        ));
    }

    #[test]
    fn generic_trait_functions_access_applied_view_backing_fields() {
        let d = lower_src(
            "module m;\n\
             trait Readable<T> { fn read(self) -> T; }\n\
             trait Writable<T> { fn write(self, value: T); }\n\
             fn read<Bus: Readable, Value>(bus: Bus) -> Value { return bus.read(); }\n\
             fn write<Bus: Writable, Value>(bus: Bus, value: Value) { bus.write(value); }\n\
             struct Spi { tx: unsigned[8], rx: unsigned[8] }\n\
             view Controller for Spi { tx out, rx in }\n\
             impl Readable<unsigned[8]> for Spi Controller {\n\
               fn read(self) -> unsigned[8] { return self.rx; }\n\
             }\n\
             impl Writable<unsigned[8]> for Spi Controller {\n\
               fn write(self, value: unsigned[8]) { self.tx = value; }\n\
             }\n\
             entity Device {\n\
               bus: Spi Controller, source: unsigned[8] in, sampled: unsigned[8] out,\n\
             }\n\
             impl Device { write(bus, source); sampled = read(bus); }\n\
             #[top] entity Link {}\n\
             impl Link {\n\
               let wire: Spi;\n\
               let source: unsigned[8];\n\
               let sampled: unsigned[8];\n\
               let device: Device = { .bus = wire, .source = source, .sampled = sampled };\n\
             }",
        );
        assert!(
            d.validate().is_empty(),
            "a view method must bind `self` to the backing struct fields"
        );
        let sampled = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(".device.sampled"))
            .expect("sampled signal");
        let driver = d
            .drivers
            .iter()
            .find(|driver| driver.target.0 as usize == sampled)
            .expect("sampled driver");
        assert!(
            matches!(driver.expr, Expr::Current(_)),
            "read() should lower to the selected backing signal: {:?}",
            driver.expr
        );
        let tx = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(".device.bus.tx"))
            .expect("view tx signal");
        assert!(
            d.drivers
                .iter()
                .any(|driver| driver.target.0 as usize == tx),
            "generic write() should inline its nested trait method as a driver"
        );
    }

    #[test]
    fn enum_signal_inits_to_first_variant() {
        // Derived `new()` default: an uninitialized enum signal powers on
        // holding its *first* variant. With a non-zero-based first
        // discriminant, that is a valid member — a bare `0` would not be.
        let d = lower_src(
            "module m;\n\
             enum Phase { Idle = 2, Run = 3, Done = 4 }\n\
             enum Step  { A, B, C }\n\
             #[top]\n\
             entity T {}\n\
             impl T { let p: Phase; let s: Step; }\n",
        );
        let init = |suffix: &str| {
            d.signals
                .iter()
                .find(|s| s.path.ends_with(suffix))
                .unwrap_or_else(|| panic!("no {suffix}"))
                .init
                .first()
                .copied()
                .unwrap_or(0)
        };
        assert_eq!(init(".p"), 2, "Phase defaults to Idle = 2, not 0");
        assert_eq!(init(".s"), 0, "0-based Step still defaults to A = 0");
    }

    #[test]
    fn explicit_enum_init_overrides_first_variant() {
        // An explicit `let p = Run` beats the first-variant default.
        let d = lower_src(
            "module m;\n\
             enum Phase { Idle = 2, Run = 3, Done = 4 }\n\
             #[top]\n\
             entity T {}\n\
             impl T { let p: Phase = Phase::Run; }\n",
        );
        let p = d
            .signals
            .iter()
            .find(|s| s.path.ends_with(".p"))
            .expect("no .p");
        assert_eq!(p.init, vec![3], "explicit initializer Run = 3 wins");
    }

    #[test]
    fn nullary_constructor_lowers_to_default() {
        // `T()` in expression position is the type's derived default: an enum →
        // its first variant, a numeric/vector → 0. Same rule as the implicit
        // signal init, now writable.
        let d = lower_src(
            "module m;\n\
             enum Phase { Idle = 2, Run = 3 }\n\
             #[top]\n\
             entity E { y: Phase out, z: unsigned[8] out }\n\
             impl E { y = Phase(); z = unsigned[8](); }\n",
        );
        let drv = |suffix: &str| -> u64 {
            let sig = d
                .signals
                .iter()
                .position(|s| s.path.ends_with(suffix))
                .expect("sig");
            let dr = d
                .drivers
                .iter()
                .find(|dr| dr.target.0 as usize == sig)
                .expect("driver");
            match &dr.expr {
                Expr::Const(c) => *c,
                other => panic!("{suffix} not a const: {other:?}"),
            }
        };
        assert_eq!(drv(".y"), 2, "Phase() == first variant Idle = 2");
        assert_eq!(drv(".z"), 0, "unsigned[8]() == 0");
    }

    #[test]
    fn nullary_constructor_defaults_struct_fields() {
        // `S()` on a struct defaults each field structurally: an enum field to
        // its first variant, a numeric field to 0 — through a composed struct
        // field as well as a direct one.
        let d = lower_src(
            "module m;\n\
             enum Phase { Idle = 2, Run = 3 }\n\
             struct Header { flag: Bit, ph: Phase }\n\
             struct Packet { header: Header, data: unsigned[8] }\n\
             #[top]\n\
             entity E { o: Packet out }\n\
             impl E { o = Packet::new(); }\n",
        );
        let drv = |suffix: &str| -> u64 {
            let sig = d
                .signals
                .iter()
                .position(|s| s.path.ends_with(suffix))
                .expect("sig");
            let dr = d
                .drivers
                .iter()
                .find(|dr| dr.target.0 as usize == sig)
                .expect("driver");
            match &dr.expr {
                Expr::Const(c) => *c,
                other => panic!("{suffix} not a const: {other:?}"),
            }
        };
        assert_eq!(
            drv(".o.header.ph"),
            2,
            "enum field → first variant Idle = 2"
        );
        assert_eq!(drv(".o.header.flag"), 0, "composed Bit field → 0");
        assert_eq!(drv(".o.data"), 0, "numeric field → 0");
    }

    #[test]
    fn range_attributes_read_declared_bounds() {
        // A descending `[7..0]` and an ascending width-only `[8]` expose the
        // VHDL range attributes; direction is preserved.
        let d = lower_src(
            "module m;\n\
             #[top]\n\
             entity E {\n\
               dn: unsigned[7..0] in, up: unsigned[8] in,\n\
               a: unsigned[8] out, b: unsigned[8] out, c: unsigned[8] out, e: unsigned[8] out,\n\
               f: unsigned[8] out, g: unsigned[8] out, h: unsigned[8] out,\n\
             }\n\
             impl E {\n\
               a = dn'left; b = dn'right; c = dn'high; e = dn'low;\n\
               f = dn'ascending; g = dn'length; h = up'ascending;\n\
             }\n",
        );
        let drv = |suffix: &str| -> u64 {
            let sig = d
                .signals
                .iter()
                .position(|s| s.path.ends_with(suffix))
                .expect("sig");
            let dr = d
                .drivers
                .iter()
                .find(|dr| dr.target.0 as usize == sig)
                .expect("driver");
            match &dr.expr {
                Expr::Const(c) => *c,
                other => panic!("{suffix} not const: {other:?}"),
            }
        };
        assert_eq!(drv(".a"), 7, "dn'left");
        assert_eq!(drv(".b"), 0, "dn'right");
        assert_eq!(drv(".c"), 7, "dn'high");
        assert_eq!(drv(".e"), 0, "dn'low");
        assert_eq!(drv(".f"), 0, "dn'ascending (descending → false)");
        assert_eq!(drv(".g"), 8, "dn'length");
        assert_eq!(drv(".h"), 1, "up'ascending (width-only → true)");
    }

    #[test]
    fn undriven_output_port_warns() {
        // `forgotten` is never assigned; `driven` is. Only the former warns.
        let diags = lower_diags(
            "module m;\n\
             entity E { a: unsigned[8] in, driven: unsigned[8] out, forgotten: unsigned[8] out }\n\
             impl E { driven = a + 1; }\n\
             #[top]\n\
             entity T {}\n\
             impl T { let a: unsigned[8]; let d: unsigned[8]; let f: unsigned[8];\n\
               let dut: E = { .a = a, .driven = d, .forgotten = f }; }\n",
        );
        let undriven: Vec<&String> = diags.iter().filter(|d| d.contains("W-P011")).collect();
        assert_eq!(
            undriven.len(),
            1,
            "one undriven-output warning: {undriven:?}"
        );
        assert!(
            undriven[0].contains("forgotten"),
            "flags forgotten: {undriven:?}"
        );
    }

    #[test]
    fn undriven_internal_signal_warns() {
        // `dead` (value-less, never assigned) warns; `used` is driven and
        // `konst` has an initializer, so neither does.
        let diags = lower_diags(
            "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E {\n  let used: unsigned[8];\n  let dead: unsigned[8];\n  let konst: unsigned[8] = 5;\n\
               used = a + 1;\n  y = used + konst;\n }\n\
             #[top]\n\
             entity T {}\n\
             impl T { let a: unsigned[8]; let y: unsigned[8]; let dut: E = { .a = a, .y = y }; }\n",
        );
        let undriven: Vec<&String> = diags
            .iter()
            .filter(|d| d.contains("W-P011") && d.contains("never driven"))
            .collect();
        assert_eq!(undriven.len(), 1, "one undriven signal: {undriven:?}");
        assert!(undriven[0].contains("dead"), "flags dead: {undriven:?}");
    }

    #[test]
    fn unused_internal_signal_warns_without_runner_false_positives() {
        let diags = lower_diags(
            "module m;\n\
             entity E { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E { let dead: unsigned[8]; dead = a + 1; y = a; }\n\
             #[test] entity T {}\n\
             impl T { let a: unsigned[8]; let observed: unsigned[8];\n\
               let dut: E = { .a = a, .y = observed }; assert!(observed == a); }\n",
        );
        let unused: Vec<&String> = diags.iter().filter(|d| d.contains("W-P003")).collect();
        assert_eq!(unused.len(), 1, "one unused internal signal: {unused:?}");
        assert!(unused[0].contains("dead"), "flags dead: {unused:?}");
    }

    #[test]
    fn if_else_mux_is_not_a_latch() {
        // A signal assigned in both the `if` and the `else` is fully covered —
        // no possible-latch warning — but one assigned only in the `if` is.
        let covered = lower_diags(
            "module m;\n\
             entity M { c: Bit in, a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out }\n\
             impl M { if c { y = a; } else { y = b; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: unsigned[8]; let b: unsigned[8]; let y: unsigned[8];\n\
               let dut: M = { .c = c, .a = a, .b = b, .y = y };\n\
             }\n",
        );
        assert!(
            !covered.iter().any(|d| d.contains("inferred latch")),
            "if/else mux wrongly flagged: {covered:?}"
        );

        let latch = lower_diags(
            "module m;\n\
             entity M { c: Bit in, a: unsigned[8] in, y: unsigned[8] out }\n\
             impl M { if c { y = a; } }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let c: Bit; let a: unsigned[8]; let y: unsigned[8];\n\
               let dut: M = { .c = c, .a = a, .y = y };\n\
             }\n",
        );
        assert!(
            latch.iter().any(|d| d.contains("inferred latch")),
            "true latch (no else) should warn: {latch:?}"
        );
    }

    #[test]
    fn strict_assignment_width_mismatch() {
        // A parameterized width (`unsigned[W]`) the type checker can't see resolves
        // at elaboration; assigning a 16-bit signal to an 8-bit target is then a
        // width mismatch surfaced by IR lowering.
        let bad = lower_diags(
            "module m;\n\
             entity E { b: unsigned[W] in, y: unsigned[8] out }\n\
             impl E { y = b; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: unsigned[16]; let y: unsigned[8];\n\
               let dut: E<W=16> = { .b = b, .y = y };\n\
             }\n",
        );
        assert!(bad.iter().any(|d| d.contains("width mismatch")), "{bad:?}");

        // A matching-width slice of the same signal is fine — the value width
        // (8) equals the target (8).
        let ok = lower_diags(
            "module m;\n\
             entity E { b: unsigned[W] in, y: unsigned[8] out }\n\
             impl E { y = b[7..0]; }\n\
             #[test] entity Tb {}\n\
             impl Tb {\n\
               let b: unsigned[16]; let y: unsigned[8];\n\
               let dut: E<W=16> = { .b = b, .y = y };\n\
             }\n",
        );
        assert!(!ok.iter().any(|d| d.contains("width mismatch")), "{ok:?}");

        // Indexing remains scalar even when the vector itself is one element
        // wide. Retained checker types keep it distinct from the vector.
        let one = lower_diags(
            "module m;\n\
             entity E { b: unsigned[1] in, y: Logic out }\n\
             impl E { y = b[0]; }\n",
        );
        assert!(!one.iter().any(|d| d.contains("width mismatch")), "{one:?}");
    }

    #[test]
    fn combinational_loop_lint() {
        // `t = t + a;` is a zero-delay self-cycle -> flagged; a plain chain
        // (`y = x + 1`) is not.
        let diags = lower_diags(
            "module m;\n\
             entity L { a: unsigned[8] in, y: unsigned[8] out }\n\
             impl L { let t: unsigned[8]; t = t + a; y = t; }\n\
             #[top] entity Top {}\n\
             impl Top { let a: unsigned[8]; let y: unsigned[8]; let d: L = { .a = a, .y = y }; }\n",
        );
        let loops: Vec<&String> = diags.iter().filter(|d| d.contains("W-P010")).collect();
        assert!(!loops.is_empty(), "self-cycle flagged: {diags:?}");
        assert!(loops.iter().any(|d| d.contains(".t")), "names t: {loops:?}");

        let ok = lower_diags(
            "module m;\n\
             entity C { x: unsigned[8] in, y: unsigned[8] out }\n\
             impl C { y = x + 1; }\n\
             #[top] entity Top {}\n\
             impl Top { let x: unsigned[8]; let y: unsigned[8]; let d: C = { .x = x, .y = y }; }\n",
        );
        assert!(
            !ok.iter().any(|d| d.contains("W-P010")),
            "no false positive: {ok:?}"
        );
    }

    #[test]
    fn possible_latch_lint() {
        // `y` is only assigned under a condition (inferred latch); `z` has an
        // unconditional default and must not be flagged.
        let diags = lower_diags(
            "module m;\n\
             entity L { c: Logic in, a: unsigned[8] in, y: unsigned[8] out, z: unsigned[8] out }\n\
             impl L { if c == '1' { y = a; } z = a; }\n\
             #[top] entity Top {}\n\
             impl Top { let c: Logic; let a: unsigned[8]; let y: unsigned[8]; let z: unsigned[8];\n\
               let d: L = { .c = c, .a = a, .y = y, .z = z }; }\n",
        );
        let latch: Vec<&String> = diags.iter().filter(|d| d.contains("W-P002")).collect();
        assert_eq!(latch.len(), 1, "exactly one latch warning: {diags:?}");
        assert!(latch[0].contains(".y"), "flags y, not z: {latch:?}");
    }

    #[test]
    fn enum_signals_carry_symbols() {
        // A Logic-typed signal records its enum type, and the design exports the
        // discriminant -> symbol map (with std's char-variant names) so
        // consumers can print `'X'` instead of `3`.
        let d = lower_src(
            "module m;\n\
             enum Logic { '0', '1', 'Z', 'X' }\n\
             enum State { Idle, Run }\n\
             entity E { a: Logic in, s: State out }\n\
             impl E { s = State::Idle; }\n\
             #[top] entity Top {}\n\
             impl Top { let a: Logic; let s: State; let e: E = { .a = a, .s = s }; }\n",
        );
        let sig = |p: &str| d.signals.iter().find(|s| s.path == p).unwrap();
        assert_eq!(sig("Top.e.a").enum_type.as_deref(), Some("Logic"));
        assert_eq!(sig("Top.e.s").enum_type.as_deref(), Some("State"));
        assert_eq!(
            d.enum_syms["Logic"].get(&3).map(String::as_str),
            Some("'X'")
        );
        assert_eq!(
            d.enum_syms["State"].get(&0).map(String::as_str),
            Some("Idle")
        );
    }

    const COUNTER: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          clk: Bit in,\n\
          rst: Logic in,\n\
          en: Bit in,\n\
          count: unsigned[W] out,\n\
        }\n\
        impl<W: integer> Counter<W> {\n\
          let value: unsigned[W] = 0;\n\
          if clk.rising() {\n\
            if rst == '1' {\n\
              value = 0;\n\
            } else if en {\n\
              value = value + 1;\n\
            }\n\
          }\n\
          count = value;\n\
        }\n\
        #[test]\n\
        entity H {}\n\
        impl H {\n\
          let clk: Bit = '0';\n\
          let rst: Logic = '1';\n\
          let en: Bit = '1';\n\
          let count: unsigned[8];\n\
          let dut: Counter<W = 8> = { .clk = clk, .rst = rst, .en = en, .count = count };\n\
        }\n";

    #[test]
    fn lowers_signals_driver_and_event_block() {
        let d = lower_src(COUNTER);
        // Counter signals: clk, rst, en, count, value. The instance's `W = 8`
        // makes the parametric `unsigned[W]` widths concrete.
        let count = d.signals.iter().find(|s| s.path == "H.dut.count").unwrap();
        assert_eq!(count.width, 8);
        assert!(d.signals.iter().any(|s| s.path == "H.dut.value"));
        // One combinational driver: count = value.
        assert_eq!(d.drivers.len(), 1);
        // One event block (clk.rising()) with two next-state updates.
        assert_eq!(d.event_blocks.len(), 1);
        assert_eq!(d.event_blocks[0].updates.len(), 2);
    }

    #[test]
    fn lowers_nested_instances_with_connections() {
        // Add2 instantiates two Add1s wired through `mid`. Each instance must
        // get its own signals, and every port connection must become a driver.
        let src = "module m;\n\
            entity Add1 { a: unsigned[8] in, y: unsigned[8] out }\n\
            impl Add1 { y = a + 1; }\n\
            entity Add2 { a: unsigned[8] in, y: unsigned[8] out }\n\
            impl Add2 {\n\
              let mid: unsigned[8];\n\
              let s1: Add1 = { .a = a, .y = mid };\n\
              let s2: Add1 = { .a = mid, .y = y };\n\
            }\n\
            #[test] entity T {}\n\
            impl T {\n\
              let a: unsigned[8] = 10;\n\
              let y: unsigned[8];\n\
              let dut: Add2 = { .a = a, .y = y };\n\
            }\n";
        let d = lower_src(src);
        let id = |path: &str| {
            d.signals
                .iter()
                .position(|s| s.path == path)
                .map(|i| SignalId(i as u32))
        };
        // Two distinct Add1 instances, each with its own signals.
        assert!(id("T.dut.s1.a").is_some() && id("T.dut.s1.y").is_some());
        assert!(id("T.dut.s2.a").is_some() && id("T.dut.s2.y").is_some());
        // Every connection is a driver: `in` ports read the parent, `out`
        // ports drive it.
        let wired = |target: &str, source: &str| {
            let (t, s) = (id(target).unwrap(), id(source).unwrap());
            d.drivers
                .iter()
                .any(|dr| dr.target == t && matches!(&dr.expr, Expr::Current(x) if *x == s))
        };
        assert!(wired("T.dut.s1.a", "T.dut.a"), "s1.a <- a");
        assert!(wired("T.dut.mid", "T.dut.s1.y"), "mid <- s1.y");
        assert!(wired("T.dut.s2.a", "T.dut.mid"), "s2.a <- mid");
        assert!(wired("T.dut.y", "T.dut.s2.y"), "y <- s2.y");
    }

    #[test]
    fn if_expression_lowers_to_select() {
        let d = lower_src(
            "module m;\n\
             entity Mux { sel: Bit in, a: unsigned[8] in, b: unsigned[8] in, y: unsigned[8] out }\n\
             impl Mux { y = if sel { a } else { b }; }\n\
             #[test] entity T {}\n\
             impl T { let sel: Bit; let a: unsigned[8]; let b: unsigned[8]; let y: unsigned[8];\n\
               let dut: Mux = { .sel = sel, .a = a, .b = b, .y = y }; }\n",
        );
        let y = d
            .signals
            .iter()
            .position(|s| s.path == "T.dut.y")
            .map(|i| SignalId(i as u32))
            .unwrap();
        let dr = d.drivers.iter().find(|dr| dr.target == y).unwrap();
        assert!(
            matches!(&dr.expr, Expr::Select { .. }),
            "if-expression must lower to a select"
        );
    }

    /// An expression that is not a place must be distinguished from supported
    /// field/index targets.
    #[test]
    fn a_bad_assignment_target_says_which_kind_it_is() {
        let not_a_place = lower_diags(
            "module m;
             fn f(x: unsigned[8]) -> unsigned[8] { return x; }
             #[top] entity E { a: unsigned[8] in, y: unsigned[8] out }
             impl E { f(a) = a; y = a; }",
        );
        assert!(
            not_a_place
                .iter()
                .any(|d| d.contains("E-P018") && d.contains("`f(a)` cannot be assigned to")),
            "{not_a_place:#?}"
        );
    }

    /// Nested array indices are independent runtime mux dimensions on reads
    /// and a conjunction of match gates on writes.
    #[test]
    fn chained_runtime_indices_lower_to_muxes_and_gated_writes() {
        let source = "module m;
             #[top] entity E {
               a: unsigned[8] in, row: integer in, col: integer in,
               y: unsigned[8] out
             }
             impl E {
               let mm: unsigned[8][2][2];
               mm[row][col] = a;
               y = mm[row][col];
             }";
        let diags = lower_diags(source);
        assert!(
            diags
                .iter()
                .all(|diagnostic| !diagnostic.contains("E-P017")),
            "nested runtime access should no longer be rejected: {diags:#?}"
        );
        let design = lower_src(source);
        assert!(design.validate().is_empty(), "{:#?}", design.validate());
        let matrix_writes: Vec<_> = design
            .drivers
            .iter()
            .filter(|driver| {
                design.signals[driver.target.0 as usize]
                    .path
                    .contains(".mm[")
            })
            .collect();
        assert_eq!(matrix_writes.len(), 4, "one gated write per scalar leaf");
        assert!(
            matrix_writes.iter().all(|driver| driver.cond.is_some()),
            "every leaf write must test both runtime indices"
        );
        let output = design
            .signals
            .iter()
            .position(|signal| signal.path == "E.y")
            .map(|index| SignalId(index as u32))
            .unwrap();
        let read = design
            .drivers
            .iter()
            .find(|driver| driver.target == output)
            .unwrap();
        assert!(
            matches!(read.expr, Expr::Select { .. }),
            "read is a mux tree"
        );
    }

    #[test]
    fn runtime_index_then_struct_field_reaches_the_scalar_leaf() {
        let source = "module m;
             struct Packet { data: unsigned[8], tag: unsigned[4] }
             #[top] entity E {
               a: unsigned[8] in, slot: integer in, y: unsigned[8] out
             }
             impl E {
               let packets: Packet[2];
               packets[slot].data = a;
               y = packets[slot].data;
             }";
        let diagnostics = lower_diags(source);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.contains("E-P017")),
            "{diagnostics:#?}"
        );
        assert!(lower_src(source).validate().is_empty());
    }

    #[test]
    fn packed_vector_indices_use_declared_labels_and_storage_offsets() {
        let design = lower_src(
            "module m; using std::bits::unsigned; using std::logic::Logic;
             #[top] entity E {
               value: unsigned[15..8] in,
               high: Logic out, low: Logic out, whole: unsigned[8] out
             }
             impl E { high = value[15]; low = value[8]; whole = value[15..8]; }",
        );
        let driver = |path: &str| {
            let signal = design
                .signals
                .iter()
                .position(|signal| signal.path == path)
                .map(|index| SignalId(index as u32))
                .unwrap();
            &design
                .drivers
                .iter()
                .find(|driver| driver.target == signal)
                .unwrap()
                .expr
        };
        assert!(matches!(driver("E.high"), Expr::Slice { hi: 7, lo: 7, .. }));
        assert!(matches!(driver("E.low"), Expr::Slice { hi: 0, lo: 0, .. }));
        assert!(matches!(
            driver("E.whole"),
            Expr::Slice { hi: 7, lo: 0, .. }
        ));
        assert!(design.validate().is_empty(), "{:#?}", design.validate());
    }

    #[test]
    fn runtime_packed_bit_read_write_updates_value_and_metavalue_planes() {
        let source = "module m; using std::bits::unsigned; using std::logic::{Bit, Logic};
             #[top] entity E {
               clk: Bit in, index: unsigned[5] in, data: Logic in, q: Logic out
             }
             impl E {
               let word: unsigned[15..8] = \"00000000\";
               if clk.rising() { word[index] = data; }
               q = word[index];
             }";
        let diagnostics = lower_diags(source);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.contains("E-P017")),
            "{diagnostics:#?}"
        );
        let design = lower_src(source);
        let word = design
            .signals
            .iter()
            .position(|signal| signal.path == "E.word")
            .unwrap() as u32;
        let meta = *design
            .meta_of
            .get(&word)
            .expect("runtime writes need a companion");
        let event = design.event_blocks.first().expect("clocked bit write");
        assert_eq!(
            event
                .updates
                .iter()
                .filter(|update| update.target.0 == word)
                .count(),
            8,
            "one mutually exclusive value update per declared label"
        );
        assert_eq!(
            event
                .updates
                .iter()
                .filter(|update| update.target.0 == meta)
                .count(),
            8,
            "the metavalue nibble follows every value update"
        );
        let q = design
            .signals
            .iter()
            .position(|signal| signal.path == "E.q")
            .unwrap() as u32;
        let mut reads = Vec::new();
        read_set(
            &design
                .drivers
                .iter()
                .find(|driver| driver.target.0 == q)
                .expect("q driver")
                .expr,
            &mut reads,
        );
        assert!(reads.iter().any(|signal| signal.0 == word));
        assert!(reads.iter().any(|signal| signal.0 == meta));
        assert!(design.validate().is_empty(), "{:#?}", design.validate());
    }

    /// What reaches the site table, and in what order.
    ///
    /// The two engines cannot disagree on the numbering -- they call this one
    /// function -- so reordering or duplicating entries would still line up.
    /// What is asserted here is the *contents*: a span only earns an index if
    /// some assignment can be blamed through it, which is what leaves index 0
    /// free to mean "fall back to the declaration". The order is pinned as the
    /// documented shape rather than as a defence against drift.
    #[test]
    fn range_sites_indexes_only_blamable_assignments() {
        let span = |start: u32| crate::diag::Span::new(crate::diag::FileId(0), start..start + 1);
        let signal = |range: Option<(i64, i64)>| Signal {
            path: "s".into(),
            declaration_span: span(0),
            width: 8,
            real: false,
            integer: true,
            char: false,
            range,
            init: vec![0],
            enum_type: None,
        };
        let driver = |target: u32, at: Option<crate::diag::Span>| Driver {
            target: SignalId(target),
            cond: None,
            expr: Expr::Const(0),
            meta: None,
            ctx: 0,
            span: at,
        };
        let design = Design {
            // 0 is ranged, 1 is not.
            signals: vec![signal(Some((0, 10))), signal(None)],
            drivers: vec![
                driver(0, Some(span(100))),
                // A driver the lowering synthesized -- a port connection has no
                // line of its own, and must not take an index.
                driver(0, None),
                // An unranged target can never fail this way.
                driver(1, Some(span(200))),
                // The same statement lowered twice (one body, two instances)
                // is one site, or the two engines would number differently.
                driver(0, Some(span(100))),
                driver(0, Some(span(300))),
            ],
            event_blocks: vec![EventBlock {
                condition: Expr::Const(1),
                updates: vec![
                    NextUpdate {
                        target: SignalId(0),
                        cond: None,
                        expr: Expr::Const(0),
                        meta: None,
                        span: Some(span(400)),
                    },
                    NextUpdate {
                        target: SignalId(1),
                        cond: None,
                        expr: Expr::Const(0),
                        meta: None,
                        span: Some(span(500)),
                    },
                ],
                ctx: 0,
            }],
            ..Design::default()
        };
        let sites = design.range_sites();
        assert_eq!(
            sites,
            vec![span(100), span(300), span(400)],
            "drivers first in lowering order, then event updates"
        );
        // Drivers with no span, and spans on unranged targets, are absent --
        // which is what leaves site 0 free to mean "fall back to the
        // declaration".
        assert!(!sites.contains(&span(200)));
        assert!(!sites.contains(&span(500)));
    }

    #[test]
    fn validate_accepts_good_and_flags_bad_ir() {
        // A lowered counter is well-formed.
        assert!(lower_src(COUNTER).validate().is_empty());

        let sig = |w: u32| Signal {
            path: "s".into(),
            declaration_span: crate::diag::Span::new(crate::diag::FileId(0), 0..0),
            width: w,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        };
        // Out-of-range signal id, an Unknown, a bad slice, and a width-0 signal.
        let bad = Design {
            signals: vec![sig(0)], // width 0 -> flagged
            drivers: vec![Driver {
                span: None,
                target: SignalId(9), // out of range
                cond: Some(Expr::Unknown),
                expr: Expr::Slice {
                    base: Box::new(Expr::Current(SignalId(0))),
                    hi: 1,
                    lo: 3,
                },
                meta: None,
                ctx: 0,
            }],
            event_blocks: vec![],
            enum_bases: HashMap::new(),
            enum_syms: HashMap::new(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };
        let issues = bad.validate();
        assert!(
            issues.iter().any(|i| i.contains("unknown width")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.contains("out of range")),
            "{issues:?}"
        );
        assert!(issues.iter().any(|i| i.contains("Unknown")), "{issues:?}");
        assert!(
            issues.iter().any(|i| i.contains("slice bounds")),
            "{issues:?}"
        );
        // Each issue names the signal it concerns. A driver's index in the
        // vector is an artefact of lowering: "driver 0 expr" sent the reader
        // to an IR dump to find out which line of their design it meant.
        assert!(
            issues
                .iter()
                .all(|i| i.contains("`s`") || i.contains("signal id")),
            "every issue names a signal: {issues:?}"
        );
    }

    #[test]
    fn persisted_leaf_layout_is_the_backend_width_authority() {
        let mut design = lower_src("module m; #[top] entity E { value: unsigned[8] in } impl E {}");
        let id = SignalId(
            design
                .signals
                .iter()
                .position(|signal| signal.path == "E.value")
                .expect("value signal") as u32,
        );
        assert_eq!(design.signal_width(id), Some(8));

        design.signals[id.0 as usize].width = 9;
        assert_eq!(
            design.signal_width(id),
            Some(8),
            "the persisted source layout, not duplicated Signal metadata, owns backend width"
        );
        let issues = design.validate();
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("disagrees with its source layout width 8")),
            "{issues:#?}"
        );
    }

    #[test]
    fn processes_carry_sensitivity_and_write_sets() {
        let d = lower_src(COUNTER);
        let sig =
            |path: &str| SignalId(d.signals.iter().position(|s| s.path == path).unwrap() as u32);
        let procs = d.processes();
        // A combinational process for `count = value` and one event process.
        let comb = procs
            .iter()
            .find(|p| matches!(&p.kind, ProcessKind::Comb { target, .. } if *target == sig("H.dut.count")))
            .unwrap();
        assert_eq!(comb.reads, vec![sig("H.dut.value")]);
        assert_eq!(comb.writes, vec![sig("H.dut.count")]);

        let event = procs
            .iter()
            .find(|p| matches!(p.kind, ProcessKind::Event { .. }))
            .unwrap();
        // Sensitive to clk (edge condition), rst and en (update guards),
        // value (increment). Writes value.
        for s in ["H.dut.clk", "H.dut.rst", "H.dut.en", "H.dut.value"] {
            assert!(event.reads.contains(&sig(s)), "event not sensitive to {s}");
        }
        assert_eq!(event.writes, vec![sig("H.dut.value")]);
    }

    #[test]
    fn composite_and_enum_signals_flatten_with_widths() {
        let d = lower_src(
            "module m;\n\
             enum S { A, B, C }\n\
             struct P { flag: Bit, val: unsigned[8] }\n\
             entity E { p: P in, a: Bit[3] in, s: S out }\n\
             impl E {}\n\
             #[top] entity H {}\n\
             impl H { let p: P; let a: Bit[3]; let s: S; let dut: E = { .p = p, .a = a, .s = s }; }\n",
        );
        let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
        assert_eq!(width("H.dut.p.flag"), Some(1)); // struct field
        assert_eq!(width("H.dut.p.val"), Some(8));
        assert_eq!(width("H.dut.a[0]"), Some(1)); // array element
        assert_eq!(width("H.dut.a[2]"), Some(1));
        assert_eq!(width("H.dut.s"), Some(2)); // enum repr width
    }

    #[test]
    fn partial_bit_slice_write() {
        // `y = 0; y[3..0] = a` merges: low nibble = a, high bits held from 0.
        let d = lower_src(
            "module m;\n\
             entity E { a: unsigned[4] in, y: unsigned[8] out }\n\
             impl E { y = 0; y[3..0] = a; }\n\
             #[top] entity H {}\n\
             impl H { let a: unsigned[4]; let y: unsigned[8]; let dut: E = { .a = a, .y = y }; }\n",
        );
        // The y driver should be a read-modify-write (an Or of a masked base
        // and a shifted value), not a bare assignment.
        let dr = d
            .drivers
            .iter()
            .find(|dr| d.signals[dr.target.0 as usize].path == "H.dut.y")
            .unwrap();
        assert!(
            matches!(dr.expr, Expr::Binary { op: BinOp::Or, .. }),
            "slice write merges: {:?}",
            dr.expr
        );
    }

    /// A derived enum width has to hold every *value*, not one code per
    /// variant. `Hi = 9` in a two-variant enum needs four bits; counting
    /// variants alone gave one, silently truncating the value to 1. The old
    /// `: unsigned[4]` repr annotation used to paper over this.
    #[test]
    fn enum_width_covers_explicit_discriminants() {
        let d = lower_src(
            "module m;\n\
             enum Code { Lo = 1, Hi = 9 }\n\
             entity E { c: Code out }\n\
             impl E { c = Code::Hi; }\n\
             #[top] entity H {}\n\
             impl H { let c: Code; let dut: E = { .c = c }; }\n",
        );
        let sig = d.signals.iter().find(|s| s.path == "H.dut.c").unwrap();
        assert_eq!(sig.width, 4, "width must hold the largest discriminant");
    }

    #[test]
    fn newtype_enum_takes_its_base_width() {
        // A derived enum is a newtype (§3.28) — same variants, so the same
        // width. Four variants in the base, two bits in the derived type.
        let d = lower_src(
            "module m;\n\
             enum Base { A, B, C, D }\n\
             enum Ext(Base);\n\
             entity E { x: Ext out }\n\
             impl E { x = Ext::A; }\n\
             #[top] entity H {}\n\
             impl H { let x: Ext; let dut: E = { .x = x }; }\n",
        );
        let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
        assert_eq!(sig.width, 2, "the newtype carries the base's width");
    }

    /// `{ ..base, .z = 7 }` copied one value per *top-level* field, so a field
    /// holding a struct read as a scalar (Unknown) and every leaf under it was
    /// silently dropped — the spread carried over nothing nested.
    /// A match naming every variant needs no `_`, but the Select chain still
    /// bottomed out in `Unknown` — so the exhaustive spelling, the one the
    /// non-exhaustive lint asks for, produced a design no engine would run.
    #[test]
    fn exhaustive_match_expression_needs_no_wildcard() {
        let d = lower_src(
            "module m;\n\
             enum Base { A, B, C }\n\
             entity E { sel: Base in, y: unsigned[8] out }\n\
             impl E { y = match sel { Base::A => 1, Base::B => 2, Base::C => 3 }; }\n\
             #[top] entity H {}\n\
             impl H { let sel: Base; let y: unsigned[8]; let e: E = { .sel = sel, .y = y }; }",
        );
        fn has_unknown(e: &Expr) -> bool {
            match e {
                Expr::Unknown => true,
                Expr::Select { cond, then, els } => {
                    has_unknown(cond) || has_unknown(then) || has_unknown(els)
                }
                Expr::Binary { lhs, rhs, .. } => has_unknown(lhs) || has_unknown(rhs),
                Expr::Unary { rhs, .. } => has_unknown(rhs),
                _ => false,
            }
        }
        let dr = d
            .drivers
            .iter()
            .find(|dr| d.signals[dr.target.0 as usize].path.ends_with(".y"))
            .expect("driver for y");
        assert!(!has_unknown(&dr.expr), "no Unknown left: {:?}", dr.expr);
    }

    /// `Counter<W = 8>` bound its parameter but `Counter<8>` silently did not:
    /// only the named form was read, so the positional one left the parameter
    /// unbound and every port kept width 0 — surfacing far downstream as
    /// "signal has unknown width (0)" with nothing pointing at the instance.
    /// A parameter carried its argument's *family* into the body but not its
    /// width, so a nested inline — `signed`'s Ord inside `abs` — read
    /// `self'length` as 1 and tested the sign bit with `>> 0`. `abs(-5)`
    /// returned 251.
    /// A call result is a value of its declared return type, so it wraps to
    /// that width. Left unmasked, `neg(x)` on a `signed[8]`-returning function
    /// carried a full-width `0 - x`, and a nested inline then tested the wrong
    /// bit for the sign. Assigning to a signal masked it anyway, which hid it.
    ///
    /// The operator dispatch that goes with this needs std's `<=>` impl, which
    /// this harness's minimal prelude does not have; `fn_return_type_test` in
    /// the corpus covers that end.
    #[test]
    fn a_call_result_carries_its_return_type() {
        let d = lower_src(
            "module m;\n\
             fn neg(v: signed[8]) -> signed[8] { return 0 - v; }\n\
             entity E { s: signed[8] in, lt: unsigned[8] out }\n\
             impl E { lt = if neg(s) < 0 { 1 } else { 0 }; }\n\
             #[top] entity H {}\n\
             impl H { let s: signed[8]; let lt: unsigned[8]; \
             let e: E = { .s = s, .lt = lt }; }",
        );
        let text = d.to_ir_string();
        assert!(
            text.contains("and 255"),
            "masked to the return width:\n{text}"
        );
    }

    #[test]
    fn an_inlined_parameter_keeps_its_argument_width() {
        let d = lower_src(
            "module m;\n\
             fn width_of(v: integer) -> integer { return v'length; }\n\
             entity E { s: signed[8] in, y: unsigned[8] out }\n\
             impl E { y = width_of(s); }\n\
             #[top] entity H {}\n\
             impl H { let s: signed[8]; let y: unsigned[8]; let e: E = { .s = s, .y = y }; }",
        );
        let dr = d
            .drivers
            .iter()
            .find(|dr| d.signals[dr.target.0 as usize].path.ends_with(".y"))
            .expect("driver for y");
        assert!(
            matches!(dr.expr, Expr::Const(8)),
            "the parameter should report the argument's width, got {:?}",
            dr.expr
        );
    }

    #[test]
    fn positional_generic_argument_binds_a_value_parameter() {
        let src = |arg: &str| {
            format!(
                "module m;\n\
                 entity Inc<W: integer> {{ a: unsigned[W] in, y: unsigned[W] out, }}\n\
                 impl<W: integer> Inc<W> {{ y = a + 1; }}\n\
                 #[top] entity H {{}}\n\
                 impl H {{ let a: unsigned[4]; let y: unsigned[4]; \
                 let i: Inc<{arg}> = {{ .a = a, .y = y }}; }}"
            )
        };
        for arg in ["W = 4", "4"] {
            let d = lower_src(&src(arg));
            let w = d
                .signals
                .iter()
                .find(|s| s.path.ends_with("i.a"))
                .map(|s| s.width);
            assert_eq!(w, Some(4), "`Inc<{arg}>` should bind W");
        }
    }

    #[test]
    fn spread_copies_nested_leaves() {
        let d = lower_src(
            "module m;\n\
             struct A { x: Bit, y: unsigned[4] }\n\
             struct B { a: A, z: unsigned[4] }\n\
             entity E { oy: unsigned[4] out }\n\
             impl E {\n\
               let base: B;\n\
               base = B { .a = A { .x = '1', .y = 9 }, .z = 2 };\n\
               let upd: B;\n\
               upd = B { ..base, .z = 7 };\n\
               oy = upd.a.y;\n\
             }\n\
             #[top] entity H {}\n\
             impl H { let oy: unsigned[4]; let e: E = { .oy = oy }; }",
        );
        let driven = |suffix: &str| {
            d.signals
                .iter()
                .position(|s| s.path.ends_with(suffix))
                .and_then(|i| d.drivers.iter().find(|dr| dr.target.0 as usize == i))
                .is_some()
        };
        assert!(driven(".upd.a.y"), "the spread must carry nested leaves");
        assert!(driven(".upd.a.x"), "every leaf, not just the read one");
        assert!(driven(".upd.z"), "and the explicit override");
    }

    #[test]
    fn composed_struct_flattens_nested_fields() {
        // Composition replaced extension (§3.28): a struct field holding
        // another struct flattens to dotted leaf signals.
        let d = lower_src(
            "module m;\n\
             struct Header { valid: Bit, kind: unsigned[4] }\n\
             struct Packet { header: Header, data: unsigned[8] }\n\
             entity E { p: Packet out }\n\
             impl E {}\n\
             #[top] entity H {}\n\
             impl H { let p: Packet; let dut: E = { .p = p }; }\n",
        );
        let width = |path: &str| d.signals.iter().find(|x| x.path == path).map(|x| x.width);
        assert_eq!(width("H.dut.p.header.valid"), Some(1), "nested field");
        assert_eq!(width("H.dut.p.header.kind"), Some(4), "nested field");
        assert_eq!(width("H.dut.p.data"), Some(8), "own field");
    }

    #[test]
    fn same_variant_enum_derivation_is_representation_identical() {
        // A bodyless derivation keeps the base's width and discriminants.
        let d = lower_src(
            "module m;\n\
             enum Base { A, B, C }\n\
             enum Alias(Base);\n\
             entity E { x: Alias out }\n\
             impl E { x = Alias::B; }\n\
             #[top] entity H {}\n\
             impl H { let x: Alias; let dut: E = { .x = x }; }\n",
        );
        let sig = d.signals.iter().find(|s| s.path == "H.dut.x").unwrap();
        assert_eq!(sig.width, 2, "3 variants -> 2 bits, same as base");
    }

    #[test]
    fn bit_string_decodes_nine_value() {
        // A plain 2-value string is unchanged; a metavalue digit decodes to its
        // `std_ulogic` value bit (X = disc 3, low bit 1) instead of collapsing
        // to 0. `"1X10"` -> value bits 1110 = 14.
        let d = lower_src(
            "module m; entity E { y: unsigned[4] out, z: unsigned[4] out, }\n\
             impl E { y = \"1010\"; z = \"1X10\"; }\n\
             #[top] entity T {}\n\
             impl T { let y: unsigned[4]; let z: unsigned[4]; let dut: E = { .y = y, .z = z }; }",
        );
        let s = d.to_ir_string();
        assert!(s.contains("driver T.dut.y = 10"), "2-value unchanged:\n{s}");
        assert!(
            s.contains("driver T.dut.z = 14"),
            "metavalue digit decodes:\n{s}"
        );
    }

    #[test]
    fn bit_string_initializer_sets_init() {
        // `let v: unsigned[4] = "1010"` seeds the signal init to 10 (was 0 — no
        // string-init arm in const_init_value).
        let d = lower_src(
            "module m; entity E { y: unsigned[4] out, }\n\
             impl E { let v: unsigned[4] = \"1010\"; y = v; }\n\
             #[top] entity T {}\n\
             impl T { let y: unsigned[4]; let dut: E = { .y = y }; }",
        );
        let v = d
            .signals
            .iter()
            .find(|s| s.path.ends_with(".v"))
            .expect("no .v");
        assert_eq!(v.init, vec![10], "b\"1010\" -> init 10");
    }

    #[test]
    fn metavalue_bit_string_creates_companion() {
        // A metavalue init spawns a `$meta` companion recording the X element;
        // a plain 2-value init does not.
        let d = lower_src(
            "module m; entity E { y: unsigned[4] out, z: unsigned[4] out, }\n\
             impl E { let v: unsigned[4] = \"1X10\"; let w: unsigned[4] = \"1010\"; y = v; z = w; }\n\
             #[top] entity T {}\n\
             impl T { let y: unsigned[4]; let z: unsigned[4]; let dut: E = { .y = y, .z = z }; }",
        );
        let v = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(".v"))
            .expect("v") as u32;
        let w = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(".w"))
            .expect("w") as u32;
        let cid = *d.meta_of.get(&v).expect("v has a metavalue companion");
        // "1X10": per-element discs, nibble i = element i. pos3=1, pos2=X(3),
        // pos1=1, pos0=0 -> 0x1310. Companion is 4 bits/element wide.
        assert_eq!(
            d.signals[cid as usize].init,
            vec![0x1310],
            "full per-element discs"
        );
        assert_eq!(d.signals[cid as usize].width, 16, "4 bits x 4 elements");
        assert!(d.signals[cid as usize].path.ends_with(".v$meta"));
        assert!(!d.meta_of.contains_key(&w), "clean init gets no companion");
    }

    #[test]
    fn resolved_metavalue_companion_is_terminal() {
        // Element-wise resolution builds the discriminant plane from both the
        // value and metavalue planes.  That expression must not make the
        // propagation fixed point infer a companion for the companion itself.
        let d = lower_src(
            "module m;\n\
             trait Resolve { fn resolve(self, rhs: Self) -> Self; }\n\
             impl<T: Resolve> Resolve for T[] {\n\
                 fn resolve(self, rhs: T[]) -> T[] { return self; }\n\
             }\n\
             impl Resolve for Logic {\n\
                 fn resolve(self, rhs: Logic) -> Logic {\n\
                     if self == 'Z' { return rhs; }\n\
                     return self;\n\
                 }\n\
             }\n\
             entity Driver { y: unsigned[2] out }\n\
             impl Driver { y = \"X0\"; }\n\
             impl Driver { y = \"01\"; }\n\
             #[top] entity T {}\n\
             impl T { let y: unsigned[2]; let d: Driver = { .y = y }; }",
        );

        assert!(
            d.signals
                .iter()
                .any(|signal| signal.path.ends_with("$meta")),
            "the resolved vector still needs one discriminant plane"
        );
        assert!(
            d.signals
                .iter()
                .all(|signal| !signal.path.contains("$meta$meta")),
            "a discriminant plane must never acquire its own companion"
        );
        assert!(
            d.meta_of
                .values()
                .all(|companion| !d.meta_of.contains_key(companion)),
            "the companion relation must have depth one"
        );
    }

    #[test]
    fn wide_metavalue_initializers_have_no_element_limit() {
        let d = lower_src(
            "module m;\n\
             #[top] entity T {}\n\
             impl T { let v: unsigned[17] = \"X0000000000000000\"; }\n",
        );
        let v = d
            .signals
            .iter()
            .position(|s| s.path.ends_with(".v"))
            .expect("v signal");
        let cid = *d.meta_of.get(&(v as u32)).expect("wide companion");
        assert_eq!(d.signals[cid as usize].width, 68);
        assert_eq!(
            d.signals[cid as usize].init,
            vec![0, 3],
            "the top element's discriminant crosses the first ABI word"
        );

        let driven = lower_src(
            "module m;\n\
             #[top] entity E { v: unsigned[17] out, }\n\
             impl E { v = \"X0000000000000000\"; }\n",
        );
        let cid = *driven.meta_of.values().next().expect("driven companion");
        assert!(driven.drivers.iter().any(|driver| {
            driver.target == SignalId(cid)
                && matches!(&driver.expr, Expr::WideConst(words) if words == &[0, 3])
        }));
    }

    #[test]
    fn clean_combinational_override_clears_metavalue_companion_in_order() {
        let design = lower_src(
            "module m;\n\
             #[top] entity E { clear: Bit in, y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[4] = \"X000\";\n\
                 y = dirty;\n\
                 if clear { y = \"0000\"; }\n\
             }\n",
        );
        let y = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".y"))
            .expect("y") as u32;
        let companion = *design
            .meta_of
            .get(&y)
            .unwrap_or_else(|| panic!("y companion missing:\n{}", design.to_ir_string()));
        let value_drivers: Vec<_> = design
            .drivers
            .iter()
            .filter(|driver| driver.target == SignalId(y))
            .collect();
        let meta_drivers: Vec<_> = design
            .drivers
            .iter()
            .filter(|driver| driver.target == SignalId(companion))
            .collect();
        assert_eq!(value_drivers.len(), 2);
        assert_eq!(meta_drivers.len(), 2, "one companion write per value write");
        assert!(meta_drivers[0].cond.is_none());
        assert!(meta_drivers[1].cond.is_some());
        assert!(matches!(meta_drivers[1].expr, Expr::Const(0)));
    }

    #[test]
    fn clean_clocked_override_clears_metavalue_companion_in_order() {
        let design = lower_src(
            "module m;\n\
             #[top] entity E { clk: Bit in, clear: Bit in, y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[4] = \"X000\";\n\
                 if clk.rising() {\n\
                     y = dirty;\n\
                     if clear { y = \"0000\"; }\n\
                 }\n\
             }\n",
        );
        let y = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".y"))
            .expect("y") as u32;
        let companion = *design.meta_of.get(&y).expect("y companion");
        let block = design.event_blocks.first().expect("clocked block");
        let value_updates: Vec<_> = block
            .updates
            .iter()
            .filter(|update| update.target == SignalId(y))
            .collect();
        let meta_updates: Vec<_> = block
            .updates
            .iter()
            .filter(|update| update.target == SignalId(companion))
            .collect();
        assert_eq!(value_updates.len(), 2);
        assert_eq!(
            meta_updates.len(),
            2,
            "one companion update per value update"
        );
        assert!(meta_updates[0].cond.is_none());
        assert!(meta_updates[1].cond.is_some());
        assert!(matches!(meta_updates[1].expr, Expr::Const(0)));
    }

    #[test]
    fn old_vector_read_uses_old_metavalue_companion() {
        let design = lower_src(
            "module m;\n\
             #[top] entity E { y: unsigned[4] out, }\n\
             impl E {\n\
                 let v: unsigned[4] = \"X000\";\n\
                 y = v'old;\n\
             }\n",
        );
        let y = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".y"))
            .expect("y") as u32;
        let companion = *design.meta_of.get(&y).expect("y companion");
        let meta_driver = design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(companion))
            .expect("companion driver");
        assert!(matches!(meta_driver.expr, Expr::Old(_)));
    }

    #[test]
    fn narrowed_arithmetic_scans_full_operand_for_metavalues() {
        let design = lower_src(
            "module m;\n\
             #[top] entity E { y: unsigned[4] out, }\n\
             impl E {\n\
                 let dirty: unsigned[8] = \"X0000000\";\n\
                 let zero: unsigned[8] = 0;\n\
                 y = (dirty + zero)[3..0];\n\
             }\n",
        );
        let y = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".y"))
            .expect("y") as u32;
        let companion = *design
            .meta_of
            .get(&y)
            .unwrap_or_else(|| panic!("y companion missing:\n{}", design.to_ir_string()));
        let rendered = render(
            &design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(companion))
                .expect("companion driver")
                .expr,
            &design,
        );
        assert!(
            rendered.contains("[31..28]"),
            "the poison predicate must inspect element 7 of the 8-element operand: {rendered}"
        );
    }

    /// Constant evaluation is best-effort and must never let host integer
    /// overflow abort the compiler. The exact expression remains available to
    /// arbitrary-width lowering even when it does not fit the narrow
    /// elaboration evaluator.
    #[test]
    fn overflowing_narrow_constant_evaluation_does_not_panic() {
        let design = lower_src(
            "module m;\n\
             const FAR: integer = 1 << 64;\n\
             #[top] entity E { y: unsigned[128] out, }\n\
             impl E { y = FAR; }\n",
        );
        assert_eq!(design.drivers.len(), 1);
    }

    #[test]
    fn kernel_integer_operations_retain_signed_semantics() {
        let design = lower_src(
            "module m;\n\
             #[top] entity E {\n\
                 a: integer<-16..15> in,\n\
                 b: integer<-16..15> in,\n\
                 lt: Bit out,\n\
                 q: integer<-16..15> out,\n\
                 shr: integer<-16..15> out,\n\
             }\n\
             impl E {\n\
                 lt = if a < b { '1' } else { '0' };\n\
                 q = a / b;\n\
                 shr = a >> 1;\n\
             }\n",
        );
        let driver = |suffix: &str| {
            let target = design
                .signals
                .iter()
                .position(|signal| signal.path.ends_with(suffix))
                .expect("target signal");
            &design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(target as u32))
                .expect("target driver")
                .expr
        };
        assert!(matches!(
            driver(".lt"),
            Expr::Select { cond, .. }
                if matches!(cond.as_ref(), Expr::Binary { op: BinOp::SLt, .. })
        ));
        assert!(matches!(
            driver(".q"),
            Expr::Binary {
                op: BinOp::SDiv,
                ..
            }
        ));
        assert!(matches!(
            driver(".shr"),
            Expr::Binary {
                op: BinOp::AShr,
                ..
            }
        ));
    }

    #[test]
    fn real_to_integer_conversion_is_signed_in_direct_comparisons() {
        let design = lower_src(
            "module m;\n\
             #[top] entity E {\n\
                 r: real in,\n\
                 lt: Bit out,\n\
             }\n\
             impl E {\n\
                 lt = if integer(r) < 0 { '1' } else { '0' };\n\
             }\n",
        );
        let lt = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".lt"))
            .expect("lt signal");
        let expr = &design
            .drivers
            .iter()
            .find(|driver| driver.target == SignalId(lt as u32))
            .expect("lt driver")
            .expr;
        assert!(matches!(
            expr,
            Expr::Select { cond, .. }
                if matches!(
                    cond.as_ref(),
                    Expr::Binary {
                        op: BinOp::SLt,
                        lhs,
                        ..
                    } if matches!(lhs.as_ref(), Expr::Unary { op: UnOp::RealToInt, .. })
                )
        ));
    }

    #[test]
    fn ranged_integer_assignment_can_change_storage_width() {
        let src = "module m;\n\
             #[top] entity E {\n\
                 narrow: integer<-16..15> in,\n\
                 wide: integer<-128..127> out,\n\
             }\n\
             impl E { wide = narrow; }\n";
        let diagnostics = lower_diags(src);
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("width mismatch")),
            "{diagnostics:?}"
        );
        let design = lower_src(src);
        let wide = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".wide"))
            .expect("wide integer signal");
        assert!(matches!(
            design
                .drivers
                .iter()
                .find(|driver| driver.target == SignalId(wide as u32))
                .map(|driver| &driver.expr),
            Some(Expr::Current(_))
        ));
    }

    #[test]
    fn chained_aliases_retain_terminal_signal_representation() {
        let design = lower_src(
            "module m;\n\
             using Small = integer<-16..15>;\n\
             using Alias = Small;\n\
             using Chars = Char[];\n\
             using Text = Chars;\n\
             #[top] entity E { x: Alias in, y: Alias out, }\n\
             impl E { let text: Text[3] = \"abc\"; y = x; }\n",
        );
        for suffix in [".x", ".y"] {
            let signal = design
                .signals
                .iter()
                .find(|signal| signal.path.ends_with(suffix))
                .expect("aliased signal");
            assert_eq!(signal.width, 5);
            assert!(signal.integer);
            assert_eq!(signal.range, Some((-16, 15)));
        }
        let text: Vec<_> = design
            .signals
            .iter()
            .filter(|signal| signal.path.contains(".text["))
            .collect();
        assert_eq!(text.len(), 3);
        assert!(text.iter().all(|signal| signal.char));
    }

    #[test]
    fn foreign_integer_calls_retain_signed_abi_types() {
        let design = lower_src(
            "module m;\n\
             using CWord = integer;\n\
             using CInteger = CWord;\n\
             extern \"C\" { pub fn labs(v: CInteger) -> CInteger; }\n\
             #[top] entity E {\n\
                 x: integer<-128..127> in,\n\
                 y: integer<-128..127> out,\n\
             }\n\
             impl E { y = labs(x); }\n",
        );
        assert!(matches!(
            design.drivers.first().map(|driver| &driver.expr),
            Some(Expr::CCall {
                integer_args,
                integer_ret: true,
                ..
            }) if integer_args == &[true]
        ));
    }

    #[test]
    fn a_hardware_block_local_does_not_leak_out_of_its_block() {
        let diagnostics = lower_diags(
            "module m;\n\
             #[top] entity E { select: Bit in, y: unsigned[8] out }\n\
             impl E {\n\
                 if select == '1' { let temporary: unsigned[8] = 3; y = temporary; }\n\
                 y = temporary;\n\
             }\n",
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("E-P001")
                && diagnostic.contains("no value named `temporary`")));
        assert!(diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains("not lowered to hardware")));
    }

    #[test]
    fn hardware_block_locals_do_not_allocate_signals_or_leave_unknown_ir() {
        let design = lower_src(
            "module m;\n\
             #[top] entity E { select: Bit in, a: unsigned[8] in, y: unsigned[8] out }\n\
             impl E {\n\
                 if select == '1' {\n\
                     let temporary: unsigned[8] = a;\n\
                     temporary = temporary + 1;\n\
                     y = temporary;\n\
                 } else { y = 0; }\n\
             }\n",
        );
        assert!(design
            .signals
            .iter()
            .all(|signal| !signal.path.ends_with(".temporary")));
        assert!(!design.to_ir_string().contains("Unknown"));
    }

    #[test]
    fn nested_runtime_access_on_a_block_local_stays_storage_free() {
        let source = "module m;\n\
             #[top] entity E {\n\
                 enable: Bit in, row: integer in, col: integer in,\n\
                 a: unsigned[8] in, y: unsigned[8] out\n\
             }\n\
             impl E {\n\
                 if enable == '1' {\n\
                     let matrix: unsigned[8][2][2] = [[1, 2], [3, 4]];\n\
                     matrix[row][col] = a;\n\
                     y = matrix[row][col];\n\
                 } else { y = 0; }\n\
             }\n";
        let diagnostics = lower_diags(source);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.contains("E-P017")),
            "{diagnostics:#?}"
        );
        let design = lower_src(source);
        assert!(design
            .signals
            .iter()
            .all(|signal| !signal.path.contains("matrix")));
        assert!(!design.to_ir_string().contains("Unknown"));
        assert!(design.validate().is_empty(), "{:#?}", design.validate());
    }

    #[test]
    fn runtime_packed_index_on_a_block_local_stays_storage_free() {
        let source = "module m; using std::bits::unsigned; using std::logic::{Bit, Logic};
             #[top] entity E {
               enable: Bit in, index: unsigned[5] in, y: Logic out
             }
             impl E {
               if enable == '1' {
                 let word: unsigned[15..8] = 0;
                 word[index] = '1';
                 y = word[index];
               } else { y = '0'; }
             }";
        let diagnostics = lower_diags(source);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.contains("E-P017")),
            "{diagnostics:#?}"
        );
        let design = lower_src(source);
        assert!(design
            .signals
            .iter()
            .all(|signal| !signal.path.ends_with(".word")));
        assert!(!design.to_ir_string().contains("Unknown"));
        assert!(design.validate().is_empty(), "{:#?}", design.validate());
    }

    #[test]
    fn nested_generic_type_arguments_preserve_recursive_layout() {
        let design = lower_src(
            "module m;\n\
             struct Box<T> { value: T }\n\
             struct Pair<T, U> { left: T, right: U }\n\
             entity Pass<T> { input: T in, output: T out }\n\
             impl<T> Pass<T> { output = input; }\n\
             #[top] entity E {\n\
                 nested: Box<Box<unsigned[8]>> in,\n\
                 named: Pair<U = Box<unsigned[16]>, T = Box<Box<unsigned[8]>>> in,\n\
                 y: unsigned[8] out,\n\
             }\n\
             impl E {\n\
                 let passed: Box<Box<unsigned[8]>>;\n\
                 let pass: Pass<Box<Box<unsigned[8]>>> = {\n\
                     .input = nested, .output = passed,\n\
                 };\n\
                 y = passed.value.value;\n\
             }\n",
        );
        let leaf = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(".nested.value.value"))
            .expect("the nested generic field should flatten to one leaf");
        assert_eq!(leaf.width, 8);
        let named_left = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(".named.left.value.value"))
            .expect("the named T argument should bind independently of order");
        let named_right = design
            .signals
            .iter()
            .find(|signal| signal.path.ends_with(".named.right.value"))
            .expect("the named U argument should bind independently of order");
        assert_eq!(named_left.width, 8);
        assert_eq!(named_right.width, 16);
        assert!(design.signals.iter().any(|signal| {
            signal.path.contains(".pass.")
                && signal.path.ends_with(".input.value.value")
                && signal.width == 8
        }));
        assert!(!design.to_ir_string().contains("Unknown"));
    }

    #[test]
    fn design_persists_recursive_concrete_source_layouts() {
        let design = lower_src(
            "module m;\n\
             struct Header { flag: Bit, code: unsigned[7..0] }\n\
             struct Packet<T> { header: Header, payload: T }\n\
             #[top] entity E {\n\
                 packets: Packet<unsigned[16]>[3..1] in,\n\
                 count: integer<-3..4> in,\n\
             }\n\
             impl E {}\n",
        );

        let packets = design
            .source_layouts
            .get("E.packets")
            .expect("the aggregate root keeps a layout despite having no signal");
        assert_eq!(
            packets.index_range(),
            Some(LayoutRange { left: 3, right: 1 })
        );
        assert_eq!(packets.bit_width(), Some(75));
        assert_eq!(packets.leaf_count(), Some(9));
        let LayoutKind::Array { element, .. } = &packets.kind else {
            panic!("packets should remain a source array: {packets:#?}");
        };
        let LayoutKind::Struct { name, fields, .. } = &element.kind else {
            panic!("the array element should retain Packet: {element:#?}");
        };
        assert_eq!(name, "Packet");
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["header", "payload"]
        );
        assert!(matches!(
            fields[1].layout.kind,
            LayoutKind::Packed {
                width: 16,
                range: Some(LayoutRange { left: 0, right: 15 }),
                ..
            }
        ));

        let code = design
            .source_layouts
            .get("E.packets[3].header.code")
            .expect("every flattened leaf also has its own concrete layout");
        assert!(matches!(
            code.kind,
            LayoutKind::Packed {
                width: 8,
                range: Some(LayoutRange { left: 7, right: 0 }),
                ..
            }
        ));

        let count = design
            .source_layouts
            .get("E.count")
            .expect("ranged integer");
        assert!(matches!(
            count.kind,
            LayoutKind::Scalar {
                width: 4,
                domain: ScalarDomain::Integer,
                value_range: Some((-3, 4)),
                ..
            }
        ));
    }

    #[test]
    fn testbench_locals_persist_layouts_without_becoming_hardware_signals() {
        let design = lower_src(
            "module m;\n\
             struct Pair<T> { left: T, right: T }\n\
             #[test] entity T {}\n\
             impl T {\n\
                 let pairs: Pair<unsigned[8]>[2..1] = [\n\
                     { .left = 1, .right = 2 },\n\
                     { .left = 3, .right = 4 },\n\
                 ];\n\
             }\n",
        );

        let root = design
            .source_layouts
            .get("T.pairs")
            .expect("testbench local should retain its concrete layout");
        assert_eq!(root.bit_width(), Some(32));
        assert_eq!(root.leaf_count(), Some(4));
        assert!(matches!(
            root.kind,
            LayoutKind::Array {
                range: Some(LayoutRange { left: 2, right: 1 }),
                ..
            }
        ));
        assert!(matches!(
            design
                .source_layouts
                .get("T.pairs[2].left")
                .map(|layout| &layout.kind),
            Some(LayoutKind::Packed { width: 8, .. })
        ));
        assert!(design
            .signals
            .iter()
            .all(|signal| !signal.path.contains("pairs")));
    }

    #[test]
    fn rising_lowers_to_event_old_current() {
        let d = lower_src(COUNTER);
        let rendered = d.to_ir_string();
        // clk.rising() expands into the explicit Event/Old/Current form. The
        // logic literals are resolved to their std positions ('0' -> 0,
        // '1' -> 1), so the IR carries plain constants, no raw chars.
        assert!(rendered.contains("Event(H.dut.clk)"));
        assert!(rendered.contains("Old(H.dut.clk) == 0"));
        assert!(rendered.contains("H.dut.clk == 1"));
        // The combinational driver and the next-state updates are present.
        assert!(rendered.contains("driver H.dut.count = H.dut.value"));
        assert!(rendered.contains("next H.dut.value = 0"));
    }

    #[test]
    fn priority_conditions_accumulate() {
        let d = lower_src(COUNTER);
        let u = &d.event_blocks[0].updates;
        // First update guarded by rst == '1'.
        assert!(matches!(
            &u[0].cond,
            Some(Expr::Binary { op: BinOp::Eq, .. })
        ));
        // Second guarded by the negation AND en.
        assert!(matches!(
            &u[1].cond,
            Some(Expr::Binary { op: BinOp::And, .. })
        ));
    }
}

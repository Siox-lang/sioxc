//! Language-neutral digital expression representation.

use super::SignalId;

/// The std type a bare, otherwise-untyped logic literal defaults to. Its
/// variants (`'0','1','Z','X','U','W','L','H','-'`) and their positions come
/// from `std/logic.siox` — the compiler names the type but holds no value
/// table of its own. A typed context (an enum signal/local or a comparison
/// counterpart) overrides this via `enum_variants`.
pub const DEFAULT_LOGIC_TYPE: &str = "ULogic";

/// Stable index into [`super::Design::lookup_tables`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LookupTableId(pub usize);

/// One constant expression lookup table.
///
/// Values remain logical integers rather than backend storage bytes. This
/// keeps the IR independent of an ABI and lets a backend select the smallest
/// convenient integer storage type for `element_width`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LookupTable {
    /// Bits per element. A backend may widen this to convenient storage.
    pub element_width: u32,
    /// The table contents, indexed from zero.
    pub values: Vec<u64>,
}

/// IR expression. `::event`/`::old` are first-class so the scheduler can read
/// them directly; `clk.rising()` lowers into `Event`/`Old`/`Current`.
#[derive(Clone, Debug)]
pub enum Expr {
    /// An integer constant that fits one ABI word.
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
        /// The comparison as lowered, used once the rule is resolved.
        inner: Box<Expr>,
    },
    /// A `real` constant; evaluates to its f64 bit pattern.
    Real(f64),
    /// A logic literal such as `'0'` or `'X'`, kept symbolic until the
    /// std-derived encoding gives it a discriminant.
    Logic(char),
    /// The signal's value in this delta cycle.
    Current(SignalId),
    /// The signal's value in the previous delta cycle, the `'old` attribute.
    Old(SignalId),
    /// Whether the signal changed since the previous delta cycle, the `'event`
    /// attribute. First-class so the scheduler reads it directly.
    Event(SignalId),
    /// A prefix operation.
    Unary {
        /// Which operation.
        op: UnOp,
        /// The operand.
        rhs: Box<Expr>,
    },
    /// An infix operation.
    Binary {
        /// Which operation, including its signed and float variants.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// Bit slice `base[hi..lo]` (inclusive), value `(base >> lo) & mask(hi-lo+1)`.
    Slice {
        /// The value being sliced.
        base: Box<Expr>,
        /// Inclusive high bit index.
        hi: u32,
        /// Inclusive low bit index.
        lo: u32,
    },
    /// Constant table lookup. An index outside `values` evaluates to zero,
    /// matching the overshift semantics of the packed expression this
    /// replaces. Tables are std-derived data owned by the finished design.
    TableLookup {
        /// Which table in [`super::Design::lookup_tables`] to read.
        table: LookupTableId,
        /// The element to read; out-of-range yields zero.
        index: Box<Expr>,
    },
    /// A runtime index together with its declared-domain predicate. The value
    /// remains an ordinary expression; simulation backends use `valid` to
    /// latch a bounds failure only on the control-flow path that evaluates the
    /// access. `left`/`right` preserve the declaration's written direction for
    /// the diagnostic rather than reducing it to an anonymous min/max pair.
    CheckedIndex {
        /// The index value itself.
        index: Box<Expr>,
        /// Predicate that is true when `index` is inside the declared domain.
        valid: Box<Expr>,
        /// The declaration's written left bound.
        left: i64,
        /// The declaration's written right bound.
        right: i64,
        /// The access site, for the runtime failure report.
        span: crate::diag::Span,
    },
    /// `cond ? then : els` — produced by inlining operator-trait impl bodies
    /// (`if`/`else` chains of `return`s become nested selects).
    Select {
        /// The tested condition.
        cond: Box<Expr>,
        /// Value when `cond` holds.
        then: Box<Expr>,
        /// Value otherwise.
        els: Box<Expr>,
    },
    /// A foreign C call (`extern "C"` declarations, spec 3.27): `real`
    /// parameters/results are f64 (bit-pattern operands), everything else a
    /// 64-bit word. Native linking resolves the named symbol.
    CCall {
        /// The C symbol to call; native linking resolves it.
        name: String,
        /// The call arguments, in order.
        args: Vec<Expr>,
        /// Per argument, whether it is passed as an f64 rather than a word.
        f64_args: Vec<bool>,
        /// Per argument, whether it is a signed kernel `integer`.
        integer_args: Vec<bool>,
        /// Whether the result is an f64.
        f64_ret: bool,
        /// Whether the result is a signed kernel `integer`.
        integer_ret: bool,
    },
    /// A reference that could not be lowered (unknown signal, unsupported form).
    Unknown,
}

/// A prefix operation in the digital IR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// Bitwise complement. A vector `not` lowers to [`BinOp::Xor`] against
    /// all-ones instead of reaching this.
    Not,
    /// Arithmetic negation.
    Neg,
    /// `integer(x)` on a `real`: the f64 *value* truncated toward zero, not
    /// its bit pattern. Every other conversion is a raw resize, and a real
    /// reaching that path reinterpreted its bits — `integer(3.5)` gave the low
    /// word of `0x400C000000000000`, i.e. 0.
    RealToInt,
}

/// An infix operation in the digital IR.
///
/// Unsigned, signed and float forms are separate variants rather than one
/// operation plus a type tag, so a backend never has to consult operand types
/// to know which machine instruction to emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// Wrapping unsigned addition.
    Add,
    /// Wrapping unsigned subtraction.
    Sub,
    /// Wrapping unsigned multiplication.
    Mul,
    /// Unsigned division.
    Div,
    /// Signed kernel-`integer` arithmetic. Add/subtract/multiply use the same
    /// bit operation as their unsigned counterparts, but retain signedness so
    /// a wider enclosing expression sign-extends their operands/results.
    SAdd,
    /// Signed counterpart of [`BinOp::Sub`].
    SSub,
    /// Signed counterpart of [`BinOp::Mul`].
    SMul,
    /// Signed division.
    SDiv,
    /// Bitwise conjunction.
    And,
    /// Bitwise disjunction.
    Or,
    /// Bitwise exclusive-or. Native like `And`/`Or` so the metavalue companion
    /// can apply the `std_logic_1164` table per element: std spells `xor` as
    /// `(a or b) - (a and b)`, which is right for two-valued arithmetic but
    /// makes the companion lowering see a subtraction and poison the whole
    /// vector. `nand`/`nor`/`xnor`/`not` all reduce to this.
    Xor,
    /// Left shift, shifting in zeroes.
    Shl,
    /// Logical right shift, shifting in zeroes.
    Shr,
    /// Arithmetic right shift for the signed kernel `integer`.
    AShr,
    /// Equality; yields 0 or 1.
    Eq,
    /// Inequality; yields 0 or 1.
    Ne,
    /// Unsigned less-than.
    Lt,
    /// Unsigned less-than-or-equal.
    Le,
    /// Unsigned greater-than.
    Gt,
    /// Unsigned greater-than-or-equal.
    Ge,
    /// Signed kernel-`integer` ordering comparisons.
    SLt,
    /// Signed less-than-or-equal.
    SLe,
    /// Signed greater-than.
    SGt,
    /// Signed greater-than-or-equal.
    SGe,
    /// Float arithmetic on f64-bit values (`real` operands).
    FAdd,
    /// Float subtraction.
    FSub,
    /// Float multiplication.
    FMul,
    /// Float division.
    FDiv,
    /// Float comparison on f64-bit values (`real` operands); the result is a
    /// `Bool` (0/1), computed with ordered IEEE-754 semantics — integer compare
    /// on the raw bits would misorder negatives and `±0.0`.
    FEq,
    /// Ordered float inequality.
    FNe,
    /// Ordered float less-than.
    FLt,
    /// Ordered float less-than-or-equal.
    FLe,
    /// Ordered float greater-than.
    FGt,
    /// Ordered float greater-than-or-equal.
    FGe,
}

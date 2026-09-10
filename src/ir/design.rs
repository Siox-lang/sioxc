//! Lowered design objects and flattened hardware storage.

use std::collections::HashMap;

use super::{Expr, LookupTable, ProcessIr, SourceLayout};

/// A lowered design, ready to simulate.
///
/// Hardware behavior appears in two deliberately separate forms: combinational
/// [`Driver`]s that settle to a fixed point within a delta cycle, and
/// [`EventBlock`]s that compute next state from pre-commit values and commit
/// together. Preserving that split is what keeps `clk.rising()` meaning the
/// same thing in both backends.
///
/// A `Logic` vector additionally carries a discriminant companion plane, keyed
/// by [`Design::meta_of`], so `'X'` and `'Z'` survive operations that a single
/// value bit could not represent.
#[derive(Default)]
pub struct Design {
    /// Every scalar storage leaf, indexed by [`SignalId`].
    pub signals: Vec<Signal>,
    /// Combinational writes. These settle to a fixed point each delta cycle.
    pub drivers: Vec<Driver>,
    /// Event-controlled next-state writes, applied together after settling.
    pub event_blocks: Vec<EventBlock>,
    /// Canonical independently scheduled behavior. During migration test CFGs
    /// enter from typed AST and hardware CFGs are imported from normalized
    /// drivers/event blocks. The remaining inversion makes this product the
    /// lowering authority and derives those compatibility forms from it.
    pub process_ir: ProcessIr,
    /// Driver-context labels retained from `process name { ... }`, qualified
    /// by instance path for diagnostics and backend tracing.
    pub process_labels: HashMap<u32, String>,
    /// Labels from every source context folded into one resolved
    /// combinational target. Resolution replaces those context drivers with a
    /// normalized driver, so target ownership must survive independently of
    /// the replacement driver's synthetic context id.
    pub resolved_process_labels: HashMap<u32, Vec<String>>,
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
    /// Nominal array family -> its element enum (`unsigned` -> `Logic`). A bit
    /// of a packed scalar array is not a signal of its own, so an operator on one has no
    /// type to dispatch by unless the family says what its elements are.
    pub array_element_of_family: HashMap<String, String>,
    /// Type name -> its `impl New for T` uninitialized default value (`Logic` ->
    /// `'U'`), so testbench-local seeding matches the hardware signal default.
    pub new_defaults: HashMap<String, u64>,
    /// Enum type -> its std-owned packed logic interpretation. These tables are
    /// elaborated from `impl LogicEncoding` and the ordinary `Operator` impls;
    /// engines consume them without knowing any logic symbols or discriminant
    /// ordering.
    pub logic_encodings: HashMap<String, LogicEncoding>,
    /// Compact constant lookup tables referenced by [`Expr::TableLookup`].
    /// Lowering interns these after all std-defined logic operations have been
    /// expanded, so a table is stored once even when thousands of expressions
    /// use it. Backends may choose their native constant-storage representation.
    pub lookup_tables: Vec<LookupTable>,
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
    /// Signals metavalue lowering created to hold a shared operand. A
    /// per-element unroll reads such a signal as a leaf instead of deep-copying
    /// the operand once per element, which is what stopped nested metavalue
    /// expressions from growing as `width^depth`. They are an implementation
    /// plane like `meta_of`'s companions, so consumers that hide companions
    /// (waveforms) hide these too.
    pub metavalue_temps: std::collections::HashSet<u32>,
    /// Packed-array signal -> enum used by each element. This is declaration
    /// metadata (`struct F(E[])`), not a std type-name
    /// convention. Consumers use it to render metavalue companions.
    pub array_element_enums: HashMap<u32, String>,
    /// Concrete source value -> its complete recursive type layout. Keys use
    /// the same hierarchical spelling as `Signal::path`; aggregates have an
    /// entry even though storage is flattened into leaf signals. Backends and
    /// tooling consume this instead of reconstructing struct inheritance,
    /// generic substitutions, array ranges, or packed-vector shape from the
    /// frontend AST.
    pub source_layouts: HashMap<String, SourceLayout>,
}

/// Elaborated semantics for one multi-valued logic enum.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogicEncoding {
    /// Full discriminant -> packed value-plane bit.
    pub value_bits: HashMap<u64, bool>,
    /// Values whose exact identity is completely represented by the value bit.
    pub binary: std::collections::HashSet<u64>,
    /// Values that numeric operations normalize to the contract's canonical
    /// unknown rather than a definite low/high.
    pub unknown: std::collections::HashSet<u64>,
    /// Values rendered as high impedance by waveform backends.
    pub high_impedance: std::collections::HashSet<u64>,
    /// Full discriminant -> the std `to_x01` result.
    pub x01: HashMap<u64, u64>,
    /// Operator symbol -> `(left discriminant, right discriminant) -> result`.
    pub binary_ops: HashMap<String, HashMap<(u64, u64), u64>>,
    /// Operator symbol -> `operand discriminant -> result`.
    pub unary_ops: HashMap<String, HashMap<u64, u64>>,
}

impl LogicEncoding {
    /// The discriminant used when an operation must produce "unknown" — the
    /// lowest unknown in std's declaration order, normally `'X'`.
    pub fn canonical_unknown(&self) -> Option<u64> {
        self.unknown
            .iter()
            .filter_map(|disc| self.x01.get(disc).copied())
            .min()
    }

    /// The 0/1 value plane bit for a discriminant, or `None` if std's
    /// encoding gives it none.
    pub fn value_bit(&self, disc: u64) -> Option<u64> {
        self.value_bits.get(&disc).map(|bit| u64::from(*bit))
    }

    /// The unique ordinary binary value represented by `bit` in the packed
    /// value plane. Taking the minimum keeps malformed duplicate contracts
    /// deterministic; a valid contract has exactly one value for each bit.
    pub fn binary_value(&self, bit: bool) -> Option<u64> {
        self.binary
            .iter()
            .copied()
            .filter(|disc| self.value_bits.get(disc) == Some(&bit))
            .min()
    }

    /// The contract's high-impedance value, when the domain has one.
    pub fn high_impedance_value(&self) -> Option<u64> {
        self.high_impedance.iter().copied().min()
    }
}

/// Index of a [`Signal`] in [`Design::signals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalId(pub u32);

/// One scalar storage leaf. Aggregates are flattened, so a struct or array in
/// the source becomes several of these.
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
    /// The signal this driver writes.
    pub target: SignalId,
    /// Guard for a conditional write. `None` drives unconditionally.
    pub cond: Option<Expr>,
    /// The value driven onto `target`.
    pub expr: Expr,
    /// Explicit discriminant-plane expression retained while lowering a write
    /// whose value expression alone cannot describe its metavalues (notably a
    /// bit-string literal and a dynamic packed-element write). The metavalue
    /// propagation pass consumes this and emits the ordinary companion driver;
    /// finalized IR always has `None` here.
    pub meta: Option<Expr>,
    /// Driver context (spec 3.14): one per source process, bare concurrent
    /// statement, or port connection.
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
///
/// Keeping this separate from [`Driver`] is the central distinction in the IR.
/// Combinational writes settle to a fixed point within a delta cycle, while
/// these compute from pre-commit state and are applied together, so
/// simultaneous updates never observe each other.
///
/// ```mermaid
/// flowchart LR
///     src["clk.rising()"] --> ev["Event(clk)"]
///     src --> old["Old(clk) == '0'"]
///     src --> cur["Current(clk) == '1'"]
///     ev --> cond["EventBlock::condition"]
///     old --> cond
///     cur --> cond
///     cond --> upd["NextUpdate:<br/>next(target) = expr"]
/// ```
#[derive(Clone, Debug)]
pub struct EventBlock {
    /// When to fire. An edge lowers to `Event(clk) && Old(clk)=='0' &&
    /// Current(clk)=='1'` rather than to a dedicated edge node.
    pub condition: Expr,
    /// The next-state writes queued when `condition` holds.
    pub updates: Vec<NextUpdate>,
    /// The driver context that lowered this block — one per source process, the
    /// same identity `Driver::ctx` carries (spec 3.14: override within a
    /// context, resolution across). Several blocks share it when one impl
    /// writes from more than one event, or when a generate loop unrolls a
    /// single clocked statement, and a partial write in a later block merges
    /// over what the earlier ones in that context left behind.
    pub ctx: u32,
}

/// One queued next-state write inside an [`EventBlock`].
#[derive(Clone, Debug)]
pub struct NextUpdate {
    /// The signal to update when the block fires.
    pub target: SignalId,
    /// Guard for a conditional update within the firing block.
    pub cond: Option<Expr>,
    /// The next-state value, computed from pre-commit state.
    pub expr: Expr,
    /// Clocked counterpart of [`Driver::meta`], consumed by metavalue
    /// propagation before the IR reaches a simulator backend.
    pub meta: Option<Expr>,
    /// The assignment this update came from — see [`Driver::span`].
    pub span: Option<crate::diag::Span>,
}

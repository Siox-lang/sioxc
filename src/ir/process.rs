//! Canonical scheduled-process control flow, values, and test descriptors.

use std::collections::HashSet;

use crate::resolve::DefId;

use super::{BinOp, Expr, LayoutDirection, LookupTableId, SignalId, SourceLayout, UnOp};
/// Index of a [`ProcessCfg`] in [`ProcessIr::processes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessId(pub u32);

/// Index of a [`ProcessBlock`] within its owning [`ProcessCfg`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessBlockId(pub u32);

/// Index of a [`ProcessLocal`] within its owning [`ProcessCfg`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessLocalId(pub u32);

/// Index of persistent testbench-owned storage in [`ProcessIr::storages`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessStorageId(pub u32);

/// Index of a [`ProcessValue`] in [`ProcessIr`]'s operand arena. CFG nodes
/// carry these rather than embedding operands, so an operand is stored once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessValueId(pub u32);

/// The canonical control-flow product owned by an elaborated [`Design`].
///
/// The temporary `test_ir` adapter fills test CFGs from typed AST and hardware
/// CFGs from the normalized scheduler decomposition. The representation and
/// its invariants live here so no backend needs a second process product. The
/// remaining migration inversion makes this arena authoritative and derives
/// [`Driver`] / [`EventBlock`] compatibility forms from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessIr {
    /// Every process control-flow graph, indexed by [`ProcessId`].
    pub processes: Vec<ProcessCfg>,
    /// Discovered `#[test]` entities and the processes each one runs.
    pub tests: Vec<ProcessTest>,
    /// Persistent values owned by test entities. These are distinct from
    /// hardware signals and from lexical process locals.
    pub storages: Vec<ProcessStorage>,
    /// Arena-owned process operands. CFG nodes carry only stable IDs, so
    /// cloning a block or edge never clones frontend type/text payloads.
    pub values: Vec<ProcessValue>,
}

/// Persistent state declared in a test entity implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessStorage {
    /// This storage object's arena id.
    pub id: ProcessStorageId,
    /// Test root owning the value.
    pub owner: crate::elab::InstanceId,
    /// Source name within that root.
    pub name: String,
    /// Resolved declaration identity, when resolution succeeded.
    pub source: Option<DefId>,
    /// Declaration extent.
    pub span: crate::diag::Span,
    /// Checked value type, when one was inferred.
    pub ty: Option<crate::types::Ty>,
    /// Concrete recursive storage shape retained by digital lowering.
    pub layout: Option<SourceLayout>,
    /// Initial value evaluated before processes start.
    pub initializer: Option<ProcessValueId>,
    /// DUT signal leaves connected to this storage object.
    pub bindings: Vec<ProcessStorageBinding>,
}

/// One flattened DUT signal connected to persistent testbench storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessStorageBinding {
    /// Field/index suffix relative to [`ProcessStorage::name`], empty for a
    /// scalar or packed value.
    pub projection: String,
    /// Connected DUT signal leaf.
    pub signal: SignalId,
    /// Direction at the DUT port endpoint. `In` means storage drives the
    /// signal; `Out` means the signal is observed through storage.
    pub direction: LayoutDirection,
}

/// Runtime test registration metadata. The behavior remains ordinary process
/// CFGs referenced by `processes`; the `test` attribute does not create a
/// different executable representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessTest {
    /// The `#[test]` entity declaration.
    pub entity: DefId,
    /// The elaborated root instance this test runs.
    pub root: crate::elab::InstanceId,
    /// Module-qualified name, used to select and report the test.
    pub qualified_name: String,
    /// The entity declaration site, for selection diagnostics.
    pub span: crate::diag::Span,
    /// Stimulus, clocks, and nested DUT hardware processes the test runs.
    pub processes: Vec<ProcessId>,
}

/// One process as a control-flow graph.
///
/// ```mermaid
/// flowchart TD
///     entry["entry block"] --> i["instructions:<br/>Declare / Assign / Runtime"]
///     i --> t{"terminator"}
///     t -->|Goto| b2["another block"]
///     t -->|Branch| b3["then / else"]
///     t -->|Match| b4["one block per arm"]
///     t -->|For| b5["body, then exit"]
///     t -->|Suspend| b6["resume block,<br/>once await is ready"]
///     t -->|Return / Stop / Finish| done["process ends"]
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessCfg {
    /// This process's own id.
    pub id: ProcessId,
    /// Elaboration-tree root this process participates in. A test descriptor
    /// uses this to select both stimulus and nested DUT processes.
    pub root: crate::elab::InstanceId,
    /// The instance whose body declared it.
    pub owner: crate::elab::InstanceId,
    /// Optional instance-qualified source label.
    pub label: Option<String>,
    /// The `process` block's source extent.
    pub span: crate::diag::Span,
    /// When the process runs.
    pub activation: ProcessActivation,
    /// The block execution starts in.
    pub entry: ProcessBlockId,
    /// Locals owned by this process, indexed by [`ProcessLocalId`].
    pub locals: Vec<ProcessLocal>,
    /// The control-flow graph's blocks, indexed by [`ProcessBlockId`].
    pub blocks: Vec<ProcessBlock>,
}

/// When a process runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessActivation {
    /// Starts once at time zero and subsequently only through explicit resume
    /// edges such as [`ProcessTerminator::Suspend`].
    TimeZero,
    /// Runs when a member of the sensitivity set changes. Clock processes are
    /// represented this way rather than entering a separate lowering path.
    /// Reactive processes also receive their initial activation at time zero.
    Reactive {
        /// Storage objects or signals whose change wakes the process.
        sensitivity: Vec<ProcessSensitivity>,
    },
}

/// One value whose committed change activates a reactive process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcessSensitivity {
    /// A hardware signal.
    Signal(SignalId),
    /// Persistent testbench storage, including clock-generator state.
    Storage(ProcessStorageId),
}

/// A binding owned by one process. Locals are not signals: they are private
/// to the process and update immediately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessLocal {
    /// This local's own id within the owning process.
    pub id: ProcessLocalId,
    /// The name as written. Equal spellings in nested scopes stay distinct
    /// locals; see `source`.
    pub name: String,
    /// Resolved source declaration used only to preserve lexical identity
    /// while the temporary AST adapter classifies writes. Equal spellings in
    /// nested scopes remain different locals. Other frontends may leave it
    /// absent once they provide structured places directly.
    pub source: Option<DefId>,
    /// The declaration site.
    pub span: crate::diag::Span,
    /// Transitional frontend type retained until expression lowering produces
    /// only concrete IR value/layout IDs.
    pub ty: Option<crate::types::Ty>,
    /// The local's source layout, when one was derived.
    pub layout: Option<SourceLayout>,
}

/// One basic block: a straight-line instruction run ending in exactly one
/// terminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessBlock {
    /// This block's own id within the owning process.
    pub id: ProcessBlockId,
    /// Instructions executed in order.
    pub instructions: Vec<ProcessInstruction>,
    /// How control leaves the block.
    pub terminator: ProcessTerminator,
}

/// A non-branching action inside a [`ProcessBlock`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessInstruction {
    /// Bring a local into scope, optionally initializing it.
    Declare {
        /// The local being declared.
        local: ProcessLocalId,
        /// Its initial value, if written.
        initializer: Option<ProcessValueId>,
        /// The `let` statement's extent.
        span: crate::diag::Span,
    },
    /// Write a value to a place.
    Assign {
        /// Whether the write is immediate or staged to the next delta.
        semantics: ProcessAssignment,
        /// Compatibility driver identity when this write was imported from
        /// the normalized hardware scheduler. Writes from one source context
        /// override in order; writes from different contexts resolve. New
        /// source-first lowering may use the owning [`ProcessId`] directly
        /// and leave this migration field absent.
        driver_context: Option<u32>,
        /// The place written.
        target: ProcessValueId,
        /// The value written.
        value: ProcessValueId,
        /// The assignment's extent.
        span: crate::diag::Span,
    },
    /// Queue a signal write for a later simulation time. Delayed writes are a
    /// scheduler operation rather than a flavour of immediate assignment, so
    /// native backends never have to infer scheduling from an optional field.
    Schedule {
        /// Compatibility driver identity, with the same meaning as on
        /// [`ProcessInstruction::Assign`]. Testbench storage clocks have none.
        driver_context: Option<u32>,
        /// The signal place written when the delay expires.
        target: ProcessValueId,
        /// The value captured for the future write.
        value: ProcessValueId,
        /// Simulation delay from the current time.
        delay: ProcessValueId,
        /// The assignment's extent.
        span: crate::diag::Span,
    },
    /// Call into the simulation runtime.
    Runtime {
        /// Which runtime operation.
        operation: ProcessRuntimeOp,
        /// Its arguments.
        arguments: Vec<ProcessValueId>,
        /// The call site.
        span: crate::diag::Span,
    },
}

/// Whether a write lands immediately or is staged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessAssignment {
    /// Process-local variables update immediately and are visible to the next
    /// instruction in the same process step.
    ImmediateLocal,
    /// Persistent testbench storage updates immediately for following source
    /// statements. Writes to connected DUT inputs are staged for the process
    /// step's commit boundary.
    ImmediateStorage,
    /// Signals stage a driver write for end-of-step resolution/commit.
    StagedSignal,
    /// A concatenation whose destination leaves have different storage
    /// classes. Each leaf keeps its own local/storage/signal timing while the
    /// right-hand value is evaluated once before any write is applied.
    PerPlace,
}

/// A call into the simulation runtime rather than into the design.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessRuntimeOp {
    /// `assert!` — fail the test and stop when the condition does not hold.
    Assert,
    /// Report a warning without failing.
    Warn,
    /// `print!` — write to the test binary's output.
    Print,
    /// Call a named function that lowering did not inline.
    Call(String),
}

/// One arm of a [`ProcessTerminator::Match`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessMatchArm {
    /// The pattern selecting this arm.
    pub pattern: ProcessPattern,
    /// The block entered when it matches.
    pub block: ProcessBlockId,
    /// The arm's extent.
    pub span: crate::diag::Span,
}

/// A match pattern, lowered to the shapes the CFG needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessPattern {
    /// `_` — matches anything.
    Wildcard,
    /// An enum variant or constant path. Valid programs carry its stable
    /// declaration identity; segments remain for diagnostics and intrinsic
    /// patterns that have no declaration.
    Path {
        /// Resolved declaration, when one exists.
        definition: Option<DefId>,
        /// Namespace segments as written.
        segments: Vec<String>,
    },
    /// A bit pattern, with don't-care positions preserved.
    BitPattern(String),
    /// Alternatives, matching if any does.
    Or(Vec<ProcessPattern>),
    /// An inclusive numeric range.
    Range {
        /// Inclusive left bound.
        left: i64,
        /// Inclusive right bound.
        right: i64,
    },
    /// A character literal naming a variant of a char-valued enum.
    Char(char),
}

/// How control leaves a [`ProcessBlock`]. Every block ends in exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessTerminator {
    /// Leave the process, optionally with a value.
    Return {
        /// The returned value, for a function-shaped body.
        value: Option<ProcessValueId>,
        /// The `return` statement's extent, when one was written.
        span: Option<crate::diag::Span>,
    },
    /// Continue unconditionally at another block.
    Goto(ProcessBlockId),
    /// Two-way branch.
    Branch {
        /// The tested condition.
        condition: ProcessValueId,
        /// Entered when the condition holds.
        then_block: ProcessBlockId,
        /// Entered otherwise.
        else_block: ProcessBlockId,
    },
    /// Multi-way branch on a pattern match.
    Match {
        /// The value being matched.
        scrutinee: ProcessValueId,
        /// The arms, tested in order.
        arms: Vec<ProcessMatchArm>,
        /// A non-exhaustive statement match continues through this block when
        /// no arm matches. Exhaustive matches have no fallback edge.
        fallback: Option<ProcessBlockId>,
    },
    /// Iterate a range/array value. Entry chooses the first element or `exit`;
    /// a body back-edge to this block advances the iterator before choosing
    /// `body` again. `local` is assigned immediately on each iteration.
    For {
        /// The loop variable, assigned immediately each iteration.
        local: ProcessLocalId,
        /// The range being iterated.
        iterable: ProcessValueId,
        /// The loop body's entry block.
        body: ProcessBlockId,
        /// The block entered once iteration finishes.
        exit: ProcessBlockId,
        /// The `for` statement's extent.
        span: crate::diag::Span,
    },
    /// Suspend this process and continue at `resume` when the runtime operation
    /// becomes ready. `await` arguments keep their typed source identity until
    /// direct value lowering replaces [`ProcessValue`].
    Suspend {
        /// Which suspending operation.
        operation: ProcessSuspendOp,
        /// Its arguments, such as the delay for `await`.
        arguments: Vec<ProcessValueId>,
        /// The block to continue at once ready.
        resume: ProcessBlockId,
        /// The suspending statement's extent.
        span: crate::diag::Span,
    },
    /// End this process, leaving the rest of the simulation running.
    Stop {
        /// The statement's extent.
        span: crate::diag::Span,
    },
    /// End the whole simulation.
    Finish {
        /// The statement's extent.
        span: crate::diag::Span,
    },
}

/// Which operation suspended a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSuspendOp {
    /// `await` — resume once the given time or condition is reached.
    Await,
}

/// A typed process operand. Composite expressions refer to earlier arena
/// values by id, making the value graph backend-independent and cheap to walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessValue {
    /// The expression's source extent.
    pub span: crate::diag::Span,
    /// Its frontend type, where one was inferred.
    pub ty: Option<crate::types::Ty>,
    /// Executable meaning of the value.
    pub kind: ProcessValueKind,
}

/// Which version of signal storage a process expression reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSignalState {
    /// The current settled value.
    Current,
    /// The value from before the most recent commit.
    Old,
    /// Whether the value changed at the most recent commit.
    Event,
}

/// A numeric literal's source-independent magnitude.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessNumber {
    /// An arbitrary-width unsigned magnitude, least-significant word first.
    Integer(Vec<u64>),
    /// An IEEE-754 `real`, stored as bits so the IR remains equality-comparable.
    Real(u64),
}

/// A unary operation after parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessUnaryOp {
    /// Arithmetic negation.
    Neg,
    /// Logical or per-element complement.
    Not,
    /// Convert an IEEE-754 real to the signed kernel integer.
    RealToInteger,
}

/// A binary operation after precedence has already shaped the expression tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessBinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Division.
    Div,
    /// Signed kernel-integer addition.
    SignedAdd,
    /// Signed kernel-integer subtraction.
    SignedSub,
    /// Signed kernel-integer multiplication.
    SignedMul,
    /// Signed kernel-integer division.
    SignedDiv,
    /// Type-directed conjunction.
    And,
    /// Type-directed disjunction.
    Or,
    /// Bitwise exclusive-or after std operator lowering.
    Xor,
    /// A user/library-defined operator. Precedence is absent because it has
    /// already done its only job during parsing.
    Custom(String),
    /// Left shift.
    Shl,
    /// Right shift.
    Shr,
    /// Arithmetic right shift.
    ArithmeticShr,
    /// Equality.
    Eq,
    /// Inequality.
    Ne,
    /// Less-than comparison.
    Lt,
    /// Less-than-or-equal comparison.
    Le,
    /// Greater-than comparison.
    Gt,
    /// Greater-than-or-equal comparison.
    Ge,
    /// Signed less-than comparison.
    SignedLt,
    /// Signed less-than-or-equal comparison.
    SignedLe,
    /// Signed greater-than comparison.
    SignedGt,
    /// Signed greater-than-or-equal comparison.
    SignedGe,
    /// IEEE-754 addition.
    FloatAdd,
    /// IEEE-754 subtraction.
    FloatSub,
    /// IEEE-754 multiplication.
    FloatMul,
    /// IEEE-754 division.
    FloatDiv,
    /// Ordered IEEE-754 equality.
    FloatEq,
    /// Ordered IEEE-754 inequality.
    FloatNe,
    /// Ordered IEEE-754 less-than comparison.
    FloatLt,
    /// Ordered IEEE-754 less-than-or-equal comparison.
    FloatLe,
    /// Ordered IEEE-754 greater-than comparison.
    FloatGt,
    /// Ordered IEEE-754 greater-than-or-equal comparison.
    FloatGe,
}

/// A value-producing match arm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessValueMatchArm {
    /// Pattern selecting this value.
    pub pattern: ProcessPattern,
    /// Value produced by the arm.
    pub value: ProcessValueId,
    /// Source extent of the arm.
    pub span: crate::diag::Span,
}

/// One field or positional element of a constructed aggregate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessAggregateField {
    /// Explicit field name, or `None` for a positional connection.
    pub name: Option<String>,
    /// Connected value. `None` exists only for parser error recovery.
    pub value: Option<ProcessValueId>,
    /// Source extent of this field.
    pub span: crate::diag::Span,
}

/// Executable value forms used by process CFGs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessValueKind {
    /// A numeric literal without a language width ceiling.
    Number(ProcessNumber),
    /// A number interpreted through a std-defined suffix such as `ns` or
    /// `MHz`. `suffix` is the semantic dispatch symbol, not source text.
    Suffixed {
        /// Literal magnitude before the suffix conversion.
        number: ProcessNumber,
        /// Suffix symbol.
        suffix: String,
    },
    /// A radix bit-string literal, already decoded into words.
    BitString {
        /// Number of meaningful bits.
        width: u32,
        /// Literal bits, least-significant word first.
        words: Vec<u64>,
    },
    /// A context-typed character or enum literal.
    Char(char),
    /// A UTF-8 string literal.
    String(String),
    /// A process-local variable.
    Local {
        /// Process owning the local.
        process: ProcessId,
        /// Local within that process.
        local: ProcessLocalId,
    },
    /// Persistent state owned by a test entity.
    Storage(ProcessStorageId),
    /// One or more flattened signals making up a source value.
    Signal {
        /// Scalar leaves, in source-layout order.
        signals: Vec<SignalId>,
        /// Which signal state is observed.
        state: ProcessSignalState,
    },
    /// A resolved declaration such as a constant or enum variant.
    Definition(DefId),
    /// A compiler/runtime name that intentionally has no source declaration.
    Intrinsic(String),
    /// A field or method member selected from a value.
    Field {
        /// Aggregate or receiver value.
        base: ProcessValueId,
        /// Selected member.
        field: String,
    },
    /// A system attribute that is not represented directly as a signal state.
    Attribute {
        /// Value being queried.
        base: ProcessValueId,
        /// Canonical attribute name.
        attribute: String,
    },
    /// Checked element indexing or slicing.
    Index {
        /// Indexed value.
        base: ProcessValueId,
        /// Index or range operand.
        index: ProcessValueId,
    },
    /// A fixed inclusive bit slice produced after range/index elaboration.
    BitSlice {
        /// Packed value being sliced.
        base: ProcessValueId,
        /// Inclusive high storage bit.
        high: u32,
        /// Inclusive low storage bit.
        low: u32,
    },
    /// A checked runtime index. Evaluation latches a bounds failure when the
    /// validity predicate is false on the active control-flow path.
    CheckedIndex {
        /// Runtime index value.
        index: ProcessValueId,
        /// In-domain predicate.
        valid: ProcessValueId,
        /// Written left endpoint of the declared range.
        left: i64,
        /// Written right endpoint of the declared range.
        right: i64,
        /// Original access site used by diagnostics.
        span: crate::diag::Span,
    },
    /// Read one elaboration-owned constant lookup table. An out-of-range index
    /// yields zero, matching the normalized digital expression.
    TableLookup {
        /// Table in [`Design::lookup_tables`].
        table: LookupTableId,
        /// Runtime table index.
        index: ProcessValueId,
    },
    /// An inclusive range; absent bounds are supplied by the indexing value.
    Range {
        /// Written left bound.
        left: Option<ProcessValueId>,
        /// Written right bound.
        right: Option<ProcessValueId>,
    },
    /// A unary operation.
    Unary {
        /// Operation performed.
        operation: ProcessUnaryOp,
        /// Operand.
        operand: ProcessValueId,
    },
    /// A binary operation.
    Binary {
        /// Operation performed.
        operation: ProcessBinaryOp,
        /// Left operand.
        left: ProcessValueId,
        /// Right operand.
        right: ProcessValueId,
    },
    /// A value-level conditional.
    Select {
        /// Condition.
        condition: ProcessValueId,
        /// Value when true.
        then_value: ProcessValueId,
        /// Value when false.
        else_value: ProcessValueId,
    },
    /// A vector comparison whose unknown-value rule is resolved after
    /// metavalue companions are known.
    MetaCompare {
        /// Inequality reverses the unknown result.
        not_equal: bool,
        /// Compared values whose companion planes decide unknownness.
        operands: Vec<ProcessValueId>,
        /// Ordinary comparison result used when all operands are binary.
        inner: ProcessValueId,
    },
    /// A value-level pattern match.
    Match {
        /// Matched value.
        scrutinee: ProcessValueId,
        /// Arms in first-match order.
        arms: Vec<ProcessValueMatchArm>,
    },
    /// A function, method, intrinsic, or conversion call.
    Call {
        /// Callable value or method field.
        callee: ProcessValueId,
        /// Explicit concrete type arguments. Phase 1 permits these only on
        /// `read<T>`, whose checked result type is the requested `T`.
        type_arguments: Vec<crate::types::Ty>,
        /// Value arguments.
        arguments: Vec<ProcessValueId>,
        /// Whether macro-shaped lazy syntax was used.
        bang: bool,
    },
    /// A normalized foreign C call with its scalar ABI made explicit.
    ForeignCall {
        /// Linker-visible C symbol.
        name: String,
        /// Arguments in source order.
        arguments: Vec<ProcessValueId>,
        /// Whether each argument is passed as f64.
        float_arguments: Vec<bool>,
        /// Whether each non-float argument is a signed kernel integer.
        integer_arguments: Vec<bool>,
        /// Whether the result is returned as f64.
        float_result: bool,
        /// Whether the non-float result is a signed kernel integer.
        integer_result: bool,
    },
    /// A struct/entity aggregate literal.
    Construct {
        /// Concrete result type, when type checking supplied one.
        ty: Option<crate::types::Ty>,
        /// Explicit and positional fields in source order.
        fields: Vec<ProcessAggregateField>,
        /// Struct-update base.
        spread: Option<ProcessValueId>,
    },
    /// Packed concatenation, most-significant part first.
    Concat(Vec<ProcessValueId>),
    /// Ordinary array literal in ascending source order.
    Array(Vec<ProcessValueId>),
    /// Error-recovery value copied from an invalid normalized digital
    /// expression. Validation rejects it before any backend runs.
    Invalid,
}

impl ProcessIr {
    /// Append one already elaborated digital expression to the shared value
    /// arena. Children are emitted first, preserving the arena's dominance
    /// invariant. This is the migration bridge from the normalized hardware
    /// representation; it deliberately carries no frontend-only type text.
    pub(crate) fn push_digital_expr(
        &mut self,
        expression: &Expr,
        fallback_span: crate::diag::Span,
    ) -> ProcessValueId {
        let kind = match expression {
            Expr::Const(value) => ProcessValueKind::Number(ProcessNumber::Integer(vec![*value])),
            Expr::WideConst(words) => {
                ProcessValueKind::Number(ProcessNumber::Integer(words.clone()))
            }
            Expr::Real(value) => ProcessValueKind::Number(ProcessNumber::Real(value.to_bits())),
            Expr::Logic(character) => ProcessValueKind::Char(*character),
            Expr::Current(signal) => ProcessValueKind::Signal {
                signals: vec![*signal],
                state: ProcessSignalState::Current,
            },
            Expr::Old(signal) => ProcessValueKind::Signal {
                signals: vec![*signal],
                state: ProcessSignalState::Old,
            },
            Expr::Event(signal) => ProcessValueKind::Signal {
                signals: vec![*signal],
                state: ProcessSignalState::Event,
            },
            Expr::Unary { op, rhs } => ProcessValueKind::Unary {
                operation: match op {
                    UnOp::Neg => ProcessUnaryOp::Neg,
                    UnOp::Not => ProcessUnaryOp::Not,
                    UnOp::RealToInt => ProcessUnaryOp::RealToInteger,
                },
                operand: self.push_digital_expr(rhs, fallback_span),
            },
            Expr::Binary { op, lhs, rhs } => ProcessValueKind::Binary {
                operation: process_binary_from_digital(*op),
                left: self.push_digital_expr(lhs, fallback_span),
                right: self.push_digital_expr(rhs, fallback_span),
            },
            Expr::Slice { base, hi, lo } => ProcessValueKind::BitSlice {
                base: self.push_digital_expr(base, fallback_span),
                high: *hi,
                low: *lo,
            },
            Expr::TableLookup { table, index } => ProcessValueKind::TableLookup {
                table: *table,
                index: self.push_digital_expr(index, fallback_span),
            },
            Expr::CheckedIndex {
                index,
                valid,
                left,
                right,
                span,
            } => ProcessValueKind::CheckedIndex {
                index: self.push_digital_expr(index, *span),
                valid: self.push_digital_expr(valid, *span),
                left: *left,
                right: *right,
                span: *span,
            },
            Expr::Select { cond, then, els } => ProcessValueKind::Select {
                condition: self.push_digital_expr(cond, fallback_span),
                then_value: self.push_digital_expr(then, fallback_span),
                else_value: self.push_digital_expr(els, fallback_span),
            },
            Expr::MetaCmp {
                ne,
                operands,
                inner,
            } => ProcessValueKind::MetaCompare {
                not_equal: *ne,
                operands: operands
                    .iter()
                    .map(|operand| self.push_digital_expr(operand, fallback_span))
                    .collect(),
                inner: self.push_digital_expr(inner, fallback_span),
            },
            Expr::CCall {
                name,
                args,
                f64_args,
                integer_args,
                f64_ret,
                integer_ret,
            } => ProcessValueKind::ForeignCall {
                name: name.clone(),
                arguments: args
                    .iter()
                    .map(|argument| self.push_digital_expr(argument, fallback_span))
                    .collect(),
                float_arguments: f64_args.clone(),
                integer_arguments: integer_args.clone(),
                float_result: *f64_ret,
                integer_result: *integer_ret,
            },
            Expr::Unknown => ProcessValueKind::Invalid,
        };
        let id = ProcessValueId(self.values.len() as u32);
        self.values.push(ProcessValue {
            span: fallback_span,
            ty: None,
            kind,
        });
        id
    }

    /// Structural invariants shared by every process backend.
    pub fn validate(&self, signal_count: u32) -> Vec<String> {
        let mut issues = Vec::new();
        let mut test_names = HashSet::new();
        let mut test_roots = HashSet::new();
        let value_count = self.values.len() as u32;

        for (index, process) in self.processes.iter().enumerate() {
            if process.id != ProcessId(index as u32) {
                issues.push(format!(
                    "process at index {index} has non-dense id {:?}",
                    process.id
                ));
            }
            if process
                .blocks
                .get(process.entry.0 as usize)
                .map(|block| block.id)
                != Some(process.entry)
            {
                issues.push(format!(
                    "process {:?} has an invalid entry block",
                    process.id
                ));
            }
            if let ProcessActivation::Reactive { sensitivity } = &process.activation {
                for item in sensitivity {
                    match item {
                        ProcessSensitivity::Signal(signal) if signal.0 >= signal_count => {
                            issues.push(format!(
                                "process {:?} has out-of-range sensitivity signal {}",
                                process.id, signal.0
                            ));
                        }
                        ProcessSensitivity::Storage(storage)
                            if storage.0 >= self.storages.len() as u32 =>
                        {
                            issues.push(format!(
                                "process {:?} has out-of-range sensitivity storage {}",
                                process.id, storage.0
                            ));
                        }
                        ProcessSensitivity::Signal(_) | ProcessSensitivity::Storage(_) => {}
                    }
                }
            }
            for (local_index, local) in process.locals.iter().enumerate() {
                if local.id != ProcessLocalId(local_index as u32) {
                    issues.push(format!(
                        "process {:?} has a non-dense local id {:?}",
                        process.id, local.id
                    ));
                }
            }
            for (block_index, block) in process.blocks.iter().enumerate() {
                if block.id != ProcessBlockId(block_index as u32) {
                    issues.push(format!(
                        "process {:?} has a non-dense block id {:?}",
                        process.id, block.id
                    ));
                }
                for instruction in &block.instructions {
                    match instruction {
                        ProcessInstruction::Declare { local, .. } => {
                            if process.locals.get(local.0 as usize).map(|value| value.id)
                                != Some(*local)
                            {
                                issues.push(format!(
                                    "process {:?} references invalid local {:?}",
                                    process.id, local
                                ));
                            }
                        }
                        ProcessInstruction::Assign {
                            semantics, target, ..
                        } => {
                            let expected = match semantics {
                                ProcessAssignment::ImmediateLocal => Some(ProcessPlaceClass::Local),
                                ProcessAssignment::ImmediateStorage => {
                                    Some(ProcessPlaceClass::Storage)
                                }
                                ProcessAssignment::StagedSignal => Some(ProcessPlaceClass::Signal),
                                ProcessAssignment::PerPlace => None,
                            };
                            let valid = match expected {
                                Some(expected) => {
                                    process_place_class(self, *target) == Some(expected)
                                }
                                None => {
                                    matches!(
                                        self.values.get(target.0 as usize).map(|value| &value.kind),
                                        Some(ProcessValueKind::Concat(_))
                                    ) && process_place_classes(self, *target)
                                        .is_some_and(|classes| classes.len() > 1)
                                }
                            };
                            if !valid {
                                issues.push(format!(
                                    "process {:?} block {:?} has {:?} assignment to incompatible place {:?}",
                                    process.id, block.id, semantics, target
                                ));
                            }
                        }
                        ProcessInstruction::Schedule { target, .. } => {
                            if !process_place_classes(self, *target).is_some_and(|classes| {
                                classes.iter().all(|class| {
                                    matches!(
                                        class,
                                        ProcessPlaceClass::Storage | ProcessPlaceClass::Signal
                                    )
                                })
                            }) {
                                issues.push(format!(
                                    "process {:?} block {:?} schedules non-storage/signal place {:?}",
                                    process.id, block.id, target
                                ));
                            }
                        }
                        ProcessInstruction::Runtime { .. } => {}
                    }
                    for value in process_instruction_values(instruction) {
                        if value.0 >= value_count {
                            issues.push(format!(
                                "process {:?} block {:?} references invalid value {:?}",
                                process.id, block.id, value
                            ));
                        }
                    }
                }
                for value in process_terminator_values(&block.terminator) {
                    if value.0 >= value_count {
                        issues.push(format!(
                            "process {:?} block {:?} terminator references invalid value {:?}",
                            process.id, block.id, value
                        ));
                    }
                }
                if let ProcessTerminator::For { local, .. } = &block.terminator {
                    if process.locals.get(local.0 as usize).map(|value| value.id) != Some(*local) {
                        issues.push(format!(
                            "process {:?} block {:?} loop references invalid local {:?}",
                            process.id, block.id, local
                        ));
                    }
                }
                for target in process_terminator_targets(&block.terminator) {
                    if process.blocks.get(target.0 as usize).map(|value| value.id) != Some(target) {
                        issues.push(format!(
                            "process {:?} branches to invalid block {:?}",
                            process.id, target
                        ));
                    }
                }
            }
        }

        let storage_count = self.storages.len() as u32;
        let mut storage_names = HashSet::new();
        for (index, storage) in self.storages.iter().enumerate() {
            let id = ProcessStorageId(index as u32);
            if storage.id != id {
                issues.push(format!(
                    "storage {:?} is stored at index {} under {:?}",
                    storage.id, index, id
                ));
            }
            if !storage_names.insert((storage.owner, storage.name.clone())) {
                issues.push(format!(
                    "storage `{}` is declared more than once in {:?}",
                    storage.name, storage.owner
                ));
            }
            if let Some(initializer) = storage.initializer {
                if initializer.0 >= value_count {
                    issues.push(format!(
                        "storage {:?} initializer references invalid value {:?}",
                        id, initializer
                    ));
                }
            }
            let mut bindings = HashSet::new();
            for binding in &storage.bindings {
                if binding.signal.0 >= signal_count {
                    issues.push(format!(
                        "storage {:?} binds `{}` to invalid signal {:?}",
                        id, binding.projection, binding.signal
                    ));
                }
                if !bindings.insert((binding.projection.clone(), binding.signal)) {
                    issues.push(format!(
                        "storage {:?} binds `{}` to {:?} more than once",
                        id, binding.projection, binding.signal
                    ));
                }
            }
        }

        for (index, value) in self.values.iter().enumerate() {
            let id = ProcessValueId(index as u32);
            for dependency in process_value_dependencies(&value.kind) {
                if dependency.0 >= value_count {
                    issues.push(format!(
                        "process value {:?} references invalid value {:?}",
                        id, dependency
                    ));
                } else if dependency.0 >= id.0 {
                    issues.push(format!(
                        "process value {:?} has non-dominating dependency {:?}",
                        id, dependency
                    ));
                }
            }
            match &value.kind {
                ProcessValueKind::Signal { signals, .. } => {
                    if signals.is_empty() {
                        issues.push(format!("process value {:?} has no signal leaves", id));
                    }
                    let mut seen = HashSet::new();
                    for signal in signals {
                        if signal.0 >= signal_count {
                            issues.push(format!(
                                "process value {:?} references invalid signal {:?}",
                                id, signal
                            ));
                        } else if !seen.insert(*signal) {
                            issues.push(format!(
                                "process value {:?} repeats signal {:?}",
                                id, signal
                            ));
                        }
                    }
                }
                ProcessValueKind::Storage(storage) => {
                    if storage.0 >= storage_count {
                        issues.push(format!(
                            "process value {:?} references invalid storage {:?}",
                            id, storage
                        ));
                    }
                }
                ProcessValueKind::Local { process, local } => {
                    match self.processes.get(process.0 as usize) {
                        Some(owner)
                            if owner.id == *process
                                && owner.locals.get(local.0 as usize).map(|item| item.id)
                                    == Some(*local) => {}
                        _ => issues.push(format!(
                            "process value {:?} references invalid local {:?} in {:?}",
                            id, local, process
                        )),
                    }
                }
                ProcessValueKind::BitSlice { high, low, .. } if low > high => {
                    issues.push(format!(
                        "process value {:?} has slice bounds low {} above high {}",
                        id, low, high
                    ));
                }
                ProcessValueKind::ForeignCall {
                    arguments,
                    float_arguments,
                    integer_arguments,
                    ..
                } if arguments.len() != float_arguments.len()
                    || arguments.len() != integer_arguments.len() =>
                {
                    issues.push(format!(
                        "process value {:?} has inconsistent foreign-call ABI vectors",
                        id
                    ));
                }
                ProcessValueKind::Invalid => {
                    issues.push(format!("process value {:?} is invalid", id));
                }
                _ => {}
            }
        }

        for test in &self.tests {
            if !test_names.insert(test.qualified_name.clone()) {
                issues.push(format!(
                    "duplicate test descriptor `{}`",
                    test.qualified_name
                ));
            }
            if !test_roots.insert(test.root) {
                issues.push(format!("test root {:?} is used more than once", test.root));
            }
            let mut referenced = HashSet::new();
            for process_id in &test.processes {
                if !referenced.insert(*process_id) {
                    issues.push(format!(
                        "test `{}` references process {:?} more than once",
                        test.qualified_name, process_id
                    ));
                    continue;
                }
                match self.processes.get(process_id.0 as usize) {
                    Some(process) if process.id == *process_id && process.root == test.root => {}
                    Some(_) => issues.push(format!(
                        "test `{}` references process {:?} assigned to another root",
                        test.qualified_name, process_id
                    )),
                    None => issues.push(format!(
                        "test `{}` references invalid process {:?}",
                        test.qualified_name, process_id
                    )),
                }
            }
        }
        issues
    }

    /// Render the process IR as text, for `--emit ir` and for debugging a
    /// lowering change.
    pub fn to_ir_string(&self) -> String {
        let mut output = String::new();
        for (index, value) in self.values.iter().enumerate() {
            let ty = value
                .ty
                .as_ref()
                .map(|ty| format!(" : {ty:?}"))
                .unwrap_or_default();
            output.push_str(&format!("value %v{index}{ty} = {:?}\n", value.kind));
        }
        for storage in &self.storages {
            let ty = storage
                .ty
                .as_ref()
                .map(|ty| format!(" : {ty:?}"))
                .unwrap_or_default();
            let initializer = storage
                .initializer
                .map(|value| format!(" = %v{}", value.0))
                .unwrap_or_default();
            let bindings = storage
                .bindings
                .iter()
                .map(|binding| {
                    format!(
                        "{}-{:?}->s{}",
                        binding.projection, binding.direction, binding.signal.0
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let bindings = if bindings.is_empty() {
                String::new()
            } else {
                format!(" bound [{bindings}]")
            };
            output.push_str(&format!(
                "storage %g{} root {} {}{ty}{initializer}{bindings}\n",
                storage.id.0, storage.owner.0, storage.name
            ));
        }
        for test in &self.tests {
            let processes = test
                .processes
                .iter()
                .map(|process| format!("%p{}", process.0))
                .collect::<Vec<_>>()
                .join(", ");
            output.push_str(&format!(
                "test @{} root {} processes [{}]\n",
                test.qualified_name, test.root.0, processes
            ));
        }
        for process in &self.processes {
            let label = process
                .label
                .as_ref()
                .map(|label| format!(" [{label}]"))
                .unwrap_or_default();
            output.push_str(&format!(
                "process %p{} root {} owner {}{label} {:?} {{\n",
                process.id.0, process.root.0, process.owner.0, process.activation
            ));
            for local in &process.locals {
                output.push_str(&format!("  local %{} {}\n", local.id.0, local.name));
            }
            for block in &process.blocks {
                output.push_str(&format!("  bb{}:\n", block.id.0));
                for instruction in &block.instructions {
                    output.push_str(&format!("    {instruction:?}\n"));
                }
                output.push_str(&format!("    {:?}\n", block.terminator));
            }
            output.push_str("}\n");
        }
        output
    }
}

/// The blocks a terminator can transfer control to, for CFG validation and
/// reachability.
fn process_terminator_targets(terminator: &ProcessTerminator) -> Vec<ProcessBlockId> {
    match terminator {
        ProcessTerminator::Return { .. }
        | ProcessTerminator::Stop { .. }
        | ProcessTerminator::Finish { .. } => Vec::new(),
        ProcessTerminator::Goto(target) => vec![*target],
        ProcessTerminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        ProcessTerminator::Match { arms, fallback, .. } => arms
            .iter()
            .map(|arm| arm.block)
            .chain(fallback.iter().copied())
            .collect(),
        ProcessTerminator::For { body, exit, .. } => vec![*body, *exit],
        ProcessTerminator::Suspend { resume, .. } => vec![*resume],
    }
}

/// The operand ids an instruction reads.
fn process_instruction_values(instruction: &ProcessInstruction) -> Vec<ProcessValueId> {
    match instruction {
        ProcessInstruction::Declare { initializer, .. } => initializer.iter().copied().collect(),
        ProcessInstruction::Assign { target, value, .. } => vec![*target, *value],
        ProcessInstruction::Schedule {
            target,
            value,
            delay,
            ..
        } => vec![*target, *value, *delay],
        ProcessInstruction::Runtime { arguments, .. } => arguments.clone(),
    }
}

/// The operand ids a terminator reads.
fn process_terminator_values(terminator: &ProcessTerminator) -> Vec<ProcessValueId> {
    match terminator {
        ProcessTerminator::Return { value, .. } => value.iter().copied().collect(),
        ProcessTerminator::Branch { condition, .. } => vec![*condition],
        ProcessTerminator::Match { scrutinee, .. } => vec![*scrutinee],
        ProcessTerminator::For { iterable, .. } => vec![*iterable],
        ProcessTerminator::Suspend { arguments, .. } => arguments.clone(),
        ProcessTerminator::Goto(_)
        | ProcessTerminator::Stop { .. }
        | ProcessTerminator::Finish { .. } => Vec::new(),
    }
}

/// Operand ids embedded by one value node.
pub(crate) fn process_value_dependencies(value: &ProcessValueKind) -> Vec<ProcessValueId> {
    match value {
        ProcessValueKind::Field { base, .. }
        | ProcessValueKind::Attribute { base, .. }
        | ProcessValueKind::BitSlice { base, .. }
        | ProcessValueKind::TableLookup { index: base, .. }
        | ProcessValueKind::Unary { operand: base, .. } => vec![*base],
        ProcessValueKind::Index { base, index } => vec![*base, *index],
        ProcessValueKind::CheckedIndex { index, valid, .. } => vec![*index, *valid],
        ProcessValueKind::Range { left, right } => left.iter().chain(right).copied().collect(),
        ProcessValueKind::Binary { left, right, .. } => vec![*left, *right],
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => vec![*condition, *then_value, *else_value],
        ProcessValueKind::MetaCompare {
            operands, inner, ..
        } => operands
            .iter()
            .copied()
            .chain(std::iter::once(*inner))
            .collect(),
        ProcessValueKind::Match { scrutinee, arms } => std::iter::once(*scrutinee)
            .chain(arms.iter().map(|arm| arm.value))
            .collect(),
        ProcessValueKind::Call {
            callee, arguments, ..
        } => std::iter::once(*callee)
            .chain(arguments.iter().copied())
            .collect(),
        ProcessValueKind::ForeignCall { arguments, .. } => arguments.clone(),
        ProcessValueKind::Construct { fields, spread, .. } => fields
            .iter()
            .filter_map(|field| field.value)
            .chain(spread.iter().copied())
            .collect(),
        ProcessValueKind::Concat(values) | ProcessValueKind::Array(values) => values.clone(),
        ProcessValueKind::Number(_)
        | ProcessValueKind::Suffixed { .. }
        | ProcessValueKind::BitString { .. }
        | ProcessValueKind::Char(_)
        | ProcessValueKind::String(_)
        | ProcessValueKind::Local { .. }
        | ProcessValueKind::Storage(_)
        | ProcessValueKind::Signal { .. }
        | ProcessValueKind::Definition(_)
        | ProcessValueKind::Intrinsic(_)
        | ProcessValueKind::Invalid => Vec::new(),
    }
}

/// Root storage class of an assignable process value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ProcessPlaceClass {
    Local,
    Storage,
    Signal,
}

/// Follow projections to the storage object an assignment ultimately writes.
fn process_place_class(ir: &ProcessIr, value: ProcessValueId) -> Option<ProcessPlaceClass> {
    let classes = process_place_classes(ir, value)?;
    let first = *classes.first()?;
    classes.iter().all(|class| *class == first).then_some(first)
}

/// Every root storage class written by an assignable process value.
fn process_place_classes(ir: &ProcessIr, value: ProcessValueId) -> Option<Vec<ProcessPlaceClass>> {
    match &ir.values.get(value.0 as usize)?.kind {
        ProcessValueKind::Local { .. } => Some(vec![ProcessPlaceClass::Local]),
        ProcessValueKind::Storage(_) => Some(vec![ProcessPlaceClass::Storage]),
        ProcessValueKind::Signal {
            state: ProcessSignalState::Current,
            ..
        } => Some(vec![ProcessPlaceClass::Signal]),
        ProcessValueKind::Field { base, .. } | ProcessValueKind::Index { base, .. } => {
            process_place_classes(ir, *base)
        }
        ProcessValueKind::Concat(values) => {
            let mut classes = Vec::new();
            for value in values {
                classes.extend(process_place_classes(ir, *value)?);
            }
            (!classes.is_empty()).then_some(classes)
        }
        _ => None,
    }
}

/// Preserve the fully selected arithmetic domain of a normalized digital
/// operation. Unlike source operators, these variants require no type lookup
/// or std dispatch in a backend.
fn process_binary_from_digital(operation: BinOp) -> ProcessBinaryOp {
    match operation {
        BinOp::Add => ProcessBinaryOp::Add,
        BinOp::Sub => ProcessBinaryOp::Sub,
        BinOp::Mul => ProcessBinaryOp::Mul,
        BinOp::Div => ProcessBinaryOp::Div,
        BinOp::SAdd => ProcessBinaryOp::SignedAdd,
        BinOp::SSub => ProcessBinaryOp::SignedSub,
        BinOp::SMul => ProcessBinaryOp::SignedMul,
        BinOp::SDiv => ProcessBinaryOp::SignedDiv,
        BinOp::And => ProcessBinaryOp::And,
        BinOp::Or => ProcessBinaryOp::Or,
        BinOp::Xor => ProcessBinaryOp::Xor,
        BinOp::Shl => ProcessBinaryOp::Shl,
        BinOp::Shr => ProcessBinaryOp::Shr,
        BinOp::AShr => ProcessBinaryOp::ArithmeticShr,
        BinOp::Eq => ProcessBinaryOp::Eq,
        BinOp::Ne => ProcessBinaryOp::Ne,
        BinOp::Lt => ProcessBinaryOp::Lt,
        BinOp::Le => ProcessBinaryOp::Le,
        BinOp::Gt => ProcessBinaryOp::Gt,
        BinOp::Ge => ProcessBinaryOp::Ge,
        BinOp::SLt => ProcessBinaryOp::SignedLt,
        BinOp::SLe => ProcessBinaryOp::SignedLe,
        BinOp::SGt => ProcessBinaryOp::SignedGt,
        BinOp::SGe => ProcessBinaryOp::SignedGe,
        BinOp::FAdd => ProcessBinaryOp::FloatAdd,
        BinOp::FSub => ProcessBinaryOp::FloatSub,
        BinOp::FMul => ProcessBinaryOp::FloatMul,
        BinOp::FDiv => ProcessBinaryOp::FloatDiv,
        BinOp::FEq => ProcessBinaryOp::FloatEq,
        BinOp::FNe => ProcessBinaryOp::FloatNe,
        BinOp::FLt => ProcessBinaryOp::FloatLt,
        BinOp::FLe => ProcessBinaryOp::FloatLe,
        BinOp::FGt => ProcessBinaryOp::FloatGt,
        BinOp::FGe => ProcessBinaryOp::FloatGe,
    }
}

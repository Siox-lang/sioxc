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

/// The canonical control-flow product owned by an elaborated
/// [`Design`](super::Design).
///
/// Source hardware and typed procedural/test CFGs lower directly under
/// `ir::lower`. The representation and its invariants live here so no backend
/// needs a second process product. Finalized [`Driver`](super::Driver) /
/// [`EventBlock`](super::EventBlock) compatibility forms are derived from this
/// arena; scheduler decomposition is never input to source CFG construction.
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
    /// Recursive source layout retained for arena values that have an
    /// aggregate representation independent of a storage/local declaration.
    ///
    /// Transitional hand-built fixtures may leave this empty. Production
    /// lowering keeps it index-aligned with [`Self::values`].
    pub value_layouts: Vec<Option<SourceLayout>>,
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
    /// Which simulation region owns the process's writes. This is independent
    /// of activation: procedural test stimulus and hardware logic can both be
    /// reactive, but only hardware regions participate in the derived
    /// `Driver`/`EventBlock` scheduler view.
    pub region: ProcessRegion,
    /// The block execution starts in.
    pub entry: ProcessBlockId,
    /// Locals owned by this process, indexed by [`ProcessLocalId`].
    pub locals: Vec<ProcessLocal>,
    /// The control-flow graph's blocks, indexed by [`ProcessBlockId`].
    pub blocks: Vec<ProcessBlock>,
}

impl ProcessCfg {
    /// Append one empty block and return its dense id.
    pub(crate) fn push_block(&mut self) -> ProcessBlockId {
        let id = ProcessBlockId(self.blocks.len() as u32);
        self.blocks.push(ProcessBlock::empty(id));
        id
    }
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

/// The semantic simulation region of one canonical process.
///
/// This metadata is intentionally explicit. Inferring it from sensitivity or
/// from an `Event` operand would misclassify reactive testbench clocks as
/// hardware, and would make scheduler derivation depend on expression shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessRegion {
    /// Ordinary procedural behavior: test stimulus, clocks, and resumable
    /// source processes. It executes only through the Process runtime.
    Procedural,
    /// Continuously settled hardware behavior. Its staged writes derive
    /// combinational [`Driver`](super::Driver) compatibility entries.
    Combinational,
    /// Edge/event-controlled hardware behavior. `condition` is evaluated once
    /// for the event block, while `body` contains its conditionally guarded
    /// staged writes.
    Event {
        /// The event condition in the shared Process value arena.
        condition: ProcessValueId,
        /// The first block containing event-controlled writes.
        body: ProcessBlockId,
        /// Source driver identity shared by this event block's writes. This is
        /// retained while compatibility scheduler forms exist; source-first
        /// lowering can allocate it from the owning process.
        driver_context: u32,
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
    /// while the source-process lowerer classifies writes. Equal spellings in
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

impl ProcessBlock {
    /// Construct an open lowering block whose default terminator returns.
    pub(crate) fn empty(id: ProcessBlockId) -> Self {
        Self {
            id,
            instructions: Vec::new(),
            terminator: ProcessTerminator::Return {
                value: None,
                span: None,
            },
        }
    }
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
    ///
    /// Assignment evaluation is transactional: the complete right-hand value
    /// and every index embedded in `target` are read from the pre-write state
    /// before any destination root is mutated or staged. Composite targets are
    /// then merged into their packed roots and published according to
    /// `semantics`. Backends must not interleave operand evaluation with leaf
    /// writes.
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
    /// native lowering never has to infer scheduling from an optional field.
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
        /// Frontend-normalized message formatting. Text and display kinds are
        /// explicit so a backend never has to parse source format syntax or
        /// reconstruct presentation semantics from an AST.
        format: Option<Vec<ProcessFormatPart>>,
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
    /// A concatenated destination. Each leaf keeps its own
    /// local/storage/signal timing while the right-hand value is evaluated
    /// once before any write is applied.
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
    /// Replace the deterministic simulation random-generator state.
    Seed,
    /// Call a named function that lowering did not inline.
    Call(String),
}

/// A value returned by a design-independent simulation host service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessHostValueOp {
    /// One raw deterministic 64-bit random word.
    Random,
    /// One deterministic integer in an inclusive, order-independent range.
    RandomRange,
    /// One deterministic IEEE-754 value in the half-open interval `0.0..1.0`.
    Uniform,
    /// Decode one UTF-8 file into a runtime-owned string and return its handle.
    ReadUtf8,
    /// Decode UTF-8 directly into a fixed `Char[N]` Process value.
    ReadUtf8Fixed,
    /// Read raw bytes into the exact-width Process value supplied by context.
    ReadBinary,
    /// Test whether a filesystem path exists without raising an I/O failure.
    FileExists,
    /// Return the Unicode scalar count of a runtime-owned string handle.
    StringLength,
    /// Read one Unicode scalar from a runtime-owned string handle.
    StringIndex,
    /// Compare a runtime-owned string handle with a retained UTF-8 literal.
    StringEqualsUtf8,
}

/// One normalized piece of a runtime message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessFormatPart {
    /// Literal text, with escaped braces already reduced.
    Text(String),
    /// A typed value rendered at this position.
    Value {
        /// Process value to render.
        value: ProcessValueId,
        /// Source-level presentation semantics.
        kind: ProcessDisplayKind,
    },
}

/// How a Process value is presented by the simulation runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessDisplayKind {
    /// Unsigned arbitrary-width decimal.
    Unsigned,
    /// Signed two's-complement arbitrary-width decimal.
    Signed,
    /// IEEE-754 `real` using the language's compact display form.
    Real,
    /// One Unicode scalar value.
    Character,
    /// A UTF-8 string value.
    String,
    /// An enum discriminant rendered through the retained symbol table.
    Enum(String),
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
    /// A resolver-selected enum variant or other exact scalar value.
    Number(ProcessNumber),
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
    /// A normalized bit pattern: only positions selected by `mask` must equal
    /// the corresponding positions in `value`.
    BitMask {
        /// Compared positions, least-significant word first.
        mask: Vec<u64>,
        /// Required bits, least-significant word first.
        value: Vec<u64>,
    },
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
    /// `await <time>` — resume after the duration on the simulation timeline.
    AwaitTime,
    /// Suspend until simulation state changes. Condition and edge awaits lower
    /// their trigger to an ordinary branch around this scheduler primitive;
    /// an edge enters the suspension before its first trigger check.
    AwaitCondition,
    /// Publish a foreground drive, settle reactive processes to a fixed
    /// point, then resume at the same simulation time.
    Settle,
}

/// A typed process operand. Composite expressions refer to earlier arena
/// values by id, making the value graph backend-independent and cheap to walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessValue {
    /// The expression's source extent.
    pub span: crate::diag::Span,
    /// Its frontend type, where one was inferred.
    pub ty: Option<crate::types::Ty>,
    /// Concrete packed width when this value has a machine representation.
    /// This is authoritative for both scalar values and statically sized
    /// aggregates; [`ProcessIr::value_layouts`] describes aggregate shape and
    /// offsets but does not supply a missing Process-value width. Ranges and
    /// values with no packed representation leave it absent.
    pub bit_width: Option<u32>,
    /// Executable meaning of the value.
    pub kind: ProcessValueKind,
}

/// Which version of signal storage a process expression reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// Convert a signed kernel integer to the IEEE-754 real of its value.
    IntegerToReal,
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
    /// A historical/event observation of persistent test state. Ordinary
    /// writes continue to use [`ProcessValueKind::Storage`], so this form can
    /// never accidentally become an assignment place.
    StorageState {
        /// Observed storage frame.
        storage: ProcessStorageId,
        /// Old or event state; current reads use `Storage`.
        state: ProcessSignalState,
    },
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
    /// The recursive default value of the declared [`ProcessValue::ty`].
    ///
    /// This represents zero-argument type construction (`T()` and
    /// `T::new()`) without retaining callable source syntax. Scalar defaults,
    /// enum first-variant values, packed families, arrays, and structs are all
    /// obtained from the same retained source layout metadata.
    Default,
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
    /// A source-labelled packed slice. Unlike [`Self::BitSlice`], endpoints
    /// retain their written order, so `value[7..4]` and `value[4..7]` select
    /// the same storage bits with opposite result significance.
    PackedSlice {
        /// Packed source value.
        base: ProcessValueId,
        /// Written left endpoint.
        left: i64,
        /// Written right endpoint.
        right: i64,
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
        /// Table in [`Design::lookup_tables`](super::Design::lookup_tables).
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
    /// Explicit packed conversion: truncate or zero-extend the operand's raw
    /// bit pattern to this value's declared width. Numeric interpretation is
    /// carried by [`ProcessValue::ty`], not by the resize operation.
    RawResize {
        /// Value whose bit pattern is resized.
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
    /// A value-producing operation owned by the fixed simulation runtime.
    HostCall {
        /// Service selected before source syntax is discarded.
        operation: ProcessHostValueOp,
        /// Normalized service arguments in source order.
        arguments: Vec<ProcessValueId>,
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

/// Natural width of an arbitrary-precision little-endian integer literal.
///
/// Both procedural and representation-normalized hardware source values use
/// this rule, so it belongs with the canonical arena rather than either input
/// lowerer.
pub(crate) fn integer_words_width(words: &[u64]) -> Option<u32> {
    let high = words.last().copied().unwrap_or(0);
    let high_width = (64 - high.leading_zeros()).max(1);
    let lower = u32::try_from(words.len().saturating_sub(1))
        .ok()?
        .checked_mul(64)?;
    lower.checked_add(high_width)
}

/// Natural result width for a left shift whose count may fold in the arena.
pub(crate) fn shifted_arena_width(
    left: u32,
    right: ProcessValueId,
    values: &[ProcessValue],
) -> Option<u32> {
    let Some(shift) = arena_constant_integer(right, values).and_then(|value| value.try_into().ok())
    else {
        return Some(left);
    };
    left.checked_add(shift)
}

/// Whether an operation reads its operands as signed kernel integers.
fn process_binary_is_signed(operation: &ProcessBinaryOp) -> bool {
    matches!(
        operation,
        ProcessBinaryOp::SignedAdd
            | ProcessBinaryOp::SignedSub
            | ProcessBinaryOp::SignedMul
            | ProcessBinaryOp::SignedDiv
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
    )
}

/// Conservatively fold an integer-only Process value graph.
///
/// Dependencies precede their users, so no recursion guard is needed. Values
/// outside this closed integer subset deliberately return `None`.
pub(crate) fn arena_constant_integer(id: ProcessValueId, values: &[ProcessValue]) -> Option<i128> {
    let value = values.get(id.0 as usize)?;
    match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => {
            let mut result = 0i128;
            for &word in words.iter().rev() {
                result = result.checked_shl(64)?.checked_add(i128::from(word))?;
            }
            Some(result)
        }
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Neg,
            operand,
        } => arena_constant_integer(*operand, values)?.checked_neg(),
        ProcessValueKind::Binary {
            operation,
            left: left_id,
            right: right_id,
        } => {
            let mut left = arena_constant_integer(*left_id, values)?;
            let mut right = arena_constant_integer(*right_id, values)?;
            // A 64-bit operand of a signed operation is a kernel integer: its
            // top bit is the sign. Read unsigned, `0 - x'low` with
            // `x'low = -4` did not fold to 4, and `1 << (0 - x'low)` kept the
            // one-bit width of its literal.
            if process_binary_is_signed(operation) {
                let as_i64 = |id: ProcessValueId, value: i128| match values.get(id.0 as usize) {
                    Some(operand) if operand.bit_width == Some(64) => {
                        i128::from(value as u64 as i64)
                    }
                    _ => value,
                };
                left = as_i64(*left_id, left);
                right = as_i64(*right_id, right);
            }
            match operation {
                ProcessBinaryOp::Add | ProcessBinaryOp::SignedAdd => left.checked_add(right),
                ProcessBinaryOp::Sub | ProcessBinaryOp::SignedSub => left.checked_sub(right),
                ProcessBinaryOp::Mul | ProcessBinaryOp::SignedMul => left.checked_mul(right),
                ProcessBinaryOp::Div | ProcessBinaryOp::SignedDiv => left.checked_div(right),
                ProcessBinaryOp::Shl => left.checked_shl(right.try_into().ok()?),
                ProcessBinaryOp::Shr | ProcessBinaryOp::ArithmeticShr => {
                    left.checked_shr(right.try_into().ok()?)
                }
                ProcessBinaryOp::And => Some(left & right),
                ProcessBinaryOp::Or => Some(left | right),
                ProcessBinaryOp::Xor => Some(left ^ right),
                ProcessBinaryOp::Eq => Some(i128::from(left == right)),
                ProcessBinaryOp::Ne => Some(i128::from(left != right)),
                ProcessBinaryOp::Lt | ProcessBinaryOp::SignedLt => Some(i128::from(left < right)),
                ProcessBinaryOp::Le | ProcessBinaryOp::SignedLe => Some(i128::from(left <= right)),
                ProcessBinaryOp::Gt | ProcessBinaryOp::SignedGt => Some(i128::from(left > right)),
                ProcessBinaryOp::Ge | ProcessBinaryOp::SignedGe => Some(i128::from(left >= right)),
                ProcessBinaryOp::FloatAdd
                | ProcessBinaryOp::FloatSub
                | ProcessBinaryOp::FloatMul
                | ProcessBinaryOp::FloatDiv
                | ProcessBinaryOp::FloatEq
                | ProcessBinaryOp::FloatNe
                | ProcessBinaryOp::FloatLt
                | ProcessBinaryOp::FloatLe
                | ProcessBinaryOp::FloatGt
                | ProcessBinaryOp::FloatGe
                | ProcessBinaryOp::Custom(_) => None,
            }
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            let selected = if arena_constant_integer(*condition, values)? != 0 {
                then_value
            } else {
                else_value
            };
            arena_constant_integer(*selected, values)
        }
        ProcessValueKind::Attribute { base, attribute } if attribute == "length" => {
            values.get(base.0 as usize)?.bit_width.map(i128::from)
        }
        _ => None,
    }
}

impl ProcessIr {
    /// First-seen distinct signal reads of one canonical value graph. Walk
    /// arena identities once rather than recursively expanding a shared DAG.
    /// Malformed dependencies fail closed even before Design validation.
    pub fn signal_reads(&self, root: ProcessValueId) -> Result<Vec<SignalId>, String> {
        let mut pending = vec![root];
        let mut seen = HashSet::new();
        let mut signals = HashSet::new();
        let mut reads = Vec::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let value = self
                .values
                .get(id.0 as usize)
                .ok_or_else(|| format!("missing Process value {id:?}"))?;
            if let ProcessValueKind::Signal { signals: ids, .. } = &value.kind {
                for signal in ids {
                    if signals.insert(*signal) {
                        reads.push(*signal);
                    }
                }
            }
            for child in process_value_dependencies(&value.kind).into_iter().rev() {
                if child.0 >= id.0 {
                    return Err(format!(
                        "Process value {id:?} has non-dominating dependency {child:?}"
                    ));
                }
                pending.push(child);
            }
        }
        Ok(reads)
    }

    /// Append one already elaborated digital expression to the shared value
    /// arena. Children are emitted first, preserving the arena's dominance
    /// invariant. Source lowering uses it for representation-normalized
    /// expressions; it deliberately carries no frontend-only type text and
    /// does not consume a scheduler decomposition.
    pub(crate) fn push_digital_expr(
        &mut self,
        expression: &Expr,
        fallback_span: crate::diag::Span,
    ) -> ProcessValueId {
        let kind = match expression {
            Expr::Canonical { value, .. } => return *value,
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
                    UnOp::IntToReal => ProcessUnaryOp::IntegerToReal,
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
            bit_width: None,
            kind,
        });
        if !self.value_layouts.is_empty() {
            self.value_layouts.push(None);
        }
        id
    }

    /// Structural invariants shared by every process backend.
    pub fn validate(&self, signal_count: u32) -> Vec<String> {
        let mut issues = Vec::new();
        let mut test_names = HashSet::new();
        let mut test_roots = HashSet::new();
        let value_count = self.values.len() as u32;

        let invalid_native_duration = |value: ProcessValueId| -> Option<&'static str> {
            let value = self.values.get(value.0 as usize)?;
            match &value.kind {
                ProcessValueKind::Suffixed {
                    number: ProcessNumber::Integer(words),
                    ..
                } if words
                    .get(1..)
                    .is_some_and(|rest| rest.iter().any(|word| *word != 0)) =>
                {
                    Some("does not fit the native 64-bit femtosecond timeline")
                }
                ProcessValueKind::Suffixed {
                    number: ProcessNumber::Integer(_),
                    ..
                } => Some("exceeds the native 64-bit femtosecond timeline"),
                ProcessValueKind::Number(ProcessNumber::Integer(words))
                    if words
                        .get(1..)
                        .is_some_and(|rest| rest.iter().any(|word| *word != 0)) =>
                {
                    Some("does not fit the native 64-bit femtosecond timeline")
                }
                _ => None,
            }
        };

        if !self.value_layouts.is_empty() && self.value_layouts.len() != self.values.len() {
            issues.push(format!(
                "process value layout arena has {} entries for {} values",
                self.value_layouts.len(),
                self.values.len()
            ));
        }

        for (index, value) in self.values.iter().enumerate() {
            let retained = (self.value_layouts.len() == self.values.len())
                .then(|| self.value_layouts[index].as_ref())
                .flatten();
            if let Some(layout) = retained.or_else(|| process_value_declaration_layout(self, value))
            {
                match (layout.bit_width(), layout.packed_width()) {
                    (Some(_), None) => issues.push(format!(
                        "process value {:?} has a layout without a representable packed width",
                        ProcessValueId(index as u32)
                    )),
                    (_, Some(width)) if value.bit_width != Some(width) => issues.push(format!(
                        "process value {:?} declares packed width {:?}, but its layout requires {}",
                        ProcessValueId(index as u32),
                        value.bit_width,
                        width
                    )),
                    _ => {}
                }
            }
        }

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
            match process.region {
                ProcessRegion::Procedural => {}
                ProcessRegion::Combinational => {
                    if !matches!(process.activation, ProcessActivation::Reactive { .. }) {
                        issues.push(format!(
                            "combinational process {:?} is not reactively activated",
                            process.id
                        ));
                    }
                }
                ProcessRegion::Event {
                    condition, body, ..
                } => {
                    if condition.0 >= value_count {
                        issues.push(format!(
                            "event process {:?} references invalid condition {:?}",
                            process.id, condition
                        ));
                    }
                    if process.blocks.get(body.0 as usize).map(|block| block.id) != Some(body) {
                        issues.push(format!(
                            "event process {:?} references invalid body block {:?}",
                            process.id, body
                        ));
                    }
                    if !matches!(process.activation, ProcessActivation::Reactive { .. }) {
                        issues.push(format!(
                            "event process {:?} is not reactively activated",
                            process.id
                        ));
                    }
                    if !matches!(
                        process.blocks.get(process.entry.0 as usize).map(|block| &block.terminator),
                        Some(ProcessTerminator::Branch {
                            condition: entry_condition,
                            then_block,
                            ..
                        }) if *entry_condition == condition && *then_block == body
                    ) {
                        issues.push(format!(
                            "event process {:?} entry does not branch through its declared condition and body",
                            process.id
                        ));
                    }
                }
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
                            semantics,
                            target,
                            value,
                            ..
                        } => {
                            let snapshot = process_assignment_snapshot(self, *target);
                            let concatenated = matches!(
                                self.values.get(target.0 as usize).map(|value| &value.kind),
                                Some(ProcessValueKind::Concat(_))
                            );
                            let valid = snapshot.as_ref().is_some_and(|snapshot| {
                                !snapshot.roots.is_empty()
                                    && match semantics {
                                        ProcessAssignment::ImmediateLocal => {
                                            !concatenated
                                                && snapshot.roots.iter().all(|root| {
                                                    matches!(
                                                        root,
                                                        ProcessPlaceRoot::Local(owner, _)
                                                            if *owner == process.id
                                                    )
                                                })
                                        }
                                        ProcessAssignment::ImmediateStorage => {
                                            !concatenated
                                                && snapshot.roots.iter().all(|root| {
                                                    matches!(root, ProcessPlaceRoot::Storage(_))
                                                })
                                        }
                                        ProcessAssignment::StagedSignal => {
                                            !concatenated
                                                && snapshot.roots.iter().all(|root| {
                                                    matches!(root, ProcessPlaceRoot::Signal(_))
                                                })
                                        }
                                        ProcessAssignment::PerPlace => {
                                            concatenated
                                                && snapshot.roots.iter().all(|root| {
                                                    !matches!(
                                                        root,
                                                        ProcessPlaceRoot::Local(owner, _)
                                                            if *owner != process.id
                                                    )
                                                })
                                        }
                                    }
                            });
                            if !valid {
                                issues.push(format!(
                                    "process {:?} block {:?} has {:?} assignment to incompatible place {:?}",
                                    process.id, block.id, semantics, target
                                ));
                            }
                            if self
                                .values
                                .get(target.0 as usize)
                                .is_some_and(|target| target.bit_width.is_none())
                            {
                                issues.push(format!(
                                    "process {:?} block {:?} assignment snapshot target {:?} has no packed width",
                                    process.id, block.id, target
                                ));
                            }
                            if self
                                .values
                                .get(value.0 as usize)
                                .is_some_and(|value| value.bit_width.is_none())
                            {
                                issues.push(format!(
                                    "process {:?} block {:?} assignment snapshot value {:?} has no packed width",
                                    process.id, block.id, value
                                ));
                            }
                            if let Some(snapshot) = snapshot {
                                for index in snapshot.indices {
                                    if self
                                        .values
                                        .get(index.0 as usize)
                                        .is_some_and(|value| value.bit_width.is_none())
                                    {
                                        issues.push(format!(
                                            "process {:?} block {:?} assignment snapshot index {:?} has no packed width",
                                            process.id, block.id, index
                                        ));
                                    }
                                }
                            }
                        }
                        ProcessInstruction::Schedule { target, delay, .. } => {
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
                            if let Some(problem) = invalid_native_duration(*delay) {
                                issues.push(format!(
                                    "process {:?} block {:?} delayed assignment duration {problem}",
                                    process.id, block.id
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
                if let ProcessTerminator::Suspend {
                    operation: ProcessSuspendOp::AwaitTime,
                    arguments,
                    ..
                } = &block.terminator
                {
                    if let Some(problem) = arguments
                        .first()
                        .and_then(|delay| invalid_native_duration(*delay))
                    {
                        issues.push(format!(
                            "process {:?} block {:?} await duration {problem}",
                            process.id, block.id
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
            if value.bit_width == Some(0) {
                issues.push(format!("process value {:?} has zero packed width", id));
            }
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
                ProcessValueKind::HostCall {
                    operation,
                    arguments,
                } if arguments.len()
                    != match operation {
                        ProcessHostValueOp::Random | ProcessHostValueOp::Uniform => 0,
                        ProcessHostValueOp::RandomRange => 2,
                        ProcessHostValueOp::ReadUtf8
                        | ProcessHostValueOp::ReadUtf8Fixed
                        | ProcessHostValueOp::ReadBinary
                        | ProcessHostValueOp::FileExists
                        | ProcessHostValueOp::StringLength => 1,
                        ProcessHostValueOp::StringIndex | ProcessHostValueOp::StringEqualsUtf8 => 2,
                    } =>
                {
                    issues.push(format!(
                        "process value {:?} has the wrong host-service arity",
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
            let width = value
                .bit_width
                .map(|width| format!(" i{width}"))
                .unwrap_or_default();
            let ty = value
                .ty
                .as_ref()
                .map(|ty| format!(" : {ty:?}"))
                .unwrap_or_default();
            output.push_str(&format!("value %v{index}{width}{ty} = {:?}\n", value.kind));
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
                "process %p{} root {} owner {}{label} {:?} {:?} {{\n",
                process.id.0, process.root.0, process.owner.0, process.activation, process.region
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

/// Declaration-owned layout for a state-reference value. Aggregate literals
/// carry their own entry in `value_layouts`; locals and persistent storage
/// deliberately share the declaration's single recursive layout instead.
fn process_value_declaration_layout<'a>(
    process_ir: &'a ProcessIr,
    value: &ProcessValue,
) -> Option<&'a SourceLayout> {
    match &value.kind {
        ProcessValueKind::Storage(storage)
        | ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => process_ir.storages.get(storage.0 as usize)?.layout.as_ref(),
        ProcessValueKind::Local { process, local } => process_ir
            .processes
            .get(process.0 as usize)?
            .locals
            .get(local.0 as usize)?
            .layout
            .as_ref(),
        _ => None,
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
        ProcessInstruction::Runtime {
            arguments, format, ..
        } => arguments
            .iter()
            .copied()
            .chain(format.iter().flatten().filter_map(|part| match part {
                ProcessFormatPart::Text(_) => None,
                ProcessFormatPart::Value { value, .. } => Some(*value),
            }))
            .collect(),
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
        | ProcessValueKind::PackedSlice { base, .. }
        | ProcessValueKind::TableLookup { index: base, .. }
        | ProcessValueKind::Unary { operand: base, .. }
        | ProcessValueKind::RawResize { operand: base } => vec![*base],
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
        ProcessValueKind::ForeignCall { arguments, .. }
        | ProcessValueKind::HostCall { arguments, .. } => arguments.clone(),
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
        | ProcessValueKind::StorageState { .. }
        | ProcessValueKind::Signal { .. }
        | ProcessValueKind::Definition(_)
        | ProcessValueKind::Intrinsic(_)
        | ProcessValueKind::Default
        | ProcessValueKind::Invalid => Vec::new(),
    }
}

/// Remap arena operands without projecting a canonical node to expressions.
/// Leaves keep their declaration/storage identities; only value ids move.
pub(crate) fn remap_process_value_dependencies(
    value: &mut ProcessValueKind,
    mut map: impl FnMut(ProcessValueId) -> ProcessValueId,
) {
    let mut remap = |value: &mut ProcessValueId| *value = map(*value);
    match value {
        ProcessValueKind::Field { base, .. }
        | ProcessValueKind::Attribute { base, .. }
        | ProcessValueKind::BitSlice { base, .. }
        | ProcessValueKind::PackedSlice { base, .. }
        | ProcessValueKind::TableLookup { index: base, .. }
        | ProcessValueKind::Unary { operand: base, .. }
        | ProcessValueKind::RawResize { operand: base } => remap(base),
        ProcessValueKind::Index { base, index } => {
            remap(base);
            remap(index);
        }
        ProcessValueKind::CheckedIndex { index, valid, .. } => {
            remap(index);
            remap(valid);
        }
        ProcessValueKind::Range { left, right } => left.iter_mut().chain(right).for_each(remap),
        ProcessValueKind::Binary { left, right, .. } => {
            remap(left);
            remap(right);
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            remap(condition);
            remap(then_value);
            remap(else_value);
        }
        ProcessValueKind::MetaCompare {
            operands, inner, ..
        } => {
            operands.iter_mut().for_each(&mut remap);
            remap(inner);
        }
        ProcessValueKind::Match { scrutinee, arms } => {
            remap(scrutinee);
            for arm in arms {
                remap(&mut arm.value);
            }
        }
        ProcessValueKind::Call {
            callee, arguments, ..
        } => {
            remap(callee);
            arguments.iter_mut().for_each(remap);
        }
        ProcessValueKind::ForeignCall { arguments, .. }
        | ProcessValueKind::HostCall { arguments, .. } => arguments.iter_mut().for_each(remap),
        ProcessValueKind::Construct { fields, spread, .. } => {
            fields
                .iter_mut()
                .filter_map(|field| field.value.as_mut())
                .for_each(&mut remap);
            spread.iter_mut().for_each(remap);
        }
        ProcessValueKind::Concat(values) | ProcessValueKind::Array(values) => {
            values.iter_mut().for_each(remap)
        }
        ProcessValueKind::Number(_)
        | ProcessValueKind::Suffixed { .. }
        | ProcessValueKind::BitString { .. }
        | ProcessValueKind::Char(_)
        | ProcessValueKind::String(_)
        | ProcessValueKind::Local { .. }
        | ProcessValueKind::Storage(_)
        | ProcessValueKind::StorageState { .. }
        | ProcessValueKind::Signal { .. }
        | ProcessValueKind::Definition(_)
        | ProcessValueKind::Intrinsic(_)
        | ProcessValueKind::Default
        | ProcessValueKind::Invalid => {}
    }
}

/// Root storage class of an assignable process value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ProcessPlaceClass {
    Local,
    Storage,
    Signal,
}

/// Identity of a mutable root captured by one assignment. Projections retain
/// this identity so validation can reason about the atomic update without
/// knowing backend offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ProcessPlaceRoot {
    Local(ProcessId, ProcessLocalId),
    Storage(ProcessStorageId),
    Signal(SignalId),
}

impl ProcessPlaceRoot {
    fn class(self) -> ProcessPlaceClass {
        match self {
            Self::Local(_, _) => ProcessPlaceClass::Local,
            Self::Storage(_) => ProcessPlaceClass::Storage,
            Self::Signal(_) => ProcessPlaceClass::Signal,
        }
    }
}

/// Operands that must be captured before an assignment publishes any update.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessAssignmentSnapshot {
    roots: Vec<ProcessPlaceRoot>,
    indices: Vec<ProcessValueId>,
}

/// Recover the canonical mutable roots and index operands encoded by a target
/// value. A recursion guard keeps malformed hand-built IR diagnostic instead
/// of letting validation recurse forever; ordinary arena dominance already
/// guarantees production targets are acyclic.
fn process_assignment_snapshot(
    ir: &ProcessIr,
    target: ProcessValueId,
) -> Option<ProcessAssignmentSnapshot> {
    fn walk(
        ir: &ProcessIr,
        value: ProcessValueId,
        active: &mut HashSet<ProcessValueId>,
        snapshot: &mut ProcessAssignmentSnapshot,
    ) -> Option<()> {
        if !active.insert(value) {
            return None;
        }
        match &ir.values.get(value.0 as usize)?.kind {
            ProcessValueKind::Local { process, local } => {
                snapshot
                    .roots
                    .push(ProcessPlaceRoot::Local(*process, *local));
            }
            ProcessValueKind::Storage(storage) => {
                snapshot.roots.push(ProcessPlaceRoot::Storage(*storage));
            }
            ProcessValueKind::Signal {
                signals,
                state: ProcessSignalState::Current,
            } if !signals.is_empty() => snapshot
                .roots
                .extend(signals.iter().copied().map(ProcessPlaceRoot::Signal)),
            ProcessValueKind::Field { base, .. }
            | ProcessValueKind::BitSlice { base, .. }
            | ProcessValueKind::PackedSlice { base, .. } => {
                walk(ir, *base, active, snapshot)?;
            }
            ProcessValueKind::Index { base, index } => {
                walk(ir, *base, active, snapshot)?;
                snapshot.indices.push(*index);
            }
            ProcessValueKind::Concat(parts) if !parts.is_empty() => {
                for part in parts {
                    walk(ir, *part, active, snapshot)?;
                }
            }
            _ => return None,
        }
        active.remove(&value);
        Some(())
    }

    let mut snapshot = ProcessAssignmentSnapshot {
        roots: Vec::new(),
        indices: Vec::new(),
    };
    walk(ir, target, &mut HashSet::new(), &mut snapshot)?;
    (!snapshot.roots.is_empty()).then_some(snapshot)
}

/// Every root storage class written by an assignable process value.
fn process_place_classes(ir: &ProcessIr, value: ProcessValueId) -> Option<Vec<ProcessPlaceClass>> {
    Some(
        process_assignment_snapshot(ir, value)?
            .roots
            .into_iter()
            .map(ProcessPlaceRoot::class)
            .collect(),
    )
}

/// Preserve the fully selected arithmetic domain of a normalized digital
/// operation. Unlike source operators, these variants require no type lookup
/// or std dispatch in a backend.
pub(crate) fn process_binary_from_digital(operation: BinOp) -> ProcessBinaryOp {
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

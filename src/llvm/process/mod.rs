//! LLVM support for the canonical process representation.
//!
//! The design object owns the source-independent metadata boundary: the linked
//! runtime discovers tests and scheduled processes from immutable tables and
//! invokes the emitted Process IR entry points beside them.

use std::collections::{BTreeMap, HashMap};

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::{FloatPredicate, IntPredicate};

use siox::ir::{
    Design, IndexSite, LayoutDirection, LayoutKind, LayoutRange, ProcessActivation,
    ProcessAssignment, ProcessBinaryOp, ProcessCfg, ProcessDisplayKind, ProcessFormatPart,
    ProcessHostValueOp, ProcessId, ProcessInstruction, ProcessLocalId, ProcessNumber,
    ProcessPattern, ProcessRuntimeOp, ProcessSensitivity, ProcessSignalState, ProcessStorageId,
    ProcessTerminator, ProcessUnaryOp, ProcessValueId, ProcessValueKind, SignalId, SourceLayout,
};

mod aggregate_logic;
mod binary;
mod bindings;
mod blocks;
mod entry;
mod flags;
mod hardware;
mod instructions;
mod logic;
mod loops;
mod metadata;
mod names;
mod places;
mod slices;
mod state;
mod support;
mod tables;
mod value_types;
mod values;
mod writes;

use aggregate_logic::*;
use binary::*;
use bindings::*;
use blocks::*;
use entry::*;
use flags::*;
pub(super) use hardware::{HardwareValueCache, HardwareValueFacts};
use instructions::*;
use logic::*;
use loops::*;
pub(super) use metadata::emit_metadata;
use names::*;
use places::*;
use slices::*;
pub(super) use state::declare_state;
pub(super) use state::validate_metadata_widths;
use state::*;
use support::*;
use tables::*;
use value_types::*;
use values::*;
use writes::*;

/// Version of the native process metadata ABI emitted into every object.
///
/// This is deliberately a data version rather than the compiler package
/// version: the reusable native runtime only needs to change when one of the
/// exported table layouts or encodings changes.
const PROCESS_ABI_VERSION: u32 = 14;

/// A process returned normally and has no pending resume.
const PROCESS_COMPLETED: u8 = 0;
/// The process yielded after registering a runtime resume operation.
const PROCESS_SUSPENDED: u8 = 1;
/// The process stopped itself while leaving the simulation alive.
const PROCESS_STOPPED: u8 = 2;
/// The process requested termination of the complete simulation.
const PROCESS_FINISHED: u8 = 3;
/// The process yielded until all reactive work caused by its foreground drive
/// reaches a fixed point at the current simulation time.
const PROCESS_SETTLING: u8 = 4;
/// This migration build encountered an instruction/terminator whose direct
/// lowering is not implemented yet. The runtime must report this, never treat
/// it as successful completion.
const PROCESS_UNSUPPORTED: u8 = u8::MAX;

#[cfg(test)]
mod tests;

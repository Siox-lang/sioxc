//! Type system and kind checking for siox Phase 1 (spec Stage 4).
//!
//! Checks primitive digital types (`Bit`, `Logic`, `Bool`), integer widths
//! (`uint[N]`, `int[N]`), structs, enums, arrays/vectors, entity types,
//! directional views and bus modes, function/method signatures, trait bounds,
//! attribute value typing, and pattern typing.
//!
//! Key Phase 1 rules to enforce:
//! - system attributes `::event`/`::old` exist on every digital value
//!   (spec 3.9), and range attributes `::width/::range/::high/::low/::left/
//!   ::right/::direction` on range-like values (spec 3.23)
//! - `::ddt` is rejected as Phase-2 analogue syntax (spec Stage 4)
//! - no implicit broad conversions (spec 3.17): `uint[8]` !-> `uint[16]`
//! - cannot write to `in` ports inside an entity (spec 3.18 / code E-P004)
//! - `Logic` is not a bare condition without comparison (spec 3.16)

use siox_diag::DiagnosticSink;
use siox_resolve::Resolved;
use siox_syntax::Module;

/// A checked, interned type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Bit,
    Logic,
    Bool,
    /// `uint[N]` / `int[N]` with a resolved width.
    UInt(u32),
    Int(u32),
    /// Named struct / enum / entity, keyed by its definition.
    Named(siox_resolve::DefId),
    /// `T[range]` array/vector of a digital element type.
    Array { elem: Box<Ty>, len: u32 },
    /// Placeholder for an as-yet-unresolved/error type.
    Error,
}

impl Ty {
    /// Whether `::event` / `::old` apply (spec 3.9). True for all digital and
    /// discrete values, structs of digital fields, arrays, and enums.
    pub fn is_digital(&self) -> bool {
        // TODO(stage-4): recurse into Named structs to confirm all-digital.
        !matches!(self, Ty::Error)
    }
}

/// Outcome of type checking: a type for every expression/signal, ready for the
/// elaborator and IR lowering.
#[derive(Default)]
pub struct Typed {
    // TODO(stage-4): expr -> Ty map, signal/port types, method resolution.
}

/// Type-check resolved modules.
pub fn check(_modules: &[Module], _resolved: &Resolved, _sink: &mut DiagnosticSink) -> Typed {
    // TODO(stage-4): traverse items, infer/check expressions and statements.
    todo!("Stage 4: type checking")
}

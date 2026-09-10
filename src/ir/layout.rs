//! Source-preserving type and aggregate layout metadata.

/// An inclusive source range in written order. `left > right` is descending;
/// layout never sorts the endpoints because direction is observable through
/// the language's range attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutRange {
    /// The written left bound, which may exceed `right` for a descending
    /// range such as `31..0`.
    pub left: i64,
    /// The written right bound.
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

    /// Whether the range covers no elements. A written range always covers at
    /// least one, so this is always false; it exists so consumers can ask.
    pub fn is_empty(self) -> bool {
        false
    }

    /// Whether the range counts upward (`0..7`) rather than downward (`7..0`).
    pub fn ascending(self) -> bool {
        self.left <= self.right
    }
}

/// The representation semantics of one scalar storage leaf. Nominal identity
/// remains on `LayoutKind::Scalar`; this enum describes how engines interpret
/// its bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScalarDomain {
    /// A plain bit vector with no arithmetic interpretation of its own.
    Bits,
    /// The signed kernel `integer`.
    Integer,
    /// A `real`; the slot holds an f64 bit pattern.
    Real,
    /// A `Char`; the slot holds a Unicode code point.
    Character,
    /// An enum, named so consumers can render a discriminant as its variant.
    Enum(String),
}

/// A port direction carried by an applied view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutDirection {
    /// Driven by the instantiator.
    In,
    /// Driven by the entity.
    Out,
    /// Driven from either side and resolved.
    InOut,
}

/// One field within a [`LayoutKind::Struct`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutField {
    /// The field's name as written in the source type.
    pub name: String,
    /// Direction supplied by an applied view. Ordinary struct fields have no
    /// direction; permissions belong to the connection using the layout.
    pub direction: Option<LayoutDirection>,
    /// The field's own recursive layout.
    pub layout: SourceLayout,
}

/// A frontend-independent, recursively complete layout for one concrete source
/// value. `span` anchors diagnostics; `kind` retains the distinction between a
/// packed vector (one signal) and an ordinary repeated array (one layout per
/// element), which a bit count alone cannot recover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLayout {
    /// The declaration this layout was derived from.
    pub span: crate::diag::Span,
    /// The shape itself.
    pub kind: LayoutKind,
}

/// The shape of a source value, retained after flattening so consumers can
/// rebuild what the user declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutKind {
    /// A single storage leaf.
    Scalar {
        /// Bit width.
        width: u32,
        /// How the bits are interpreted.
        domain: ScalarDomain,
        /// The declared type's name, when it has a nominal identity.
        nominal: Option<String>,
        /// Dynamic value constraint for ranged numerics, not an index range.
        value_range: Option<(i64, i64)>,
    },
    /// A source array represented by one packed signal.
    Packed {
        /// Total bit width of the packed signal.
        width: u32,
        /// The nominal vector family, such as `unsigned`.
        family: String,
        /// The declared index range, when it is known.
        range: Option<LayoutRange>,
        /// The element enum, when elements render as variants.
        element_enum: Option<String>,
    },
    /// A source array represented recursively (and currently flattened into
    /// one signal per scalar leaf).
    Array {
        /// The declared index range, when it is known.
        range: Option<LayoutRange>,
        /// The element's own layout, applied at every index.
        element: Box<SourceLayout>,
    },
    /// A source struct, flattened into one signal per scalar leaf.
    Struct {
        /// The struct type's name.
        name: String,
        /// Applied directional view, when this value was declared through one.
        view: Option<String>,
        /// The fields, in declaration order.
        fields: Vec<LayoutField>,
    },
    /// A best-effort placeholder for an unresolved/parametric source type.
    /// Keeping it in the tree is more useful to diagnostics and tools than
    /// silently dropping that branch.
    Opaque {
        /// The rendered type name, so diagnostics can still say what it was.
        name: String,
        /// The width, when even that much is known.
        width: Option<u32>,
    },
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

    /// The declared index range for an indexable layout, or `None` for a
    /// scalar or an unranged one.
    pub fn index_range(&self) -> Option<LayoutRange> {
        match &self.kind {
            LayoutKind::Packed { range, .. } | LayoutKind::Array { range, .. } => *range,
            _ => None,
        }
    }
}

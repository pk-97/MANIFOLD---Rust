//! Parameter types exposed by an [`EffectNode`](crate::node_graph::EffectNode).
//!
//! Parameters are the user-facing knobs on a node: numeric values, booleans,
//! vectors, colours, enums. Each `EffectNode` declares its parameters
//! statically via `EffectNode::parameters()`. Per-instance values live
//! separately in `ParamValues` and are supplied to the node each frame.
//!
//! Parameters are also the public surface of a graph: the per-parameter
//! "expose" flag (V2 graph editor) decides which internal node parameters
//! become slots on the outer effect card. The modulation system targets
//! exposed parameters by name, identical to today's effect parameters.
//!
//! ## Numeric storage is `Float` only
//!
//! There used to be separate `Float` and `Int` variants on [`ParamValue`].
//! That asymmetry was the source of a class of silent bugs: any reader
//! that pattern-matched on `Float` only would fall through to a default
//! whenever a value was constructed as `Int` (e.g. from a JSON preset that
//! declared `{"type":"Int","value":N}`). The slider would move and nothing
//! visible would change.
//!
//! The fix: numbers live in one cell — `Float(f32)`. `f32`'s 24-bit mantissa
//! exactly represents every integer in our param ranges, so there's no
//! precision loss. [`ParamType::Int`] survives on [`ParamDef`] as a
//! *presentation* hint — it tells the editor to render a whole-number
//! stepper, format without decimals, and round-on-drag — but storage
//! doesn't need to redundantly enforce the constraint.

/// Type of a parameter knob on an [`EffectNode`](crate::node_graph::EffectNode).
///
/// `Int` is a *presentation* hint, not a storage discriminator: the editor
/// uses it to choose a whole-number stepper widget and integer-formatted
/// display. The actual value lives in `ParamValue::Float` like every other
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamType {
    Float,
    Int,
    Bool,
    Vec2,
    Vec3,
    Vec4,
    Color,
    /// Index into the parameter definition's `enum_values` list.
    Enum,
}

/// Runtime value of one parameter.
///
/// Numeric storage collapses to a single `Float(f32)` cell — see the
/// module-level docs for the rationale. Use [`ParamValue::as_scalar`] /
/// [`ParamValue::as_u32_clamped`] when reading; they're the single
/// point of truth for coercion and shield primitives from the "did
/// this come in as Int or Float" question that no longer exists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamValue {
    Float(f32),
    Bool(bool),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    Color([f32; 4]),
    Enum(u32),
}

impl ParamValue {
    /// Coerce to `f32` for scalar reads.
    ///
    /// Returns `Some` only for `Float` — `Bool`/`Enum` are intentionally
    /// not auto-coerced because they're not "numbers" in the user's mental
    /// model (toggles and dropdowns). If a primitive genuinely wants a
    /// 0.0/1.0 from a Bool param, it should match `Bool` explicitly so the
    /// intent is visible at the call site.
    #[inline]
    pub fn as_scalar(&self) -> Option<f32> {
        match self {
            ParamValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Read as a `u32`, rounding then clamping to `min..`.
    ///
    /// Replaces the recurring pattern (and the bug class it spawned — a
    /// pre-collapse reader that handled only one of the old `Int` /
    /// `Float` storage variants silently fell through for the other):
    /// ```ignore
    /// match params.get("foo") {
    ///     Some(ParamValue::Float(f)) => f.round().max(min as f32) as u32,
    ///     _ => default,
    /// }
    /// ```
    /// One helper, one truth, no `Int` branch to forget.
    #[inline]
    pub fn as_u32_clamped(&self, min: u32) -> Option<u32> {
        self.as_scalar()
            .map(|f| f.round().max(min as f32) as u32)
    }
}

/// Static description of one parameter on an
/// [`EffectNode`](crate::node_graph::EffectNode).
#[derive(Debug, Clone)]
pub struct ParamDef {
    /// Stable parameter name. Wire-key for modulation bindings and the
    /// save format. Treated as public API once shipped — never rename in place.
    pub name: &'static str,

    /// Human-readable label for the effect card UI. Free to change.
    pub label: &'static str,

    pub ty: ParamType,
    pub default: ParamValue,

    /// Numeric range `(min, max)`. Applied to `Float` and `Int` types only.
    pub range: Option<(f32, f32)>,

    /// Discrete options for `ParamType::Enum`. Empty for non-Enum types.
    pub enum_values: &'static [&'static str],
}
